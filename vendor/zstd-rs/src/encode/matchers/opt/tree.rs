//! One binary tree per hash bucket, ordered by suffix bytes. Each node uses
//! two slots in the existing chain table. Older generations are ignored.
use super::super::{count, hash, read64, BlockInput, Tables};
use super::{Cand, Chain};
use alloc::vec::Vec;

pub(super) fn collect(t: &mut Tables, b: &BlockInput<'_>, ip: usize, c: &Chain, best: usize, out: &mut Vec<Cand>) {
    let mut cursor = t.next_insert.max(b.low);
    while cursor < ip {
        let matched = insert(t, b, cursor, c, 0, |_, _| {});
        // A long repeated string also matches at the intervening positions.
        // Keep an indexed suffix near its end without rescanning the string.
        cursor += matched.saturating_sub(8).max(1).min(ip - cursor);
    }
    if cursor == ip {
        insert(t, b, ip, c, best, |len, off| out.push(Cand { off: off as u32, len: len as u32 }));
        t.next_insert = ip + 1;
    }
}

#[inline(always)]
fn insert(t: &mut Tables, b: &BlockInput<'_>, ip: usize, c: &Chain, mut best: usize, mut matched: impl FnMut(usize, usize)) -> usize {
    let src = b.src;
    let epoch = t.epoch as usize;
    let token = epoch + ip;
    let floor = epoch + b.low.max(ip.saturating_sub(b.max_dist));
    let nodes = 1usize << (c.chain_log - 1);
    let slot = |pos| (pos & (nodes - 1)) * 2;
    let bucket = hash(read64(src, ip), c.mls, c.hash_log);
    let mut candidate = t.hash[bucket] as usize;
    t.hash[bucket] = token as u32;
    let mut smaller = slot(token);
    let mut larger = smaller + 1;
    let mut lo_lcp = 0;
    let mut hi_lcp = 0;
    let mut longest = 0;
    for _ in 0..c.attempts {
        if candidate < floor || candidate >= token { break; }
        let m = candidate - epoch;
        let common = lo_lcp.min(hi_lcp);
        let len = common + count(src, m + common, ip + common, b.end);
        longest = longest.max(len);
        if len > best {
            best = len;
            matched(len, ip - m);
        }
        // After wraparound the referenced suffix is still valid, but its
        // table slots now belong to a newer suffix.
        if token - candidate >= nodes || ip + len == b.end { break; }
        if src[m + len] < src[ip + len] {
            t.chain[smaller] = candidate as u32;
            smaller = slot(candidate) + 1;
            candidate = t.chain[smaller] as usize;
            lo_lcp = len;
        } else {
            t.chain[larger] = candidate as u32;
            larger = slot(candidate);
            candidate = t.chain[larger] as usize;
            hi_lcp = len;
        }
    }
    t.chain[smaller] = 0;
    t.chain[larger] = 0;
    longest
}
