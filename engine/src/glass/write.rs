// Modified for the standalone zimru-xapian tools on 2026-11-17. See PROVENANCE.md.
//! Writing a compacted single-file Glass database, the way libzim's indexes
//! are stored. Documents are added in docid order (1, 2, …) into memory;
//! [`Builder::write`] lays out the B-trees bottom-up.
//!
//! Layout: block 0 is the version block (written last), then each table's
//! leaves in key order and its branch levels, root last. No freelist,
//! revision 1, tags never compressed, branch keys unshortened.

use super::db::{self, MAGIC};
use super::{bitstream, pack};
use crate::{Error, Result};
use std::collections::BTreeMap;
use std::io::{Seek, SeekFrom, Write};

pub const BLOCKSIZE: usize = 8192;
const MAX_ITEM: usize = 2043; // (8192 - 11 - 4 × 2) / 4: four items fit in a block
const CHUNK: usize = 2000; // Xapian's posting and value chunk size
const NULL_ITEM: [u8; 3] = [0x60, 0, 0]; // first + last, empty key and tag

/// A document's terms: wdf and positions (empty for none), by term.
pub type Terms = BTreeMap<Vec<u8>, (u32, Vec<u32>)>;

#[derive(Default)]
struct Postings {
    tf: u32,
    cf: u64,
    last: u32,
    /// `uint(did − previous − 1)`, `uint(wdf)` per posting (previous = 0 at first).
    buf: Vec<u8>,
}

impl Postings {
    fn push(&mut self, did: u32, wdf: u32) {
        pack::put_uint(&mut self.buf, u64::from(did - self.last - 1));
        pack::put_uint(&mut self.buf, wdf.into());
        self.tf += 1;
        self.cf += u64::from(wdf);
        self.last = did;
    }
}

#[derive(Default)]
pub struct Builder {
    terms: BTreeMap<Vec<u8>, Postings>,
    /// Position table entries, by key.
    positions: BTreeMap<Vec<u8>, Vec<u8>>,
    // Encode document lengths as postings at insertion time. This avoids building
    // an additional archive-sized buffer when writing the postlist table.
    doclens: Postings,
    doclen_lbound: u32,
    doclen_ubound: u32,
    data: Vec<Vec<u8>>,
    values: BTreeMap<u32, Vec<(u32, Vec<u8>)>>,
    metadata: BTreeMap<String, Vec<u8>>,
    wdf_ubound: u32,
}

impl Builder {
    /// Add the next document; returns its docid. Empty values aren't stored.
    pub fn add(&mut self, data: &[u8], terms: &Terms, values: &[(u32, Vec<u8>)]) -> Result<u32> {
        // Validate before mutation so a rejected document consumes no id.
        let did = self.doccount().checked_add(1).ok_or_else(|| {
            Error::Invalid("document identifier exceeds the supported range".into())
        })?;
        let len = terms
            .values()
            .try_fold(0u32, |sum, (wdf, _)| sum.checked_add(*wdf))
            .ok_or_else(|| Error::Invalid("document length exceeds the supported range".into()))?;
        self.data.push(data.to_vec());
        for (t, (wdf, pos)) in terms {
            let postings = match self.terms.get_mut(t.as_slice()) {
                Some(existing) => existing,
                None => self.terms.entry(t.clone()).or_default(),
            };
            postings.push(did, *wdf);
            self.wdf_ubound = self.wdf_ubound.max(*wdf);
            if !pos.is_empty() {
                let pos: Vec<u64> = pos.iter().map(|&p| p.into()).collect();
                self.positions
                    .insert(db::position_key(t, did), bitstream::encode(&pos));
            }
        }
        self.doclens.push(did, len);
        if len != 0 && (self.doclen_lbound == 0 || len < self.doclen_lbound) {
            self.doclen_lbound = len;
        }
        self.doclen_ubound = self.doclen_ubound.max(len);
        for (slot, v) in values {
            if !v.is_empty() {
                self.values.entry(*slot).or_default().push((did, v.clone()));
            }
        }
        Ok(did)
    }

    pub fn set_metadata(&mut self, key: &str, value: &[u8]) {
        self.metadata.insert(key.into(), value.to_vec());
    }

    pub fn doccount(&self) -> u32 {
        self.doclens.tf
    }

    /// Write a complete database to an empty seekable destination.
    pub fn write(&self, out: &mut (impl Write + Seek), uuid: [u8; 16]) -> Result<()> {
        // Write + Seek cannot truncate a previous database's trailing blocks.
        let position = out.stream_position()?;
        if out.seek(SeekFrom::End(0))? != 0 {
            out.seek(SeekFrom::Start(position))?;
            return Err(Error::Invalid("Glass output must be empty".into()));
        }
        out.write_all(&[0; BLOCKSIZE])?;
        let mut next = 1u32;
        let mut t = Table::new(out, &mut next);
        self.postlist_entries(&mut t)?;
        let postlist = t.finish()?;
        let mut t = Table::new(out, &mut next);
        for (i, d) in self.data.iter().enumerate() {
            if !d.is_empty() {
                t.add(&db::docdata_key(i as u32 + 1), d)?;
            }
        }
        let docdata = t.finish()?;
        let mut t = Table::new(out, &mut next);
        for (k, v) in &self.positions {
            t.add(k, v)?;
        }
        let position = t.finish()?;

        // The version block.
        let mut v = MAGIC.to_vec();
        v.extend_from_slice(&uuid);
        pack::put_uint(&mut v, 1); // revision
        let fake = TableRoot::default();
        let roots = [
            (postlist, 0),
            (docdata, 18),
            (fake.clone(), 18), // termlist
            (position, 0),
            (fake.clone(), 18), // spelling
            (fake, 18),         // synonym
        ];
        for (r, compress_min) in roots {
            pack::put_uint(&mut v, r.root.into());
            pack::put_uint(
                &mut v,
                if r.entries == 0 {
                    3
                } else {
                    u64::from(r.level) << 2
                },
            );
            pack::put_uint(&mut v, r.entries);
            pack::put_uint(&mut v, (BLOCKSIZE >> 11) as u64);
            pack::put_uint(&mut v, compress_min);
            let mut fl = vec![];
            if r.entries != 0 {
                for x in [1, next.into(), 0, 0, 0, 0] {
                    pack::put_uint(&mut fl, x); // revision, first unused block, no freelist
                }
            }
            pack::put_string(&mut v, &fl);
        }
        let n = self.doccount() as u64;
        let wdf = u64::from(self.wdf_ubound);
        for x in [
            n,
            0,
            self.doclen_lbound.into(),
            wdf,
            u64::from(self.doclen_ubound) - wdf,
            0,
            self.doclens.cf,
            0,
        ] {
            pack::put_uint(&mut v, x);
        }
        out.seek(SeekFrom::Start(0))?;
        out.write_all(&v)?;
        out.seek(SeekFrom::Start(u64::from(next) * BLOCKSIZE as u64))?;
        out.flush()?;
        Ok(())
    }

    /// The postlist table, in key order: metadata (`00 C0`), value stats
    /// (`00 D0`), value chunks (`00 D8`), doclens (`00 E0`), then terms.
    fn postlist_entries(&self, t: &mut Table<impl Write + Seek>) -> Result<()> {
        for (k, v) in &self.metadata {
            if !v.is_empty() {
                t.add(&db::metadata_key(k.as_bytes()), v)?;
            }
        }
        // Slots sorted by their encoded keys.
        let mut stats: Vec<_> = self
            .values
            .keys()
            .map(|&s| (db::value_stats_key(s), s))
            .collect();
        stats.sort();
        for (k, slot) in &stats {
            let vals = &self.values[slot];
            let lo = vals.iter().map(|v| &v.1).min().unwrap();
            let hi = vals.iter().map(|v| &v.1).max().unwrap();
            let mut tag = vec![];
            pack::put_uint(&mut tag, vals.len() as u64);
            pack::put_string(&mut tag, lo);
            if lo != hi {
                tag.extend_from_slice(hi);
            }
            t.add(k, &tag)?;
        }
        let mut slots: Vec<_> = self
            .values
            .keys()
            .map(|&s| (db::value_key(s, None), s))
            .collect();
        slots.sort();
        for (_, slot) in &slots {
            let mut chunk = vec![];
            let (mut first, mut prev) = (0, 0);
            for (did, v) in &self.values[slot] {
                if chunk.is_empty() {
                    first = *did;
                } else {
                    pack::put_uint(&mut chunk, u64::from(did - prev - 1));
                }
                pack::put_string(&mut chunk, v);
                prev = *did;
                if chunk.len() >= CHUNK {
                    t.add(&db::value_key(*slot, Some(first)), &chunk)?;
                    chunk.clear();
                }
            }
            if !chunk.is_empty() {
                t.add(&db::value_key(*slot, Some(first)), &chunk)?;
            }
        }
        postlist_chunks(t, b"", &self.doclens)?;
        for (term, p) in &self.terms {
            postlist_chunks(t, term, p)?;
        }
        Ok(())
    }
}

/// A term's (or the doclens') postings, as ~2000-byte chunks.
fn postlist_chunks(t: &mut Table<impl Write + Seek>, term: &[u8], p: &Postings) -> Result<()> {
    let bad = || crate::Error::Invalid("corrupt postings".into());
    let mut b = &p.buf[..];
    let mut did = 0u32;
    let mut first_chunk = true;
    while !b.is_empty() {
        let mut body = vec![];
        let first = did + pack::uint(&mut b).ok_or_else(bad)? as u32 + 1;
        did = first;
        pack::put_uint(&mut body, pack::uint(&mut b).ok_or_else(bad)?);
        while body.len() < CHUNK && !b.is_empty() {
            let gap = pack::uint(&mut b).ok_or_else(bad)?;
            did += gap as u32 + 1;
            pack::put_uint(&mut body, gap);
            pack::put_uint(&mut body, pack::uint(&mut b).ok_or_else(bad)?);
        }
        let mut tag = vec![];
        if first_chunk {
            // Glass reserves zero frequency fields for document-length postings.
            let (tf, cf) = if term.is_empty() {
                (0, 0)
            } else {
                (p.tf, p.cf)
            };
            pack::put_uint(&mut tag, tf.into());
            pack::put_uint(&mut tag, cf);
            pack::put_uint(&mut tag, u64::from(first - 1));
        }
        tag.push(if b.is_empty() { b'1' } else { b'0' });
        pack::put_uint(&mut tag, u64::from(did - first));
        tag.extend_from_slice(&body);
        let key = db::postlist_key(term, (!first_chunk).then_some(first));
        t.add(&key, &tag)?;
        first_chunk = false;
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct TableRoot {
    root: u32,
    level: u8,
    entries: u64,
}

/// One table being written: leaves stream to `out` as they fill.
struct Table<'a, W> {
    out: &'a mut W,
    next: &'a mut u32,
    /// The leaf being filled: items, and their total size.
    items: Vec<Vec<u8>>,
    size: usize,
    /// Per child block of the level above: (first key, its component, block).
    children: Vec<(Vec<u8>, u16, u32)>,
    entries: u64,
    last_key: Vec<u8>,
}

fn fits(items: usize, size: usize, item: usize) -> bool {
    btree_dir_end(items + 1) + size + item <= BLOCKSIZE
}

fn btree_dir_end(items: usize) -> usize {
    super::btree::DIR_START + 2 * items
}

// Glass stores each component's one-based number in a u16.
fn component_count(tag_len: usize, first_cap: usize, later_cap: usize) -> Result<u16> {
    let later = tag_len.saturating_sub(first_cap).div_ceil(later_cap);
    u16::try_from(later)
        .ok()
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| Error::Invalid("Glass value has too many components".into()))
}

impl<'a, W: Write + Seek> Table<'a, W> {
    fn new(out: &'a mut W, next: &'a mut u32) -> Self {
        Table {
            out,
            next,
            items: vec![NULL_ITEM.to_vec()],
            size: NULL_ITEM.len(),
            children: vec![],
            entries: 0,
            last_key: Vec::new(),
        }
    }

    /// Add an entry; keys must come in strictly increasing order.
    fn add(&mut self, key: &[u8], tag: &[u8]) -> Result<()> {
        if key.is_empty() || key.len() > 255 {
            return Err(crate::Error::Invalid(format!(
                "index key of {} bytes",
                key.len()
            )));
        }
        if self.last_key.as_slice() >= key {
            return Err(Error::Invalid("Glass table keys are out of order".into()));
        }
        self.last_key.clear();
        self.last_key.extend_from_slice(key);
        self.entries += 1;
        let first_cap = MAX_ITEM - 3 - key.len();
        let later_cap = first_cap - 2;
        let m = component_count(tag.len(), first_cap, later_cap)?;
        let mut rest = tag;
        for c in 1..=m {
            let cap = if c == 1 { first_cap } else { later_cap };
            let (part, tail) = rest.split_at(rest.len().min(cap));
            rest = tail;
            let size = 3 + key.len() + if c == 1 { 0 } else { 2 } + part.len();
            let flags: u16 = if c == 1 { 0x2000 } else { 0 } | if c == m { 0x4000 } else { 0 };
            let mut it = Vec::with_capacity(size);
            it.extend_from_slice(&(flags | (size - 3) as u16).to_be_bytes());
            it.push(key.len() as u8);
            it.extend_from_slice(key);
            if c > 1 {
                it.extend_from_slice(&c.to_be_bytes());
            }
            it.extend_from_slice(part);
            if !fits(self.items.len(), self.size, it.len()) {
                self.flush_leaf()?;
            }
            self.size += it.len();
            self.items.push(it);
        }
        Ok(())
    }

    /// Write `items` as a block at level `level`; returns its number.
    fn write_block(&mut self, level: u8, items: &[Vec<u8>]) -> Result<u32> {
        let n = *self.next;
        let next = n.checked_add(1).ok_or_else(|| {
            Error::Invalid("Glass block number exceeds the supported range".into())
        })?;
        let mut b = [0u8; BLOCKSIZE];
        let dir_end = btree_dir_end(items.len());
        let mut pos = BLOCKSIZE;
        for (i, it) in items.iter().enumerate() {
            pos -= it.len();
            b[pos..pos + it.len()].copy_from_slice(it);
            let d = super::btree::DIR_START + 2 * i;
            b[d..d + 2].copy_from_slice(&(pos as u16).to_be_bytes());
        }
        let free = (pos - dir_end) as u16;
        b[0..4].copy_from_slice(&1u32.to_be_bytes()); // revision
        b[4] = level;
        b[5..7].copy_from_slice(&free.to_be_bytes());
        b[7..9].copy_from_slice(&free.to_be_bytes());
        b[9..11].copy_from_slice(&(dir_end as u16).to_be_bytes());
        // All table blocks are appended in allocation order after block zero.
        self.out.write_all(&b)?;
        *self.next = next;
        Ok(n)
    }

    fn flush_leaf(&mut self) -> Result<()> {
        let items = std::mem::take(&mut self.items);
        self.size = 0;
        let first = &items[0];
        let k = first[2] as usize;
        let comp = if first[0] & 0x20 != 0 {
            1
        } else {
            u16::from_be_bytes([first[3 + k], first[4 + k]])
        };
        let key = first[3..3 + k].to_vec();
        let n = self.write_block(0, &items)?;
        self.children.push((key, comp, n));
        Ok(())
    }

    fn finish(mut self) -> Result<TableRoot> {
        if self.entries == 0 {
            return Ok(TableRoot::default());
        }
        self.flush_leaf()?;
        let mut level = 0;
        while self.children.len() > 1 {
            level += 1;
            let children = std::mem::take(&mut self.children);
            let mut items: Vec<Vec<u8>> = vec![];
            let mut size = 0;
            let mut first: Option<(Vec<u8>, u16)> = None;
            for (key, comp, child) in children {
                let mut it = child.to_be_bytes().to_vec();
                it.push(key.len() as u8);
                it.extend_from_slice(&key);
                it.extend_from_slice(&comp.to_be_bytes());
                if !fits(items.len(), size, it.len()) {
                    let (k, c) = first.take().unwrap();
                    let n = self.write_block(level, &items)?;
                    self.children.push((k, c, n));
                    items.clear();
                    size = 0;
                }
                if items.is_empty() {
                    first = Some((key, comp));
                }
                size += it.len();
                items.push(it);
            }
            let (k, c) = first.unwrap();
            let n = self.write_block(level, &items)?;
            self.children.push((k, c, n));
        }
        Ok(TableRoot {
            root: self.children[0].2,
            level,
            entries: self.entries,
        })
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn component_count_respects_the_16_bit_limit() {
        let (first, later) = (2000, 1998);
        let largest = first + (usize::from(u16::MAX) - 1) * later;
        assert_eq!(component_count(0, first, later).unwrap(), 1);
        assert_eq!(component_count(largest, first, later).unwrap(), u16::MAX);
        assert!(component_count(largest + 1, first, later).is_err());
    }

    #[test]
    fn rejected_document_does_not_change_the_database() {
        let mut b = Builder::default();
        let terms = Terms::from([
            (b"first".to_vec(), (u32::MAX, vec![])),
            (b"second".to_vec(), (1, vec![])),
        ]);
        assert!(b.add(b"too large", &terms, &[]).is_err());
        assert!(b.data.is_empty() && b.terms.is_empty());
        assert_eq!(b.add(b"valid", &Terms::new(), &[]).unwrap(), 1);
        b.doclens.tf = u32::MAX;
        assert!(b.add(b"past the last id", &Terms::new(), &[]).is_err());
        assert_eq!(b.data.len(), 1);
    }

    #[test]
    fn exhausted_block_counter_fails_before_output() {
        let mut bytes = Cursor::new(Vec::new());
        let mut next = u32::MAX;
        let mut t = Table::new(&mut bytes, &mut next);
        assert!(t.write_block(0, &[NULL_ITEM.to_vec()]).is_err());
        assert!(bytes.into_inner().is_empty());
        assert_eq!(next, u32::MAX);
    }
}
