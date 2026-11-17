//! Reading Glass B-tree tables (`glass_table.h`, `glass_table.cc`).
//!
//! A block: `REV u32 | LEVEL u8 | MAX_FREE u16 | TOTAL_FREE u16 | DIR_END u16`,
//! then the directory (u16 item offsets, in key order) from byte 11 to
//! `DIR_END`, items packed at the end. All big-endian.
//!
//! - Leaf item: `u16 I | u8 K | key | [u16 component, unless first] | tag`,
//!   size `(I & 0x1fff) + 3`; I's top bits: compressed, last, first.
//! - Branch item: `u32 child | u8 K | key | u16 component`. A branch's first
//!   item is never compared (it's −∞).

use crate::{Error, Result};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};

pub const DIR_START: usize = 11;
const COMPRESSED: u8 = 0x80;
const LAST: u8 = 0x40;
const FIRST: u8 = 0x20;
const CACHE_BLOCKS: usize = 256;

fn corrupt(what: &str) -> Error {
    Error::Format(format!("Xapian index: {what}"))
}

/// Positional reads of the database's bytes (offset 0 = block 0).
pub trait Source: Send + Sync {
    fn read_at(&self, buf: &mut [u8], off: u64) -> Result<()>;
}

impl Source for Vec<u8> {
    fn read_at(&self, buf: &mut [u8], off: u64) -> Result<()> {
        let s = usize::try_from(off)
            .ok()
            .and_then(|o| self.get(o..o + buf.len()));
        buf.copy_from_slice(s.ok_or_else(|| corrupt("read past the end"))?);
        Ok(())
    }
}

pub type Block = Arc<[u8]>;

/// The blocks of one database, with a small cache.
pub struct Blocks {
    src: Box<dyn Source>,
    pub size: usize,
    // ponytail: cleared when full, not LRU; make it an LRU if hit rates matter.
    cache: Mutex<HashMap<u32, Block>>,
}

impl Blocks {
    pub fn new(src: Box<dyn Source>, size: usize) -> Blocks {
        Blocks {
            src,
            size,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, n: u32) -> Result<Block> {
        if let Some(b) = self.cache.lock().unwrap().get(&n) {
            return Ok(b.clone());
        }
        let mut v = vec![0; self.size];
        self.src.read_at(&mut v, u64::from(n) * self.size as u64)?;
        let dir_end = be16(&v, 9);
        if dir_end < DIR_START || dir_end > self.size || !(dir_end - DIR_START).is_multiple_of(2) {
            return Err(corrupt(&format!("block {n} is corrupt")));
        }
        let b: Block = v.into();
        let mut c = self.cache.lock().unwrap();
        if c.len() >= CACHE_BLOCKS {
            c.clear();
        }
        c.insert(n, b.clone());
        Ok(b)
    }
}

pub fn be16(b: &[u8], at: usize) -> usize {
    u16::from_be_bytes([b[at], b[at + 1]]) as usize
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn level(b: &[u8]) -> u8 {
    b[4]
}

fn count(b: &[u8]) -> usize {
    (be16(b, 9) - DIR_START) / 2
}

fn item_off(b: &[u8], i: usize) -> Result<usize> {
    let o = be16(b, DIR_START + 2 * i);
    if o + 3 > b.len() {
        return Err(corrupt("item offset out of the block"));
    }
    Ok(o)
}

/// A leaf item: flags, key, component number, tag bytes.
struct Leaf<'a> {
    flags: u8,
    key: &'a [u8],
    component: usize,
    tag: &'a [u8],
}

fn leaf(b: &[u8], i: usize) -> Result<Leaf<'_>> {
    let o = item_off(b, i)?;
    let size = (be16(b, o) & 0x1fff) + 3;
    let flags = b[o];
    let k = b[o + 2] as usize;
    let first = flags & FIRST != 0;
    let tag_at = o + 3 + k + if first { 0 } else { 2 };
    if o + size > b.len() || tag_at > o + size {
        return Err(corrupt("leaf item out of the block"));
    }
    Ok(Leaf {
        flags,
        key: &b[o + 3..o + 3 + k],
        component: if first { 1 } else { be16(b, o + 3 + k) },
        tag: &b[tag_at..o + size],
    })
}

/// A branch item: (child block, key, component).
fn branch(b: &[u8], i: usize) -> Result<(u32, &[u8], usize)> {
    let o = item_off(b, i)?;
    let k = b[o + 4] as usize;
    if o + 7 + k > b.len() {
        return Err(corrupt("branch item out of the block"));
    }
    Ok((be32(b, o), &b[o + 5..o + 5 + k], be16(b, o + 5 + k)))
}

/// A table's root info (`RootInfo`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Root {
    pub root: u32,
    pub level: u8,
    pub entries: u64,
    pub fake: bool,
    pub sequential: bool,
    pub blocksize: u32,
    pub compress_min: u32,
    pub freelist: Vec<u8>,
}

/// A position in a table: blocks from the leaf (`[0]`) up to the root, and
/// the item index in each. A leaf index of −1 is "before the first item".
pub struct Cursor<'a> {
    blocks: &'a Blocks,
    path: Vec<(Block, isize)>,
}

impl<'a> Cursor<'a> {
    /// The cursor at the last entry ≤ `key` (the first component of each
    /// tag is compared, so an equal key matches its entry). `None` for an
    /// empty table.
    pub fn seek_le(blocks: &'a Blocks, root: &Root, key: &[u8]) -> Result<Option<Cursor<'a>>> {
        if root.fake {
            return Ok(None);
        }
        let mut path = vec![];
        let mut n = root.root;
        for lvl in (0..=root.level).rev() {
            let b = blocks.get(n)?;
            if level(&b) != lvl {
                return Err(corrupt(&format!("block {n} isn't at level {lvl}")));
            }
            if lvl == 0 {
                // Last item ≤ (key, 1), or −1.
                let (mut lo, mut hi) = (-1isize, count(&b) as isize);
                while hi - lo > 1 {
                    let mid = (lo + hi) / 2;
                    let it = leaf(&b, mid as usize)?;
                    if (it.key, it.component) <= (key, 1) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                path.insert(0, (b, lo));
                break;
            }
            // Last item ≤ key, the first one never compared.
            let (mut lo, mut hi) = (0usize, count(&b));
            if hi == 0 {
                return Err(corrupt("empty branch block"));
            }
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                let (_, k, c) = branch(&b, mid)?;
                if (k, c) <= (key, 1) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            n = branch(&b, lo)?.0;
            path.insert(0, (b, lo as isize));
        }
        let mut c = Cursor { blocks, path };
        // Back to the entry's first component, or before the leaf: step back.
        while c.path[0].1 < 0 || leaf(&c.path[0].0, c.path[0].1 as usize)?.flags & FIRST == 0 {
            if !c.step(-1)? {
                break; // before the first item of the table
            }
        }
        Ok(Some(c))
    }

    /// The cursor at the first entry ≥ `key`, if any.
    pub fn seek_ge(blocks: &'a Blocks, root: &Root, key: &[u8]) -> Result<Option<Cursor<'a>>> {
        let Some(mut c) = Cursor::seek_le(blocks, root, key)? else {
            return Ok(None);
        };
        if c.key()? != Some(key) && !c.next()? {
            return Ok(None);
        }
        Ok(Some(c))
    }

    /// Move one item along (−1 or +1), across leaves. False at either end
    /// (the cursor is then before the first item, or stays on the last).
    fn step(&mut self, dir: isize) -> Result<bool> {
        let (b, i) = &self.path[0];
        let ni = i + dir;
        if ni >= 0 && (ni as usize) < count(b) {
            self.path[0].1 = ni;
            return Ok(true);
        }
        // Up to the first level that can move, then down its edge.
        let mut lvl = 1;
        loop {
            let Some((b, i)) = self.path.get(lvl) else {
                if dir < 0 {
                    self.path[0].1 = -1;
                }
                return Ok(false);
            };
            let ni = i + dir;
            if ni >= 0 && (ni as usize) < count(b) {
                self.path[lvl].1 = ni;
                break;
            }
            lvl += 1;
        }
        while lvl > 0 {
            let (b, i) = &self.path[lvl];
            let child = branch(b, *i as usize)?.0;
            let cb = self.blocks.get(child)?;
            if level(&cb) as usize != lvl - 1 {
                return Err(corrupt(&format!("block {child} is at the wrong level")));
            }
            let ci = if dir > 0 { 0 } else { count(&cb) as isize - 1 };
            lvl -= 1;
            self.path[lvl] = (cb, ci);
        }
        Ok(true)
    }

    /// To the next entry (skipping a tag's later components). False at the end.
    pub fn next(&mut self) -> Result<bool> {
        loop {
            if !self.step(1)? {
                return Ok(false);
            }
            if leaf(&self.path[0].0, self.path[0].1 as usize)?.flags & FIRST != 0 {
                return Ok(true);
            }
        }
    }

    /// The current entry's key (`None` before the first item).
    pub fn key(&self) -> Result<Option<&[u8]>> {
        let (b, i) = &self.path[0];
        if *i < 0 {
            return Ok(None);
        }
        Ok(Some(leaf(b, *i as usize)?.key))
    }

    /// The current entry's whole tag: its components joined, inflated if
    /// compressed. Leaves the cursor on the entry.
    pub fn tag(&self) -> Result<Vec<u8>> {
        let (b, i) = &self.path[0];
        if *i < 0 {
            return Err(corrupt("no current entry"));
        }
        let it = leaf(b, *i as usize)?;
        let compressed = it.flags & COMPRESSED != 0;
        let mut tag = it.tag.to_vec();
        if it.flags & LAST == 0 {
            let mut c = Cursor {
                blocks: self.blocks,
                path: self.path.clone(),
            };
            loop {
                if !c.step(1)? {
                    return Err(corrupt("tag's last component is missing"));
                }
                let it = leaf(&c.path[0].0, c.path[0].1 as usize)?;
                tag.extend_from_slice(it.tag);
                if it.flags & LAST != 0 {
                    break;
                }
            }
        }
        if compressed {
            let mut out = vec![];
            flate2::read::DeflateDecoder::new(&tag[..])
                .read_to_end(&mut out)
                .map_err(|e| corrupt(&format!("bad compressed tag: {e}")))?;
            tag = out;
        }
        Ok(tag)
    }
}

/// The tag of `key` exactly, if present.
pub fn get(blocks: &Blocks, root: &Root, key: &[u8]) -> Result<Option<Vec<u8>>> {
    match Cursor::seek_le(blocks, root, key)? {
        Some(c) if c.key()? == Some(key) => Ok(Some(c.tag()?)),
        _ => Ok(None),
    }
}
