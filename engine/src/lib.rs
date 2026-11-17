//! libzim's Xapian search indexes in pure Rust (a reimplementation, not
//! bindings): read, search and write the single-file Glass databases Kiwix
//! archives embed (`X/fulltext/xapian`, `X/title/xapian`), with libzim's
//! semantics on Xapian 1.4 — its query parsing, term generation and ranking.
//!
//! This crate knows libzim's index conventions (documents' data is their
//! full path, `C/<path>`; the value slots; the title anchor term) but not
//! the ZIM container: an [`Index`] reads its bytes through a [`Source`], and
//! results are document paths. `zimfile` maps them to entries.

mod glass;
mod html;
mod index;
mod matcher;
mod query;
mod text;

#[cfg(test)]
mod testdata;

use glass::Db;
use matcher::Match;
use std::io::{Seek, Write};
use text::{Stemmer, parse_terms, remove_accents};

pub use glass::Source;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The index is corrupt, or not a Glass database.
    Format(String),
    /// The writer was given something it can't write.
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

/// One embedded index, opened.
pub struct Index {
    db: Db,
}

impl Index {
    pub fn open(src: Box<dyn Source>) -> Result<Index> {
        Ok(Index { db: Db::open(src)? })
    }

    /// The stemmer: the index's `language` metadata, else `lang` (old indexes
    /// have none; libzim then uses the archive's `M/Language`).
    fn stemmer(&self, lang: &str) -> Option<Stemmer> {
        let meta = self
            .db
            .metadata("language")
            .ok()
            .flatten()
            .unwrap_or_default();
        let meta = String::from_utf8_lossy(&meta);
        let lang = if meta.is_empty() { lang } else { &meta };
        text::stemmer(lang.split(',').next().unwrap_or(""))
    }

    fn exists(&self) -> impl Fn(&str) -> bool + '_ {
        |t| self.db.freqs(t.as_bytes()).is_ok_and(|f| f.0 > 0)
    }

    /// Full-text search: the best `limit` documents for `q`, as libzim ranks them.
    pub fn search(&self, q: &str, limit: usize, lang: &str) -> Result<Vec<Hit>> {
        let db = &self.db;
        let stem = self.stemmer(lang);
        let Some(query) = query::fulltext(&remove_accents(q), stem.as_ref(), &self.exists()) else {
            return Ok(vec![]);
        };
        let (mut ms, total) = matcher::matches(db, &query, matcher::FULLTEXT)?;
        let Some(best) = ms.iter().fold(None::<Match>, |b, m| match b {
            Some(b) if b.weight >= m.weight => Some(b),
            _ => Some(*m),
        }) else {
            return Ok(vec![]);
        };
        // By weight, then docid (the input order: the sort is stable).
        ms.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        let mut hits = vec![];
        for m in ms {
            if hits.len() == limit {
                break;
            }
            let Some(path) = db.data(m.did)? else {
                continue;
            };
            let word_count = db
                .value(m.did, 1)?
                .and_then(|v| String::from_utf8(v).ok()?.parse().ok());
            hits.push(Hit {
                path: String::from_utf8_lossy(&path).into_owned(),
                score: m.weight,
                percent: matcher::percent(m.weight, &best, total),
                word_count,
            });
        }
        Ok(hits)
    }

    /// Title suggestions, as libzim makes them: document paths.
    pub fn suggest(&self, q: &str, limit: usize, lang: &str) -> Result<Vec<String>> {
        let db = &self.db;
        let stem = self.stemmer(lang);
        let Some(query) = query::suggestion(&remove_accents(q), stem.as_ref(), &self.exists())
        else {
            return Ok(vec![]);
        };
        let (mut ms, _) = matcher::matches(db, &query, matcher::SUGGEST)?;
        ms.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        // Ties on weight: by title (value 0), then docid. Collapse on the
        // target path (value 1). Values are read only for groups reached.
        let mut out = vec![];
        let mut seen = std::collections::HashSet::new();
        let mut i = 0;
        while i < ms.len() && out.len() < limit {
            let j = i + ms[i..]
                .iter()
                .take_while(|m| m.weight == ms[i].weight)
                .count();
            let mut group = vec![];
            for m in &ms[i..j] {
                group.push((db.value(m.did, 0)?.unwrap_or_default(), m.did));
            }
            group.sort();
            for (_, did) in group {
                let key = db.value(did, 1)?.unwrap_or_default();
                if !key.is_empty() && !seen.insert(key) {
                    continue;
                }
                if out.len() < limit
                    && let Some(path) = db.data(did)?
                {
                    out.push(String::from_utf8_lossy(&path).into_owned());
                }
            }
            i = j;
        }
        Ok(out)
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
    builder: glass::write::Builder,
}

impl IndexWriter {
    fn new(fulltext: bool, language: &str) -> IndexWriter {
        IndexWriter {
            fulltext,
            language: language.into(),
            stemmer: text::stemmer(language),
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

    /// Add an HTML item at `C/<path>` (skipped if it has no text or asks not
    /// to be indexed, as in libzim).
    pub fn add_html(&mut self, path: &str, title: &str, html: &str) {
        index::add_fulltext(&mut self.builder, path, title, html, self.stemmer.as_ref());
    }

    /// Add a front article, or a redirect to `target`, at `C/<path>`.
    pub fn add_title(&mut self, path: &str, title: &str, target: Option<&str>) {
        index::add_title(
            &mut self.builder,
            path,
            title,
            target,
            self.stemmer.as_ref(),
        );
    }

    pub fn is_empty(&self) -> bool {
        self.builder.doccount() == 0
    }

    /// Write the database (with libzim's metadata) to `out`, from offset 0.
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
    let starts: Vec<usize> = text
        .char_indices()
        .filter(|&(i, c)| {
            !c.is_whitespace() && (i == 0 || text[..i].ends_with(char::is_whitespace))
        })
        .map(|(i, _)| i)
        .collect();
    let hit = starts.iter().position(|&s| {
        let w: String = text[s..]
            .chars()
            .take_while(|c| !c.is_whitespace())
            .collect();
        let w = remove_accents(w.trim_matches(|c: char| !c.is_alphanumeric()));
        words
            .iter()
            .any(|q| q.len() >= 3 && w.starts_with(q.as_str()) || *q == w)
    });
    // A few words of context before the first hit.
    let from = hit.map_or(0, |h| starts[h.saturating_sub(4)]);
    let mut s: String = text[from..]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if s.chars().count() > CHARS {
        s = s.chars().take(CHARS).collect();
        if let Some(cut) = s.rfind(' ') {
            s.truncate(cut);
        }
        s.push('…');
    }
    s
}
