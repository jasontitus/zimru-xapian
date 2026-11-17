# Review of the original zimfile-xapian implementation

**Decision.** The original engine can read, search and create usable Glass indexes. Its English writer matched native Xapian on 50,000 articles, but the original reader can turn a read failure into an empty result, and the writer can produce a structurally valid index with different search terms. Use the standalone fork for the supported libzim/Xapian profile. Public ingestion of arbitrary ZIM files needs a resource policy at the outer archive boundary as well as the Glass reader limits. [Original engine][up] · [Fork audit](AUDIT.md)

## Source and verification

The source reviewed was Gordon Brander's `gordonbrander/zim-mcp` at `3632a45929b907109eed592653a5dd35f43c1f75`. Its `crates/zimfile-xapian` and the fork's original `e8bfdb7:engine` share Git tree `83d07e21e8fc4f51dc6c6478b66d7d6230540c4d`. All 17 files, including 4,496 lines across 14 Rust files, matched. The Rust sources in the published 0.1.0 crate also matched. Reviewers read the original B-tree, integer and position codecs, posting and value tables, bulk writer, matcher, query parser, public APIs, HTML parser, term generation and text normalization. They compared the findings with the fork at `645559b`. Tests, fixture adapters, manifests and license provenance were inspected separately. [Pinned source][up] · [Provenance](PROVENANCE.md)

| Check on the pristine source | Result |
| --- | --- |
| Locked upstream release workspace, 90 verified fixture files, two real Kiwix archives, Xapian checker 1.4.29, zimcheck 3.8.0 | 128 passed, one long-title test ignored upstream. All 24 engine tests passed, including seven archive-dependent tests. The 128 also include the upstream archive and MCP packages. |
| Independent replay of exported queries | All 66 matched the upstream reference python-libzim 3.7.0. The older Debian 3.6.0 binding differed on two trailing-space suggestions. The native tool version matters when reproducing these results. |
| Six native writer comparisons on eight records, title and full text for each language | All six original databases passed structural checks. Only the two `eng` indexes matched the full native snapshots. Both `en` indexes differed in stopword handling and both `fr-FR` indexes differed in stemming. The pre-review fork already matched all six. |
| 50,000 English articles, two independently built indexes | Native Xapian 1.4.29 decoded identical logical content from the original and native writers for both indexes, including postings, document values and positions. |
| HTML and Unicode | All 253 named entities with both optional-semicolon forms and 12 ordinary parsing cases matched native (518 cases). The 20 multilingual normalization samples also matched. Separate raw-text, geotag and numeric-reference checks exposed differences described below. |

The workspace suite can be repeated in a detached checkout of the pinned upstream source with its `just testdata` fixtures and documented native tools, using `cargo test --workspace --release --locked`. Its 120 stored real-archive queries and its 66 exported live comparisons are different test sets. [Original workspace][workspace] · [Upstream conformance tests][up-tests]

## Findings from the original source

| Priority and area | Impact in the original | Fork disposition |
| --- | --- | --- |
| High · read failures | Language and term-existence reads discarded errors. A selected document with missing path data could disappear; invalid UTF-8 could change the returned path. A successful miss was unreliable after an I/O error. [Reader API][reader] | Previously fixed. The fork propagates I/O and selected-path errors. The previous failure-injection review covered retries at 25 read points. |
| High · Glass resource use and traversal | First use allocated a dense table indexed by the largest document ID, even on a valid sparse database. Position decoding allocated from an index-supplied count. Tag assembly and inflation had no output ceiling. Branch reads and posting arithmetic also lacked checks required to turn damage into a format error. [Database][db] · [Tags and B-trees][btree] · [Positions][bits] | Previously fixed with streaming lengths, bounded decoding and forward-progress checks. This review additionally aligned revision ordering, value-key consumption and position padding with the native format. |
| High for interoperability · named value slots | The API always read slots 0 and 1. A valid index can declare different slots or omit individual fields. On six ordinary native-built ZIMs the pristine reader reported the wrong word counts and collapsed distinct suggestions. [Search and suggestions][values] · [Native word count][native-count] · [Native suggestion ordering][native-order] | Fixed in this review. The fork reads named slots, preserves independent title sorting and redirect collapse, and uses legacy word-count slot 3 when the map is absent. Slots accept decimal numbers with surrounding ASCII whitespace. Invalid slot declarations are diagnosed. The Rust API returns `None` for an unavailable count. Native libzim returns zero for a declared count missing on an individual document. |
| High for large builds · writer validation and storage | The original narrowed document IDs, component numbers and block numbers, allowed index modes to be mixed, and could leave old blocks at the end of a reused output. It also accumulated all index content in memory and rebuilt document-length postings during serialization. [Glass writer][writer] · [Document construction][index] | Previous changes checked representation and modes, required an empty destination and removed the extra postings buffer. The core bulk writer remains memory-resident. This review fixed an empty redirect target in the public library API that could return both an article and its alias. |
| Medium · text and search parity | The original omitted stopwords, dropped a Latin term at some punctuated CJK boundaries, treated markup-looking script/style text as HTML, and accepted invalid or imprecise coordinates. The original term lookup missed locale-qualified stemmers. [HTML parser][html] · [Text and stemmers][text] · [Query parser][query] | Previously fixed for the tested profile. Unchanged behavior and remaining language/query differences are listed below. |
| Medium · low result limits and broad queries | The original matcher materialized and sorted matches before applying the user's limit. A suggestion tie group could also require all its stored titles. [Matcher][matcher] · [Result selection][values] | The fork streams posting intersections and retains top results. It still scores accepted documents, scans broad dictionary prefixes, and reads whole posting tags for term frequencies. |

The three additional Glass checks follow native Xapian's root/child revision comparison and integrity checker's requirements for position padding and the end of a value-chunk key. A normal lookup validates the blocks it visits. An out-of-order separator elsewhere in a damaged tree can still cause a false miss. Use a full native `xapian-check` when the task is to establish database integrity. [Native B-tree][native-tree] · [Native integrity checker][native-check] · [Native bit reader][native-bits]

## Verification after fixes

Fork commit `e0c0b65` passed all 64 workspace tests in release mode and all 64 with AddressSanitizer on 64-bit Linux. Tests included real archives, the three independent value-slot behaviors and a multi-leaf Glass record with older child revisions. All 120 real-archive ordered-result comparisons passed. Native writer snapshots matched in six configurations. Final packaged CJK comparisons against Xapian 1.4 and 2.0 reproduced the earlier results, including the documented divergences. Formatting and Clippy with warnings denied passed. [Fork tests](tests) · [Version-specific CJK results](AUDIT.md#cjk-archive-comparison)

For a direct performance comparison, the pristine engine and the reviewed fork opened the same 50,000-article Glass databases using the same file-backed Rust `Source`. Both returned identical top-ten output. Seven warm-cache release runs per version alternated order after warm-up. For the common full-text term, median process time was 4.58 ms upstream and 3.78 ms in the fork, with median peak RSS of 3,696 and 1,888 KiB. For the broad title query, medians were 160.89 and 110.16 ms, with 10,860 and 3,844 KiB. These measurements include process startup and exclude opening a ZIM container. They are separate from the [end-to-end CLI benchmark](PERFORMANCE.md).

## Confirmed differences still present in the fork

- **Suggestions with grouped exclusions.** On an ordinary title archive, libzim returned 28 paths for `meadow -(gray guide)` and zero for `-(meadow guide)`. Rust returned zero and 44 respectively. The parser binds the exclusion to one token. This was confirmed against libzim 9.2.3 and 9.8.2. Implementing grouped suggestion syntax requires native-backed tests for both positive and negative results. [Original query parser][query]
- **Programming-language names.** On an archive containing plain `c` and no `c++` or `c#` terms, both native stacks returned no hits for the suffixed queries. The Rust reader fell back to plain `c`, returning three full-text hits and two suggestions for each query. An application needing native parity cannot rely on the Rust suffix fallback for these terms. [Suffix selection][suffix]
- **Stemmers and version profiles.** Native has Armenian, Basque, Catalan, Indonesian, Irish, Lithuanian and Nepali stemmers absent from this Rust dependency. The fork also stems `fre`, `ger`, `dut` and `rum` where the tested native helper with ICU 76.1 leaves words unchanged. This affects stored terms for both default and per-record languages. Preserve the distinction when reading previously built indexes. The CJK differences for newer Han and Xapian 2.0 fullwidth terms are independently documented. [Original text module][text] · [CJK measurements](AUDIT.md#cjk-archive-comparison)
- **HTML, query expansion and snippets.** Both the original parser and native reference concatenate visible text across some HTML5 sectioning elements. The Rust text scanner still creates a `Vec<char>` for each input and can accumulate a full token before the indexing length check. The snippet function is an approximation. Equal-frequency terms at the wildcard cutoff can also change top-ten results. [HTML parser][html] · [Original snippet][snippet] · [Expansion measurements](PERFORMANCE.md)

## Archive and dependency boundary

The fork's CLI uses MIT zimru `061afbc`. Three operations in that dependency happen **before Glass limits take effect**. These are source-level availability findings; cost depends on the archive and available memory.

| Reachable operation | Resource exposure | Where a limit belongs |
| --- | --- | --- |
| Archive open parses the MIME list. | Copies strings until the terminator. The parser's scan can extend to the end of the file. | Bound the MIME region, count and cumulative bytes before allocation. [MIME parser][mime] |
| Directory lookup and redirect resolution parse entry paths and titles. | Binary-search probes scan for a terminator. Matching and redirected entries own copies of strings. A redirect hop count leaves individual field size unbounded. | Bound scan span and field bytes before copying. [Directory lookup][dir] · [String reader][raw] |
| First direct index lookup resolves a cluster boundary. | The archive copies and sorts its entire cluster pointer table, using eight bytes per cluster plus sort time. | Budget the table or find the next higher offset with a constant-space scan that also handles unordered clusters. [Cluster-boundary code][clusters] |

The CLI maps the archive. A stable open handle prevents pathname replacement from changing the object read. The underlying file must remain immutable while mapped. Several outer-reader offsets also use `u64 as usize`; the verified target is 64-bit Linux. The bounded XZ and Zstandard language fallback passes memory/window limits into the native decoders before reading. Cluster validation takes an additional copy of the decoded payload. [CLI archive access](src/bin/zimsearch.rs) · [zimru archive reader][archive]

The original and forked engine build graphs on this target have the same 41 package/version entries. Direct dependencies are flate2, ICU4X normalization/properties and rust-stemmers. The upstream workspace lockfile scan covered 211 packages and reported one unmaintained `chrono` warning. The fork's 137-package lockfile reported two unsoundness advisories in zimru's `lru 0.12.5`. Its cluster cache stays empty along the traced CLI index path. These are separate lockfiles. The source review covered the imported engine and reachable integration/codec paths; the package inventory and advisory scans have narrower evidentiary value for other transitive code. [Original manifest][manifest] · [Fork dependency audit](AUDIT.md#dependencies-and-distribution) · [chrono advisory][chrono]

The imported crate declares GPL-3.0-or-later. Source comments identify ports of Xapian Omega through libzim, and the Xapian query, format and ranking algorithms. The four query goldens, 58 copied stopword files and both stopword notices matched their documented sources. Matching Git objects establishes origin. Distribution also needs the applicable attribution and corresponding source for the components actually shipped. [Provenance and license records](PROVENANCE.md)

[workspace]: https://github.com/gordonbrander/zim-mcp/tree/3632a45929b907109eed592653a5dd35f43c1f75
[up]: https://github.com/gordonbrander/zim-mcp/tree/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian
[up-tests]: https://github.com/gordonbrander/zim-mcp/tree/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile/tests
[chrono]: https://rustsec.org/advisories/RUSTSEC-2026-0543.html
[manifest]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/Cargo.toml
[reader]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/lib.rs#L81-L95
[values]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/lib.rs#L118-L168
[db]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/glass/db.rs#L161-L178
[btree]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/glass/btree.rs#L284-L323
[bits]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/glass/bitstream.rs#L122-L148
[writer]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/glass/write.rs
[index]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/index.rs
[matcher]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/matcher.rs#L380-L398
[html]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/html.rs
[text]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/text.rs#L232-L264
[query]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/query.rs
[suffix]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/query.rs#L172-L191
[snippet]: https://github.com/gordonbrander/zim-mcp/blob/3632a45929b907109eed592653a5dd35f43c1f75/crates/zimfile-xapian/src/lib.rs#L235-L278
[archive]: https://github.com/jasontitus/zimru/blob/061afbc2920c78fc4ecb7211cb9ef4f3655dba35/src/archive.rs#L186-L196
[mime]: https://github.com/jasontitus/zimru/blob/061afbc2920c78fc4ecb7211cb9ef4f3655dba35/src/mime.rs#L13-L25
[dir]: https://github.com/jasontitus/zimru/blob/061afbc2920c78fc4ecb7211cb9ef4f3655dba35/src/archive.rs#L595-L615
[raw]: https://github.com/jasontitus/zimru/blob/061afbc2920c78fc4ecb7211cb9ef4f3655dba35/src/raw.rs#L36-L46
[clusters]: https://github.com/jasontitus/zimru/blob/061afbc2920c78fc4ecb7211cb9ef4f3655dba35/src/archive.rs#L1152-L1171
[native-tree]: https://github.com/xapian/xapian/blob/v2.0.0/xapian-core/backends/glass/glass_table.cc#L338-L345
[native-check]: https://github.com/xapian/xapian/blob/v2.0.0/xapian-core/backends/glass/glass_dbcheck.cc
[native-bits]: https://github.com/xapian/xapian/blob/v2.0.0/xapian-core/common/bitstream.h#L140-L148
[native-count]: https://github.com/openzim/libzim/blob/26ec526f74e8342a40da5ab14e364546988e0e1a/src/search_iterator.cpp#L204-L219
[native-order]: https://github.com/openzim/libzim/blob/26ec526f74e8342a40da5ab14e364546988e0e1a/src/suggestion.cpp#L279-L286
