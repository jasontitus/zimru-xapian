//! Position lists: `uint last`, then (2+ positions) an interpolative-coded
//! bitstream, LSB first (`glass_positionlist.cc`, `common/bitstream.cc`).

use super::pack;

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

    fn interpolative(&mut self, pos: &mut [u64], mut j: usize, k: usize) -> Option<()> {
        while j + 1 < k {
            let mid = j + (k - j) / 2;
            let outof = (pos[k] + j as u64 + 1).checked_sub(pos[j] + k as u64)?;
            pos[mid] = self.decode(outof)? + pos[j] + (mid - j) as u64;
            self.interpolative(pos, j, mid)?;
            j = mid;
        }
        Some(())
    }
}

/// Decode a position list (`None` if it's corrupt).
pub fn decode(data: &[u8]) -> Option<Vec<u64>> {
    let mut p = data;
    let last = pack::uint(&mut p)?;
    if p.is_empty() {
        return Some(vec![last]);
    }
    let mut r = Reader {
        buf: p,
        idx: 0,
        acc: 0,
        n: 0,
    };
    let first = r.decode(last)?;
    let n = usize::try_from(r.decode(last - first)?).ok()? + 2;
    if n as u64 > last - first + 1 {
        return None;
    }
    let mut pos = vec![0; n];
    pos[0] = first;
    pos[n - 1] = last;
    r.interpolative(&mut pos, 0, n - 1)?;
    Some(pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(decode(&[0x04, 0x02]), Some(vec![2, 4])); // from a real index
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
            assert_eq!(decode(&encode(&c)).as_ref(), Some(&c));
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
            assert_eq!(decode(&encode(&v)), Some(v));
        }
    }
}
