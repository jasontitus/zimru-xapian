// Modified for the standalone zimru-xapian tools on 2026-11-17. See PROVENANCE.md.
//! A single-file Glass database: the version block and the tables libzim's
//! indexes use (postlist, docdata, position). See `plans/glass-indexes.md`.

use super::btree::{self, Blocks, Cursor, Root, Source};
use super::{bitstream, pack};
use crate::{Error, ReaderLimits, Result};
use std::cell::Cell;

pub const MAGIC: &[u8] = b"\x0f\x0dXapian Glass\x04\x6e";
pub const POSTLIST: usize = 0;
pub const DOCDATA: usize = 1;
pub const POSITION: usize = 3;

fn corrupt(what: &str) -> Error {
    Error::Format(format!("Xapian index: {what}"))
}

fn bad() -> Error {
    corrupt("bad data")
}

/// The database statistics (`GlassVersion`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub doccount: u32,
    pub last_docid: u32,
    pub doclen_lbound: u32,
    pub wdf_ubound: u32,
    pub doclen_ubound: u32,
    pub total_doclen: u64,
}

pub struct Db {
    blocks: Blocks,
    #[cfg_attr(not(test), allow(dead_code))] // checked in tests
    pub uuid: [u8; 16],
    #[cfg_attr(not(test), allow(dead_code))]
    pub revision: u64,
    /// postlist, docdata, termlist, position, spelling, synonym.
    pub roots: [Root; 6],
    pub stats: Stats,
}

/// The key of a term's first postlist chunk, or (`did`) of a later one.
/// The empty term is the document-length list.
pub fn postlist_key(term: &[u8], did: Option<u32>) -> Vec<u8> {
    let mut k = vec![];
    if term.is_empty() {
        k.extend_from_slice(b"\x00\xe0");
    } else {
        pack::put_sort_string(&mut k, term, did.is_none());
    }
    if let Some(d) = did {
        pack::put_pus(&mut k, d.into());
    }
    k
}

pub fn position_key(term: &[u8], did: u32) -> Vec<u8> {
    let mut k = vec![];
    pack::put_sort_string(&mut k, term, false);
    pack::put_pus(&mut k, did.into());
    k
}

/// The key of a value chunk of `slot`: the prefix, then the chunk's first docid.
pub fn value_key(slot: u32, did: Option<u32>) -> Vec<u8> {
    let mut k = b"\x00\xd8".to_vec();
    pack::put_uint(&mut k, slot.into());
    if let Some(d) = did {
        pack::put_pus(&mut k, d.into());
    }
    k
}

pub fn value_stats_key(slot: u32) -> Vec<u8> {
    let mut k = b"\x00\xd0".to_vec();
    pack::put_uint_last(&mut k, slot.into());
    k
}

pub fn metadata_key(name: &[u8]) -> Vec<u8> {
    [b"\x00\xc0", name].concat()
}

pub fn docdata_key(did: u32) -> Vec<u8> {
    let mut k = vec![];
    pack::put_pus(&mut k, did.into());
    k
}

/// If `key` is a term's first postlist chunk, the term.
fn term_of(key: &[u8]) -> Option<Vec<u8>> {
    if key.is_empty() || key[0] == 0 && key.get(1) != Some(&0xff) {
        return None; // the null item, metadata, values, doclens
    }
    let mut p = key;
    let t = pack::sort_string(&mut p);
    // A later chunk has an unescaped `\0` and a docid after the term.
    p.is_empty().then_some(t)
}

fn u32_of(v: u64) -> Result<u32> {
    u32::try_from(v).map_err(|_| bad())
}

fn add_delta(base: u32, delta: u64) -> Result<u32> {
    base.checked_add(u32_of(delta)?).ok_or_else(bad)
}

impl Db {
    #[cfg(test)]
    pub fn open(src: Box<dyn Source>) -> Result<Db> {
        Self::open_with_limits(src, ReaderLimits::default())
    }

    pub fn open_with_limits(src: Box<dyn Source>, limits: ReaderLimits) -> Result<Db> {
        let mut head = vec![0; 256];
        src.read_at(&mut head, 0)?;
        if !head.starts_with(MAGIC) {
            return Err(corrupt("not a Glass database (or not format 2016-03-14)"));
        }
        let uuid = head[16..32].try_into().unwrap();
        let mut p = &head[32..];
        let revision = pack::uint(&mut p).ok_or_else(bad)?;
        let mut roots: [Root; 6] = Default::default();
        for r in &mut roots {
            let root = pack::uint(&mut p).ok_or_else(bad)?;
            let val = pack::uint(&mut p).ok_or_else(bad)?;
            *r = Root {
                root: u32_of(root)?,
                level: u8::try_from(val >> 2).map_err(|_| bad())?,
                sequential: val & 2 != 0,
                fake: val & 1 != 0,
                entries: pack::uint(&mut p).ok_or_else(bad)?,
                blocksize: u32_of(pack::uint(&mut p).ok_or_else(bad)?)?
                    .checked_mul(2048)
                    .ok_or_else(bad)?,
                compress_min: u32_of(pack::uint(&mut p).ok_or_else(bad)?)?,
                freelist: pack::string(&mut p).ok_or_else(bad)?.to_vec(),
            };
            // Xapian supports ten B-tree levels (0 through 9).
            if r.level >= 10 || (r.fake && r.level != 0) {
                return Err(corrupt("bad table root level"));
            }
            if !(2048..=65536).contains(&r.blocksize) || !r.blocksize.is_power_of_two() {
                return Err(corrupt("bad table block size"));
            }
        }
        let mut s = [0u64; 8];
        for v in &mut s {
            *v = pack::uint(&mut p).ok_or_else(bad)?;
        }
        let stats = Stats {
            doccount: u32_of(s[0])?,
            last_docid: add_delta(u32_of(s[0])?, s[1])?,
            doclen_lbound: u32_of(s[2])?,
            wdf_ubound: u32_of(s[3])?,
            doclen_ubound: add_delta(u32_of(s[3])?, s[4])?,
            total_doclen: s[6],
        };
        let size = roots[POSTLIST].blocksize as usize;
        if roots
            .iter()
            .any(|r| !r.fake && r.blocksize as usize != size)
        {
            return Err(corrupt("inconsistent single-file block sizes"));
        }
        Ok(Db {
            blocks: Blocks::new(src, size, limits),
            uuid,
            revision,
            roots,
            stats,
        })
    }

    pub fn limits(&self) -> ReaderLimits {
        self.blocks.limits
    }

    #[cfg(test)]
    pub fn blocksize(&self) -> usize {
        self.blocks.size
    }

    fn get(&self, table: usize, key: &[u8]) -> Result<Option<Vec<u8>>> {
        btree::get(&self.blocks, &self.roots[table], key)
    }

    pub fn has_positions(&self) -> bool {
        !self.roots[POSITION].fake
    }

    /// User metadata (`get_metadata`).
    pub fn metadata(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.get(POSTLIST, &metadata_key(name.as_bytes()))
    }

    /// The document's data: in libzim's indexes, its full path (`C/...`).
    pub fn data(&self, did: u32) -> Result<Option<Vec<u8>>> {
        self.get(DOCDATA, &docdata_key(did))
    }

    /// (term frequency, collection frequency); (0, 0) if absent.
    pub fn freqs(&self, term: &[u8]) -> Result<(u32, u64)> {
        match self.get(POSTLIST, &postlist_key(term, None))? {
            None => Ok((0, 0)),
            Some(tag) => {
                let mut p = &tag[..];
                let tf = pack::uint(&mut p).ok_or_else(bad)?;
                let cf = pack::uint(&mut p).ok_or_else(bad)?;
                Ok((u32_of(tf)?, cf))
            }
        }
    }

    /// The term's postings, `None` if absent.
    #[cfg(test)]
    pub fn postlist(&self, term: &[u8]) -> Result<Option<PostList<'_>>> {
        PostList::open(self, term, None)
    }

    pub fn budgeted_postlist<'a>(
        &'a self,
        term: &[u8],
        budget: &'a PostingBudget,
    ) -> Result<Option<PostList<'a>>> {
        PostList::open(self, term, Some(budget))
    }

    /// Every document's length, in docid order.
    #[cfg(test)]
    pub fn doclens(&self) -> Result<Option<PostList<'_>>> {
        self.postlist(b"")
    }

    /// Value `slot` of `did` (empty values aren't stored: `None`).
    pub fn value(&self, did: u32, slot: u32) -> Result<Option<Vec<u8>>> {
        let key = value_key(slot, Some(did));
        let Some(c) = Cursor::seek_le(&self.blocks, &self.roots[POSTLIST], &key)? else {
            return Ok(None);
        };
        let Some(k) = c.key()? else { return Ok(None) };
        let Some(mut p) = k.strip_prefix(&value_key(slot, None)[..]) else {
            return Ok(None); // no chunk for this slot at or before did
        };
        let mut d = u32_of(pack::pus(&mut p).ok_or_else(bad)?)?;
        let tag = c.tag()?;
        let mut p = &tag[..];
        let mut v = pack::string(&mut p).ok_or_else(bad)?;
        while d < did && !p.is_empty() {
            d = add_delta(d + 1, pack::uint(&mut p).ok_or_else(bad)?)?;
            v = pack::string(&mut p).ok_or_else(bad)?;
        }
        Ok((d == did).then(|| v.to_vec()))
    }

    /// (frequency, lower bound, upper bound) of a value slot.
    #[cfg(test)]
    pub fn value_stats(&self, slot: u32) -> Result<Option<ValueStats>> {
        let Some(tag) = self.get(POSTLIST, &value_stats_key(slot))? else {
            return Ok(None);
        };
        let mut p = &tag[..];
        let freq = u32_of(pack::uint(&mut p).ok_or_else(bad)?)?;
        let lo = pack::string(&mut p).ok_or_else(bad)?.to_vec();
        let hi = if p.is_empty() { lo.clone() } else { p.to_vec() };
        Ok(Some((freq, lo, hi)))
    }

    /// The positions of `term` in `did` (empty if none stored).
    pub fn positions(&self, did: u32, term: &[u8]) -> Result<Vec<u32>> {
        match self.get(POSITION, &position_key(term, did))? {
            None => Ok(vec![]),
            Some(t) => bitstream::decode(&t, self.blocks.limits.max_positions),
        }
    }

    /// Terms starting with `prefix`, in byte order, with their term
    /// frequencies.
    pub fn visit_terms(
        &self,
        prefix: &[u8],
        mut visit: impl FnMut(Vec<u8>, u32) -> Result<()>,
    ) -> Result<()> {
        let mut start = vec![];
        pack::put_sort_string(&mut start, prefix, true);
        let Some(mut c) = Cursor::seek_ge(&self.blocks, &self.roots[POSTLIST], &start)? else {
            return Ok(());
        };
        while let Some(k) = c.key()?
            && k.starts_with(&start)
        {
            if let Some(t) = term_of(k) {
                let tag = c.tag()?;
                let tf = pack::uint(&mut &tag[..]).ok_or_else(bad)?;
                visit(t, u32_of(tf)?)?;
            }
            if !c.next()? {
                break;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn terms(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, u32)>> {
        let mut out = Vec::new();
        self.visit_terms(prefix, |term, count| {
            out.push((term, count));
            Ok(())
        })?;
        Ok(out)
    }
}

/// A value slot's (frequency, lower bound, upper bound).
#[cfg(test)]
pub type ValueStats = (u32, Vec<u8>, Vec<u8>);

/// Shared by this query's forward-only cursors. Capacity is returned when
/// a cursor changes chunks, reaches its end, or is dropped early.
pub struct PostingBudget {
    remaining: Cell<usize>,
}

impl PostingBudget {
    pub fn new(bytes: usize) -> Self {
        Self {
            remaining: Cell::new(bytes),
        }
    }
}

/// A posting list (term or doclen), read one chunk at a time.
pub struct PostList<'a> {
    db: &'a Db,
    budget: Option<&'a PostingBudget>,
    term: Vec<u8>,
    /// Term frequency and collection frequency (0 for doclens).
    pub tf: u32,
    pub cf: u64,
    chunk: Vec<u8>,
    pos: usize,
    did: u32,
    wdf: u32,
    last_in_chunk: u32,
    is_last_chunk: bool,
    started: bool,
    ended: bool,
}

impl<'a> PostList<'a> {
    fn open(
        db: &'a Db,
        term: &[u8],
        budget: Option<&'a PostingBudget>,
    ) -> Result<Option<PostList<'a>>> {
        let key = postlist_key(term, None);
        let Some(cursor) = Cursor::seek_le(&db.blocks, &db.roots[POSTLIST], &key)? else {
            return Ok(None);
        };
        if cursor.key()? != Some(&key) {
            return Ok(None);
        }
        let mut pl = PostList {
            db,
            budget,
            term: term.to_vec(),
            tf: 0,
            cf: 0,
            chunk: vec![],
            pos: 0,
            did: 0,
            wdf: 0,
            last_in_chunk: 0,
            is_last_chunk: false,
            started: false,
            ended: false,
        };
        let tag = pl.read_tag(&cursor)?;
        pl.load_first(tag)?;
        Ok(Some(pl))
    }

    fn release_chunk(&mut self) {
        if let Some(budget) = self.budget {
            budget
                .remaining
                .set(budget.remaining.get() + self.chunk.capacity());
        }
        self.chunk = Vec::new();
    }

    fn read_tag(&mut self, cursor: &Cursor) -> Result<Vec<u8>> {
        self.release_chunk();
        let Some(budget) = self.budget else {
            return cursor.tag();
        };
        let remaining = budget.remaining.get();
        cursor.tag_limited(remaining).map_err(|e| match e {
            Error::Invalid(message) if remaining < self.db.limits().max_tag_bytes => {
                Error::Invalid(format!(
                    "{message} (remaining posting budget: {remaining} bytes)"
                ))
            }
            other => other,
        })
    }

    fn load_first(&mut self, tag: Vec<u8>) -> Result<()> {
        let mut p = &tag[..];
        self.tf = u32_of(pack::uint(&mut p).ok_or_else(bad)?)?;
        self.cf = pack::uint(&mut p).ok_or_else(bad)?;
        let first = add_delta(1, pack::uint(&mut p).ok_or_else(bad)?)?;
        let used = tag.len() - p.len();
        self.load(tag, used, first)
    }

    /// Start reading chunk `tag` at `at` (`is_last`, `last − first`, then
    /// the postings), whose first docid is `first`.
    fn load(&mut self, tag: Vec<u8>, at: usize, first: u32) -> Result<()> {
        let mut p = &tag[at..];
        self.is_last_chunk = pack::bool(&mut p).ok_or_else(bad)?;
        self.last_in_chunk = add_delta(first, pack::uint(&mut p).ok_or_else(bad)?)?;
        if first == 0 || self.last_in_chunk > self.db.stats.last_docid {
            return Err(corrupt("posting outside database document range"));
        }
        self.did = first;
        self.wdf = u32_of(pack::uint(&mut p).ok_or_else(bad)?)?;
        self.pos = tag.len() - p.len();
        if let Some(budget) = self.budget {
            let rest = budget
                .remaining
                .get()
                .checked_sub(tag.capacity())
                .ok_or_else(|| Error::Invalid("posting cursor memory limit exceeded".into()))?;
            budget.remaining.set(rest);
        }
        self.chunk = tag;
        Ok(())
    }

    /// Load the chunk at cursor `c` (a later chunk: its key ends in its first docid).
    fn load_at(&mut self, c: &Cursor) -> Result<bool> {
        let Some(k) = c.key()? else { return Ok(false) };
        let prefix = postlist_key(&self.term, Some(0));
        let prefix = &prefix[..prefix.len() - 2];
        if k == postlist_key(&self.term, None) {
            let tag = self.read_tag(c)?;
            self.load_first(tag)?;
            return Ok(true);
        }
        let Some(mut p) = k.strip_prefix(prefix) else {
            return Ok(false);
        };
        let first = u32_of(pack::pus(&mut p).ok_or_else(bad)?)?;
        if !p.is_empty() {
            return Err(bad());
        }
        let tag = self.read_tag(c)?;
        self.load(tag, 0, first)?;
        Ok(true)
    }

    pub fn did(&self) -> u32 {
        self.did
    }

    pub fn wdf(&self) -> u32 {
        self.wdf
    }

    /// Advance to the next posting (the first, on the first call). False at the end.
    pub fn next(&mut self) -> Result<bool> {
        if self.ended {
            return Ok(false);
        }
        if !self.started {
            self.started = true;
            return Ok(true);
        }
        if self.pos < self.chunk.len() {
            let mut p = &self.chunk[self.pos..];
            let next = self.did.checked_add(1).ok_or_else(bad)?;
            self.did = add_delta(next, pack::uint(&mut p).ok_or_else(bad)?)?;
            if self.did > self.last_in_chunk {
                return Err(corrupt("posting outside chunk document range"));
            }
            self.wdf = u32_of(pack::uint(&mut p).ok_or_else(bad)?)?;
            self.pos = self.chunk.len() - p.len();
            return Ok(true);
        }
        if self.did != self.last_in_chunk {
            return Err(corrupt("posting chunk ended before its last document"));
        }
        if self.is_last_chunk {
            self.release_chunk();
            self.ended = true;
            return Ok(false);
        }
        let next_min = self.last_in_chunk.checked_add(1).ok_or_else(bad)?;
        let key = postlist_key(&self.term, Some(next_min));
        let blocks = &self.db.blocks;
        let c = Cursor::seek_ge(blocks, &self.db.roots[POSTLIST], &key)?;
        match c {
            Some(c) if self.load_at(&c)? && self.did >= next_min => Ok(true),
            _ => Err(corrupt("posting list chunk missing")),
        }
    }

    /// Advance to the first posting with docid ≥ `target`. False at the end.
    pub fn skip_to(&mut self, target: u32) -> Result<bool> {
        if self.ended {
            return Ok(false);
        }
        if !self.started {
            self.started = true;
        }
        if self.did >= target {
            return Ok(true);
        }
        if target > self.last_in_chunk && !self.is_last_chunk {
            let key = postlist_key(&self.term, Some(target));
            let blocks = &self.db.blocks;
            if let Some(c) = Cursor::seek_le(blocks, &self.db.roots[POSTLIST], &key)?
                && self.load_at(&c)?
                && self.did >= target
            {
                return Ok(true);
            }
        }
        while self.did < target {
            if !self.next()? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

impl Drop for PostList<'_> {
    fn drop(&mut self) {
        self.release_chunk();
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::IndexWriter;

    #[test]
    fn live_posting_chunks_share_and_return_the_query_budget() {
        let mut writer = IndexWriter::fulltext("eng");
        for did in 1..=1400 {
            writer.add_text(&did.to_string(), "", "alpha beta").unwrap();
        }
        let mut cursor = std::io::Cursor::new(Vec::new());
        writer.write(&mut cursor, [0; 16]).unwrap();
        let db = Db::open(Box::new(cursor.into_inner())).unwrap();
        let first_size = db.postlist(b"alpha").unwrap().unwrap().chunk.capacity();
        let budget = PostingBudget::new(first_size);
        let mut alpha = db.budgeted_postlist(b"alpha", &budget).unwrap().unwrap();
        assert_eq!(budget.remaining.get(), 0);
        assert!(matches!(
            db.budgeted_postlist(b"beta", &budget),
            Err(Error::Invalid(_))
        ));
        assert_eq!(budget.remaining.get(), 0);
        let mut count = 0;
        while alpha.next().unwrap() {
            count += 1;
        }
        assert_eq!(count, 1400);
        assert_eq!(budget.remaining.get(), first_size);
        let beta = db.budgeted_postlist(b"beta", &budget).unwrap().unwrap();
        assert_eq!(budget.remaining.get(), 0);
        drop(beta);
        assert_eq!(budget.remaining.get(), first_size);
        drop(alpha);
        assert_eq!(budget.remaining.get(), first_size);
    }
}
