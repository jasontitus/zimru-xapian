# Native application packaging

The app reads ZIM content with Zimru and searches embedded indexes with the Rust Xapian reader. Cargo fetches both exact Git revisions from the private repository listed in `src-tauri/Cargo.lock`. Configure read access before building. The native archive SDK recipes below reproduce earlier releases and provide independent comparison tools.

All commands below run from the standalone `kiwix-tauri` repository root. No sibling checkout is required. The workflow is `.github/workflows/build.yml`.

## Targets and evidence

| Target | Packaging path | Prerequisites and current limits |
| --- | --- | --- |
| macOS, Apple Silicon or Intel | Build a native Tauri app, close its dylib graph and sign it | Xcode command-line tools, Node 22 and stable Rust. Each architecture needs compatible speech and platform libraries. |
| Linux x86_64 | Build an AppImage on Ubuntu 24.04 | GTK3, WebKitGTK 4.1 and the development packages below. Test graphics, WebKit and FUSE on the target system. |
| Windows x86_64 MSVC | Run the manually dispatched NSIS job on its restricted runner | Visual Studio C++ tools, Windows SDK, Node 22, stable Rust and WebView2. |

StreetZim uses the same Rust recipe assembly and archive validation on these targets. Building a package leaves map assembly, offline reopening and clean-machine compatibility to the runtime acceptance steps below.

macOS app metadata declares a minimum of **13.3**, including Safari/WebKit 16.4 Fetch Metadata support required to distinguish the trusted iframe retirement document from archive fetch/XHR/script requests. The native handoff fails closed without navigation metadata; there is no origin-only fallback. Linux requires WebKitGTK 2.40 or later; the Windows package uses supported evergreen WebView2. This does not lower the deployment target of Homebrew bottles: a package needs the maximum minimum-OS version of **all** its Mach-O images. CI Homebrew dependencies may require macOS 15 or newer. To ship macOS 13.3, rebuild libzim and its entire dependency closure with that deployment target and test on 13.3; do not infer support from `tauri.conf.json` alone.

Fetch Metadata compatibility is recorded in [MDN's browser compatibility data](https://github.com/mdn/browser-compat-data/blob/main/http/headers/Sec-Fetch-Mode.json). Native-window screen capture is separate from app operation: this workstation denies global screen-recording permission, which was not changed. An owned-WKWebView software snapshot captured chrome but omitted the accelerated map canvas, consistent with [WebKit bug 221662](https://bugs.webkit.org/show_bug.cgi?id=221662). Do not treat a white snapshot canvas as proof that the native map is blank—or as proof that its GPU pixels rendered correctly.

### Native comparison tools and historical evidence

The SDK setup in this section applies to native comparison tools and previous releases.

- [libzim 9.8.2 upstream source](https://github.com/openzim/libzim/tree/26ec526f74e8342a40da5ab14e364546988e0e1a), [release-tag API](https://api.github.com/repos/openzim/libzim/git/ref/tags/9.8.2), and its [Meson dependencies](https://github.com/openzim/libzim/blob/26ec526f74e8342a40da5ab14e364546988e0e1a/meson.build). The comparison recipe pins commit `26ec526f74e8342a40da5ab14e364546988e0e1a`, the resolved 9.8.2 release tag, rather than trusting a moving branch.
- Homebrew supplies native ICU, xz, zstd and zlib build dependencies. `scripts/build-desktop-sdk.py` builds its own matched Xapian and libzim rather than trusting the [stock libzim formula](https://formulae.brew.sh/formula/libzim) or an unrelated system Xapian ABI.
- [Upstream build instructions](https://github.com/openzim/libzim/blob/26ec526f74e8342a40da5ab14e364546988e0e1a/README.md) describe POSIX builds and caution that Microsoft compiler builds must use release-mode dependencies. They are not evidence of an arbitrary prebuilt Windows MSVC SDK URL.

The SDK requires `src-tauri/native/libzim-split-ranges.patch` followed by `libzim-retained-fd.patch`; POSIX also requires `xapian-fd-offset.patch` against pinned Xapian 1.4.32. Split-range ordering and empty ranges are corrected. Borrowed descriptors are duplicated and owned, logical archive extents drive checksums, retained metadata avoids path reopening, and Xapian opens at explicit physical offsets without moving shared cursors. FD-only public direct-access information does not invent a reopenable filename. Actual macOS embedded and 7/73/remainder multipart scenarios passed checksum, non-prefix title/full-text search, delayed index opening, unchanged caller cursor, unlink and caller-FD reuse. This is not Android SAF or physical iOS acceptance. The Rust retained-byte checksum remains an independent application validation layer.

## iOS and Android builds

Current app builds use the platform toolchain and the locked Rust reader. Follow the mobile frontend and Rust target instructions below the historical receipts.

<details>
<summary>Historical native SDK builds, measurements and reproduction commands</summary>


### Current release SDK rebuild — 2026-11-11

The post-PR-#18 SDK rebuild is recorded in the [SDK rebuild receipt](SDK-REBUILD-2026-11-11.md):
macOS arm64, Linux arm64/x86_64, iOS device/simulator and Android ARM64/ARMv7 manifests all pin the
current retained-fd patch SHA-256 `cefe049c5e529bdfa8c71b113f258a92790b40ed661c0326b96a0436fdb17f80`
and passed independent file-hash checks. The arm64 physical Samsung build rendered the Hawaii StreetZim
map with the existing 12-archive library preserved. Android APKs must use
`TAURI_ENV_PLATFORM=android` assets; a generic desktop frontend is rejected by the boot guard.

### Current ARM64 idle-memory candidate acceptance — 2026-11-05

The installed production ARM64 APK is **67,145,771 bytes**, SHA-256
`7d45d21ed9480c5aefcb54f641ecb6d851a28b1ce7156fb397c06edc0e71b398`,
from frozen `8002bff…` plus reviewed storage/acquisition/maps overlays.
It retains the five-correction ARM64 SDK (`093f4dd4…`), existing development
signer, byte-identical accepted ASCII manifest, all 27 JNI exports and
16 KiB ELF/ZIP alignment. All 19 build/inspection commands succeeded.
Actual Animal and Hawaii/Hilo physical smoke passed after in-place installation;
no paused working-tree Library changes were included. It remains 4.58% below
the official ARM64 APK, **not a memory or speed win**. See the
[receipt](benchmarks/2026-11-05-android-idle-memory.json) and
[comparison](PLATFORM_COMPARISON.md). Dated artifact records below are historical.

The later [application-package compiler trial](benchmarks/2026-11-05-android-root-size.json)
produced a **66,400,299-byte** APK with `opt-level=s` and explicit symbol
stripping, preserving the SDK, frontend, manifest, exports and alignment.
Physical real-archive smoke passed, but no whole-app memory/startup advantage
was established; it was not promoted and the default above is restored.
An earlier 82,137,131-byte trial accidentally retained static symbol tables
and was rejected without installation. No repository Cargo-profile policy
changed; do not treat that confounded artifact as an optimizer-size regression.

### Android decoder-memory SDK requirements — 2026-11-04

Android additionally requires `libzim-zstd-cache-accounting.patch`
(`kiwix-zstd-cache-accounting-v1`) and
`libzim-decoded-cluster-retirement.patch`
(`kiwix-decoded-cluster-retirement-v1`). Schema 2 therefore requires **five**
patch records for ARM64 and **six** for ARMv7, including its existing LP32
correction. The iOS and desktop patch requirements do not change.

The first correction measures the live Zstd stream rather than its unused
encoder/decoder counterpart. The second releases a compressed reader only
after every logical blob has independent decoded storage; cached rereads
remain available. Cost inspection and retirement share the reader mutex.
Partial/failed reads, raw readers and LP32 oversized placeholders keep their
existing behavior. This does not add trailing-frame/checksum validation,
change the nominal 16 MiB cache budget, or turn its one-time estimate into a
hard resident-memory limit.

Older Android SDKs are intentionally rejected. Rebuild with a **fresh**
workspace/prefix; reuse only verified downloads, not old libraries/manifests:

```sh
python3.12 scripts/build-android-sdk.py --target aarch64-linux-android --jobs 6 \
  --work-dir .native/android-sdk-build-decoded-cluster-v1 \
  --prefix .native/mobile-decoded-cluster-v1/aarch64-linux-android \
  --downloads-dir .native/android-sdk-build-retained-fd-v2/downloads
export TAURI_KIWIX_MOBILE_SDK_ROOT="$PWD/.native/mobile-decoded-cluster-v1/aarch64-linux-android"
```

Omit `--downloads-dir` when that local download cache does not exist. The
actual fresh ARM64 build passed, including strict patch application,
provenance, archive-object and retained ICU-member checks. Native host
regressions separately passed seven decoder cases and one stream-accounting
case after their intended before-fix failures. A subsequent fresh ARMv7 SDK
and production APK passed the separately scoped acceptance below.
See the [app comparison](PLATFORM_COMPARISON.md) for separately scoped
release identities, phone measurements and functional acceptance.

### ARMv7 production compatibility acceptance — 2026-11-05

The fresh six-correction SDK manifest SHA-256 is
`b4162d1d00e67cb6d9492fab1829274fb5d089c6d5d1a64481c1020905caba3e`.
The frozen `a044370…` ARMv7 production APK is **57,122,861 bytes**, SHA-256
`c032fdb82a229ac3b63ee06def8098ee18c706cd4fdb5524573a0588e1fb2bd2`.
ELF32 ARM, all 27 JNI exports, 16 KiB ELF/ZIP alignment, same development
signer and byte-identical accepted ARM64 manifest passed. No shared C++
runtime, product instrumentation or debuggable flag was introduced.

Physical Samsung installation and actual Wikipedia/StreetZIM smoke passed,
as did the separate failing-before/passing-after LP32 >4 GiB boundary probe.
However, Android displayed a compatibility warning, and the 32-bit native app
used a 64-bit WebView renderer on this 64-bit OS. This is **not** warning-free
release support or a 32-bit-only/low-memory device certification. It is not
an architecture-matched official-app size, memory or speed comparison.
The accepted ARM64 ASCII build was restored afterward. See
[reliability scope](RELIABILITY.md#armv7-real-archive-and-lp32-boundary-acceptance--2026-11-05)
and the [receipt](benchmarks/2026-11-05-android-armv7-compatibility.json).

### Shared HTML response packaging acceptance — 2026-11-04

The follow-on response-buffer/head-scan change rebuilt both optimized mobile
targets using the same target-specific frozen frontends and native SDKs. It
changes neither feature selection nor the ICU reader policy. The final ARM64
APK remains **66,981,931 bytes**, SHA-256
`5d28b3c6c2fdc9d08879e8e0b5b2edf27ae82990f2898dac71ab4cdb5efeaa03`.
Local development v2/v3 signing, all 27 exports, the same five system dependencies,
symbol stripping and 16 KiB ELF/ZIP alignment passed. Actual final-APK offline
Wikipedia search/Animal images and production Hawaii map rendering passed.

The signed iPhone executable SHA-256 is
`eedde700007207bd969c50aa25de307ed70e233ef2ee19f6403cf7ef3be5cd6b`.
Its expanded regular-file bundle is **51,116,323 bytes**; the actual locally
zipped development IPA is **22,771,864 bytes**, SHA-256
`cccd62ce6d9b5045fe016e93be1fff8a8ca045ab16d7a099a84b3cfdc0045dc1`.
The same authorized development certificate/profiles and separate main/widget
entitlements were retained, with strict verification before in-place installation.
This is not an App Store/thinned artifact or an official-IPA size comparison.

Both final updates were installed in place and normally launched. Preservation
is scoped to the fresh captures and explicitly reviewed iOS relocation/title
differences in [RELIABILITY](RELIABILITY.md#shared-html-response-ownership-and-delivery--2026-11-04).
Archive content hashes and new physical-reader rendering were not established
by the installation checks. The [response-path receipt](benchmarks/2026-11-04-response-path.json)
separates allocation/constructor/debug-HTTP gains from unproven settled-RAM and
shipping-device startup/search improvements. VoiceOver stayed off and untouched.

### Reader-efficiency packaging acceptance — 2026-11-04

This is the current mobile cutover; the dated records below retain their own
historical scope. See the [definitive receipt](benchmarks/2026-11-04-reader-efficiency.json),
[official-app comparison](PLATFORM_COMPARISON.md) and
[deployment limits](RELIABILITY.md#reader-efficiency-and-in-place-phone-updates--2026-11-04).
The accepted Android ARM64 APK is **66,981,931 bytes**, SHA-256
`c42c1fc678a0cc415b62bd4f6f2cc2719bbe893a0107898eb0716d45824f6c14`:
4.81% below the official Android APK, not a memory or startup-speed claim.
The signed iOS bundle's regular files total **51,116,419 bytes** and the local
development IPA is **22,770,533 bytes**; no official IPA or matched Apple runtime
comparison was obtained.

`scripts/build-android-sdk.py` now builds the ICU **`reader-services-v1`**
data policy for the libzim reader/search closure, not a general-purpose ICU
formatting SDK. It excludes only four service trees: `curr` (currency),
`zone` (timezone display), `unit` (measurement units) and `rbnf` (rule-based
number formatting). There is **no locale pruning**. Every other member,
including locale, collation, transliteration, break-iterator/dictionary and
converter data, is retained byte-identically.

The recipe runs `icupkg` dependency checking without `--ignore-deps`, verifies
the exact retained inventory and hashes each extracted retained member against
the full source package. The installed `share/provenance/icu-reader-data.json`
records source/filtered package hashes and retained/removed member hashes;
the schema-2 manifest binds its receipt hash and installed file hashes.
Recipe, policy, toolchain, source and patch identities fingerprint workspaces;
completion stamps rehash installed outputs before reuse. Changed inputs require
a **fresh workspace and prefix and a real rebuild**, not relabeling old SDKs.
Only checksum-verified source downloads may be reused across that boundary.

The fresh ARM64 build and actual native ABI probe compared full/reader SDK
normalization, title/full-text search and article bytes, including retained-FD
access. This is **not ARMv7 filtered-SDK certification**: ARMv7 was not rebuilt.
The iOS package still uses the **full ICU SDK**, not this filtered policy.
The accepted application retains default release **O3**, unwinding and feature
selection; smaller `s`/`z` experiments are not the accepted artifact.

Freeze mobile frontend assets with the platform selected explicitly:

```sh
TAURI_ENV_PLATFORM=android npm run build -- --outDir .native/frontend/android
TAURI_ENV_PLATFORM=ios npm run build -- --outDir .native/frontend/ios
```

Then use the corresponding `frontendDist` override shown below if bypassing
`beforeBuildCommand`. A generic desktop `dist/` is **not** a mobile substitute:
this batch rejected an initial generic-frontend candidate and rebuilt with the
correct platform-specific frozen assets. Seed the separate common RustEmbed
`dist/` input as described below, without overwriting it during native builds;
verify the actually packaged/loaded mobile assets, not just a browser preview.

Latest public-QUIC cutover (2026-11-02): both final reviewed mobile builds and
strict iOS signatures passed; both packages were installed in place and launched.
Samsung retained eleven books, three tabs and settings; iPhone retained three
books, one tab, bookmarks and history, with app-container paths correctly rebased.
These supersede older pending-install statements only for that cutover.
Unlocked iPhone Nearby surface acceptance passed, not phone connection/transfer.
The later ULA metadata/on-link correction passed both mobile builds, strict iOS
signatures and in-place installation. Samsung cold launch, state preservation,
Share all/advertising, inventory browsing and verified Samsung-to-Mac transfer
passed. iPhone's prelaunch state remained exact; its new launch/transfer test was
blocked by lock/authentication cancellation. These packages do not establish
phone-pair throughput, original-file intake or OS permission/join acceptance. See
[RELIABILITY](RELIABILITY.md#public-quic-and-original-file-intake-verification--2026-11-02).

Schema-2 SDKs and application archives have built for `aarch64-apple-ios`, `aarch64-apple-ios-sim` and `aarch64-linux-android`. The earlier consumer V2 physical iPhone build (`ee63e118ef4a8893b66e1f8757763f31652219a7f14e038ee5a0a3b19ebe8d33`) was signed, strictly/deeply verified, installed in place and launched; three books/five tabs and managed archive hashes survived normal container rebasing. Evidence: `.native/iphone-consumer-bounded-map-20261030`. Initial physical crashes exposed forbidden ancestor-directory opens and a Foundation/POSIX canonical-path mismatch; both failures remain recorded, with no-follow root capability traversal retained. The Android V2 APK installed/launched on Samsung SM-A356U, but Back exposed a diagnosed HWUI global-destruction race through Tao's process-exit path. Its corrected replacement subsequently passed native emulator lifecycle/transfer acceptance; it remains undeployed on the disconnected Samsung. SDK2 Android direct-provider read/search/verification/restart, managed import and a distinct 237,529,098-byte torrent completion/import/read passed in the emulator. SDK2 iOS simulator torrent pause/restart, finite background checkpoint, explicit resume, exact hash/import and offline reading passed; these do not certify physical background transfers. The separate ARMv7 evaluation has now run the production JNI assembly/integrity workload on a physical 2GB Moto e20; see [measurements and build qualifications](ANDROID_PERFORMANCE.md), not a full-app acceptance claim. No published release or physical sensor/backup/torrent certification is claimed.

On reconnect, the iPhone 17 Pro Max test device received the signed **priority location/sharing/sizing/autosave build**, binary SHA-256 `001d529037b449ec89ec9e89b445fbd10f141836ba493765ced09971ea828317`, in place. Strict/deep signature verification, install, foreground launch, actual Maps screenshot and subsequent same-process survival passed. All four book records, eight tab records, active tab, existing settings, reader-storage contents and root-capability identity survived normal container rebasing. All three managed archives matched their pre-update SHA-256; the external Taipei provider record was preserved but its original bytes were not copied. The new notification preference defaulted off without changing existing preferences. Evidence: `.native/iphone-priority-reconnect-20261030/deployment-summary.json`. This is the accepted pre-PDF priority archive, not the later PDF integration candidate; install alone does not establish physical fresh-GPS, camera or routerless-sharing behavior.

The signed **consumer UI redesign** is `.native/ios-consumer-ui-20261030/consumer-device.xcarchive/Products/Applications/Kiwix.app`, binary SHA-256 `00900471929df731c2e23754bb19257aea67103d5d2be7c2424c1d3287eb39da`, Apple Development certificate/team A6G8H8NGAM, strict/deep signature verified. Matching source/frontend passed actual simulator screens, HTTP progress/completion/reading and native three-page PDF save/cancel. In-place installation on the iPhone 17 Pro Max test device preserved its complete pre-launch state, four books/eight original tabs, capability identity and all managed archive hashes. The initial locked-phone launch denial is retained; subsequent user launch is observed as PID46845 with the actual new UI/Silicon Valley map. All original tabs remain, with one user-added tab; only the expected download-directory container rebasing changed preferences. The user then reported map-session expiry and a crossed-out location control; those regressions remain under correction. Evidence: `.native/iphone-consumer-deploy-20261030` and `.native/ios-map-session-regression-20261030`.

The consumer Android ARM64 UI cutover APK is `.native/android-consumer-ui-20261030/kiwix-final-android-consumer-arm64-debug.apk`, SHA-256 `3e999b8788adb41c9b07e19214ef467aa73e55a4b2cd4d87971b9bbed8896415`. Actual emulator-5560 loaded frontend SHA-256 `b25e30d26be8250c8f37dab6363a572726ef82f4a5a70afd9fa3eed84fa874d2`; native 320-CSS-pixel Library, Catalog, Maps, Downloads, Nearby and Settings checks passed. All six archive payloads, five original tabs, active map, preferences and URI grants survived the in-place cutover. A retained old map journal remained Unavailable rather than being silently cleared or counted as a fresh success. Native PDF/settings-lifecycle acceptance used the separate earlier frozen-frontend APK; JNI/lifecycle/transfer evidence is likewise build-scoped. No new physical Samsung deployment is implied.

The final macOS consumer review used `.native/mac-consumer-final-20261030/Kiwix.app`, binary SHA-256 `c7cc580f497afe893dd739e7700bfd9a49e361b999e93c9fb4fdb212a81fc882`, with the frozen desktop frontend SHA-256 `13cc05cd7fb9736e6a4215c71d7337f6fe35328eecdca11e48b93ddefcfa61a4`. The copied dependency-bundled debug app is ad-hoc signed under an acceptance-only bundle identifier, not a distributable Developer ID/notarized release. Actual WKWebView screens, archive reading, bookmarks, recent reading, theme persistence and normal Quit passed. Read-only notification authorization observation returned NotDetermined for that isolated bundle; no permission request or delivery is claimed. Evidence: `.native/mac-consumer-final-20261030/native-consumer-acceptance.json`.

The latest Android cache-corrected consumer APK is `.native/android-catalog-cache-20261030/kiwix-final-android-cache-fixed-arm64-debug.apk`, SHA-256 `f414960b7de266e87ce60e4bb37f12e9a25f024392728ee903aca20990946a25`, with the same final consumer frontend. Readable capability-relative directory permission updates replace invalid `fchmod(O_PATH)` on Linux/Android only. Actual Android page/language/category cache writes, conditional revalidation and process-relaunch revalidation passed without cache warnings; directory/file modes were 0700/0600. All original books/payload hashes, settings, tabs and URI grants were preserved. Controlled native offline behavior is a separate Linux check, not inferred from WebView offline emulation. Evidence: `.native/android-catalog-cache-20261030/13-final-native-cache-acceptance-summary.json`.

The final Linux ARM64 consumer/cache binary is retained inert under `.native/linux-consumer-desktop-20261030`, SHA-256 `0efd40cc4f3a93fe985d8623497a542249fe32cccb4cb3a9d34e444a61e04d7a`, using the same frozen desktop frontend. Actual GTK surfaces at 1280/760px, native picker cancellation, live page/facet persistence and 15 existing Linux cache tests passed. Offline reopening used the same profile and unchanged real HTTPS URLs in an owned loopback-only network namespace: cached page cards and language choices remained available with honest stale/network warnings and unchanged cache bytes. The displayed total of 3,656 is feed metadata, not a claim that every catalog page was cached. Paced trusted native Nearby activation passed; the earlier instantaneous harness attempt delivered no DOM pointer events. Original profiles/payloads were preserved; owned sessions exited 0 and temporary harnesses/namespaces were retired. Evidence: `.native/linux-consumer-desktop-20261030/acceptance.json`.

On reconnect, Samsung SM-A356U (`<samsung-serial>`) received the tested `f414960b…` consumer/cache APK through an in-place `adb install -r -t`. Actual cold launch and native Back → warm reopen passed with the same PID22698, and the physical Library screenshot was inspected. The pre-update profile had zero books/tabs; the complete previous state was preserved, with only the new default-off completion-notifications field materialized. Every preexisting download/torrent/root-capability file hash remained unchanged. No reset/uninstall or emulator substitution occurred. Evidence: `.native/android-phone-consumer-deploy-20261030/deployment-summary.json`. This is the accepted frozen Android build, not the subsequent iOS permission/history/autosave correction candidate.

The user-requested macOS launch opened the tested consumer app with its normal default KiwixWorkbench profile, without observer injection or a test `--data-dir` override. Public running-application/window queries observed successful activation and an on-screen 1280×850 window. It remains a development/ad-hoc-signed app, not a notarized release. Evidence: `.native/consumer-desktop-20261030/user-launch.json` and `user-visible-native-window.json`.

The subsequent reviewed direct-action/permission/session packages supersede those earlier cutovers:

- macOS: `.native/mac-sensor-final-20261030/Kiwix.app`, binary SHA-256 `595b8da4516933d90112ea5467c47497b3c7cf7910307745f126e0d519a9aad9`. Actual native WK and unmodified packaged headless assets matched; MapLibre consent/denial/reopen/reset passed. Includes the Apple QUIC ownership fix; the later mobile-only admission delta does not change macOS behavior. The previous user app retired through normal Quit, then this package opened using the normal default profile with no runtime observer or data-directory override. Actual PID15051 finished launching with an on-screen 1152×766 window; complete persisted state remained exact. It is left running. Evidence: `.native/mac-user-upgrade-20261030/deployment-summary.json`.
- Android: `.native/android-cold-bootstrap-20261030/kiwix-final-android-cold-bootstrap-arm64-debug.apk`, SHA-256 `d3ad24b08d991a07ced77d6cf052cc8d185c8b1383611957aeeeb907754739a6`, supersedes the mobile-admission package. Real emulator cold-asleep startup reproduced the old pre-hydration flush failure and passed after correction with five tabs preserved. Installed in place on Samsung; physical Maps/Library/Nearby/Downloads and Home/foreground passed using actual input and inspected screenshots, with no generic connection banner. Settings, failed Taipei job and staged bytes remained exact; normal navigation returned to Downloads. A new empty torrent temporary file was retained and is not claimed as an unchanged file set. Current PID30048 is left running without an observer. Evidence: `.native/android-phone-unlocked-20261030/acceptance-summary.json` and `.native/android-cold-bootstrap-20261030/21-final-cold-bootstrap-acceptance.json`.
- iOS: `.native/ios-map-session-regression-20261030/simulator/admission-final-device.xcarchive/Products/Applications/Kiwix.app`, executable SHA-256 `69b840104675c3acf940cd4fef6e96e299a810000a87a0961c4984713bd45cb2`, normal development signing/team A6G8H8NGAM, strict/deep signature verification passed. Final simulator three-scenario native UI run and separately executed iOS-target admission regression passed. Installed on the iPhone 17 Pro Max test device with complete state exact (four books/12 tabs) and all nine selected payload/capability/job-file hashes unchanged. Physical launch is blocked by the normal Locked policy; no bypass or physical-GPS certification. Deployment: `.native/iphone-final-sensor-deploy-20261030/deployment-summary.json`.

The subsequent iOS cold-bootstrap archive is `.native/ios-map-session-regression-20261030/simulator/cold-bootstrap-8091/device.xcarchive`, executable SHA-256 `5c607f538233d71fe8e226e54fdaf6d2f0ca334040330430ed750bf079231f4d`, development team A6G8H8NGAM. Two actual simulator UI tests passed genuine unhydrated background checkpoint, late unchanged snapshot callback release and foreground restoration with all four tabs and archive hashes preserved. No debugger pause, mock response, profile injection or permission reset was used. This archive is built/signed but not installed on the physical iPhone; the installed package above remains a separate deployment boundary.

The Android shell-zoom correction supersedes the preceding physical Android package: `.native/android-shell-zoom-20261030/emulator/viewport-final/kiwix-android-shell-viewport-arm64-debug.apk`, SHA-256 `c2f19089b2f7fab403004feee911e0b78f031dd8d4333520a83ba95191fa91ec`. Installed in place on Samsung SM-A356U; cold launch completed in 1,105 ms. Actual native pinch (36 events), double-tap (4) and quick-scale (36) were accepted and left shell scale 1 with navigation visible. The two existing LocalSwarm job records, including the Silicon Valley transfer's 239 verified chunks, remained exact. Emulator proof separately covered focused input/keyboard, reader size controls, native MapLibre pinch, rotation and Activity recreation. This package does **not** contain the subsequent sharing-lifetime, throughput or swipe-action work.

On macOS with full Xcode and required tools, build fresh schema-2 SDKs; these commands must not reuse schema-1 workspaces:

```sh
python3.12 scripts/build-ios-sdk.py --jobs 6 \
  --work-dir .native/ios-retained --prefix-root .native/mobile-retained
python3.12 scripts/build-android-sdk.py --jobs 4 \
  --work-dir .native/android-sdk-build-retained \
  --prefix .native/mobile-retained/aarch64-linux-android
# Separate 32-bit build: never reuse the ARM64 workspace/prefix.
python3.12 scripts/build-android-sdk.py --target armv7-linux-androideabi --jobs 4 \
  --work-dir .native/android-armv7-sdk-build-retained \
  --prefix .native/mobile-retained/armv7-linux-androideabi
```

The Android recipe requires NDK r28c (`28.2.13676358`); pass `--ndk` or set `ANDROID_NDK_HOME` when installed elsewhere. It supports API24 ARM64 and ARMv7 with explicit 64-bit file offsets; ARMv7 additionally requires `libzim-lp32-raw-blob.patch` and validates ELF32 ARM rather than ELF64. Recipes pin source archives and corrections, retain licenses/provenance and fingerprint their build workspaces. Android completion stamps also validate installed outputs before reuse. Changed recipe/toolchain/patch inputs require a fresh workspace and prefix rather than relabeling old libraries. `--downloads-dir` may reuse checksum-verified source archives, not compiled libraries. The build loader rehashes actual selected static archives against manifest `files` (Android) or `archives` (iOS).

`TAURI_KIWIX_MOBILE_SDK_ROOT` selects one target's SDK directory, containing `kiwix-sdk.json`, `include/` and `lib/`. The `TAURI_` prefix is required by the mobile CLI's [environment allowlist](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-cli/src/mobile/mod.rs); an arbitrary custom variable or `XCODE_XCCONFIG_FILE` was not forwarded in the actual iOS archive attempts. For example, export `TAURI_KIWIX_MOBILE_SDK_ROOT="$PWD/.native/mobile-retained/aarch64-apple-ios"` before the device build. Never pass a macOS SDK or desktop `LIBZIM_*` overrides to mobile. Android embeds ICU data and the static NDK C++ runtime; the manifest's 16 KiB-page linker flag applies to the final shared library. Device/simulator iOS archives are distinct despite both using arm64.

</details>

Use the Rust compiler whose sysroot contains the installed mobile target libraries. On this workstation, `rustup target add` installed Android/iOS standard libraries into the rustup toolchain, while the default Homebrew `rustc` used a different sysroot; the first Android application build consequently failed to find `core`. Put the actual rustup toolchain's `bin` directory first in `PATH` for the entire Tauri/Gradle invocation, rather than repeatedly installing targets into a different compiler's sysroot:

```sh
export PATH="$(dirname "$(rustup which rustc)"):$PATH"
rustc --print sysroot
rustup target list --installed
```


The first iOS application build stopped before native compilation because cargo-mobile requires an installed simulator runtime at least as new as Xcode's default simulator SDK. Installing the actual iOS 27.1 runtime resolved that prerequisite; no SDK metadata or validation was bypassed. This workstation's 27.1 runtime supports the iPhone Duo simulator, while the earlier 26.5 runtime supports iPhone 17 Pro. Application launch is not implied by runtime installation.

Android application builds additionally require a compatible JDK, Android SDK platform 37 (Android 17; package `platforms;android-37.0`) for the application module plus platform 36 (`platforms;android-36`) for the vendored `:tauri-android`, `:rustls-platform-verifier`, `:tauri-plugin-dialog` and `:tauri-plugin-kiwix-mobile` modules, which still declare `compileSdk = 36`. No module pins `buildToolsVersion`, so AGP 9.3.1's default Build-Tools 36.0.0 is used; NDK `28.2.13676358` and the matching Rust target are also required. The committed Gradle wrapper and mobile source projects are build inputs; nested `.tauri/` API copies, Gradle/build caches and local SDK paths regenerate or remain local and are not repository inputs.

The Android app links Oboe’s selected static C++ runtime through the NDK’s API-level `libc++.a` linker script on ARM64 and ARMv7. The script includes both the standard library and its exception and runtime type information support. The Android shared-library link fails on unresolved symbols. This catches missing C++ definitions before an APK reaches `System.loadLibrary` on startup.

The Android Gradle build stages the speech runtime from the same Cargo output directory as the selected ABI’s app library. Both project-local and external Cargo targets use this path. Tauri shares one generated app-library link per ABI in a checkout. Use separate checkouts when building debug and release concurrently for the same ABI. A missing packaged copy is restored even when an earlier build left a speech version marker. If the Cargo output itself is missing its speech libraries, clean the locked `sherpa-onnx-sys` package for the affected target and profile and rerun the Android build. APKs and app bundles must contain `libkiwix_app.so`, `libsherpa-onnx-c-api.so` and `libonnxruntime.so` for each selected ABI.

Mobile frontend behavior is selected by `TAURI_ENV_PLATFORM`, including the iOS-only archive-backup controls. Normal Tauri builds inject that environment into `beforeBuildCommand`. Do not skip that command and reuse a generic desktop `dist/` for mobile acceptance. For independent parallel platform builds, generate separate output directories and point each Tauri invocation at its own bundle:

```sh
# Seed RustEmbed's common dist/ input before parallel native compilation.
npm run build
TAURI_ENV_PLATFORM=android npm run build -- --outDir .native/frontend/android
TAURI_ENV_PLATFORM=ios npm run build -- --outDir .native/frontend/ios
# With the platform toolchain and Rust target selected:
npm run tauri -- android build --debug --target aarch64 --apk --ci \
  --config '{"build":{"beforeBuildCommand":"","frontendDist":"../.native/frontend/android"}}'
npm run tauri -- ios build --debug --target aarch64-sim --no-sign --archive-only \
  --config '{"build":{"beforeBuildCommand":"","frontendDist":"../.native/frontend/ios"}}'
```

Separate `frontendDist` outputs do not remove the Rust headless server's compile-time `dist/` requirement. Seed that common directory once before parallel native builds and do not rewrite it while those compilations run. The platform-specific output remains the native UI asset source; headless-preview assets and actually loaded native assets require separate identity checks.

The iOS plugin's static Swift product must match the Cargo package name `tauri-plugin-kiwix-mobile`; the Swift target/module remains `KiwixMobile`. iOS DNS-SD symbols come from `libSystem`, not a standalone `libdns_sd`. Both contracts were verified by the actual full simulator application link.

### iOS application identity and signing

The iOS-only `src-tauri/tauri.ios.conf.json` overrides the shared identifier with
`com.tiltastech.zimreader`; desktop and Android retain `org.kiwix.workbench`.
The generated application and extension use `com.tiltastech.zimreader` and
`com.tiltastech.zimreader.DownloadActivity`. A different identifier creates a
separate application container; it does not migrate an older installation.
Background acquisition callbacks use the new identifier as well.

`project.yml` generates boolean `com.apple.developer.networking.HotspotConfiguration`
and `com.apple.developer.networking.wifi-info` entitlements. These declarations
are not authorization: the Apple App ID, issued provisioning profile and final
application signature must authorize both. Verify the concrete application and
extension identifiers, device membership, signing certificate and profile
validity before installing. The widget does not request either Wi-Fi privilege.
Do not substitute Xcode capability-display metadata for signed entitlements.

The download Live Activity adds one embedded target to the generated Xcode project. `src-tauri/gen/apple/project.yml` declares `KiwixDownloadActivity` (`type: app-extension`, bundle id `com.tiltastech.zimreader.DownloadActivity`, `IPHONEOS_DEPLOYMENT_TARGET 16.4` to match the app, `NSExtensionPointIdentifier com.apple.widgetkit-extension`), and the application target embeds it with `dependencies: - target: KiwixDownloadActivity, embed: true`, which xcodegen turns into an "Embed Foundation Extensions" copy phase (`PlugIns`). The widget's SwiftUI sources live in `src-tauri/gen/apple/DownloadActivity/`; the `ActivityAttributes` model is one file, `plugins/tauri-plugin-kiwix-mobile/ios/Sources/DownloadActivityAttributes.swift`, compiled into both the extension target and the app's Swift package so the Codable shape cannot drift. The app's `Info.plist` gains `NSSupportsLiveActivities`. `project.yml` is the source of truth: after changing it, regenerate the committed project with `xcodegen generate` **inside** `src-tauri/gen/apple` and use the intended development team for device signing. The plugin Swift package's platform is iOS 16.4, because ActivityKit is unavailable below 16.1 and the application requires 16.4. A simulator `--no-sign` archive is not device-installation evidence. Device installation requires authorized profiles and valid signatures for both the main app and the embedded widget; a suitable existing wildcard profile can authorize the widget's concrete identifier.

### One-command iPhone build, installation and bounded testing

```sh
npm run iphone
```

Device mode prepares an exact **committed** revision, a production ARM64 app
and a signed XCTest runner before asking for phone access. Comparison mode
also prepares a separate, clean source-built Kiwix Apple release. Dirty
working-tree edits are deliberately excluded; use `--revision` to select
another commit for our app. It reuses fingerprinted artifacts after checking
their current hashes, signatures and profile/device authorization.
Preparation is **off-phone**, not a claim of process-wide network isolation.

Configure the workstation once in ignored `.native/iphone-config.json`:

| Key | Value |
| --- | --- |
| `device` | Explicit paired physical iPhone UDID; no automatic device choice |
| `signing_source` | Previously authorized signed `Kiwix.app`, including its widget and valid profiles |
| `signing_identity` | Available signing certificate SHA-1 authorized by both profiles |
| `pmd` | Optional explicit `pymobiledevice3` executable; the actual CLI must report supported 9.36.x |
| `map_source` | Reader/comparison workloads: real `osm_en_hawaii_2026-10-26.zim` fixture, 150,605,015 bytes; not required by `launch` |
| `mode` | `launch` for an in-place update/foreground smoke without fixtures or navigation; `comparison` for both apps, `acceptance` for our reader, or `nearby` for phone sharing; default `acceptance` |
| `order` | `AB` or `BA`; A is ours, B is upstream; default `AB` |
| `wiki_source` | Comparison: real `wikipedia_en_100_2026-08.zim`, 333,429,922 bytes |
| `upstream_revision` | Comparison: pinned official Kiwix Apple revision; reviewed 3.18.0 is `09a5ff1c9c3ffeda258ad593bbe16da612cfb149` |
| `upstream_repo` | Optional official-origin local repository reference; its working tree is never built or modified |
| `allow_payment_entitlement_omission` | Comparison: explicit `true` only after accepting the Apple Pay signing limitation below |
| `simulator_device` | Simulator: explicit booted, available simulator UUID with name prefix `KiwixProfilingValidation-` |
| `corekiwix_archive` | Optional verified official framework archive shared with simulator preparation |
| `upstream_package_lock` | Simulator: existing frozen upstream package-resolution receipt |
| `upstream_sourcepackages` | Optional existing validated SwiftPM checkout cache; no cache cloning |
| `upstream_bundle_id` | Optional separate upstream comparison identity |

Relative paths resolve against the repository. Keep signing material local;
never commit profiles, credentials or device identifiers. The configured
signing source must remain available; it can be a verified prepared app in
the persistent cache rather than an expiring temporary-directory copy.
Existing Node dependencies, Rust iOS target, Xcode, XcodeGen, Cargo sources
and signing authorization are prerequisites, not installed during a phone
window. Runner signing uses existing local authorization without a
provisioning-update flag or developer-account changes.

For a requested update without reader fixtures, prepare with `--mode launch`,
then run its prepared receipt in the authorized window. This mode requires an
existing initialized app profile, preserves every existing archive, setting,
bookmark and tab without fixture exceptions, and does not change radio state
or start Android. It captures the foreground app's visible WebView; the result
is installation/launch/preservation evidence, not reader or sharing acceptance.

Runner preparation uses Manual signing for manually managed profiles. For an
Xcode-managed profile it permits local Automatic signing without provisioning
updates, then still requires the selected profile's exact UUID/bytes and the
selected certificate before declaring READY. It does not accept a substituted
online profile for an offline-prepared app.

The official upstream SDK is cached only after its published checksum
matches. A new download has a 15-second connection limit, a 300-second total
limit (or the smaller remaining preparation budget), and fails below
100 KiB/s for 30 seconds. An interrupted partial is preserved for inspection,
not silently deleted or retried. Download and checksum-complete stages are
reported separately from compilation.

The upstream baseline comes from an owned fetch/archive of the official
revision, not local fork modifications. It records the official CoreKiwix
SDK download/checksum, actual resolved Swift packages, source and signing
identities. The existing local profile cannot authorize upstream's
`merchant.org.kiwix.apple` entitlement. The acknowledged reader-comparison
build omits only that entitlement, retaining reader and Stripe source;
it cannot establish Apple Pay parity. A separate bundle identifier avoids
replacing the owner's App Store or Maps Beta installation. This is a
**source-built developer-signed baseline**, not an official App Store binary.

Only after preparation does the interactive command ask for `START`. It then
updates the app **in place**, runs one combined scenario, and reserves cleanup
within a **600-second total window**. Comparison mode installs both prepared
apps and runs one paired AB or BA session; the opposite order requires
another explicitly authorized window. Unexpected state stops the run rather
than starting a debugging/retry loop. A successful **DEVICE RELEASED** message
requires a matching-device receipt confirming host cleanup and remote-runner
termination. It reports completed steps and the failure, if any; release
does not mean the tests passed. The assistant must immediately repeat the
device handoff in chat and obtain a new agreement before further access.

For an assistant/API workflow, prepare first while the owner uses the phone:

```sh
npm run iphone -- --prepare-only
```

After the owner explicitly grants the short window, run the prepared artifact
without any build step:

```sh
npm run iphone -- --prepared .native/iphone-tool/prepared.json --start-device-window
```

Acceptance mode requires **Wikipedia 100**, UUID
`dd8a2bd2-c05b-ac6c-8074-6c30e14803fb`, already admitted exactly once.
Comparison mode hash-binds both real Wikipedia and Hawaii source files for
the same workloads in both apps. Ordinary UI/URL intake is used, not injected
library records. A unique existing fixture tab can be reused through the
normal tab UI; unrelated tabs, archives, bookmarks and settings are preserved.
Owned staging sources, normally admitted fixtures and test-created reader
tabs remain intact; no reset/deletion is performed.

The paired scenario captures both Wikipedia home checkpoints **before**
attempting potentially fragile search controls, then performs an Animal
query/article checkpoint and Hawaii Home/zoom/pan. Saved screenshots still
require human inspection of real article content, rendered geography and
gesture changes before comparison admission.

Private evidence is under `.native/iphone-tool/runs/`. `result.json`
distinguishes completion from failure and unconfirmed cleanup. Screenshots,
hierarchies, process dumps and raw diagnostics can contain private data;
do not publish them wholesale. The memory classifier identifies the exact
installed executable and its observed nonzero kernel coalition. It sums
per-process physical-footprint ledgers for main, WebContent, GPU, networking
and other observed coalition members; compressed bytes are separate, not
added again. Three snapshots must retain the same main/member generations.
Missing reader WebContent, incomplete metrics, ambiguous ownership or member
churn makes the aggregate **unavailable**, rather than silently reporting a
main-only value as total. A stable main-only value may remain diagnostic.

Physical windows now start with a bounded PID-1 launchd transport/schema
probe, before installation or UI. That probe is not a reader-memory sample.
At each measured checkpoint, CoreDevice identifies the unique exact installed
executable PID; a PID-filtered DVT probe verifies its path, generation and
nonzero kernel coalition. All three captures request **every observed member
of that coalition**, not just main/WebKit names, and must retain the probed
root identity. Raw filtered responses are checked against their filter;
this is not a global all-process uniqueness audit.

PMD 9.36 applies filters before its sequential executable-name RPCs, avoiding
unnecessary enrichment of every system process. `--userspace` takes
precedence over `--mobdev2`; the ineffective latter flag is removed.
The userspace tunnel is USB-first with Bonjour fallback, **not forced Wi-Fi**.
The 12-second command limits and overall window remain unchanged. The actual
physical run completed five three-sample reader checkpoints, including both
Wikipedia homes, both Animal bodies and native Hawaii. Our map was blocked
by an app document-closing error; no map pair or general memory win is claimed.
See [measured results and limits](PLATFORM_COMPARISON.md#physical-iphone-paired-wikipedia-memory-is-mixed).

Host verification passed 12 memory-accounting and seven transport/ownership
regressions, including actual offline CoreDevice parser coverage. Device,
timeout and output options must precede positional app arguments; otherwise
`process launch` consumes them as app arguments. The later physical run
also exercised corrected payload launches successfully.
A temporary subprocess smoke exercised the actual installed
PMD task's filtering/enrichment/JSON serialization with only external DVT
input replaced: launchd readiness, exact root admission, three captures and
all coalition roles completed. Unrelated executable lookups were excluded.
This was synthetic host evidence—not PMD CLI parsing, physical transport,
device timing or real memory attribution—and the temporary provider was removed.

This is **observed-coalition ledger accounting**, not Android PSS, deduplicated
physical RAM or proof that every shared WebKit service is attributed.
One paired window is not an independent replication series. Instrumented
timings are observation bounds, not calibrated cold-start/search benchmarks.

The app build uses one persistent
`.native/iphone-tool/build/cargo-target`, not cloned multi-target caches.
The exercised paired preparation command completed in **13.17 seconds** with
both apps cached and a newly compiled/signed runner; no phone was contacted.
This is one workstation observation, not a guaranteed clean-build duration.
The initial upstream SDK download took roughly 40 minutes before the bounded
download guard was added. An actual loopback TLS slow-transfer smoke then
exited with curl's low-speed error after **30.085 seconds**, preserving its
partial file; the guard was not merely checked by inspecting its arguments.
Host-only cancellation regressions:

```sh
python3 scripts/test_iphone_window_deadline.py
python3 scripts/test_iphone_memory.py
python3 scripts/test_iphone_window_transport.py
```

#### Simulator-first comparison workflow validation

Run the real paired fixture workflow without contacting a phone:

```sh
npm run iphone -- --platform simulator --mode comparison --revision 3cce5af5677350996301b6cc335b30a5a00d4938
```

The current simulator scenario is bound to that committed reader revision.
Configure an explicitly owned, already booted simulator as above; the tool
never chooses, resets or erases another simulator. Preparation can be split
with `--prepare-only`, followed by `--prepared` using the simulator receipt
under `.native/iphone-tool/simulator/`. No physical `START` prompt or
`--start-device-window` flag is used. Physical signing inputs, PMD and
CoreDevice calls are excluded from simulator paths.

Simulator applications are genuine ARM64/IOSSIMULATOR builds. The app is
ad-hoc signed; the simulator XCTest runner is built without device
provisioning. Schema-3 preparation receipts require an explicit platform,
and every app/runner manifest must match it. A device binary is not a
simulator artifact. The default simulator app build shares the existing
Cargo cache, whose target triples remain separate.

The same XCTest UI scenario uses actual Wikipedia and Hawaii files,
ordinary **Add to library** intake, Animal search/body assertions and map
Home/zoom/pan. Targeted URL delivery uses public `XCUIApplication.open`,
checked against the phase's authorized URL; library records are not
injected. Simulator checkpoints validate real UI/state and save screenshots;
**physical memory and profiling transport are not tested**, not replaced
with invented values.
StreetZIM persists its camera across page reloads. Before the map-home
screenshot, both apps receive the archive's real **Show the whole map**
action, which restores configured center/zoom and zero bearing/pitch.
Reload alone is not a matched camera reset. The same relative map gesture
and screenshot-admission requirements then apply to both products.

Every capture records its screenshot first. Full accessibility-tree dumps
are failure diagnostics only: real Animal-page trees exceeded 2 MB, and
serializing the entire hierarchy at every successful checkpoint added
unnecessary automation overhead. Content assertions and screenshots remain
required; this is a runner optimization, not an app-performance claim.

Tauri's simulator archive command requires a runtime matching the installed
SDK. Install that normal prerequisite rather than changing SDK metadata or
reimplementing Tauri's private build-options server:

```sh
xcodebuild -downloadPlatform iOS -buildVersion 27.2 -architectureVariant arm64
```

The 8.32 GB runtime download completed in 117.95 seconds. The available
27.2 runtime supports only iPhone Duo on this workstation; the actual
owned iPhone 17 Pro Max UI target uses runtime **26.5**, with SDK **27.2**.
Do not relabel it as physical-phone or 27.2-runtime acceptance.

A simulator execution exposed a real harness defect:
`simctl listapps` returns OpenStep plist, not XML/binary plist. The helper
now decodes it with Apple's `plutil` before parsing, with a real decoder
regression. Timeout diagnostics retain captured stdout/stderr and stage,
duration and failure metadata before terminating/reaping owned processes.

ACK payloads and copy-command receipts have distinct host paths; they must
not overwrite or collide with one another. Native accessibility snapshots
can contain zero-sized WebViews with infinite origins, so the runner rejects
invalid geometry before asking XCTest to hit-test it. Selection still
requires exactly one enabled, genuinely hittable target.

Nested accessibility WebViews are wrappers, not separate archive documents.
The runner selects the visible leaf structurally, binds its accessibility
identity, reads geometry/enabled state from one public snapshot, and retains
the successful selection rather than repeating remote queries. An actual
Animal-page run exposed that redundant query cost; the fix does not replace
article-body assertions with a container-presence check.
Article text is checked for actual content presence, not control hittability
or a requirement that an ordinary word appear only once. The exact Animal
text and both introductory fragments, `are multicellular,` and
`organisms belonging to the biological kingdom`, must exist within the
selected archive document; neither a tab title nor a search result suffices.
The real archive and captured accessibility tree show bold/link boundaries
splitting that sentence into separate text nodes, so requiring one
`Animals are multicellular` node was an invalid automation assumption.

WebKit exposes the search toggle as `Switch` and the reader-navigation
dialog trigger as `Other`; selectors follow the observed named controls
rather than assuming every actionable control has the `Button` role.
The 45-second progress watchdog recognizes changed semantic query stages,
not polling keepalives; the absolute action deadline and cleanup reserve
remain unchanged.

Upstream archive-size metadata is not an exact payload length:
[libkiwix 14.2.1](https://github.com/kiwix/libkiwix/blob/14.2.1/src/book.cpp)
stores whole KiB converted back to bytes, using
[`getArchiveFileSize`](https://github.com/kiwix/libkiwix/blob/14.2.1/src/tools/archiveTools.cpp).
Native metadata admission therefore checks that exact quantization contract;
it does not add a tolerance or claim payload-byte readback. Our metadata
checks and both source-fixture SHA-256/byte-size checks remain exact.

For an existing simulator installation, capture the old container and
persisted profile before reinstalling. Reinstall can relocate the container;
capturing its new identity alongside an unmigrated old profile gives a false
preservation failure after normal launch repairs those managed paths. The
helper retains exact authority/settings checks and normalizes only the
verified old/new owned profile roots.

The upstream tab sheet can disappear when selecting a different tab replaces
its presenting reader. The runner waits for that observed disappearance;
only a still-present Tabs navigation bar receives its scoped **Done** action.
It never assumes an unconditional second dismissal tap.

#### Simulator memory and launch diagnostics

The read-only memory collector requires an already-running app's exact host
PID and executable path, plus the competing app's exact executable path:

```sh
python3 scripts/profile-ios-simulator-memory.py \
  --pid "$PID" --main-executable "$MAIN_EXECUTABLE" \
  --competing-executable "$OTHER_EXECUTABLE" --output "$PRIVATE_OUTPUT"
```

Foreground the target normally and allow 30 seconds to settle before
collecting. The collector never activates or terminates apps. It records
three generation-checked `proc_pid_rusage` samples, two seconds apart.
Simulator host coalitions are shared with system services and both apps:
only the main-process diagnostic is reported, never a fabricated WebKit or
whole-reader sum. Raw output contains host process paths; keep it private.

The dedicated standard XCTest launch observer is prepared off-device:

```sh
python3 scripts/profile-ios-simulator-launch.py --prepare-only
# Review the generated plan, test binary and tracked Swift/YAML sources.
python3 scripts/profile-ios-simulator-launch.py \
  --reviewed-plan-sha256 "$REVIEWED_PLAN_SHA256" \
  --reviewed-test-binary-sha256 "$REVIEWED_BINARY_SHA256" \
  --host-idle-confirmed
```

Preparation writes `.native/iphone-tool/simulator/metrics/launch-prepared.json`.
Execution validates those artifact/source bindings and the explicitly owned
booted simulator's UUID/name, then runs only the dedicated launch tests.
It does not rebuild/reset measured apps or contact the iPhone. This observer
currently binds the profiling simulator declared in its source; a different
target requires a reviewed source change, not automatic device selection.
The 140-second action deadline reserves bounded runner cleanup. Each app
gets one discarded iteration and three retained process-cold launches.
`XCTApplicationLaunchMetric(waitUntilResponsive: true)` measures first
displayed frame plus main-thread responsiveness, not archive content-ready,
search speed, storage-cold launch or physical-device performance.

Fresh observer preparation and a separate repeat completed in 6.74 and
33.02 seconds respectively. Both actual batches passed two launch tests and
confirmed runner absence. The [comparison](PLATFORM_COMPARISON.md#simulator-first-responsive-frame-launch-ours-shorter-in-both-batches)
links their separate public receipts; do not pool the samples or substitute
AX wall times for this metric.


### Mobile acquisition and archive-backup policy (source implementation)

Mobile transfer policy uses iOS `NWPathMonitor` and Android `ConnectivityManager`, not browser connection hints. Cellular/metered Internet acquisition is opt-in/default-off; unknown/offline state denies Internet admission. Changes retire HTTP/torrent workers durably and cancel StreetZim through its partial journal; allowed connectivity never overrides manual Pause or grants background execution. Maps require explicit restart. Apple nearby sharing is different: discovery can create an AWDL route without an access point, so a known default cellular/no-LAN state may admit an attempt. Native QUIC independently prohibits cellular and validates/binds its actual local numeric route before handshake traffic. Internet consent is not silently enabled.

Standalone map catalogs register owned work before spawning and retain the admitted denial revision throughout request/body/parsing; network retirement cancels and joins them before acknowledging that revision. Map cancellation reports failed terminal-state writes instead of acknowledging nondurable retirement. A later retirement attempt may retry only the retained terminal checkpoint after storage recovers; it does not retry networking or composition.

Quinn-based mobile LocalSwarm sockets retain an iOS interface-index or Android `Network` binding receipt. Locality is checked against that same still-confirmed route; a private IP or later default interface is not authority. Apple Bonjour peers instead use the native AWDL/infrastructure adapter: scoped DNS-SD candidates, on-link address checks, required NWInterface, hop limit 1 and connected-path revalidation. Unknown status still fails closed. Ten actual iOS-simulator policy tests preserve separation between native peer-to-peer attempts, cellular Internet and physical Quinn admission. Native harness/M5 evidence is in `.native/mobile-priority-20261030`; physical iPhone 5G and two-device routerless radio acceptance are not implied. Android retains its normal [network-state permissions](https://developer.android.com/develop/connectivity/network-ops/reading-network-state).

On iOS, backup defaults off for app-owned archive payloads. Before the first payload byte, creation hooks exclude the caller's retained file descriptor, not a reopened pathname. The native adapter discovers the current OS backing of the supported `NSURLIsExcludedFromBackupKey` on an empty private disposable file, then uses descriptor-only `fsetxattr`/`fremovexattr` with descriptor readback and read-only Foundation confirmation on payloads; unknown backing mechanisms fail closed. Only Store-admitted or durably published owned `.zim`/split archive leaves may become backup-eligible. Descriptor-bound publication evidence survives history deletion and inode-preserving moves. Startup/preference reconciliation covers all profile-owned locations, including hidden names and user-selected download folders, without treating interrupted native imports as complete. HTTP/torrent/map publication recovery reapplies current policy before acknowledging completion. This is eligibility, not a backup scheduler or guarantee: OS/user backup settings and available storage still apply. No whole Documents/profile directory, provider original, preference, credential, cache or temporary payload is included. External provider backup is governed by its provider. Retained-descriptor and reconciliation regressions are authored but unexecuted; these source changes do not claim iOS backup/restore acceptance.

Historical completion paths are lookup hints, not admission authority. Reconciliation checks the same retained descriptor against recorded HTTP SHA-256/size, torrent publication inode/device/SHA-256/size, or LocalSwarm owned inode/device/size before creating a publication marker. Unmarked current library entries require their recorded UUID plus retained-byte checksum/structural verification. A self-valid unrelated replacement is not promoted merely because an old job says “complete.” Older HTTP records without an expected digest, torrents without publication intents, and completed map records that discarded their original inode have no recoverable admission proof; unmarked payloads stay excluded until verified current library admission or explicit reimport. Archive-like leaves and reserved owned transfer/cache payload namespaces may be conservatively excluded without granting admission; ordinary preference/credential files are not blanket-modified. New publications persist their inode-bound admission marker before backup mutation and completion acknowledgement.

Startup reconciliation can run under a shared privacy read lease while workers publish. Its negative branch uses a conditional native exclusion: admission is rechecked under the same backup-attribute transaction lock used by publication. A delayed negative scan therefore cannot revoke a newer publisher's inclusion, including when the retained temporary inode has already acquired its final name. This lock covers only native backup/admission metadata operations, not application-wide filesystem or privacy work.

Android has no equivalent safe per-file runtime backup switch. Its application manifest disables backup, and the legacy/full-backup plus Android 12+ cloud/device-transfer XML rules exclude all app data. The interface therefore explains static exclusion rather than offering an unenforceable toggle. OEM transfer mechanisms can differ from documented Android backup behavior; explicitly export files to retain separate copies. No native network-switch, iOS backup/restore, Android cloud-backup or OEM device-transfer execution is asserted by these source changes.



## macOS local application

Run from the repository root with Xcode command-line tools and access to the private Rust dependencies.

```sh
npm ci
npm run build:mcp   # externalBin: src-tauri/binaries/kiwix-mcp-<host triple>
npm run tauri -- build --bundles app
```

`npm run bundle:macos` also builds the MCP sidecar.

Then finalize the actual `.app` (this is the exact local post-build command):

```sh
node scripts/bundle-macos.mjs \
  src-tauri/target/release/bundle/macos/Kiwix.app --identity -
```

`npm run bundle:macos` combines the app build and postprocessing and resolves the resulting app from the effective cargo target directory: `CARGO_TARGET_DIR` when set, otherwise `src-tauri/target`. A relative `CARGO_TARGET_DIR` is resolved from the current directory (the repository root under `npm run`) and, if no bundle is there, from `src-tauri/`. It respects `APPLE_SIGNING_IDENTITY`; without credentials it produces a local ad-hoc signature, not a notarized release. If no app exists at the resolved location, the script names every path it tried and asks for an explicit `APP.app` argument.

A `--target <triple>` build still changes the output directory and needs the resulting `.app` path passed explicitly. Do not point this command at a DMG or an archive. It modifies only the chosen application bundle; rebuild from Tauri if a packaging failure leaves it partially modified.

The script:

1. Validates a real `.app`, reads `CFBundleExecutable`, and rejects symlinks escaping the app.
2. Enumerates bundled Mach-O files and recursively resolves all non-system dylib dependencies using `otool` load commands, original `LC_RPATH`s, `@loader_path`, `@executable_path`, and inherited executable/loader runpaths. `--library-dir DIR` adds an explicit trusted fallback lookup directory if the SDK requires one.
3. Resolves the graph before rewriting any references, checks architecture coverage, copies external dylibs into `Contents/Frameworks`, and refuses basename collisions/overwrites. `/System/Library` and `/usr/lib` stay system-provided. External non-system frameworks require a complete framework packaging path and fail explicitly rather than copying a resource-less framework binary.
4. Rewrites dylib IDs to `@rpath`/`@loader_path`, rewrites references to bundle-relative paths, removes original SDK runpaths, and adds `@executable_path/../Frameworks` to the app executable. The original SDK is not modified.
5. Rejects native libzim and Xapian names in the bundle and its Mach-O dependencies. Audits every rewritten image for escaping/missing dependencies and Homebrew/Cellar load-command paths, then signs inside-out. It verifies the resulting code signatures. This audits loader commands, not unrelated embedded debug/source strings.
6. Before sealing the app, verifies every Mach-O directly under `Contents/Frameworks` and re-signs any that is not valid, because signing the bundle executable validates nested code and bundles can also contain dylibs the dependency walk does not rewrite. An image codesign still refuses is named and aborts packaging; resolved `.framework` directories are skipped.

All subprocesses use argument arrays, not shell command interpolation. Copies do not overwrite existing files, and temporary notarization files are created in a private temporary directory and removed afterward. A failed command aborts packaging. Dylibs must contain all slices needed by their consumer; architecture-specific differences in universal load commands may require separate native builds instead. If `install_name_tool` reports insufficient load-command space, rebuild the executable with linker header padding (for example `RUSTFLAGS='-C link-arg=-Wl,-headerpad_max_install_names'`) rather than distributing an unmodified binary.

`--identity -` is an **ad-hoc** signature only. It does not authenticate Kiwix to Apple and is not a notarized distribution build. Without an explicit identity, the script uses `APPLE_SIGNING_IDENTITY` or ad-hoc signing for unsigned/ad-hoc apps, and refuses to silently downgrade an existing Apple signature.

Launch the finalized bundle, not the raw target executable:

```sh
open src-tauri/target/release/bundle/macos/Kiwix.app
```

Create any distribution archive **after** finalization:

```sh
ditto -c -k --sequesterRsrc --keepParent \
  src-tauri/target/release/bundle/macos/Kiwix.app Kiwix-macos-local.zip
shasum -a 256 Kiwix-macos-local.zip > Kiwix-macos-local.zip.sha256
```

A DMG produced by an earlier `tauri build --bundles all` still contains the earlier, unrelocated app. Do not distribute it. If a DMG is needed, make a new disk image from the finalized app; do not rebuild the app as part of that step.

## Real macOS signing and notarization

An installed, unlocked **Developer ID Application** identity and Apple credentials are prerequisites, not generated by the script. Keep notarization credentials in a notarytool keychain profile rather than exporting them to the initial Tauri build: notarization before relocation would cover the wrong bytes. See [Apple's notarization instructions](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow) and [Tauri's signing guide](https://v2.tauri.app/distribute/sign/macos/).

After building `.app`, use the real identity and an already configured notarytool profile:

```sh
security find-identity -v -p codesigning
node scripts/bundle-macos.mjs \
  src-tauri/target/release/bundle/macos/Kiwix.app \
  --identity "${APPLE_SIGNING_IDENTITY:?Set APPLE_SIGNING_IDENTITY to an installed Developer ID Application certificate}" \
  --notarize-profile kiwix-distribution
```

Set `APPLE_SIGNING_IDENTITY` to the actual installed certificate name reported by `security find-identity`; the shell refuses to proceed if it is absent. The script enables hardened runtime and a secure timestamp for real identities, uses the repository app entitlements (override with `--entitlements FILE`), submits a temporary archive with `notarytool --wait`, requires Apple's `Accepted` response, then staples and validates the ticket. An ad-hoc identity cannot request notarization. Archive the app only after stapling. Apple rejection is a failed packaging operation, never relabeled as a successful release. This path has no credential-free substitute.

## Linux x86_64 commands

On Ubuntu 24.04, from the repository root:

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libssl-dev \
  libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev patchelf file libfuse2t64 libasound2-dev squashfs-tools
npm ci
npm run build:mcp   # externalBin: src-tauri/binaries/kiwix-mcp-<host triple>
npm run tauri -- build --bundles appimage
```

The supported x86-64 AppImage targets Ubuntu 24.04. Install `libasound2t64`, `libwebkit2gtk-4.1-0` and `libgtk-3-0t64` on the desktop. For the tested WebM/VP8 playback path install `gstreamer1.0-plugins-base` and `gstreamer1.0-plugins-good`. Other formats depend on installed codecs. The launcher checks the executable’s host library dependencies before opening the app. GTK/WebKit translations and media plugins come from the system. The application’s own translations are built into its frontend.

This research AppImage has a release blocker in its bundled upstream bootstrap. In extract-and-run mode the bootstrap accepts a pre-existing extraction directory and may reuse files without checking their ownership or contents before launch. Concurrent launches under different accounts reproduced an extraction failure. Use the Debian package for distribution while bootstrap hardening is pending. The regular FUSE route mounts the image and requires separate runtime validation.

For isolated no-FUSE testing, give each invocation a fresh private temporary directory. Run this example from the directory containing the AppImage, supplying its filename.

```sh
(
  set -eu
  image_tmp=$(mktemp -d)
  trap 'rmdir "$image_tmp"' EXIT
  TMPDIR="$image_tmp" ./org.kiwix.workbench_0.1.0_amd64.AppImage --appimage-extract-and-run
)
```

The npm build command produces a Debian staging package and assembles the AppImage from its application, MCP relay, desktop integration, icons and configured resources. To build just the Debian package, use `npm run tauri -- build --bundles deb`. The type-2 loader is pinned in `upstream/appimage` and mounts or extracts the SquashFS image. Its statically linked native dependencies remain part of the package. Its source, build information, dependency notices and FUSE source archives accompany the existing reader licenses under `usr/lib/org.kiwix.workbench/licenses`. The image supplies no WebKit, JavaScriptCore, GTK or GStreamer libraries, helpers, or plugins. The host supplies the renderer that handles archive content.

CI inspects the final image layout, content and application bundle marker before upload. Release staging audits the final AppImage before signing it. Manifest assembly authenticates downloaded bytes before auditing them. Use the npm wrapper for the supported image. An explicit raw upstream Tauri CLI AppImage uses a different layout and fails the release audit. The workflow does not publish a Flatpak.

The Linux runtime/GeoClue application identifier is `org.kiwix.workbench`. Tauri's [desktop generator](https://github.com/tauri-apps/tauri/blob/dev/crates/tauri-bundler/src/bundle/linux/freedesktop/mod.rs) names the desktop file after `productName`, not the bundle identifier or the desktop template. Therefore `src-tauri/tauri.linux.conf.json` sets the Linux-only packaging product name to `org.kiwix.workbench`, while `linux/main.desktop` keeps the displayed name `Kiwix`, sets the matching startup class, accepts associated local archive paths and `kiwix://` links through `%U`, and declares `x-scheme-handler/kiwix`. AppImage generation uses the Debian desktop template, so both Debian/AppImage and RPM template settings are supplied. macOS/Windows keep their `Kiwix` product name. The resulting Linux artifact filename starts with `org.kiwix.workbench`, and its installed desktop entry is `org.kiwix.workbench.desktop`. The workflow checks the packaged AppImage's `usr/share/applications/org.kiwix.workbench.desktop` before upload and fails on disagreement. A matching filename is necessary, not sufficient: GeoClue availability, desktop integration, and the user's location permission still need runtime validation.

## Guarded Windows prerequisite and build

The workflow's Windows job requires manual dispatch with `build_windows=true` on `main`, an approved `native-windows` environment, and a self-hosted runner labeled `Windows`, `X64`, and `kiwix-libzim-9_8`. Configure environment required reviewers; do not allow arbitrary contributors to execute code on this runner. No untrusted pull request is allowed to use it.

The existing `kiwix-libzim-9_8` label selects the restricted Windows runner. Install stable Rust with `x86_64-pc-windows-msvc`, Node 22, Visual Studio release C++ tools and a matching Windows SDK. The installer resource helper packages the four checksum-pinned speech runtime DLLs and the agent connector executable. Clean-machine acceptance includes WebView2 bootstrap and offline installation behavior.

Run the local build in PowerShell from the repository root.

```powershell
$ErrorActionPreference = 'Stop'
$config = Join-Path $env:TEMP ('kiwix-bundle-' + [guid]::NewGuid() + '.json')
try {
  $overlay = node scripts/windows-bundle-resources.mjs
  if ($LASTEXITCODE -ne 0) { throw 'Windows runtime verification failed' }
  [IO.File]::WriteAllLines($config, [string[]]$overlay)
  npm ci
  if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
  npm run tauri -- build --target x86_64-pc-windows-msvc --bundles nsis --config $config
  if ($LASTEXITCODE -ne 0) { throw 'Tauri build failed' }
} finally {
  Remove-Item -LiteralPath $config
}
```

This produces an **unsigned** installer unless a real organization-approved Windows signing identity/signing command is configured. Authenticode credentials, timestamp service, and signature verification are separate release prerequisites; this workflow does not fake them.

## Windows ARM64

Windows ARM64 (`aarch64-pc-windows-msvc`) is cross-compiled on an x64 Windows host. These are local, unsigned test builds; the scripts are in `docs/windows-agent-tools/arm64/`.

**Prerequisites** (Visual Studio Build Tools 2022 components):
- `Microsoft.VisualStudio.Component.VC.Tools.ARM64`: the MSVC ARM64 compiler and libraries.
- `Microsoft.VisualStudio.Component.VC.Llvm.Clang`: `ring` and `aws-lc-sys` compile their ARM64 Windows sources with clang.
- The Rust target: `rustup target add aarch64-pc-windows-msvc`.

**App and installer** (`build-nsis-arm64.ps1`):
- The four sherpa-onnx 2.0.1 `win-arm64-shared-MT-Release` DLLs are pinned by SHA-256, like x64.
- The ARM64 Visual C++ runtime is installed app-local, because clean ARM64 PCs often lack it.
- Every shipped executable and DLL is checked to be ARM64. The NSIS installer and uninstaller stubs are x86 by design.

On ARM64, `sha2`'s assembly backend is disabled: its AArch64 code is GNU assembly, which MSVC cannot build, so SHA-256 uses the portable implementation there.

**Limits:**
- **No tests on ARM64.** The migrated app needs runtime verification on ARM64 hardware. An x64 host cannot execute ARM64 binaries, and this repository has no ARM64 CI runner.
- **Release use** needs an ARM64 test pass (for example a Snapdragon PC or a hosted `windows-11-arm` runner).

## Signed updater feed

Desktop builds use `tauri-plugin-updater`; the trust model, key handling,
platform limits and the loopback acceptance procedure are in
[UPDATE.md](UPDATE.md). The short release sequence is:

```sh
# One long-lived keypair; keep the private key and password in the release secret store.
npx tauri signer generate -w updater.key
# Put the base64 contents of updater.key.pub into plugins.updater.pubkey in
# src-tauri/tauri.conf.json. Change plugins.updater.endpoints to the real HTTPS host.

# After the notarized .app / signed Windows installer / Linux package is built:
node scripts/make-update-manifest.mjs \
  --version 0.1.1 \
  --notes release-notes.txt \
  --base-url https://updates.example.org/kiwix/0.1.1/ \
  --artifact darwin-aarch64-app=Kiwix.app \
  --artifact windows-x86_64-nsis=Kiwix_0.1.1_x64-setup.nsis.zip \
  --artifact linux-x86_64-appimage=org.kiwix.workbench_0.1.1_amd64.AppImage \
  --out dist/update.json
```

The script packages a `.app` into `Kiwix.app.tar.gz`, copies or packages every
other artifact into `--stage-dir`, reads an existing `.sig` or signs with
`TAURI_SIGNING_PRIVATE_KEY_PATH`, and writes the static manifest atomically.
Every signature is created with `--app-version`, because
`requireSignedVersion: true` rejects a manifest whose announced version differs
from the signed version.

Upload artifacts before replacing `update.json`, and keep the previous
release's artifacts reachable. Verify the feed from the previous build before
announcing it. The checked-in public key is a placeholder, so a build that has
not been configured reports that updates are unavailable instead of trusting
the wrong key.

macOS distribution requires a Developer ID signature and notarization (above).
Windows requires Authenticode for the installer. Linux packages follow their
distribution's signing policy. None of those credentials are produced by this
repository, and the loopback test in UPDATE.md uses a throwaway key and debug
artifacts only.

## Artifacts, channels, and release acceptance

`.github/workflows/build.yml` uploads packages plus `SHA256SUMS`, the Rust lockfile and archive-reader license notices through GitHub Actions artifacts. macOS artifacts are ZIPs of the finalized `.app`, Linux artifacts are AppImages, and guarded Windows artifacts are NSIS installers. These are build artifacts, not published stable/beta releases. Reproducibility is limited by resolved Rust/npm/runner/Homebrew tooling; the archive-reader revisions are pinned and the npm/Rust lockfiles belong in the source release.

Before publication, record and perform the following on clean machines/VMs without the build SDK:

1. Verify the downloaded archive/installer checksum before extraction/install. A checksum detects corruption, not publisher identity; authenticate the release channel separately.
2. On macOS, inspect each image with `otool -L` and `otool -l` (no external SDK or Cellar references), inspect `LC_BUILD_VERSION`/minimum deployment versions for every slice, and run `codesign --verify --deep --strict --verbose=2 Kiwix.app`. For a real release also run `spctl --assess --type execute --verbose=2 Kiwix.app` and `xcrun stapler validate Kiwix.app`. Test the downloaded, quarantined app on the oldest claimed macOS with no Homebrew installed. Do not turn off Gatekeeper or remove quarantine and call that release validation.
3. On Linux, copy only the AppImage to a fresh supported Ubuntu machine without the SDK, then launch it with the SDK's `LD_LIBRARY_PATH`/`PKG_CONFIG_PATH` unset. For no-FUSE testing, use the private temporary directory procedure above. Inspect extracted ELF dependencies for unresolved libraries and check the package for obsolete native libzim/Xapian dependencies. Confirm desktop integration retains `org.kiwix.workbench.desktop`, then exercise location permission with GeoClue available; report unsupported/service-denied states honestly instead of treating a filename check as a successful location fix.
4. On Windows, install in a fresh standard-user VM without the SDK directories on `PATH`. Check DLL resolution and WebView2 installation behavior, and verify the installer and executable Authenticode signatures for a signed channel. Uninstall/reinstall and verify that the installed archive association opens the application.
5. On every target: launch from the platform launcher; open a known local ZIM through the dialog and file association; read text/images, navigate links, and run full-text search. Exercise a split ZIM, persistence after restart, library import/removal, download pause/resume, export, window close/reopen, and the actual offline behavior. For StreetZim, fetch the real HTTPS catalog, assemble a known recipe, validate/open the completed archive, restart and reopen the saved map, then demonstrate local-part reuse and offline reopening with networking disabled. Run those map checks on Windows, macOS, and Linux; a successful local fixture on one OS is not evidence for the other two. Record app and locked reader versions, OS/architecture, and results. Packaging success alone does not verify these behaviors.
6. Publish corresponding source and dependency license notices/source offers as required by GPL and bundled dependency licenses. SDK provenance metadata alone is not a complete license-compliance package.

Stable/beta publication requires an authorized release manager, appropriate repository/channel write credentials, and platform signing/notarization credentials. Upload only accepted artifacts to the intended GitHub release or other authenticated channel. No workflow here creates a release or asserts notarization without Apple's response. The in-app updater is implemented (see [UPDATE.md](UPDATE.md)), but the release manifest, public key, HTTPS endpoint and platform signing credentials must be configured with the actual publication infrastructure before advertising a channel.
