# Experimental safe text storage for the HTML parser

This standalone experiment replaces the byte and UTF-8 storage used by the app’s HTML parser with standard-library types. The same DOM and sanitized export bytes came back on the tested inputs. The prototype took longer than published Tendril on every measured Mac workload. Production continues to use published Tendril.

The comparison starts from app commit `18388c2`, which provides the checked html5ever and markup5ever 0.39.0 implementations. Both arms run that scanner and dom_query 0.28.0 with default features disabled. Only the experimental harness selects this storage crate. The implementation carries Tendril 0.5.1’s original MIT and Apache license files. The application-licensed checks live outside that crate.

## Storage and decoding

The implementation stores up to eight bytes inline. Longer UTF-8 text uses a reference-counted `String`, and raw bytes use a reference-counted `Vec<u8>`. An interval within the backing allocation selects the visible text. The byte interval stays within a checked 32-bit length, and UTF-8 ranges require character boundaries. Cloning or slicing a large view shares its backing. A write copies the visible portion if another view owns the backing, and adjacent ranges of the same allocation can rejoin without copying. Front removal advances the interval. A unique append compacts a consumed prefix once it spans at least 4 KiB and is at least as large as the remaining visible data. A shared reservation leaves the view in place.

A live slice as short as nine bytes can retain its original allocation. Multiple shared views that subsequently append can each need their own allocation. Process peak memory includes the allocator, parser, DOM and test runner. The buffered UTF-8 decoder retains up to three unfinished bytes between calls, emits replacement characters and reports invalid sequences to the sink. Its synchronous stream reader retries interrupted reads and continues until EOF or another I/O error. The application’s cancellation and budget checks run in its bounded parser.

## What was checked

- Five crate tests cover shared mutation, alias-preserving reservation, validated ranges, contiguous joins, bounded sliding-window compaction and interrupted short reads.
- The published and experimental crates agreed in 587,599 short-input and parsed-byte stream cases. The independent comparator also checked 800,000 byte sequences with split positions and 200,000 mutations of shared text. The byte converters were checked against Rust’s standard lossy UTF-8 output. The two implementations also matched on reported decoding errors and their positions.
- Across 1,584 document and 1,690 fragment runs, serialized HTML, token debug output and line numbers, parse errors and quirks mode matched. The input set includes two real Wikipedia articles, malformed nesting, foreign content, empty chunks, multibyte splits and entities.
- Four complete PDF-input HTML serializations matched byte for byte. Two came from the app’s reader-bridge test and two modeled captured real article bodies with resolved visual resources. The bounded parser also returned identical resource counts at six chunk sizes and refused cancellation, node-budget and work-budget controls as expected. Output lengths were 533, 772, 16,430 and 52,086 bytes.

The experiment covers the selected single-threaded UTF-8/byte consumers. Public Tendril formats beyond these two, atomic or sendable handles, optional encoding adapters, its general raw APIs, external layout compatibility and other configurations need separate implementations or evidence. The dynamic atom interner, other parser dependencies, standard-library internals and the system WebView retain their own memory-safety boundaries. Signed app integration, mobile execution and final PDF rendering are outside this test.

## Performance

On ARM64 macOS, the table reports median elapsed time per parse or four-document sanitization pass. The last column uses the median of the eight within-pair ratios. Each process verified serialization before the timed interval. The test alternated first position, discarded the first pair and retained eight fresh process pairs per workload. DOM runs timed 200 parses after ten warm-ups. Export runs timed twelve four-document passes. The prototype was slower in all forty measured pairs. [Mac output hashes, elapsed times and process peak RSS](evidence/mac.json)

| Workload | Published storage | Prototype | Paired change |
|---|---:|---:|---:|
| Climate article, app-sized chunks | 127.67 µs | 148.59 µs | +17.4% |
| Nollywood article, app-sized chunks | 775.50 µs | 919.83 µs | +18.7% |
| English sample, app-sized chunks | 13.70 µs | 15.78 µs | +15.3% |
| Chinese sample, app-sized chunks | 4.86 µs | 5.74 µs | +18.2% |
| Bounded export, four documents | 1.77 ms | 1.96 ms | +10.2% |

On Linux x86-64, six held-out Climate and Wikipedia 100 article bodies produced the same DOM in both arms. The earlier prototype took 29.8–35.2% longer with the app’s chunking rule and 18.6–25.7% longer with whole-document input, using the median of pairs two through nine after discarding the first alternating pair. The Linux study predates the correction to shared reservation. Its checked scanner used the same x86 scan width as the final one. The receipt records its exact source identities and held-out archive paths. [Linux samples](evidence/linux-prior.json) · [Source and archive identities](evidence/mac.json)

## Repeat the comparison

The standalone crate has no third-party dependencies. From the app checkout, run these checks.

```sh
cargo test --release --locked --manifest-path experiments/html-storage/tendril/Cargo.toml
cargo clippy --all-targets --locked --manifest-path experiments/html-storage/tendril/Cargo.toml -- -D warnings
```

The end-to-end harness requires Python 3.11 or newer, Cargo and the app’s locked parser dependencies already cached for offline use. It creates fresh build directories under the new output path. The published crate’s registry checksum is `5fed54709c5b3a53d09bb1c113ea4f5ceafd1e772ddcb0030a82e1d56c087b08`. The app’s production source supplies the bounded tree sink and export sanitizer. The resource-counter check widens the visibility of one parser method in its temporary program.

To reconstruct the measured capture inputs, extract the item `100%_renewable_energy` and `content.parsoid.css` from `wikipedia_en_climate_change_mini_2024-06.zim`, and `2014_Nigeria_Entertainment_Awards` from `wikipedia_en_nollywood_maxi_2026-10.zim`. The bridge check uses Node and the app’s locked frontend dependencies. Substitute the local file paths below. Both output directories must be new.

```sh
python3 experiments/html-storage/make-captures.py . captures climate.html nollywood.html climate.css
python3 experiments/html-storage/check.py . sample-run climate.html nollywood.html captures --measure
```

The checker compares the caller’s article bodies and four capture files to the published Tendril control and records all input and output hashes. Matching the input identities in the Mac receipt reproduces the recorded corpus. Timing and process memory vary with host load.
