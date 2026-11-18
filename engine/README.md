# zimfile-xapian

libzim's Xapian search indexes in pure Rust: a reimplementation, not
bindings. It reads, searches and writes the single-file Glass databases Kiwix
archives embed (`X/fulltext/xapian`, `X/title/xapian`), with libzim's
semantics on Xapian 1.4.

In this fork, `zimsearch` uses zimru to open the archive. The engine reads the embedded database through a `Source` and returns document paths such as `C/<path>`.

```rust
use zimfile_xapian::{Index, IndexWriter};

let index = Index::open(Box::new(bytes))?;           // any Source: Vec<u8>, a file range…
let hits = index.search("plate tectonics", 10, "eng")?; // path, BM25 score, percent, word count
let paths = index.suggest("plate", 10, "eng")?;

let mut w = IndexWriter::fulltext("eng");
w.add_html("Plate_tectonics", "Plate tectonics", &html)?;
w.write(&mut file, uuid)?;
```

The document insertion methods return a `Result` and validate the writer mode and supported Glass sizes. Pass a new temporary file or an empty `Cursor<Vec<u8>>` to `write`. A nonempty destination is rejected before any database bytes are written. Publish a temporary file after `write` succeeds, since an I/O error during encoding can leave an incomplete file.

The language argument picks the stemmer when an index doesn't record one.

For interactive searches, `search_page(query, offset, limit, language, include_stored_snippets, &cancelled)` returns ranked hits, optional index-provided snippets and a full-text match count. `suggest_page(query, offset, limit, language, &cancelled)` returns original document paths and an estimated total that can include aliases. A zero limit retrieves the count. Both methods poll the cancellation callback during matching and enforce the configured ranking and string budgets. For article excerpts, construct `SnippetQuery::new(query)` once per page and call `excerpt(html, &cancelled)` on each article prefix.


## Compatibility with libzim

Searches rank like libzim on Xapian 1.4: the same query parsing, term
generation (`removeAccents`, Xapian's word splitting, CJK n-grams, Snowball
stemmers as Xapian 1.4 ships them), BM25 parameters, sort and collapse.

The [standalone verification suite](../README.md#verify) compares regenerated terms, within-document frequencies and positions on two Kiwix archives. It checks 120 ordered top-ten results against libzim reference data. Native interoperability tests open both writers' databases in Xapian and search the packaged archives with libzim. The [review](../AUDIT.md) records the tested limits and compatibility differences.

Implementation coverage

| Shortcut | Cost | Upgrade |
|---|---|---|
| Only the postlist, docdata and position tables; termlist, spelling and synonym are empty, as in Kiwix's own indexes | No spelling correction, synonyms or term lists | A termlist writer (~80 lines) |
| Tags are never compressed on write (compressed ones are read) | Indexes a few percent bigger | Deflate docdata tags |
| Simple B-tree layout: unshortened branch keys, no freelist, not `sequential` | Slightly bigger branch levels | Shorten separators |
| Query parser: what libzim's lowercase queries reach (no brackets, field prefixes, `AND`/`OR`/`NEAR`) | Unusual suggestion syntax may parse differently | Port more of the grammar |
| Every match is scored exactly: no max-weight pruning | A very common word on a huge archive is slow | Xapian's weight bounds |
| Snippets: a plain-text window around the first query word | Not Kiwix's snippets | Port `MSet::snippet` |
| No geo search (geo values are written), no multi-archive search, no pre-Glass (Chert) indexes | — | — |
| The writer keeps an index in memory through `write` | Memory grows with indexed content and vocabulary | Spill sorted runs to disk |
| Stemmers: `rust-stemmers` (Snowball 2017, identical to Xapian 1.4's English on 237,897 words), which lacks Armenian, Basque, Catalan, Indonesian, Irish, Lithuanian and Nepali | Archives in those 7 languages aren't stemmed, so search matches Kiwix's indexes poorly and Kiwix matches zimfile-written ones poorly | Generate Rust stemmers from Xapian 1.4's Snowball sources |

Older libzim versions (e.g. the one that built the 2024-06 climate archive)
left articles whose title equals their path out of the title index; current
libzim, and zimfile, index them.

License: GPL-3.0-or-later.

The standalone fork reads full-text stopwords from database metadata. Its writer selects the Stopwords ISO list by exact language code, matching xapianbuilder defaults. For example, `en` selects the English list and `eng` selects an empty list. See [stopword provenance and notices](LICENSES/README.md).

## Unicode search data

Queries, titles, index text and snippets share the same character rules. The release engine reads generated Unicode 18.0.0 category flags from a 4,352-byte page directory and 37,376 bytes of unique 256-code-point pages. Its 2,019 canonical replacement mappings and three non-Hangul composition pairs come from the locked ICU4X 2.4.0 data packages (CLDR 49). The Unicode license and copyright notice accompany the data. [Unicode data notice](LICENSES/Unicode-3.0.txt)

The generator checks every scalar. All marks decompose to marks, and every surviving scalar has canonical combining class zero. Removing marks preserves survivor order. Adjacent canonical composition still joins Hangul Jamo and the three Kirat Rai vowel pairs, including across removed marks. The engine keeps the previous whole-string Rust lowercase step, including contextual Greek sigma. Lowercase version therefore depends on the Rust toolchain. The two-page lookup and generated mappings run under the engine's `forbid(unsafe_code)`; query characters never index an unchecked slice. The ICU normalizer and property provider remain development dependencies for data generation and comparison. Other components, including the stemmer's serde dependency, retain their own implementation and review boundaries.

Run from the paired workspace root after fetching the locked dependencies. Omit `--check` to regenerate the four table files, then inspect changes and run the equivalence checks before updating a Unicode version.

```sh
cargo run --locked --offline --release -p zimfile-xapian --example generate_unicode -- --check
cargo test --locked --offline --release -p zimfile-xapian --lib unicode_tests
cargo tree --locked --offline -p zimfile-xapian --edges normal
cargo bench --locked --offline -p zimfile-xapian --bench unicode_search
```
