// Modified for the standalone zimru-xapian tools on 2026-11-17. See PROVENANCE.md.
//! Position lists: `uint last`, then (2+ positions) an interpolative-coded
//! bitstream, LSB first (`glass_positionlist.cc`, `common/bitstream.cc`).

use super::pack;
use crate::{Error, Result};

fn hob(x: u64) -> u32 {
    64 - x.leading_zeros()
}

struct Writer {
    buf: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Writer {
    fn encode(&mut self, mut value: u64, outof: u64) {
        let mut bits = hob(outof - 1);
        let spare = (1u64.checked_shl(bits).unwrap_or(0)).wrapping_sub(outof);
        if spare != 0 {
            let mid_start = (outof - spare) / 2;
            if value >= mid_start + spare {
                value = (value - (mid_start + spare)) | (1 << (bits - 1));
            } else if value >= mid_start {
                bits -= 1;
            }
        }
        if bits + self.n > 64 {
            self.acc |= value << self.n;
            self.buf.push(self.acc as u8);
            self.acc >>= 8;
            value >>= 8;
            bits -= 8;
        }
        self.acc |= value.checked_shl(self.n).unwrap_or(0);
        self.n += bits;
        while self.n >= 8 {
            self.buf.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn interpolative(&mut self, pos: &[u64], mut j: usize, k: usize) {
        while j + 1 < k {
            let mid = j + (k - j) / 2;
            let outof = pos[k] - pos[j] + j as u64 - k as u64 + 1;
            let lowest = pos[j] + (mid - j) as u64;
            self.encode(pos[mid] - lowest, outof);
            self.interpolative(pos, j, mid);
            j = mid;
        }
    }
}

/// Encode strictly increasing positions (at least one).
pub fn encode(pos: &[u64]) -> Vec<u8> {
    let last = *pos.last().expect("no positions");
    let mut w = Writer {
        buf: vec![],
        acc: 0,
        n: 0,
    };
    pack::put_uint(&mut w.buf, last);
    if pos.len() > 1 {
        w.encode(pos[0], last);
        w.encode(pos.len() as u64 - 2, last - pos[0]);
        w.interpolative(pos, 0, pos.len() - 1);
        if w.n > 0 {
            w.buf.push(w.acc as u8);
        }
    }
    w.buf
}

struct Reader<'a> {
    buf: &'a [u8],
    idx: usize,
    acc: u64,
    n: u32,
}

impl Reader<'_> {
    fn bits(&mut self, count: u32) -> Option<u64> {
        if count > 57 {
            let lo = self.bits(32)?;
            return Some(lo | (self.bits(count - 32)? << 32));
        }
        while self.n < count {
            self.acc |= u64::from(*self.buf.get(self.idx)?) << self.n;
            self.idx += 1;
            self.n += 8;
        }
        let r = self.acc & ((1u64 << count) - 1);
        self.acc >>= count;
        self.n -= count;
        Some(r)
    }

    fn decode(&mut self, outof: u64) -> Option<u64> {
        let bits = hob(outof.checked_sub(1)?);
        let spare = (1u64.checked_shl(bits).unwrap_or(0)).wrapping_sub(outof);
        if spare == 0 {
            return self.bits(bits);
        }
        let mid_start = (outof - spare) / 2;
        let mut p = self.bits(bits - 1)?;
        if p < mid_start && self.bits(1)? == 1 {
            p += mid_start + spare;
        }
        Some(p)
    }

    fn interpolative(
        &mut self,
        pos: &mut [u32],
        mut j: usize,
        k: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        while j + 1 < k {
            let mid = j + (k - j) / 2;
            if mid.is_multiple_of(256) {
                crate::check_cancel(cancelled)?;
            }
            let range = u64::from(pos[k])
                .checked_sub(u64::from(pos[j]))
                .ok_or_else(bad_positions)?;
            let outof = range
                .checked_sub((k - j) as u64)
                .ok_or_else(bad_positions)?
                + 1;
            let lowest = u64::from(pos[j]) + (mid - j) as u64;
            pos[mid] = u32::try_from(lowest + self.decode(outof).ok_or_else(bad_positions)?)
                .map_err(|_| bad_positions())?;
            self.interpolative(pos, j, mid, cancelled)?;
            j = mid;
        }
        Ok(())
    }
}

fn bad_positions() -> Error {
    Error::Format("Xapian index: bad position list".into())
}

/// Decode a single term's positions within one document.
#[cfg(test)]
pub fn decode(data: &[u8], max_positions: usize) -> Result<Vec<u32>> {
    decode_checked(data, max_positions, &|| false)
}

pub fn decode_checked(
    data: &[u8],
    max_positions: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u32>> {
    crate::check_cancel(cancelled)?;
    let mut p = data;
    let last = pack::uint(&mut p)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(bad_positions)?;
    let mut r = Reader {
        buf: p,
        idx: 0,
        acc: 0,
        n: 0,
    };
    let (first, count) = if p.is_empty() {
        (last, 1)
    } else {
        let first = r.decode(u64::from(last)).ok_or_else(bad_positions)? as u32;
        let span = u64::from(last - first);
        let count = r.decode(span).ok_or_else(bad_positions)? + 2;
        if count > span + 1 {
            return Err(bad_positions());
        }
        (first, usize::try_from(count).map_err(|_| bad_positions())?)
    };
    if count > max_positions {
        return Err(Error::Invalid(format!(
            "Glass position list exceeds the {max_positions}-position reader limit"
        )));
    }
    let mut positions = Vec::new();
    positions
        .try_reserve_exact(count)
        .map_err(|_| Error::Invalid("cannot allocate Glass position list".into()))?;
    positions.resize(count, 0);
    positions[0] = first;
    positions[count - 1] = last;
    r.interpolative(&mut positions, 0, count - 1, cancelled)?;
    // Xapian BitReader::check_all_gone permits only zero padding in the
    // final partial byte of a position list.
    if r.idx != r.buf.len() || r.n > 7 || r.acc != 0 {
        return Err(bad_positions());
    }
    Ok(positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_limit_is_applied_to_the_decoded_count() {
        let encoded = encode(&[1, 2, 3, u64::from(u32::MAX)]);
        assert_eq!(decode(&encoded, 4).unwrap(), [1, 2, 3, u32::MAX]);
        assert!(matches!(decode(&encoded, 3), Err(Error::Invalid(_))));
        let single = encode(&[0]);
        assert_eq!(decode(&single, 1).unwrap(), [0]);
        assert!(matches!(decode(&single, 0), Err(Error::Invalid(_))));
    }

    #[test]
    fn one_long_position_list_can_be_cancelled_during_decode() {
        let data = encode(&(1..=10000).collect::<Vec<u64>>());
        let calls = std::cell::Cell::new(0);
        let error = decode_checked(&data, 10000, &|| {
            calls.set(calls.get() + 1);
            calls.get() == 5
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "Search cancelled");
    }
    #[test]
    fn round_trips() {
        assert_eq!(decode(&[0x04, 0x02], 2).unwrap(), vec![2, 4]); // from a real index
        let cases: Vec<Vec<u64>> = vec![
            vec![1],
            vec![7],
            vec![1, 2],
            vec![2, 4],
            vec![1, 2, 3, 4, 5],
            vec![3, 17, 18, 400, 401, 9000, 1 << 20],
            (1..300).map(|i| i * i).collect(),
        ];
        for c in cases {
            assert_eq!(
                decode(&encode(&c), c.len())
                    .unwrap()
                    .into_iter()
                    .map(u64::from)
                    .collect::<Vec<_>>(),
                c
            );
        }
        // Pseudo-random lists.
        let mut x = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..500 {
            let mut v = vec![];
            let mut p = 0;
            for _ in 0..(x % 40 + 1) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                p += x % 1000 + 1;
                v.push(p);
            }
            assert_eq!(
                decode(&encode(&v), v.len())
                    .unwrap()
                    .into_iter()
                    .map(u64::from)
                    .collect::<Vec<_>>(),
                v
            );
        }
    }
}
