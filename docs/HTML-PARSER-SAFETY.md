# HTML parsing in the archive reader

The app handles ZIM article bytes and embedded index fields supplied by archive authors. The same text can reach several consumers. Treat rendered HTML, text extraction and export resource discovery as distinct operations. The initial observations trace the locked Rust sources and normal workbench call sites on commit `9c0325b`. The CSS measurements below use the export and PDF call sites from `96e7afc`. They identify where untrusted input reaches unsafe implementations. This review found no memory-corruption defect.

| App operation | Input and processing | Remaining boundary |
|---|---|---|
| Search rows and shared search | The Rust archive bridge produces an escaped plain title or an escaped generated excerpt. An index can also supply arbitrary stored snippet HTML. `search::snippet_text` recognizes up to 2,048 bytes of text with only the exact `&amp;`, `&lt;` and `&gt;` escapes. It removes one leading BOM, decodes once and checks cancellation. Raw markup, CR, NUL, other ampersands and longer inputs take the original bounded fragment path. | The accepted text subset uses Rust string operations. Other snippets use html5ever. The app displays the result in React text nodes, the shared-reader script uses `textContent`, and the shared HTML response escapes its excerpt. |
| Agent article text and preview excerpts | `agent_connector.rs` and `service/dispatch.rs` pass article HTML through `exports::html::Html::parse_document_bounded`, then inspect the tree. Selection rules distinguish hidden content, templates, whitespace and document structure. | Tokenizer, tree builder, atom and text storage. |
| HTML bundle resource discovery | `exports::markup_references` follows potentially nested HTML, including base URLs, styles, foreign content and template semantics. Multiple parses charge a shared work/CPU allowance. `exports::css_tokens` interprets stylesheet tokens. | HTML stack plus the checked cssparser fork and its dependencies. A parser that loses an attribute namespace, stylesheet escape or base URL can drop a required resource or collect the wrong one. |
| Live PDF | `native_pdf::live::sanitize` builds a bounded DOM from the captured document, validates element/attribute context and CSS, and serializes an inert document for the platform print renderer. Raw text, RCDATA and foreign-content namespaces affect escaping. | HTML and CSS parsing, followed by separate platform rendering. The live sanitizer and the WebView remain required for this workflow. |
| Ordinary article display | The reader serves article bytes to the OS WebView. | Browser HTML, script, image and media processing have their own implementation and update cycle. Changing the Rust fragment parser does not alter this path. |

## Dependency worklist

| Locked component | Source and conditions | Replacement and acceptance criteria |
|---|---|---|
| `html5ever 0.39.0` | This experimental fork checks 32-byte blocks on x86/x86-64 and 16-byte blocks on AArch64 in the tokenizer's data state. It preserves the published CPU feature gate, line accounting, and scalar fallback. Its library forbids unsafe code. | Compare exact tokens, callback lines, DOMs and bounded work when updating. |
| `markup5ever 0.39.0` | The experimental input queue uses Tendril's checked slice and prefix-removal APIs. Its library forbids unsafe code. | Preserve chunk consumption, UTF-8 boundaries, CR/LF state across feeds and EOF handling. |
| `tendril 0.5.1` | `src/buf32.rs`, `src/tendril.rs` and `src/fmt.rs` implement manual allocation, inline/shared representation, refcount transitions, raw pointer access and UTF-8 conversions. Both markup5ever and the DOM consume `StrTendril`; changing just the app's input type leaves these paths active. | Prototype a checked copy-on-write string and byte buffer with shared backing and explicit slice offsets. Exercise clone, slicing, repeated front removal, append-after-slice, splitting multibyte characters across feeds and the decoder. Repeated copying at every token or front removal risks quadratic work. Keep upstream dual MIT/Apache-2.0 notices with reused source. |
| `web_atoms 0.2.6` and `string_cache 0.9.0` | Markup names use static atoms and a dynamic interner. `string_cache/src/atom.rs` and `dynamic_set.rs` use tagged representations, pointers and reference counts. Dynamic archive-controlled element and attribute names can reach the interner. | Preserve the generated static names and namespace/equality contracts. Test a safe owned/shared representation for dynamic names with mixed-case foreign attributes and concurrent distinct names. |
| `dom_query 0.28.0` | The app's declaration disables default features. A source scan found no explicit unsafe block or unsafe trait implementation in its `src/` tree. It stores parser-provided names and text. Tauri build tooling also resolves the same DOM library. | Retain the existing tree semantics when changing the storage layer. Compare normal elements, detached/reparented nodes, foster-parented table text, templates, duplicate attributes and deep input. Verify both the app and Tauri's build-time HTML processing after dependency patching. |
| `cssparser 0.37.0` | PDF and export code visit CSS tokens. The local fork forbids unsafe code throughout its own crate. | Token strings use borrowed UTF-8 or shared owned strings. Slicing, token escape decoding, ASCII serialization and case normalization use checked operations. Retain upstream fixtures and app tests when updating. |

The branch vendors the two modified crates for application and build-time consumers. The original 0.39.0 crate archives are in `upstream/servo-html5ever`. The shared copyright, licenses and source revision ship under `licenses/html5ever`. Tendril and the atom interner still use unsafe storage operations.

For ordinary DOM parsing, eight measured pairs alternated the order of the published and checked parsers. Each sample timed 200 parses. On ARM64 macOS the median of paired times increased 1.4% and 2.1% for whole Climate and Nollywood articles, or 3.7% and 3.5% using the application’s feed rule and an ordinary DOM sink. An extracted bounded PDF sanitizer took 1.0% longer on four inputs and returned identical bytes.

On x86-64 Linux the 32-byte scanner used 77.8% and 86.3% of the published parser’s time for whole English and Chinese prose fixtures. With the application’s feed rule it used 98.5% and 102.7%. Six further complete Wikipedia articles took 2.2–4.8% longer with that feed rule. All 14 Linux inputs produced matching serialized bytes with both feed styles. The mobile parser consumer typechecked for ARM64 iOS, iOS simulator and Android, and for ARMv7 Android. [Source and input hashes, methods and raw timing pairs](benchmarks/2026-11-18-checked-html-tokenizer.json)

On an ARM64 iOS 26.5 simulator and an ARM64 Android API 35 emulator, both parsers returned identical trees for the four inputs and matched the bounded work counters, cancellation and node-budget refusals. The four PDF sanitizer outputs also matched. Median checked-to-published time ratios for bounded Climate and Nollywood parsing were 1.094× and 1.035× on iOS, and 0.902× and 0.743× on Android. For a pass through all four sanitizer inputs, the medians were 1.071× and 0.849× respectively. The eight measured Android sanitizer pairs ranged from 0.444× to 2.230×. These in-process samples leave physical-device performance, full application latency and platform PDF rendering unmeasured. [Mobile fixture hashes, work counters, raw timing pairs and cleanup](benchmarks/2026-11-18-checked-html-tokenizer-mobile.json)

The normal feature graph has no safe-storage switch for Tendril. The `encoding_rs` feature changes decoding support. dom_query's `atomic` feature changes sharing, and html5ever's `serde` feature changes serialization. None of these implements a checked string representation.

A development comparison used html5ever and dom_query directly to evaluate the existing exact-error option. The documents were entries 17 and 43 from the pinned Climate and Nollywood Wikipedia fixtures. Their serialized DOMs matched between modes. With the host cache warm on ARM64 macOS, 20 alternating rounds of ten parses measured medians of 107 versus 512 microseconds for the 13,395-byte Climate article and 664 versus 1,903 microseconds for the 53,657-byte Nollywood article (default versus exact errors). This measurement used dom_query's ordinary sink, which records parser errors. The app uses a bounded sink that discards diagnostics. Exact-error mode retains Tendril and atom storage and needs a separate app-budget evaluation.

## Verification

Run the search equivalence test and the manual release timing comparison from the app checkout.

```sh
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib search::tests::escaped_and_plain_snippets_match_the_bounded_html_fragment_parser
cargo test --locked --release --features custom-protocol --manifest-path src-tauri/Cargo.toml --lib search::tests::measure_search_snippet_pages -- --ignored --nocapture
cargo tree --locked --manifest-path src-tauri/Cargo.toml --target aarch64-apple-darwin -e features -i tendril
```

The fast-path test compares its result to the previous bounded fragment parser, including repeated high bytes at the byte cap, named and numeric references, markup, CR, NUL, control characters, single and repeated BOMs, Unicode and cancellation. The timing comparison processes fifty result strings per page and alternates both implementations over twenty measured rounds after two warmups. It measures text conversion inside the process. Archive I/O, IPC, layout, and mobile rendering require separate measurements.

On ARM64 macOS 27.2.1 with rustc 1.99.1, the median of five consecutive release-test process medians was:

| Fifty strings | Always parse DOM (µs) | Selected path (µs) |
|---|---:|---:|
| Plain text | 59.94 | 9.23 |
| Escaped text | 292.67 | 18.54 |
| Stored markup | 289.67 | 289.54 |

Independent differential checks matched all 34,196 accepted mixed strings. The app suite passed 1,191 tests with temporary local network fixtures enabled. Mobile CPU and memory behavior remain unmeasured.

## CSS export and PDF token handling

The app uses one patched cssparser in export resource discovery, PDF style validation and selector consumers. Its public token and parser APIs still accept UTF-8. Export resource discovery continues to ignore malformed components where specified, and live PDF validation still refuses them. The existing byte, work, nesting and URL budgets at the app call sites continue to apply. Raw style serialization still neutralizes an HTML closing-style delimiter. The fork changes the representation and checked access inside the tokenizer. The archive HTML parser and platform print renderer remain separate boundaries.

The crate-level prohibition covers cssparser itself. Its app graph still includes runtime dependencies such as smallvec and numeric serialization crates. Procedural macros such as cssparser-macros run during compilation, and cssparser itself also has build-tool consumers. Cargo feature output and the vendored source distinguish these from archive bytes parsed at runtime.

A release comparison on ARM64 macOS exercised the actual CSS token walk, resource discovery and raw style serialization on every stylesheet in the pinned Climate and Nollywood archives. The 49 UTF-8 stylesheets contained 722,325 bytes and produced 126,530 tokens per pass. Token values, source offsets, raw slices, discovered URLs or errors, and PDF CSS serialization agreed across the three variants. Each process validated its inputs, then timed twelve in-memory corpus passes. The table reports medians across twelve processes per variant. Maximum RSS covers the whole process, including validation.

| Workload | Published crate, ms / MiB | Checked token storage, ms / MiB | Fully checked crate, ms / MiB |
|---|---:|---:|---:|
| Export resource discovery | 5.33 / 3.67 | 5.14 / 3.99 | 5.18 / 3.98 |
| PDF CSS validation and raw style serialization | 9.80 / 4.53 | 9.81 / 4.77 | 9.99 / 4.85 |

On this host, the checked representation keeps CowRcStr at 16 bytes and Token at 32 bytes. Option<CowRcStr> grows from 16 to 24 bytes. The stylesheet timings cover CSS work inside the process. A separate comparison ran the full live PDF sanitizer on two production bridge test snapshots and two modeled Wikipedia snapshots. The latter used the real Climate and Nollywood article HTML, self-contained visual references and the same real Climate stylesheet. All four serialized outputs matched. A four-document pass took 2.44 ms with the published crate and 2.46 ms with the checked crate. Median process maximum RSS was 4.58 and 4.77 MiB, respectively. Archive I/O, platform PDF rendering and mobile behavior were outside these measurements. [Fixture identities, raw timing and memory samples](benchmarks/2026-11-18-checked-css-strings.json)
