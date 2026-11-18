# Review of the standalone Glass reader and writer

The codec and storage descriptions below record the original audit. The current paired branches use positioned I/O and Rust Zstandard, XZ and Glass DEFLATE implementations. [Current codec review](https://github.com/jasontitus/zimru-xapian/blob/1b07bd682638694fc2a8d3152e507e351941b5e8/docs/rust-codecs.md)

Reviewed on 2026-11-17 against the standalone fork at `54b39d4`. The changes cover the search engine, index writer, archive/JSONL command-line boundary and locked dependency graph. Reviewers challenged changes written by other reviewers. Native Xapian and libzim served as independent references.

The [third-party source review](THIRD_PARTY_REVIEW.md) covers the pristine upstream implementation, native comparisons, inherited behavioral differences and the outer archive reader.

## Findings and repairs

| Area | Finding | Change and evidence |
| --- | --- | --- |
| Glass decoding | Stored tags, DEFLATE output, decoded position lists and the collection of active posting chunks could consume substantial memory. | Enforce configurable limits, require decoder progress and check block levels, leaf ordering and posting ranges. Check the limit before growing a tag or decoding a position list. Tests use real multi-component metadata, compressed text, positions and concurrent posting cursors at and across configured limits. |
| Query execution | A small result limit still materialized large sets of matches and document lengths. Unrestricted wildcard expansion retained every selected posting list. | Stream posting intersections and document lengths, score accepted documents and retain the best requested results. Bound query length, expanded terms and live posting bytes, including the document-length cursor. Phrase evaluation keeps the candidate spans and one subsequent position list. Test ranking ties, late redirect replacements and small limits on valid databases. |
| Document paths and failures | A selected document without a path could look like a successful miss. Lossy UTF-8 decoding could produce an unusable path. | Require a nonempty UTF-8 path for each returned result. A native-generated Glass archive verified that both commands return a diagnostic when the invalid result is selected. A lower-ranked invalid path leaves a selected Unicode hit intact. Across 25 simulated transient read failures, the library returned an I/O error and a retry on the same index succeeded. |
| Archive metadata | Reading the fallback language could decompress a whole archive cluster even for an empty query. | Keep reads on the original open file and use positional index reads on Unix. Use the first language field only. Enforce compressed-input, decoded-output and codec memory limits for legacy metadata. Skip language lookup when the index provides a language or the query requests no hits. |
| Index creation and publication | Narrow Glass fields could lose information. Writing a shorter index into a reused seekable destination could leave old blocks at its end. | Check document IDs, counts, lengths and component/block representation. Return insertion errors through the library and CLI. Reject a nonempty destination before writing. The CLI writes and syncs a temporary file in the destination directory and publishes it without overwriting an existing name. Tests cover I/O failure, existing files, dangling symlinks and competing writers. |
| HTML and query text | Script/style text containing markup could affect indexed text. Some mixed Latin/CJK queries lost their leading term. | Match raw-text closing tags and preserve first-attribute handling. Check entity decoding and Unicode normalization. Both native-generated index snapshots matched on an independent seven-document HTML corpus. Ten mixed-script punctuation queries matched native rankings, including four corrected cases. |
| Stopword metadata | A tag-byte limit alone permitted much higher memory use when creating a hash table of stopwords. | Validate UTF-8 and enforce a configurable cap on distinct entries. Duplicate and empty lines consume no extra entries. Tests exercise the exact limit and one below using the bundled Zulu list. |

## Operating limits

Defaults for the library's `ReaderLimits` are 64 MiB per stored or inflated Glass tag, 16,777,216 positions per position list, 128 KiB of query input, 4,096 expanded terms, 64 MiB of live posting chunks per query and 65,536 distinct stopwords. A configured limit can reject an otherwise valid archive or query. Library callers can set these limits with `Index::open_with_limits`.

The CLI fallback for indexes lacking language metadata reads at most 1,025 bytes of a directly accessible language item to identify a first field of up to 1,024 bytes. For compressed items, the limits are 65 MiB for the stored cluster, 64 MiB for decoded data, 128 MiB for XZ decoder memory and a 64 MiB Zstandard window. The decoder, validated cluster copy, archive mapping, retained result paths/titles and block cache have separate memory costs. OS process limits apply independently. The CLI's archive mapping requires the input file to stay immutable while in use.

Broad queries can still visit every match and large prefix regions can scan the dictionary. Result memory grows with the requested limit and returned string lengths. The writer consumes a whole JSONL line at a time and keeps the index in memory through serialization. Wikipedia-scale creation, crash recovery after a machine loses power and hostile multi-tenant workloads are outside the measurements. The CLI syncs the completed file before publishing it. Directory-entry durability across power loss depends on the filesystem. The verified platform is x86-64 Linux.

## Compatibility

- Both Kiwix archives matched the four 30-query libzim ordered top-ten reference lists. On a separate full-text differential, 114 comparisons matched paths, scores (relative tolerance 1e-12) and percentages. An alias-heavy title fixture matched the earlier Rust release in 70 comparisons.
- Across a prefix-expansion cutoff with equally frequent terms, native Xapian may choose a different subset. In the alias fixture six deeper lists diverged from native. The first ten matched on that fixture. A synthetic 10,000-way tie also changed the top ten. The Rust fork retains lexical order for equal frequencies. [Benchmark and expansion details](PERFORMANCE.md)
- `add_html`, `add_text` and `add_title` return results that callers should propagate. `IndexWriter::write` requires an empty destination. An I/O error can partially fill a caller-supplied sink. The CLI publishes completed files only.

### CJK archive comparison

Each writer built a ZIM from the same articles. Native Xapian decoded each embedded title and full-text index for comparison. A query counts as a match when both readers returned the same ordered paths on both writers' archives.

| Native Xapian writer / query reader | Corpus | Matching index pairs | Matching queries |
| --- | --- | ---: | ---: |
| 1.4.29 / 1.4.23 | Chinese, Japanese and Korean | 6/6 | 63/63 |
| 1.4.29 / 1.4.23 | Han extensions G, H and I | 0/2 | 6/12 |
| 2.0.0 / 2.0.0 | Chinese, Japanese and Korean | 4/6 | 61/63 |
| 2.0.0 / 2.0.0 | Han extensions G, H and I | 0/2 | 6/12 |

Both query comparisons used libzim 9.8.2. The ordinary 2.0 differences were the fullwidth queries `ＡＩ` and `１２３`. Each of the three rare-Han queries differed in both full-text and title search. The reviewed release reproduced the pre-review results on both native versions. Older libzim releases can also differ on single-character suggestions. [CJK comparison runner](tests/cjk_zim_parity.py) · [libzim wildcard fallback change](https://github.com/openzim/libzim/commit/2b5a9056b1c29f3ecaa9820ee0bb389265ca64af)

## Verification of the combined changes

The final Linux workspace passed all 61 tests in release mode with both real-archive fixtures enabled, and all 61 in a separate AddressSanitizer run with leak detection. The sanitizer run instrumented the Rust workspace. Prebuilt standard-library and native codec components have different instrumentation coverage. Formatting and Clippy with warnings denied passed.

The native builder comparison matched title and full-text logical snapshots for `eng`, `en` and `fr-FR`. The end-to-end test created ZIMs with each builder and checked full-text search, title suggestions and an empty query using both readers. The 50,000-article performance corpus produced matching native-decoded indexes and all 396 complete JSON comparisons matched the prior Rust build. [Native reference comparison](tests/native-writer-parity.sh) · [Archive integration](tests/native-interop.sh) · [Measurement protocol](PERFORMANCE.md)

## Dependencies and distribution

The lockfile scan used cargo-audit 0.23.0 and RustSec database `233155b`. It reported zero entries in its vulnerability category and two unsoundness warnings for `lru 0.12.5`, brought in by the pinned zimru revision. One concerns `IterMut`; the other concerns `pop()` when a key destructor panics. Source inspection found a cache keyed by `u32`, using `get`, `put`, `push` and `pop_lru`. The affected API usages were absent in this path. The dependency warrants an upstream upgrade. An isolated upgrade to `lru 0.18.5` passed 22 zimru reader tests. The shipped dependency pin still resolves to 0.12.5. [IterMut advisory](https://rustsec.org/advisories/RUSTSEC-2026-0002.html) · [pop advisory](https://rustsec.org/advisories/RUSTSEC-2026-0253.html)

The Glass crate forbids unsafe Rust. The archive CLI maps the input with memmap2, and the archive codecs include native code. A build without discoverable liblzma development metadata can fall back to the old liblzma 5.2.5 bundled with lzma-sys. The audited Linux release used pkg-config and liblzma 5.8.4. For distribution, control the codec versions in the build environment and verify the resulting binary's linkage. [XZ project](https://tukaani.org/xz/) · [Dependency lockfile](Cargo.lock)

All changes are in this standalone GPL project. The MIT zimru dependency remains pinned. The two processes exchange JSONL, ZIMs and Glass databases through their documented interfaces. [Source provenance and notices](PROVENANCE.md)
