# Forks and patches

This page tracks local changes to third-party Rust and Kotlin packages and the patches used for native libzim and Xapian comparison builds. The app also builds rustls-platform-verifier's Android component from vendored source. Each one has to be re-applied, by hand, on every upstream upgrade. This page lists what each change does and why. It also records whether the change fixes a real upstream bug or is a Kiwix-specific policy, and what it would take to drop it.

Surveyed 2026-11-05 against libzim `main` at ad888b4 (2026-10-19, after 9.8.2), tauri/tauri-runtime-wry 3.0.0-alpha.5, wry 0.57.1, mdns-sd 0.21.7 and librqbit-dualstack-sockets 0.7.0. An independent fact-check against those sources corrected the first draft; the corrections are included here.

## Summary

| Fork | Size | Upstream bug? | Still present upstream? | Recommendation |
|---|---|---|---|---|
| libzim: retained-fd | 926 lines | Yes, several, including one that disables lazy full-text search | Yes | Split into separate upstream PRs; the lazy-search fix is one line |
| libzim: zstd cache accounting | 92 lines | Yes: branches swapped | Yes | Upstream, together with the `getMemorySize` lock |
| libzim: decoded-cluster retirement | 266 lines | Memory waste, plus a lock for an existing race | Yes | Upstream with the zstd fix |
| libzim: LP32 raw blob | 80 lines | Yes: 32-bit silent wrong data | Yes | Upstream PR |
| libzim: split-ranges | 68 lines | Edge cases only (zero-size and end-of-file reads) | Yes | Upstream the edge cases; do not claim a lookup bug |
| Xapian: fd-offset | 581 lines | Feature gap, not a bug | n/a | Propose upstream; needed only for fd-based search |
| tauri-runtime-wry | 4 lines | Yes: Linux incognito IPC | Yes, in 3.0.0-alpha.5 | Upstream PR to tauri-apps/tauri |
| wry | ~100 lines | Apple media policy; Windows: permission persistence and accelerator forwarding (a real bug) | n/a | Keep the policy; report accelerator forwarding upstream |
| tauri-android | ~200 lines | Yes: Activity references kept after recreation | Not re-checked against `dev` | Upstream PR to tauri-apps/tauri |
| html5ever and markup5ever | Five Rust source and test files across two crates | Kiwix memory-safety hardening | Published 0.39.0 uses intrinsics and unchecked slices | Keep the original crates, token behavior, bounded consumer tests and platform measurements with the forks |
| cssparser | Five changed Rust source files | Memory-safety hardening | Audited at 0.37.0 | Keep the source and fixtures with the app and compare CSS behavior on upgrades |
| mdns-sd | ~1,900 lines | Hardening against hostile LAN packets | Partly fixed in 0.21 | Rebase onto 0.21.7, then offer the cache bounds upstream |
| librqbit-dualstack-sockets | iOS binding and checked interface handling | Yes: does not build for iOS | Yes, in 0.7.0 | Upstream PR; trivial |
| rustls-platform-verifier (Android Kotlin) | Unchanged source; build files only | No: vendored instead of upstream's GitHub-hosted Maven archive | n/a | Keep pinned to Cargo.lock; re-vendor on every crate update |

## libzim (C++)

The app links a pinned libzim 9.8.2 built with five patches (`src-tauri/native/libzim-*.patch`). Every SDK applies split-ranges and retained-fd. Android also applies zstd-accounting and decoded-cluster-retirement, and ARMv7 also applies LP32 (`scripts/build-*-sdk.py`). POSIX SDKs add the Xapian patch; Windows does not.

### retained-fd: opening archives from file descriptors

Mobile platforms hand the app a file descriptor, not a reopenable path: Android's Storage Access Framework, and iOS security-scoped files. libzim has an `FdInput` API, but its implementation turns each descriptor into the path `/dev/fd/N` and keeps re-resolving that path. The patch fixes the following:

1. **Lazy full-text search is never loaded (one line).** With `OpenConfig().preloadXapianDb(false)`, `FileImpl::getXapianDb()` calls `loadXapianDb()` and discards the result (`src/fileimpl.cpp:914`; the eager path at line 283 assigns it). Every lazily opened archive reports no full-text index. This app always opens lazily, so without this hunk search would not work at all. It is the smallest and most clearly correct upstream PR in this list.
2. **Checksum verification reopens each part by name.** It uses `std::ifstream("/dev/fd/N")` and also ignores `FdInput`'s offset and size, so archives embedded at an offset in a larger file are hashed wrongly. The patch verifies through libzim's own reader. (The app also checks checksums itself over retained handles.)
3. **The Xapian indexes are opened by re-resolving `/dev/fd/N`, seeking, then `Xapian::Database(fd)`.** On Darwin, opening `/dev/fd/N` duplicates the descriptor and shares its file offset; on Linux it does not. Stock Xapian also seeks the descriptor on each version read. The resulting race is between concurrent Xapian opens: the title index opens once per `SuggestionSearcher` without a shared lock, and the full-text index under its own mutex. Callers that rely on the fd's position are also affected; ordinary content reads use `pread` and are not. The patch passes Xapian a private duplicate plus an absolute offset (this needs the Xapian patch below).
4. **Anything that re-resolves `/dev/fd/N` later breaks once the caller closes or reuses descriptor N.** That covers the checksum, a lazy Xapian open and modification-time lookups. The patch duplicates descriptors with `F_DUPFD_CLOEXEC` and owns the copies.
5. **Smaller defects:** `fstat` failures are ignored and the uninitialized result is used. `getMTime` reads an uninitialized field and stats the path. The FD move-assignment leaks the old descriptor. `FileCompound::addPart` leaks parts if a constructor throws after some were added, and does not check sizes for overflow. A zero-size part silently drops the following part from the map. `FdInput` extents are not validated.

Behavior changes to note when upstreaming: the public `getDirectAccessInformation` now reports "no direct access" for fd-only archives instead of a synthesized `/dev/fd/N` name, and zero-size parts are rejected.

Suggested PR split: (1) the one-line lazy-load fix; (2) `fstat`, `getMTime`, FD move and `addPart` hygiene; (3) owned duplicates of descriptors; (4) checksum through the reader; (5) Xapian open from descriptor plus offset, together with the Xapian proposal.

### zstd-cache-accounting: memory accounting uses the wrong stream

`ZSTD_INFO::state_size` has its branches swapped (`src/compression.cpp:179-185`). When a decoder exists it calls `ZSTD_sizeof_CStream(encoder_stream)` on a null pointer, which returns 0, so the cluster cache never counts decoder memory. A fresh zstd decoder is about 96 KB, and the undercount per cluster is capped by that cluster's decompressed size. That matters most on low-memory phones holding many cached clusters.

**Ship this together with the retirement patch's lock, not alone.** The cache computes an entry's cost *after* it has published the cluster to other threads (`src/concurrent_cache.h:147-156`). With correct accounting, `getMemorySize` then reads the live zstd decoder while another thread may be decoding with it.

### decoded-cluster-retirement: free decompressors once a cluster is fully decoded

After every blob in a compressed cluster has been decoded into its own buffer, libzim keeps the decompressor stream alive. The patch releases it, which saves memory on mobile. It also takes the cluster's reader lock in `Cluster::getMemorySize()`. That method already races upstream: the cache computes cost after publication, and for LZMA it calls `lzma_memusage` while another thread decodes. The lock fixes the existing race, and the race the zstd fix would otherwise widen.

### LP32-raw-blob: blobs over 4 GiB on 32-bit

On 32-bit builds (ARMv7 Android), a blob larger than `SIZE_MAX` is replaced by an empty buffer, even in an uncompressed cluster where it is only a file range. The raw stream is never advanced past it, so every following blob in that cluster silently reads the wrong bytes, and bounded reads of the oversized blob fail. The patch keeps 64-bit file ranges for uncompressed blobs. This is rare (multi-GB single media files), but it is a real silent-corruption bug on 32-bit.

### split-ranges: edge cases in multi-part range lookup

The comparator `less_range` is not a strict weak ordering when ranges overlap. Upstream already replaced the `equal_range` call that broke on libc++. With the remaining `lower_bound`/`upper_bound` calls, an exhaustive randomized test (about 1.46 million part layouts and queries, against both libc++ and libstdc++) found **no wrong lookups**. So the patch does not fix a demonstrated wrong-part read.

What it does fix is edge cases: zero-size reads, and reads ending exactly at the end of the archive, where `get_mmap_buffer` can increment past `end()`. It also adds bounds assertions and orders ranges by start offset. Upstream it as an edge-case and robustness fix, not as a lookup bug.

### Xapian fd-offset (`xapian-fd-offset.patch`)

This adds `Xapian::Database::open_fd_at(fd, offset)`, which opens a single-file Glass database at an absolute offset with `pread`, never touching the descriptor's position. Xapian 1.4.32's `Database(int fd)` takes its base offset from `lseek(fd, 0, SEEK_CUR)` and seeks again on version reads. This is a feature request rather than a bug fix, and it is only needed for fd-based search (retained-fd item 3).

## Replacing libzim with zimru

[zimru](https://github.com/jasontitus/zimru) is a clean-room, MIT-licensed Rust ZIM reader/writer. A separate `libzim-shim` (not yet public) exposes libzim's C++ API (`zim::Archive`, `zim::Searcher`, `zim::SuggestionSearcher`, …) on top of zimru and Xapian. [xapianbuilder](https://github.com/jasontitus/xapianbuilder) builds indexes only and has no query API, so it is not part of the read path. Nothing in this repository's history mentions zimru, so why the original implementation chose libzim was never recorded.

### What the app needs from a ZIM engine

`src-tauri/native/archive.cpp` uses a small slice of libzim:

- open by path, or from a vector of `FdInput` (whole or split archives, already-open descriptors);
- `OpenConfig().preloadXapianDb(false)`;
- entry lookup, main and random entries, `iterByTitle` (title batches, not Xapian);
- metadata, illustrations, counts, UUID and file size;
- `Searcher`/`Query` with snippets, scores and estimated match counts;
- `SuggestionSearcher`;
- `checkIntegrity(CHECKSUM)`.

### Gaps today

| Need | zimru | libzim-shim |
|---|---|---|
| Path open, lookup, metadata, illustrations, random, title iteration | Yes | Yes |
| Full-text and title search | Exposes direct-access offset and size for the index blobs (`blob_direct_access`), but has no query engine | Yes: Xapian-backed `Searcher` and `SuggestionSearcher` (the no-title-index fallback is pending shim wiring) |
| Open from a descriptor | Only by memory-mapping it (`Archive::from_mmap`) | No `FdInput` API |
| Read without mmap (`pread`) | No | No |
| Split archives (`.zimaa`, …) | No (join first) | No |
| `OpenConfig` | n/a | No |
| `checkIntegrity` | Checksum exists (`check()`) | Not under libzim's name |
| Xapian opened without the path | Offset and size only | Re-opens the ZIM by path, then `lseek` |

Memory-mapping is the main platform problem. A 32-bit ARMv7 process cannot map a multi-GB archive. A mapped file on removable or provider-backed storage can also raise `SIGBUS` if it is truncated or the storage disappears.

### Recommended path

Use zimru directly from Rust for archive reading, and keep Xapian only for search. That removes the C++ bridge and all five libzim patches from the most attacker-exposed code: parsing untrusted ZIM files.

1. **zimru:** add a `pread`-based reader over one or more file descriptors (a split archive is an ordered list of descriptors) next to the mmap path, and use it on 32-bit and for provider-backed files. Add multi-part support on the same reader abstraction.
2. **Search:** reuse libzim-shim's Xapian search code (`src/search.cpp`, `src/suggestion.cpp`; GPL, compatible with this GPL-3.0 app) as a small C++ search-only bridge. Feed it zimru's direct-access offsets and a private duplicate descriptor. Use the Xapian `open_fd_at` patch, or upstream it, so the bridge never re-resolves paths or shares an offset.
3. **Migrate in stages:** put zimru behind a Cargo feature for reads and checksums, and compare against libzim with the existing tests and fixtures (including split and fd-only archives). Then move search, then remove libzim.

Alternative: extend libzim-shim with `FdInput`, `OpenConfig` and `checkIntegrity` and link it instead of libzim, leaving `archive.cpp` unchanged. That needs the same zimru reader work, keeps a C++ layer, and gives up most of the memory-safety benefit.

## Rust and Android forks

### tauri-runtime-wry: secondary incognito windows lose IPC on Linux

On Linux, Tauri registers the app's custom URI schemes (`tauri://`, `ipc://`) once per WebKitGTK web context and skips any scheme it has already registered. In incognito mode, wry 0.57 creates a fresh ephemeral context for each webview, so every window after the first gets no `ipc://` scheme and its frontend cannot call the backend. The patch always registers schemes for incognito webviews. This is a real upstream bug and is unchanged in tauri-runtime-wry 3.0.0-alpha.5: a good, small upstream PR.

### wry: media policy on Apple platforms

- **Autoplay default.** When autoplay is off, the patch requires a user action for audible media but lets silent video autoplay. That is Kiwix policy, not a bug. To upstream it, propose a configurable media-autoplay attribute.
- **Picture-in-picture.** Upstream sets the private macOS key `allowsPictureInPictureMediaPlayback` on `WKPreferences` for every Apple target. The patch limits that to macOS and sets the public `WKWebViewConfiguration` property on iOS, which already defaults to YES. This is tidier, but it is not a demonstrated upstream bug.
- The patch also moves `allowsInlineMediaPlayback` into the iOS block; the effect is unchanged.

### wry: WebView2 on Windows (added 2026-11-15/16)

- **Permission decisions are not saved in the profile.** The permission handler calls `SetSavesInProfile(false)`, so no Allow outlives the app's own decision (the photo chooser's camera window depends on this).
- **Menu accelerators while a page has focus.**
  - **Problem:** keys typed into WebView2 go to the browser's window, never through the message loop where Tauri runs `TranslateAcceleratorW`, so app menu shortcuts never fired on Windows. WebView2's own browser accelerators (Ctrl+P, Ctrl+R, Alt+Left/Right, F11) also handled the same keys.
  - **Patch:** the vendored default disables WebView2's browser accelerator keys, and `forward_accelerator_keys` re-posts these key-downs to the top-level window:
    - Ctrl key-downs, except the Ctrl+A/C/V/X/Y/Z editing keys, which the browser handles and muda's predefined Edit items re-synthesize;
    - the app's Alt keys (0–9, Home, Left, Right);
    - function keys other than F10.

    Ctrl+Alt (AltGr) and auto-repeat are skipped.
  - **Upstream:** the Alt allowlist is Kiwix-specific, so upstream would want a configurable accelerator set or a forwarding callback. This is a real upstream bug worth reporting.
  - **Companion change:** `menus.rs` falls back to the foreground window, because tao reports a single-webview window as unfocused while WebView2 has focus.

### tauri-android: Activity lifetime

Tauri's Android plugins (`Plugin`, `AppPlugin`, `PathPlugin`) hold a strong reference to the Activity that created them. This app's manifest already handles orientation, size, layout and UI-mode changes without recreating the Activity, and process restore creates fresh plugins. So the remaining triggers are configuration changes not listed in the manifest, the system destroying the Activity while the process lives, and a second Activity.

In those cases the plugins keep the dead Activity alive. The new Activity also gets no back-press handler, so Back does the system default instead of web history or the app's back-button event.

The fork holds the Activity and WebView weakly and rebinds on resume and on WebView creation (a new `onAppWebViewCreated` hook that `MainActivity` calls). It keeps one-shot activity-result and permission callbacks per Activity and clears them on destroy, uses the application context for path lookups, and makes `exit` reject when no Activity is available. These are real upstream issues; re-check against tauri `dev` before sending a PR.

### cssparser

The app vendors cssparser 0.37.0 under MPL-2.0 in `src-tauri/vendor/cssparser-0.37.0`. The registry archive SHA-256 is `8c9cdaae01d5ed7882b04d795e7f752f46ff52d2fa3b50a20d28c464510bba98`. Its recorded upstream commit is `4c49486494fb24dc01390e3baca9698ef1744c71`. The additional test inputs under `src/css-parsing-tests` and `src/big-data-url.css` come from that same revision. The CSS parsing test data includes its original CC0 notice. [Upstream source](https://github.com/servo/rust-cssparser/tree/4c49486494fb24dc01390e3baca9698ef1744c71)

Application resources include the complete matching source, manifests, upstream fixtures and license under `licenses/cssparser/source`, with a notice alongside the directory.

The local edits in `lib.rs`, `cow_rc_str.rs`, `tokenizer.rs`, `serializer.rs` and `macros.rs` remove raw reference-count manipulation and unchecked byte/string access. The previously hidden macro helpers have changed with the exported macro expansion. On upgrades, run the upstream test suite and the app's export and PDF tests, then compare token streams, source spans and output on real stylesheets. [App call sites and measurements](HTML-PARSER-SAFETY.md#css-export-and-pdf-token-handling)

### mdns-sd: bounds against hostile LAN traffic

Nearby sharing listens to mDNS from any device on the LAN. The fork (based on 0.17.2) bounds cache entries, owners, payload sizes, reverse indexes, pending removals, resolver/retry/probe state, packet-processing work per turn and subscriber queues. It also adds cycle and length bounds to DNS name decompression and makes TTL arithmetic overflow-safe. Without the cache bounds, a hostile device on the same network can grow memory without limit or stall the daemon.

Upstream 0.21.7 now caps expanded names at 255 bytes (which stops compression loops), uses saturating TTL arithmetic, fixes a truncated-HINFO panic and limits outgoing packet size. It has no cache-size limits. Plan: rebase onto 0.21.7, drop the parser and TTL changes it covers, and offer the cache and queue bounds upstream.

### librqbit-dualstack-sockets: iOS interface binding

On iOS the crate compiles the Linux `bind_device` path, which socket2 provides only on Android, Fuchsia and Linux, so the crate does not build for iOS. The patch extends the macOS index-based (`IP_BOUND_IF`) branch to iOS. The original iOS fix changes two platform conditions.

The app fork now also forbids unsafe Rust across the socket crate. Named interface selection uses the same typed Unix name-to-index lookup as the nearby route code. It checks the retained UTF-8 name and preserves socket2's platform-specific interface binding. Tests configure tracing directly without changing the process environment. The upstream Apache-2.0 notice applies to this fork. Both the app and the fork select nix 0.32.0 for the interface APIs. Version 0.31.3's network feature references macOS-only constants on iOS. Version 0.32.0 guards those APIs and the fork compiles for both ARM64 iOS devices and simulators.

### rustls-platform-verifier: Android JVM component

librqbit's reqwest 0.13 client verifies HTTPS through `rustls-platform-verifier`, which on Android calls `org.rustls.platformverifier.CertificateVerifier` over JNI. Upstream ships that Kotlin class as an AAR in a Maven repository hosted on GitHub. This app instead builds it from `src-tauri/vendor/rustls-platform-verifier-android-0.2.0`, which holds the source from the commit that published both crates (`252e2516`, tag `v/0.7.1`). `upstream-provenance.json` records that commit and the file hashes. The Kotlin source is unchanged. The Gradle module, the consumer keep rule and the manifest differ, and upstream's network-security-config resource is kept outside `res/` (see below).

Upstream's library manifest also sets an application-wide `networkSecurityConfig` that permits cleartext to CA CRL hosts. The app's own `network_security_config.xml` overrides a library resource of the same name, so the app copies upstream's 376 hosts into its own config. On an API 24 emulator, a CRL-only certificate failed as `Revoked` while those hosts were blocked, and verified once they were allowed. `scripts/test-android-platform-verifier.mjs` fails if Cargo.lock moves either crate, a vendored file changes, or the host list drifts from upstream's. Runtime evidence and steps: `scripts/android-https-tracker-probe/README.md`.

## Upstreaming order

1. libzim: the one-line lazy `loadXapianDb()` result fix.
2. librqbit-dualstack-sockets: iOS binding and the subsequent checked interface handling.
3. tauri-runtime-wry: incognito scheme registration (four lines).
4. libzim: LP32 raw-blob fix, with its test.
5. libzim: `getMemorySize` lock, then the zstd `state_size` swap. Never the swap alone.
6. libzim: retained-fd hygiene PRs, then owned descriptors, checksum through the reader, and Xapian descriptor-plus-offset with the `open_fd_at` proposal.
7. libzim: split-range edge cases.
8. tauri-android: Activity lifetime.
9. mdns-sd: cache bounds, after rebasing onto 0.21.7.

Each fork that lands upstream removes a re-application step from every future upgrade. Moving reading to zimru (above) would retire the libzim items entirely.

## Linux AppImage runtime

The x86-64 AppImage uses a fork of the type-2 runtime at `fedfad51acc2860b2916a1eef32ba4a500ff31ef`. The fork creates private extraction directories, uses Ubuntu’s system extractor and validates portable profile directories. The archived source and bootstrap binary hashes, linked components and licensing material are recorded in [the runtime provenance](../upstream/appimage/SOURCE.txt). The package launcher runs the application against the installed Linux desktop stack. [Linux package instructions](PACKAGING.md#linux-x86_64-commands)
