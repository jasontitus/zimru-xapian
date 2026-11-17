// Modified for the standalone zimru-xapian tools on 2026-11-17. See PROVENANCE.md.
//! libzim's Xapian search indexes in pure Rust (a reimplementation, not
//! bindings): read, search and write the single-file Glass databases Kiwix
//! archives embed (`X/fulltext/xapian`, `X/title/xapian`), with libzim's
//! semantics on Xapian 1.4 — its query parsing, term generation and ranking.
//!
//! This crate knows libzim's index conventions (documents' data is their
//! full path, `C/<path>`; the value slots; the title anchor term) but not
//! the ZIM container: an [`Index`] reads its bytes through a [`Source`], and
//! results are document paths. `zimfile` maps them to entries.

#![forbid(unsafe_code)]

mod glass;
mod html;
mod index;
mod matcher;
mod query;
mod stopwords;
mod text;

#[cfg(test)]
mod testdata;

use glass::Db;
use matcher::Match;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Seek, Write};
use text::{Stemmer, parse_terms, remove_accents};

pub use glass::Source;

/// Resource limits for Glass records, query evaluation, and index-provided stopwords.
#[derive(Clone, Copy, Debug)]
pub struct ReaderLimits {
    /// Maximum bytes in a tag, both on disk and after DEFLATE expansion.
    pub max_tag_bytes: usize,
    /// Maximum number of 32-bit positions in a single position list.
    pub max_positions: usize,
    /// Maximum UTF-8 bytes of user input per query.
    pub max_query_bytes: usize,
    /// Maximum number of query terms after parsing and expansion.
    pub max_query_terms: usize,
    /// Maximum bytes of live decoded posting chunks for a single query.
    pub max_posting_bytes: usize,
    /// Maximum distinct nonempty words in the full-text index stoplist.
    pub max_stopwords: usize,
}

impl Default for ReaderLimits {
    fn default() -> Self {
        Self {
            max_tag_bytes: 64 * 1024 * 1024,
            max_positions: 16 * 1024 * 1024,
            max_query_bytes: 128 * 1024,
            max_query_terms: 4096,
            max_posting_bytes: 64 * 1024 * 1024,
            max_stopwords: 65_536,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The index is corrupt, or not a Glass database.
    Format(String),
    /// An operation exceeds a configured limit or cannot be represented.
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Io(e) => e.fmt(f),
            Error::Format(m) | Error::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

/// One full-text search result.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// The document's full path (`C/<path>`).
    pub path: String,
    /// BM25 weight, as Xapian computes it.
    pub score: f64,
    /// Xapian's percentage relevance.
    pub percent: u8,
    /// The article's word count, if the index stores it.
    pub word_count: Option<u32>,
}

/// Best results sort first. Title suggestions additionally break weight ties
/// by the original title bytes. In both modes docid is the final tie-break.
#[derive(Clone, Debug)]
struct Rank {
    weight: f64,
    title: Vec<u8>,
    did: u32,
}

impl Ord for Rank {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .weight
            .total_cmp(&self.weight)
            .then_with(|| self.title.cmp(&other.title))
            .then_with(|| self.did.cmp(&other.did))
    }
}

impl PartialOrd for Rank {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Rank {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Rank {}

/// The names used by libzim's `valuesmap` metadata. Other names are
/// permitted. Only the three slots consumed by this reader need storage.
#[derive(Default)]
struct ValueSlots {
    title: Option<u32>,
    target: Option<u32>,
    word_count: Option<u32>,
}

impl ValueSlots {
    fn read(db: &Db) -> Result<Self> {
        let raw = db.metadata("valuesmap")?.unwrap_or_default();
        let metadata = std::str::from_utf8(&raw)
            .map_err(|_| Error::Format("invalid UTF-8 in valuesmap".into()))?;
        let mut slots = Self::default();
        let mut fields = metadata.split(';').filter(|s| !s.is_empty()).peekable();
        if fields.peek().is_none() {
            // libzim's fallback for indexes predating named value slots.
            slots.word_count = Some(3);
        }
        for field in fields {
            let (name, value) = field
                .split_once(':')
                .ok_or_else(|| Error::Format("missing slot number in valuesmap".into()))?;
            let slot = value
                .trim_ascii()
                .parse::<u32>()
                .map_err(|_| Error::Format("invalid slot number in valuesmap".into()))?;
            // libzim keeps the first entry for a repeated name.
            match name {
                "title" => slots.title.get_or_insert(slot),
                "targetPath" => slots.target.get_or_insert(slot),
                "wordcount" => slots.word_count.get_or_insert(slot),
                _ => continue,
            };
        }
        Ok(slots)
    }
}

fn slot_value(db: &Db, did: u32, slot: Option<u32>) -> Result<Option<Vec<u8>>> {
    match slot {
        Some(slot) => db.value(did, slot),
        None => Ok(None),
    }
}

/// One embedded index, opened.
pub struct Index {
    db: Db,
}

impl Index {
    pub fn open(src: Box<dyn Source>) -> Result<Index> {
        Self::open_with_limits(src, ReaderLimits::default())
    }

    /// Open an index with explicit per-record decoder limits.
    pub fn open_with_limits(src: Box<dyn Source>, limits: ReaderLimits) -> Result<Index> {
        Ok(Index {
            db: Db::open_with_limits(src, limits)?,
        })
    }

    /// Read an index metadata value, preserving the distinction between missing data and read errors.
    pub fn metadata(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.db.metadata(key)
    }

    /// The stemmer: the index's `language` metadata, else `lang` (old indexes
    /// have none; libzim then uses the archive's `M/Language`).
    fn stemmer(&self, lang: &str) -> Result<Option<Stemmer>> {
        let meta = self.db.metadata("language")?.unwrap_or_default();
        let meta = String::from_utf8_lossy(&meta);
        let lang = if meta.is_empty() { lang } else { &meta };
        Ok(text::stemmer(lang.split(',').next().unwrap_or("")))
    }

    fn normalize_query(&self, q: &str) -> Result<String> {
        let limit = self.db.limits().max_query_bytes;
        if q.len() > limit {
            return Err(Error::Invalid(format!(
                "query exceeds the {limit} byte limit"
            )));
        }
        Ok(remove_accents(q))
    }

    fn document_path(&self, did: u32) -> Result<String> {
        let bytes = self
            .db
            .data(did)?
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Format(format!("missing document path for docid {did}")))?;
        String::from_utf8(bytes)
            .map_err(|_| Error::Format(format!("invalid UTF-8 document path for docid {did}")))
    }

    fn exists(&self, term: &str) -> Result<bool> {
        Ok(self.db.freqs(term.as_bytes())?.0 > 0)
    }

    /// Full-text search: the best `limit` documents for `q`, as libzim ranks them.
    pub fn search(&self, q: &str, limit: usize, lang: &str) -> Result<Vec<Hit>> {
        if limit == 0 || q.is_empty() {
            return Ok(vec![]);
        }
        let normalized = self.normalize_query(q)?;
        let db = &self.db;
        let stem = self.stemmer(lang)?;
        let metadata = db.metadata("stopwords")?.unwrap_or_default();
        let words = stopwords::from_metadata(&metadata, db.limits().max_stopwords)?;
        let Some(query) = query::fulltext(
            &normalized,
            stem.as_ref(),
            &|term| self.exists(term),
            &words,
        )?
        else {
            return Ok(vec![]);
        };
        let mut top = BTreeSet::<Rank>::new();
        let mut best: Option<Match> = None;
        let total = matcher::for_each_match(db, &query, matcher::FULLTEXT, |m| {
            if best.is_none_or(|b| m.weight > b.weight) {
                best = Some(m);
            }
            let rank = Rank {
                weight: m.weight,
                title: vec![],
                did: m.did,
            };
            if top.len() < limit || top.last().is_some_and(|worst| rank < *worst) {
                top.insert(rank);
                if top.len() > limit {
                    top.pop_last();
                }
            }
            Ok(())
        })?;
        let Some(best) = best else {
            return Ok(vec![]);
        };
        let slots = ValueSlots::read(db)?;
        let mut hits = Vec::with_capacity(top.len());
        for rank in top {
            let word_count = slot_value(db, rank.did, slots.word_count)?
                .and_then(|v| String::from_utf8(v).ok()?.parse().ok());
            hits.push(Hit {
                path: self.document_path(rank.did)?,
                score: rank.weight,
                percent: matcher::percent(rank.weight, &best, total),
                word_count,
            });
        }
        Ok(hits)
    }

    /// Title suggestions, as libzim makes them: document paths.
    pub fn suggest(&self, q: &str, limit: usize, lang: &str) -> Result<Vec<String>> {
        if limit == 0 || q.is_empty() {
            return Ok(vec![]);
        }
        let normalized = self.normalize_query(q)?;
        let db = &self.db;
        let stem = self.stemmer(lang)?;
        let Some(query) = query::suggestion(&normalized, stem.as_ref(), &|term| self.exists(term))?
        else {
            return Ok(vec![]);
        };
        let slots = ValueSlots::read(db)?;
        // Retain the best representative of each target among the top K.
        // An evicted target may re-enter later through a better-scoring alias.
        let mut top = BTreeMap::<Rank, Vec<u8>>::new();
        let mut retained_targets = HashMap::<Vec<u8>, Rank>::new();
        matcher::for_each_match(db, &query, matcher::SUGGEST, |m| {
            let worst = top.last_key_value().map(|(rank, _)| rank);
            if top.len() == limit && worst.is_some_and(|w| m.weight < w.weight) {
                return Ok(());
            }
            let rank = Rank {
                weight: m.weight,
                title: slot_value(db, m.did, slots.title)?.unwrap_or_default(),
                did: m.did,
            };
            if top.len() == limit && worst.is_some_and(|w| rank >= *w) {
                return Ok(());
            }
            let target = slot_value(db, m.did, slots.target)?.unwrap_or_default();
            if retained_targets.get(&target).is_some_and(|r| rank >= *r) {
                return Ok(());
            }
            if !target.is_empty()
                && let Some(old) = retained_targets.insert(target.clone(), rank.clone())
            {
                top.remove(&old);
            }
            top.insert(rank, target);
            if top.len() > limit
                && let Some((_, key)) = top.pop_last()
            {
                retained_targets.remove(&key);
            }
            Ok(())
        })?;
        top.into_keys()
            .map(|rank| self.document_path(rank.did))
            .collect()
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use crate::glass::write::{Builder, Terms};
    use std::io::Cursor;

    #[test]
    fn named_and_legacy_value_slots_follow_libzim() {
        let index = |map: Option<&[u8]>| {
            let mut db = Builder::default();
            if let Some(map) = map {
                db.set_metadata("valuesmap", map);
            }
            let terms = Terms::from([(b"prairie".to_vec(), (1, vec![]))]);
            for (path, title, target) in [
                ("C/first", "Zulu", "shared"),
                ("C/second", "Alpha", "shared"),
                ("C/third", "Bravo", "other"),
            ] {
                db.add(
                    path.as_bytes(),
                    &terms,
                    &[
                        (1, b"1".to_vec()),
                        (3, b"30".to_vec()),
                        (5, title.as_bytes().to_vec()),
                        (6, target.as_bytes().to_vec()),
                        (7, b"70".to_vec()),
                    ],
                )
                .unwrap();
            }
            let mut file = Cursor::new(Vec::new());
            db.write(&mut file, [0; 16]).unwrap();
            Index::open(Box::new(file.into_inner())).unwrap()
        };
        let named = index(Some(b"wordcount: 7 ;title:5;targetPath:6;wordcount:1"));
        assert_eq!(
            named.search("prairie", 1, "").unwrap()[0].word_count,
            Some(70)
        );
        assert_eq!(
            named.suggest("prairie", 3, "").unwrap(),
            ["C/second", "C/third"]
        );
        let missing = index(Some(b"title:5"));
        assert_eq!(
            missing.search("prairie", 1, "").unwrap()[0].word_count,
            None
        );
        assert_eq!(
            missing.suggest("prairie", 3, "").unwrap(),
            ["C/second", "C/third", "C/first"]
        );
        let legacy = index(None);
        assert_eq!(
            legacy.search("prairie", 1, "").unwrap()[0].word_count,
            Some(30)
        );
        assert_eq!(
            legacy.suggest("prairie", 3, "").unwrap(),
            ["C/first", "C/second", "C/third"]
        );
    }

    #[test]
    fn selected_documents_require_a_utf8_path() {
        let terms = Terms::from([(b"prairie".to_vec(), (1, vec![]))]);
        for data in [&b""[..], &[0xff]] {
            let mut db = Builder::default();
            db.add(data, &terms, &[]).unwrap();
            let mut file = Cursor::new(Vec::new());
            db.write(&mut file, [0; 16]).unwrap();
            let i = Index::open(Box::new(file.into_inner())).unwrap();
            assert!(matches!(i.search("prairie", 1, ""), Err(Error::Format(_))));
            assert!(matches!(i.suggest("prairie", 1, ""), Err(Error::Format(_))));
        }
    }
}

/// One index being built, as libzim's creator builds it: the full-text
/// index over HTML documents, or the title index over front articles and
/// redirects. Documents are added in docid order, kept in memory, and laid
/// out by [`write`](Self::write).
pub struct IndexWriter {
    fulltext: bool,
    language: String,
    stemmer: Option<Stemmer>,
    stopwords: HashSet<&'static str>,
    builder: glass::write::Builder,
}

impl IndexWriter {
    fn new(fulltext: bool, language: &str) -> IndexWriter {
        IndexWriter {
            fulltext,
            language: language.into(),
            stemmer: text::stemmer(language),
            stopwords: stopwords::words(stopwords::bundled(language)).collect(),
            builder: Default::default(),
        }
    }

    /// The full-text index. `language` is ISO 639-3 (`eng`) or 639-1; it picks the stemmer.
    pub fn fulltext(language: &str) -> IndexWriter {
        IndexWriter::new(true, language)
    }

    /// The title (suggestion) index.
    pub fn title(language: &str) -> IndexWriter {
        IndexWriter::new(false, language)
    }

    /// Set the language used to stem subsequent documents. The database
    /// metadata continues to describe the default query language.
    pub fn set_stemming_language(&mut self, language: &str) {
        self.stemmer = text::stemmer(language);
    }

    /// Index literal non-HTML content. Returns an error for a title writer
    /// or when the document exceeds the supported Glass limits.
    pub fn add_text(&mut self, path: &str, title: &str, body: &str) -> Result<()> {
        if !self.fulltext {
            return Err(Error::Invalid(
                "text documents require a full-text writer".into(),
            ));
        }
        if body.is_empty() {
            return Ok(());
        }
        let title = remove_accents(title);
        let terms =
            index::fulltext_terms(body, &title, "", self.stemmer.as_ref(), &self.stopwords)?;
        let values = [
            (0, title.into_bytes()),
            (
                1,
                index::word_count_value(body.split_whitespace().count())?
                    .to_string()
                    .into_bytes(),
            ),
        ];
        self.builder
            .add(format!("C/{path}").as_bytes(), &terms, &values)?;
        Ok(())
    }

    /// Add an HTML item at `C/<path>` (skipped if it has no text or asks not
    /// to be indexed, as in libzim). Returns an error for a title writer
    /// or when the document exceeds the supported Glass limits.
    pub fn add_html(&mut self, path: &str, title: &str, html: &str) -> Result<()> {
        if !self.fulltext {
            return Err(Error::Invalid(
                "HTML documents require a full-text writer".into(),
            ));
        }
        index::add_fulltext(
            &mut self.builder,
            path,
            title,
            html,
            self.stemmer.as_ref(),
            &self.stopwords,
        )
    }

    /// Add a front article, or a redirect to `target`, at `C/<path>`.
    /// Returns an error for a full-text writer, a title with too much
    /// discarded text, or a document exceeding the supported Glass limits.
    pub fn add_title(&mut self, path: &str, title: &str, target: Option<&str>) -> Result<()> {
        if self.fulltext {
            return Err(Error::Invalid(
                "title documents require a title writer".into(),
            ));
        }
        index::add_title(
            &mut self.builder,
            path,
            title,
            target,
            self.stemmer.as_ref(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.builder.doccount() == 0
    }

    /// Finish the database in an empty seekable destination with the given UUID.
    /// A nonempty destination is rejected before writing, restoring its cursor.
    /// A later I/O error can leave a partial database. Write to a fresh temporary
    /// file and publish it after success when the result will be shared.
    pub fn write(mut self, out: &mut (impl Write + Seek), uuid: [u8; 16]) -> Result<()> {
        index::set_metadata(&mut self.builder, self.fulltext, &self.language);
        self.builder.write(out, uuid)
    }
}

/// A plain-text extract of an article's HTML around the first words of `q`
/// (about 200 characters).
// ponytail: the first window holding a query word, not Xapian's snippet
// algorithm (MSet::snippet); port that if matching Kiwix's snippets matters.
pub fn snippet(html: &str, q: &str) -> String {
    const CHARS: usize = 200;
    let text = html::parse(html).dump;
    let mut words = vec![];
    parse_terms(&remove_accents(q), true, false, |t, _| {
        words.push(t.to_string());
        true
    });
    // Follow the scan four words behind. Only the returned window needs
    // allocation, even if the first matching word is near the end.
    let mut start = text.split_whitespace();
    let mut found = false;
    if !words.is_empty() {
        for (i, word) in text.split_whitespace().enumerate() {
            let word = remove_accents(word.trim_matches(|c: char| !c.is_alphanumeric()));
            if words
                .iter()
                .any(|q| q == &word || (q.len() >= 3 && word.starts_with(q)))
            {
                found = true;
                break;
            }
            if i >= 4 {
                start.next();
            }
        }
    }
    if !found {
        start = text.split_whitespace();
    }
    let mut chars = start
        .flat_map(|word| std::iter::once(' ').chain(word.chars()))
        .skip(1);
    let mut s: String = chars.by_ref().take(CHARS).collect();
    if chars.next().is_some() {
        if let Some(cut) = s.rfind(' ') {
            s.truncate(cut);
        }
        s.push('…');
    }
    s
}

#[cfg(test)]
mod snippet_tests {
    use super::snippet;

    #[test]
    fn keeps_four_preceding_words_and_truncates_on_a_character_boundary() {
        assert_eq!(
            snippet("<p>one two three four five six café seven</p>", "cafe"),
            "three four five six café seven"
        );
        let html = format!("<p>{}</p>", "界".repeat(250));
        assert_eq!(snippet(&html, ""), format!("{}…", "界".repeat(200)));
        assert_eq!(snippet("<p>first     second</p>", "absent"), "first second");
    }
}
