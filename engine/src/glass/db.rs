//! A single-file Glass database: the version block and the tables libzim's
//! indexes use (postlist, docdata, position). See `plans/glass-indexes.md`.

use super::btree::{self, Blocks, Cursor, Root, Source};
use super::{bitstream, pack};
use crate::{Error, Result};

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
    // ponytail: every document length in memory (4 bytes/doc) once searched;
    // read them per document from the doclen list if that's too much.
    doclens: std::sync::OnceLock<Vec<u32>>,
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

impl Db {
    pub fn open(src: Box<dyn Source>) -> Result<Db> {
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
                blocksize: u32_of(pack::uint(&mut p).ok_or_else(bad)? << 11)?,
                compress_min: u32_of(pack::uint(&mut p).ok_or_else(bad)?)?,
                freelist: pack::string(&mut p).ok_or_else(bad)?.to_vec(),
            };
        }
        let mut s = [0u64; 8];
        for v in &mut s {
            *v = pack::uint(&mut p).ok_or_else(bad)?;
        }
        let stats = Stats {
            doccount: u32_of(s[0])?,
            last_docid: u32_of(s[1] + s[0])?,
            doclen_lbound: u32_of(s[2])?,
            wdf_ubound: u32_of(s[3])?,
            doclen_ubound: u32_of(s[4] + s[3])?,
            total_doclen: s[6],
        };
        let size = roots[POSTLIST].blocksize as usize;
        if !(2048..=65536).contains(&size) || !size.is_power_of_two() {
            return Err(corrupt("bad block size"));
        }
        Ok(Db {
            blocks: Blocks::new(src, size),
            uuid,
            revision,
            roots,
            stats,
            doclens: Default::default(),
        })
    }

    /// Document lengths, indexed by docid (index 0 unused).
    pub fn doclen_table(&self) -> Result<&[u32]> {
        if let Some(v) = self.doclens.get() {
            return Ok(v);
        }
        let mut v = vec![0; self.stats.last_docid as usize + 1];
        if let Some(mut pl) = self.doclens()? {
            while pl.next()? {
                if let Some(x) = v.get_mut(pl.did() as usize) {
                    *x = pl.wdf();
                }
            }
        }
        Ok(self.doclens.get_or_init(|| v))
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
    pub fn postlist(&self, term: &[u8]) -> Result<Option<PostList<'_>>> {
        PostList::open(self, term)
    }

    /// Every document's length, in docid order.
    pub fn doclens(&self) -> Result<Option<PostList<'_>>> {
        PostList::open(self, b"")
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
        let mut d = pack::pus(&mut p).ok_or_else(bad)?;
        let tag = c.tag()?;
        let mut p = &tag[..];
        let mut v = pack::string(&mut p).ok_or_else(bad)?;
        while d < u64::from(did) && !p.is_empty() {
            d += pack::uint(&mut p).ok_or_else(bad)? + 1;
            v = pack::string(&mut p).ok_or_else(bad)?;
        }
        Ok((d == u64::from(did)).then(|| v.to_vec()))
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
            Some(t) => bitstream::decode(&t)
                .ok_or_else(|| corrupt("bad position list"))?
                .into_iter()
                .map(u32_of)
                .collect(),
        }
    }

    /// Terms starting with `prefix`, in byte order, with their term
    /// frequencies.
    pub fn terms(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, u32)>> {
        let mut start = vec![];
        pack::put_sort_string(&mut start, prefix, true);
        let mut out = vec![];
        let Some(mut c) = Cursor::seek_ge(&self.blocks, &self.roots[POSTLIST], &start)? else {
            return Ok(out);
        };
        while let Some(k) = c.key()?
            && k.starts_with(&start)
        {
            if let Some(t) = term_of(k) {
                let tag = c.tag()?;
                let tf = pack::uint(&mut &tag[..]).ok_or_else(bad)?;
                out.push((t, u32_of(tf)?));
            }
            if !c.next()? {
                break;
            }
        }
        Ok(out)
    }
}

/// A value slot's (frequency, lower bound, upper bound).
#[cfg(test)]
pub type ValueStats = (u32, Vec<u8>, Vec<u8>);

/// A posting list (term or doclen), read one chunk at a time.
pub struct PostList<'a> {
    db: &'a Db,
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
    fn open(db: &'a Db, term: &[u8]) -> Result<Option<PostList<'a>>> {
        let Some(tag) = db.get(POSTLIST, &postlist_key(term, None))? else {
            return Ok(None);
        };
        let mut pl = PostList {
            db,
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
        let mut p = &tag[..];
        pl.tf = u32_of(pack::uint(&mut p).ok_or_else(bad)?)?;
        pl.cf = pack::uint(&mut p).ok_or_else(bad)?;
        let first = u32_of(pack::uint(&mut p).ok_or_else(bad)? + 1)?;
        let used = tag.len() - p.len();
        pl.load(tag, used, first)?;
        Ok(Some(pl))
    }

    /// Start reading chunk `tag` at `at` (`is_last`, `last − first`, then
    /// the postings), whose first docid is `first`.
    fn load(&mut self, tag: Vec<u8>, at: usize, first: u32) -> Result<()> {
        let mut p = &tag[at..];
        self.is_last_chunk = pack::bool(&mut p).ok_or_else(bad)?;
        self.last_in_chunk = u32_of(u64::from(first) + pack::uint(&mut p).ok_or_else(bad)?)?;
        self.did = first;
        self.wdf = u32_of(pack::uint(&mut p).ok_or_else(bad)?)?;
        self.pos = tag.len() - p.len();
        self.chunk = tag;
        Ok(())
    }

    /// Load the chunk at cursor `c` (a later chunk: its key ends in its first docid).
    fn load_at(&mut self, c: &Cursor) -> Result<bool> {
        let Some(k) = c.key()? else { return Ok(false) };
        let prefix = postlist_key(&self.term, Some(0));
        let prefix = &prefix[..prefix.len() - 2];
        if k == postlist_key(&self.term, None) {
            // The first chunk again: reopen.
            let fresh = PostList::open(self.db, &self.term)?.ok_or_else(bad)?;
            *self = PostList {
                started: true,
                ..fresh
            };
            return Ok(true);
        }
        let Some(mut p) = k.strip_prefix(prefix) else {
            return Ok(false);
        };
        let first = u32_of(pack::pus(&mut p).ok_or_else(bad)?)?;
        let tag = c.tag()?;
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
            self.did = u32_of(u64::from(self.did) + pack::uint(&mut p).ok_or_else(bad)? + 1)?;
            self.wdf = u32_of(pack::uint(&mut p).ok_or_else(bad)?)?;
            self.pos = self.chunk.len() - p.len();
            return Ok(true);
        }
        if self.is_last_chunk {
            self.ended = true;
            return Ok(false);
        }
        let key = postlist_key(&self.term, Some(self.last_in_chunk + 1));
        let blocks = &self.db.blocks;
        let c = Cursor::seek_ge(blocks, &self.db.roots[POSTLIST], &key)?;
        match c {
            Some(c) if self.load_at(&c)? => Ok(true),
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
