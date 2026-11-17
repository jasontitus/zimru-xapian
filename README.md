# zimru-xapian

Two standalone GPL-3.0-or-later command-line tools for ZIM search indexes. `zimsearch` reads a ZIM using the MIT-licensed zimru library and searches its embedded single-file Xapian Glass database. `xapianbuilder` accepts zimwriterfs's JSON Lines input and creates a title or full-text Glass database. The index implementation is Rust. Native Xapian, libzim and ICU are only used as references in interoperability tests.

## Build and search

Use a current stable Rust toolchain, a C toolchain and liblzma development files for zimru's archive codecs. The lockfile pins the Rust dependencies and zimru revision.

```sh
cargo build --release --locked
target/release/zimsearch wikipedia.zim "plate tectonics"
target/release/zimsearch --suggest --limit 5 wikipedia.zim "plate"
```

Each hit is one JSON object on stdout. Full-text hits include `path`, `score`, `percent` and `word_count`. Suggestions include `path`. Paths are the document data recorded in the index, normally `C/<archive path>`. A successful empty search produces zero lines. A missing or unreadable index exits unsuccessfully with a diagnostic on stderr.

The reader uses the embedded item's file offset and size and fetches Glass blocks on demand. Keep archives immutable while open. It supports directly accessible `X/fulltext/xapian` and `X/title/xapian` blobs. It also checks the legacy full-text entry location for Glass databases with the supported schema.

## Build indexes

Pass the Rust helper to your existing zimwriterfs invocation.

```sh
zimwriterfs --xapianbuilder-path "$PWD/target/release/xapianbuilder" [your usual options] HTML_DIR OUTPUT.zim
```

You can also build indexes separately. Each record requires an archive-relative path and a title. Full-text indexing accepts HTML and plain text. Title records can supply a redirect target in `target_path`.

```sh
printf '%s\n' '{"path":"Earth","title":"Earth","mimetype":"text/html","body":"<p>Plate tectonics</p>"}' > corpus.jsonl
target/release/xapianbuilder fulltext --language eng --input corpus.jsonl --output fulltext.xapian
target/release/xapianbuilder title --language eng --input corpus.jsonl --output title.xapian
```

Use `--input -` to stream JSONL. Publication is no-clobber and requires a new output filename. The CLI implements the commands and flags used by zimwriterfs. See `xapianbuilder fulltext --help` for the supported options. The writer uses the default language for index metadata, accepts per-record stemming languages, and selects the bundled stopword list by exact language code, as the native helper does by default.

## Compatibility and scale

The target is libzim's title and full-text schema on single-file Glass, including the standard value slots and full-path document data. The ranking and tokenization baseline is Xapian 1.4. The engine implements the subset of query syntax used by libzim and has limited stemmer coverage. Geo queries, pre-Glass databases, termlist generation and incremental database updates are outside the implemented feature set.

Search streams matching document IDs and retains the best requested results. Broad terms can still require scoring every matching document. A broad title prefix can scan many dictionary terms. When more than 100 terms tie at the expansion cutoff, Rust chooses deterministically by term order and native Xapian may return different suggestions.

The library exposes per-query and per-record limits through `Index::open_with_limits` and `ReaderLimits`. The writer holds one index in memory and reads a complete JSONL record at a time. The measured scale is 50,000 synthetic articles plus two real Kiwix archives. See the [review findings and supported operating limits](AUDIT.md) and [benchmark results](PERFORMANCE.md).

The [third-party source review](THIRD_PARTY_REVIEW.md) records which findings apply to the original engine, the native interoperability tests and the remaining limits on untrusted archive ingestion.

## Verify

```sh
cargo test --workspace --locked
python3 tests/real_archives.py
```

The Python runner downloads two SHA-256-verified fixtures into `target/real-archives`. It runs the archive-dependent engine tests and compares path order for 120 full-text and suggestion queries with recorded libzim results. Set `ZIM_TESTDATA` to use a local fixture directory. Run the native writer comparison against a checkout of jasontitus/xapianbuilder with its development dependencies installed. This checks both databases with native Xapian, then compares decoded metadata, document data, values, terms, postings and positions.

```sh
tests/native-writer-parity.sh /path/to/xapianbuilder /path/to/native/xapianbuilder "$PWD/target/release/xapianbuilder"
```

For the end-to-end archive test with zimwriterfs and native libzim, see [tests/native-interop.sh](tests/native-interop.sh).

[The CJK parity test](tests/cjk_zim_parity.py) builds paired ZIMs for Chinese, Japanese, Korean, and newer Han characters. It checks the actual embedded title and full-text indexes with native Xapian and compares ordered paths across both readers and writers. The runner saves the archives, logical snapshots, diffs and `results.json`. It exits unsuccessfully on any mismatch. Run `python3 tests/cjk_zim_parity.py --help` in an environment with python-libzim for the required executable paths. Use `--case zho`, `--case jpn`, `--case kor` or `--case zho-extensions` to isolate a corpus.

## Provenance and distribution

The engine is extracted from Gordon Brander's `zimfile-xapian`. The archive reader is an ordinary MIT dependency of this GPL program. zimwriterfs invokes the helper as a separate process and exchanges JSONL and standard Glass files. Retain the GPL and third-party notices when redistributing, including the notices for stopword lists embedded in the executables. See [PROVENANCE.md](PROVENANCE.md), [LICENSE](LICENSE) and [the stopword notices](engine/LICENSES/README.md).
