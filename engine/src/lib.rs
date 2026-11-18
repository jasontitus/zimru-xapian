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
use std::sync::Arc;
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
    /// Maximum number of retained candidates when ranking a deep page.
    pub max_ranked_hits: usize,
    /// Maximum bytes of a selected path, snippet, title sort key or collapse key.
    pub max_result_field_bytes: usize,
    /// Limit for live title/collapse key bytes and, independently, emitted string bytes.
    pub max_result_bytes: usize,
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
            max_ranked_hits: 65_536,
            max_result_field_bytes: 1 << 20,
            max_result_bytes: 32 << 20,
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

/// A ranked page. Full-text totals count matching documents. Title totals
/// can include alias matches that collapse into one suggestion.
#[derive(Debug, PartialEq)]
pub struct Page<T> {
    pub hits: Vec<T>,
    pub total: u64,
}

impl<T> Page<T> {
    fn empty() -> Self {
        Self {
            hits: Vec::new(),
            total: 0,
        }
    }
}

/// Stored snippets are archive-provided text. A declared but empty snippet
/// slot yields Some("") and a missing legacy slot yields None.
#[derive(Debug, PartialEq)]
pub struct PageHit {
    pub hit: Hit,
    pub snippet: Option<String>,
}

pub(crate) fn check_cancel(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "Search cancelled",
        )))
    } else {
        Ok(())
    }
}

fn page_window(offset: usize, limit: usize) -> Result<usize> {
    if limit == 0 {
        return Ok(0);
    }
    offset
        .checked_add(limit)
        .ok_or_else(|| Error::Invalid("search page offset overflow".into()))
}

fn charge_bytes(used: &mut usize, count: usize, max: usize) -> Result<()> {
    *used = used
        .checked_add(count)
        .filter(|&size| size <= max)
        .ok_or_else(|| Error::Invalid(format!("search results exceed the {max}-byte budget")))?;
    Ok(())
}

/// Best results sort first. Title suggestions additionally break weight ties
/// by the original title bytes. In both modes docid is the final tie-break.
#[derive(Clone, Debug)]
struct Rank<T> {
    weight: f64,
    title: T,
    did: u32,
}

impl<T: Ord> Ord for Rank<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .weight
            .total_cmp(&self.weight)
            .then_with(|| self.title.cmp(&other.title))
            .then_with(|| self.did.cmp(&other.did))
    }
}

impl<T: Ord> PartialOrd for Rank<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<T: Ord> PartialEq for Rank<T> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl<T: Ord> Eq for Rank<T> {}

fn rank_limit(limits: ReaderLimits) -> Error {
    Error::Invalid(format!(
        "ranked search window exceeds {} candidates",
        limits.max_ranked_hits
    ))
}

/// The names used by libzim's `valuesmap` metadata. Other names are
/// permitted. Only the three slots consumed by this reader need storage.
#[derive(Default)]
struct ValueSlots {
    title: Option<u32>,
    target: Option<u32>,
    word_count: Option<u32>,
    snippet: Option<u32>,
    legacy: bool,
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
            slots.snippet = Some(1);
            slots.legacy = true;
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
                "snippet" => slots.snippet.get_or_insert(slot),
                _ => continue,
            };
        }
        Ok(slots)
    }
}

fn slot_value(
    db: &Db,
    did: u32,
    slot: Option<u32>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Vec<u8>>> {
    match slot {
        Some(slot) => db.value_limited(did, slot, db.limits().max_result_field_bytes, cancelled),
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

    fn document_path(&self, did: u32, remaining: usize) -> Result<String> {
        let bytes = self
            .db
            .data_limited(did, remaining.min(self.db.limits().max_result_field_bytes))?
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Format(format!("missing document path for docid {did}")))?;
        String::from_utf8(bytes.into_boxed_slice().into_vec())
            .map_err(|_| Error::Format(format!("invalid UTF-8 document path for docid {did}")))
    }

    fn exists(&self, term: &str) -> Result<bool> {
        Ok(self.db.freqs(term.as_bytes())?.0 > 0)
    }

    /// Full-text search: the best `limit` documents for `q`, as libzim ranks them.
    pub fn search(&self, q: &str, limit: usize, lang: &str) -> Result<Vec<Hit>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        Ok(self
            .search_page(q, 0, limit, lang, false, &|| false)?
            .hits
            .into_iter()
            .map(|entry| entry.hit)
            .collect())
    }

    /// Rank the requested page without materializing paths for earlier hits.
    /// A zero-length page returns the total only. Cancellation is cooperative.
    pub fn search_page(
        &self,
        q: &str,
        offset: usize,
        limit: usize,
        lang: &str,
        include_stored_snippets: bool,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Page<PageHit>> {
        check_cancel(cancelled)?;
        let window = page_window(offset, limit)?;
        if q.is_empty() {
            return Ok(Page::empty());
        }
        let normalized = self.normalize_query(q)?;
        let db = &self.db;
        let limits = db.limits();
        let stem = self.stemmer(lang)?;
        let metadata = db.metadata("stopwords")?.unwrap_or_default();
        let words = stopwords::from_metadata(&metadata, limits.max_stopwords)?;
        let Some(query) = query::fulltext(
            &normalized,
            stem.as_ref(),
            &|term| {
                check_cancel(cancelled)?;
                self.exists(term)
            },
            &words,
        )?
        else {
            return Ok(Page::empty());
        };
        let mut top = BTreeSet::new();
        let mut best: Option<Match> = None;
        let mut matches = 0u64;
        let leaves = matcher::for_each_match(db, &query, matcher::FULLTEXT, cancelled, |m| {
            matches += 1;
            if window == 0 {
                return Ok(());
            }
            if best.is_none_or(|b| m.weight > b.weight) {
                best = Some(m);
            }
            // Full-text rank doesn't use title. The tuple needs no per-match allocation.
            let rank = Rank {
                weight: m.weight,
                title: (),
                did: m.did,
            };
            if top.len() < window || top.last().is_some_and(|worst| rank < *worst) {
                if top.len() == window {
                    top.pop_last();
                }
                if top.len() >= limits.max_ranked_hits {
                    return Err(rank_limit(limits));
                }
                top.insert(rank);
            }
            Ok(())
        })?;
        check_cancel(cancelled)?;
        let mut page = Page {
            hits: Vec::new(),
            total: matches,
        };
        let Some(best) = best else {
            return Ok(page);
        };
        if offset >= top.len() {
            return Ok(page);
        }
        let slots = ValueSlots::read(db)?;
        let mut output_bytes = 0;
        for Rank { weight, did, .. } in top.into_iter().skip(offset) {
            check_cancel(cancelled)?;
            let path = self.document_path(did, limits.max_result_bytes - output_bytes)?;
            charge_bytes(&mut output_bytes, path.len(), limits.max_result_bytes)?;
            let word_count = slot_value(db, did, slots.word_count, cancelled)?
                .and_then(|v| String::from_utf8(v).ok()?.parse().ok());
            let snippet = if include_stored_snippets && let Some(slot) = slots.snippet {
                let allowance = limits
                    .max_result_field_bytes
                    .min(limits.max_result_bytes - output_bytes);
                let raw = db
                    .value_limited(did, slot, allowance, cancelled)?
                    .unwrap_or_default();
                if slots.legacy && raw.is_empty() {
                    None
                } else {
                    charge_bytes(&mut output_bytes, raw.len(), limits.max_result_bytes)?;
                    Some(
                        String::from_utf8(raw)
                            .map_err(|_| Error::Format("invalid UTF-8 stored snippet".into()))?,
                    )
                }
            } else {
                None
            };
            page.hits.push(PageHit {
                hit: Hit {
                    path,
                    score: weight,
                    percent: matcher::percent(weight, &best, leaves),
                    word_count,
                },
                snippet,
            });
        }
        check_cancel(cancelled)?;
        Ok(page)
    }

    /// Title suggestions as original document paths, after alias collapse.
    pub fn suggest(&self, q: &str, limit: usize, lang: &str) -> Result<Vec<String>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        Ok(self.suggest_page(q, 0, limit, lang, &|| false)?.hits)
    }

    /// Seek by ranked offset after collapse. Per-query retained title sort keys
    /// and alias keys share a byte budget independent of the output page.
    pub fn suggest_page(
        &self,
        q: &str,
        offset: usize,
        limit: usize,
        lang: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Page<String>> {
        check_cancel(cancelled)?;
        let window = page_window(offset, limit)?;
        if q.is_empty() {
            return Ok(Page::empty());
        }
        let normalized = self.normalize_query(q)?;
        let db = &self.db;
        let limits = db.limits();
        let stem = self.stemmer(lang)?;
        let Some(query) = query::suggestion(&normalized, stem.as_ref(), &|term| {
            check_cancel(cancelled)?;
            self.exists(term)
        })?
        else {
            return Ok(Page::empty());
        };
        let slots = ValueSlots::read(db)?;
        let mut top = BTreeMap::<Rank<Arc<[u8]>>, Arc<[u8]>>::new();
        let mut retained_targets = HashMap::<Arc<[u8]>, Rank<Arc<[u8]>>>::new();
        let mut key_bytes = 0;
        let mut matches = 0u64;
        matcher::for_each_match(db, &query, matcher::SUGGEST, cancelled, |m| {
            matches += 1;
            if window == 0 {
                return Ok(());
            }
            let worst = top.last_key_value().map(|(rank, _)| rank);
            if top.len() == window && worst.is_some_and(|w| m.weight < w.weight) {
                return Ok(());
            }
            let rank = Rank {
                weight: m.weight,
                title: slot_value(db, m.did, slots.title, cancelled)?
                    .unwrap_or_default()
                    .into(),
                did: m.did,
            };
            if top.len() == window && worst.is_some_and(|w| rank >= *w) {
                return Ok(());
            }
            let target: Arc<[u8]> = slot_value(db, m.did, slots.target, cancelled)?
                .unwrap_or_default()
                .into();
            if let Some(old) = retained_targets.get(&target) {
                if rank >= *old {
                    return Ok(());
                }
                top.remove(old);
                key_bytes -= old.title.len() + target.len();
                retained_targets.remove(&target);
            } else if top.len() == window
                && let Some((old, key)) = top.pop_last()
            {
                key_bytes -= old.title.len() + key.len();
                retained_targets.remove(&key);
            }
            if top.len() >= limits.max_ranked_hits {
                return Err(rank_limit(limits));
            }
            charge_bytes(&mut key_bytes, rank.title.len(), limits.max_result_bytes)?;
            charge_bytes(&mut key_bytes, target.len(), limits.max_result_bytes)?;
            if !target.is_empty() {
                retained_targets.insert(target.clone(), rank.clone());
            }
            top.insert(rank, target);
            Ok(())
        })?;
        drop(retained_targets);
        let mut page = Page {
            hits: Vec::new(),
            total: matches,
        };
        let mut output_bytes = 0;
        for rank in top.into_keys().skip(offset) {
            check_cancel(cancelled)?;
            let path = self.document_path(rank.did, limits.max_result_bytes - output_bytes)?;
            charge_bytes(&mut output_bytes, path.len(), limits.max_result_bytes)?;
            page.hits.push(path);
        }
        check_cancel(cancelled)?;
        Ok(page)
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
    fn stored_snippets_respect_legacy_missing_and_declared_empty_slots() {
        let index = |map: Option<&[u8]>, values: &[(u32, Vec<u8>)]| {
            let mut builder = Builder::default();
            if let Some(map) = map {
                builder.set_metadata("valuesmap", map);
            }
            let terms = Terms::from([(b"prairie".to_vec(), (1, vec![]))]);
            builder.add(b"C/field", &terms, values).unwrap();
            let mut output = Cursor::new(Vec::new());
            builder.write(&mut output, [0; 16]).unwrap();
            Index::open(Box::new(output.into_inner())).unwrap()
        };
        for (index, expected) in [
            (index(None, &[]), None),
            (index(None, &[(1, b"old".to_vec())]), Some("old")),
            (index(Some(b"snippet:7"), &[]), Some("")),
            (
                index(Some(b"snippet:7"), &[(7, b"named".to_vec())]),
                Some("named"),
            ),
            (index(Some(b"other:1"), &[(1, b"ignored".to_vec())]), None),
        ] {
            let page = index
                .search_page("prairie", 0, 1, "", true, &|| false)
                .unwrap();
            assert_eq!(page.hits[0].snippet.as_deref(), expected);
            let page = index
                .search_page("prairie", 0, 1, "", false, &|| false)
                .unwrap();
            assert_eq!(page.hits[0].snippet, None);
        }
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

/// Query terms shared by every generated excerpt on a search page.
pub struct SnippetQuery {
    short: HashSet<String>,
    prefixes: Vec<String>,
}

impl SnippetQuery {
    pub fn new(q: &str) -> Self {
        let mut short = HashSet::new();
        let mut prefixes = Vec::new();
        parse_terms(&remove_accents(q), true, false, |term, _| {
            if term.len() < 3 {
                short.insert(term.to_owned());
            } else {
                prefixes.push(term.to_owned());
            }
            true
        });
        // If one term is a prefix of another it covers all of the latter's
        // matches. Keeping the shorter term leaves disjoint prefixes in lexical order.
        prefixes.sort_unstable();
        prefixes.dedup_by(|next, prev| next.starts_with(prev.as_str()));
        Self { short, prefixes }
    }

    fn matches(&self, word: &str) -> bool {
        if word.len() < 3 {
            return self.short.contains(word);
        }
        let end = self.prefixes.partition_point(|p| p.as_str() <= word);
        end != 0 && word.starts_with(&self.prefixes[end - 1])
    }

    /// Extract up to 200 Unicode characters around the first matching word.
    /// The caller sets the maximum article prefix to parse. Cancellation is
    /// polled around parsing and while searching the resulting plain text.
    pub fn excerpt(&self, html: &str, cancelled: &dyn Fn() -> bool) -> Result<String> {
        const CHARS: usize = 200;
        check_cancel(cancelled)?;
        let text = html::parse(html).dump;
        check_cancel(cancelled)?;
        let mut start = text.split_whitespace();
        let mut found = false;
        if !self.short.is_empty() || !self.prefixes.is_empty() {
            for (i, word) in text.split_whitespace().enumerate() {
                if i.is_multiple_of(64) {
                    check_cancel(cancelled)?;
                }
                let word = remove_accents(word.trim_matches(|c: char| !c.is_alphanumeric()));
                if self.matches(&word) {
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
        check_cancel(cancelled)?;
        Ok(s)
    }
}

/// Plain-text extract for callers producing a single snippet.
pub fn snippet(html: &str, q: &str) -> String {
    SnippetQuery::new(q)
        .excerpt(html, &|| false)
        .expect("uncancelled snippet")
}

#[cfg(test)]
mod snippet_tests {
    use super::{SnippetQuery, snippet};

    #[test]
    fn reusable_queries_preserve_short_and_overlapping_prefixes() {
        let q = SnippetQuery::new("ab alps alpine alp café 東京");
        assert!(q.matches("ab"));
        assert!(!q.matches("about"));
        for word in ["alpine", "alps", "alpaca", "cafeteria", "東京駅"] {
            assert!(q.matches(word));
        }
        assert!(!q.matches("almanac"));
        assert_eq!(
            q.excerpt("0 1 2 3 4 5 Alpinist", &|| false).unwrap(),
            "2 3 4 5 Alpinist"
        );
        assert_eq!(
            q.excerpt("a b c d e f Café", &|| false).unwrap(),
            "c d e f Café"
        );
    }

    #[test]
    fn broad_excerpt_can_cancel_mid_article_and_retry() {
        let terms = (0..4096)
            .map(|i| format!("keyword{i:04}"))
            .collect::<Vec<_>>()
            .join(" ");
        let q = SnippetQuery::new(&terms);
        assert_eq!(q.prefixes.len(), 4096);
        let text = "ordinary ".repeat(1024) + "keyword4095suffix";
        let polls = std::cell::Cell::new(0);
        let error = q
            .excerpt(&text, &|| {
                polls.set(polls.get() + 1);
                polls.get() == 5
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "Search cancelled");
        assert_eq!(
            q.excerpt(&text, &|| false).unwrap(),
            "ordinary ordinary ordinary ordinary keyword4095suffix"
        );
    }

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
