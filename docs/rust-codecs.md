# Rust archive codecs and review handoff

The MIT archive reader and writer use Rust for Zstandard, XZ and raw clusters. Normal opens use positioned I/O. The standalone GPL search project reads the same archive through the Rust API, and uses its own raw DEFLATE decoder for Glass tags. Its README pins the paired MIT commit. This private branch is unpublished. Use a checkout or Git dependency so that Cargo includes the vendored codec.

## Codec selection

Ten Rust Zstandard libraries decoded 65 native-produced frames from real Kiwix archives byte-for-byte, covering 128,005,261 uncompressed bytes. Three passed the Linux codec-level check for compiler-reported unsafe operations in their runtime dependencies. Avarok's zstd-rs provided both compression and decompression, numeric compression levels, and single-frame job primitives. The fork adds incremental input, explicit allocation and history limits, and frame validation. The code and the licenses for its test fixtures are preserved in the repository. [Fork provenance](../vendor/zstd-rs/ZIMRU.md)

| Candidate | Finding affecting selection |
| --- | --- |
| zstd-rs 0.1.0 | Selected. Forbids unsafe Rust. No production dependencies. |
| yolowzstd 0.1.2 | Safe alternative with numeric levels. Its whole-buffer interface would require streaming work. |
| ms-compress-ruzstd 0.9.1 | Safe fork. Encoder offers its fastest mode. The streaming reader needs a wrapper to advance to the next frame. |
| ruzstd, zstdx, zstd-pure-rs, structured-zstd, zstd-native, cosmoz | The compiler found unsafe operations in the candidate builds, including the portable configuration tested for zstd-native. |
| oxiarc-zstd 0.4.2 | Its required oxiarc-core contained active unsafe operations. |

Zstandard readers check frame headers, block sizes, history, aggregate output, checksums, and declared lengths. They handle concatenated and skippable frames. Physical ZIM intervals can include padding after complete compressed data. The exact-slice path checks the complete supplied body. The writer emits one pledged frame per cluster, including large streamed items. It preserves level 19 as the default. Bounded parallel jobs and their history affect throughput and compression ratio. [Reader](../src/codec_zstd.rs) · [Writer](../src/writer/zstd.rs)

XZ uses the raw LZMA2 implementation from lzma-rust2 0.25.0 with unsafe optimizations disabled. The local reader verifies block headers, filter chains, checks, ordered index records and the footer. It enforces separate limits on decoded bytes and decoder working memory. The local writer emits CRC64 and supports presets 0–9. Native liblzma and libzstd are test references. The writer CLI also uses safe Rust for gzip member parsing and DEFLATE. [XZ reader](../src/codec_xz.rs) · [XZ writer](../src/writer/xz.rs) · [Gzip implementation](../src/bin/_gzip_html.rs)

## Evidence collected during development

* Native libzim and zimcheck accepted eight Rust-created ZIMs across Zstandard levels 1, 3, 9, 19 and 22 and XZ presets 0, 6 and 9. All 17 sampled items per output matched. In the opposite direction, 442 item comparisons passed across 17 archives. These include historical, aligned, multiblock, prefiltered and checksummed inputs.
* The Rust reader matched 241 XZ-backed blobs from three legacy Kiwix fixtures. Independent XZ Utils checks covered 24 stream variations. An i686 Linux build under QEMU read valid oversized dictionary declarations with small output. An independent native reader verified a 1,073,872,919-byte item from the Rust Zstandard writer by SHA-256.
* CLI gzip differential tests against the previous decoder agreed on 292 valid and 73 invalid or incomplete inputs. Mixed-language indexes reproduced the previous release's logical results. On Xapian 1.4 the ordinary Chinese, Japanese and Korean comparisons matched all 63 native query results. Six of twelve rare-Han cases matched. Xapian 2.0 also differs on two ordinary fullwidth queries. These are existing search compatibility limits.
* All seven MIT command-line programs, three library artifacts and both GPL executables were screened for native Zstandard and liblzma symbols or dynamic dependencies. The tested release artifacts passed. The regular Rust reader and each archive codec forbid first-party unsafe code. OS bindings, concurrency and collection crates, Rust's standard library and optional C exports still contain unsafe code. An explicitly enabled legacy mapping accepts a caller-provided mapping.

On a 3,071,865-byte XZ workload, median CRC64 decoding measured 109.8 MiB/s in Rust and 119.2 MiB/s with native liblzma. SHA-256 decoding measured 95.2 and 103.2 MiB/s. Early Zstandard level-19 encoding took about twice as long as native on a Ray Charles archive. Initial decoder profiles identified FSE sequence processing and match copying. Whole-archive Zstandard decode and high-level write performance are open acceptance items. Host contention makes the early wall times unsuitable for a parity claim. The default code keeps bounds and compatibility checks active.

## Resume and reproduce

Run the crate's normal tests, the separate upstream codec tests and the command below on Linux with a Rust toolchain and the native test prerequisites in the project README.

```sh
cargo test --locked --no-default-features
cargo test --locked
cargo test --locked --all-features
cargo test --manifest-path vendor/zstd-rs/Cargo.toml --locked --release
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo tree --locked -e normal,build
```

For paired timing, build the reference MIT revision `849aa41605065a82e5b0dd998d741f0f8c213efe` and the candidate with the same release toolchain. Copy `examples/codec_read.rs` into the reference checkout before building. For each checkout run `cargo build --release --locked --bin zimru --bin zimrecreate --example codec_read`. Place its two release binaries and `target/release/examples/codec_read` together in a separate directory. Pass those directories to the benchmark. It alternates order, discards a warm-up round and verifies every recreated archive's content using the reference binary.

```sh
python3 bench/compare_codecs.py /path/to/native /path/to/rust chemistry.zim --rounds 7 --levels 3 19
python3 bench/compare_codecs.py /path/to/native /path/to/rust legacy.zim --rounds 7 --codec xz --levels 3
```

For performance sign-off, compare the final code on Chinese Chemistry, Climate and Nollywood with workers 1 and 4 and include peak memory and archive size. Recheck high-level encoding on adjacent real clusters and repeated content spanning job boundaries. Match-search depth dominated level-19 CPU samples. Experiments varying it reduce time at a compression-size cost. Re-run the native-index/CJK matrix from the companion checkout and keep its version-specific differences visible.

## Checkpoint for the next session

The companion branch is `review/zimru-storage` and the MIT archive branch is `review/zimru-memory-safety`, both on private `jasontitus/zimru-xapian`. Its public MIT upstream was left at its prior revision.

* On the two larger real archives, six interleaved scans measured median process CPU of 56.86 ms native and 86.53 ms Rust for Chinese Chemistry, and 75.72 ms native and 138.03 ms Rust for Nollywood. Both were measured on a shared Linux host. Three decoder variations failed to improve both workloads consistently. [Read samples](review-data/zstd-native-read.json) · [Candidate study, inputs and reproduction scripts](review-data/zstd-alternatives.tar.gz)
* A bounded binary-tree match finder is an **experiment**. Its patch, harness and native controls travel with this checkout. The initial level-19 sample timings approached the native codec, with output-size differences below 1% on the first three samples. Apply in a separate worktree, review bounds and history, run upstream and large-item tests and compare full-archive CPU, peak RSS and ratio before integrating. [Writer experiment](review-data/zstd-writer-experiment.tar.gz)
* The Linux compiler audit found active unsafe code in 18/30 reader dependency packages, 18/32 default writer packages, 15/27 Glass engine packages and 44/77 complete companion packages. The first-party regular binaries, archive codecs and Glass code had zero reported unsafe sites. The optional C ABI has 182. Counts exclude the prebuilt standard library. A Unicode 18 normalization alternative matched ICU4X on 1,112,064 scalars and 20,171 conformance rows but took around 1.75–2.36 times as long in initial text timings. ICU4X remains in the GPL search engine. [Compiler audit](review-data/codec-trust.json)
* On the first checkpoint the reader/default/all-feature test suites passed 104/174/184 tests respectively, plus 6/7/7 doctests. The vendor passed 52 tests. The final two edits refine XZ decoder memory checks and the single-lane Zstandard window. Repeat the complete paired validation on the exact final commits after any subsequent performance changes.
