# zimfile-xapian

libzim's Xapian search indexes in pure Rust: a reimplementation, not
bindings. It reads, searches and writes the single-file Glass databases Kiwix
archives embed (`X/fulltext/xapian`, `X/title/xapian`), with libzim's
semantics on Xapian 1.4.

It's the search engine behind [zimfile](../zimfile), and knows libzim's index
conventions but not the ZIM container: an `Index` reads bytes through a
`Source`, and results are document paths (`C/<path>`). zimfile maps them to
entries.

```rust
use zimfile_xapian::{Index, IndexWriter};

let index = Index::open(Box::new(bytes))?;           // any Source: Vec<u8>, a file range…
let hits = index.search("plate tectonics", 10, "eng")?; // path, BM25 score, percent, word count
let paths = index.suggest("plate", 10, "eng")?;

let mut w = IndexWriter::fulltext("eng");
w.add_html("Plate_tectonics", "Plate tectonics", &html);
w.write(&mut file, uuid)?;
```

The language argument picks the stemmer when an index doesn't record one.

## Compatibility with libzim

Searches rank like libzim on Xapian 1.4: the same query parsing, term
generation (`removeAccents`, Xapian's word splitting, CJK n-grams, Snowball
stemmers as Xapian 1.4 ships them), BM25 parameters, sort and collapse.

How that's checked:

- Every title and full-text document of two Kiwix archives regenerates
  libzim's exact terms, wdfs and positions from its HTML and title (this
  crate's tests, which read the archives through zimfile).
- On 120 reference queries over those archives, zimfile returns libzim's top
  10 exactly (`zimfile/tests/xapian.rs`).
- Indexes written by `IndexWriter` (through zimfile's `Creator::index`) pass
  `xapian-check` 1.4 and 2.1. The archives pass `zimcheck -A`, and libzim
  returns the same results as zimfile on them.
- libzim's own suggestion and search tests are ported, and they pass through
  real libzim too (`zimfile/tests/libzim_xapian.rs`).

Shortcuts, each harmless to libzim readers:

| Shortcut | Cost | Upgrade |
|---|---|---|
| Only the postlist, docdata and position tables; termlist, spelling and synonym are empty, as in Kiwix's own indexes | No spelling correction, synonyms or term lists | A termlist writer (~80 lines) |
| Tags are never compressed on write (compressed ones are read) | Indexes a few percent bigger | Deflate docdata tags |
| Simple B-tree layout: unshortened branch keys, no freelist, not `sequential` | Slightly bigger branch levels | Shorten separators |
| No stopwords, ever (libzim finds none for `eng`, so its archives have none) | Common words are indexed and must match | Ship libzim's stopword lists |
| Query parser: what libzim's lowercase queries reach (no brackets, field prefixes, `AND`/`OR`/`NEAR`) | Unusual suggestion syntax may parse differently | Port more of the grammar |
| Every match is scored exactly: no max-weight pruning | A very common word on a huge archive is slow | Xapian's weight bounds |
| Snippets: a plain-text window around the first query word | Not Kiwix's snippets | Port `MSet::snippet` |
| No geo search (geo values are written), no multi-archive search, no pre-Glass (Chert) indexes | — | — |
| Title words over 240 bytes are dropped; libzim aborts the archive instead (`TitleIndexingError`) | — | — |
| The writer holds the full-text index in memory until `finish` | RAM ≈ the index size (1–2 GB for all of English Wikipedia) | Spill sorted runs to disk |
| Stemmers: `rust-stemmers` (Snowball 2017, identical to Xapian 1.4's English on 237,897 words), which lacks Armenian, Basque, Catalan, Indonesian, Irish, Lithuanian and Nepali | Archives in those 7 languages aren't stemmed, so search matches Kiwix's indexes poorly and Kiwix matches zimfile-written ones poorly | Generate Rust stemmers from Xapian 1.4's Snowball sources |

Older libzim versions (e.g. the one that built the 2024-06 climate archive)
left articles whose title equals their path out of the title index; current
libzim, and zimfile, index them.

License: GPL-3.0-or-later.
