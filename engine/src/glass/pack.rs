//! Xapian's integer and string encodings (`common/pack.h`). Decoders take a
//! cursor (`&mut &[u8]`) and return `None` on bad or missing data.

/// `unpack_uint`: little-endian base-128, high bit = more bytes follow.
pub fn uint(p: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for (i, &b) in p.iter().enumerate() {
        let shift = 7 * i as u32;
        if shift >= 64 {
            return None;
        }
        v |= u64::from(b & 0x7f).checked_shl(shift)?;
        if b < 0x80 {
            *p = &p[i + 1..];
            return Some(v);
        }
    }
    None
}

pub fn put_uint(s: &mut Vec<u8>, mut v: u64) {
    while v >= 128 {
        s.push(v as u8 | 0x80);
        v >>= 7;
    }
    s.push(v as u8);
}

/// `unpack_string`: a `uint` length, then the bytes.
pub fn string<'a>(p: &mut &'a [u8]) -> Option<&'a [u8]> {
    let n = usize::try_from(uint(p)?).ok()?;
    let s = p.get(..n)?;
    *p = &p[n..];
    Some(s)
}

pub fn put_string(s: &mut Vec<u8>, v: &[u8]) {
    put_uint(s, v.len() as u64);
    s.extend_from_slice(v);
}

/// `unpack_bool`: `'0'` or `'1'`.
pub fn bool(p: &mut &[u8]) -> Option<bool> {
    let (&b, rest) = p.split_first()?;
    *p = rest;
    match b {
        b'0' => Some(false),
        b'1' => Some(true),
        _ => None,
    }
}

/// `unpack_uint_preserving_sort` (Glass): below 0x8000 two bytes big-endian,
/// else a unary length prefix in the first byte's high bits.
pub fn pus(p: &mut &[u8]) -> Option<u64> {
    let (&first, rest) = p.split_first()?;
    if first < 0x80 {
        let v = (u64::from(first) << 8) | u64::from(*rest.first()?);
        *p = &rest[1..];
        return Some(v);
    }
    if first == 0xff {
        return None;
    }
    let len = 2 + (first << 1).leading_ones() as usize; // bytes after the first
    if len > 8 {
        return None;
    }
    let mut v = u64::from(first & (0xff >> (len - 1)));
    for &b in rest.get(..len)? {
        v = (v << 8) | u64::from(b);
    }
    *p = &rest[len..];
    Some(v)
}

pub fn put_pus(s: &mut Vec<u8>, v: u64) {
    if v < 0x8000 {
        s.extend_from_slice(&(v as u16).to_be_bytes());
        return;
    }
    let len = (64 + 5 - v.leading_zeros() as usize) / 7; // total bytes
    let mask = (0xffu32 << (10 - len)) as u8;
    let bytes = v.to_be_bytes();
    s.push(bytes[8 - len] | mask);
    s.extend_from_slice(&bytes[9 - len..]);
}

/// `pack_string_preserving_sort`: `\0` escaped as `\0\xff`, then a `\0`
/// terminator unless `last`.
pub fn put_sort_string(s: &mut Vec<u8>, v: &[u8], last: bool) {
    for &b in v {
        s.push(b);
        if b == 0 {
            s.push(0xff);
        }
    }
    if !last {
        s.push(0);
    }
}

/// `unpack_string_preserving_sort`: up to an unescaped `\0` (consumed) or the end.
pub fn sort_string(p: &mut &[u8]) -> Vec<u8> {
    let mut out = vec![];
    let mut i = 0;
    while i < p.len() {
        let b = p[i];
        i += 1;
        if b == 0 {
            if p.get(i) != Some(&0xff) {
                break;
            }
            i += 1;
        }
        out.push(b);
    }
    *p = &p[i..];
    out
}

/// `pack_uint_last`: little-endian bytes, no terminator (the end of the key ends it).
pub fn put_uint_last(s: &mut Vec<u8>, mut v: u64) {
    while v != 0 {
        s.push(v as u8);
        v >>= 8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for v in [
            0u64,
            1,
            127,
            128,
            0x7fff,
            0x8000,
            0x3f_ffff,
            0x40_0000,
            1 << 40,
            u64::MAX >> 8,
        ] {
            let mut s = vec![];
            put_uint(&mut s, v);
            put_pus(&mut s, v);
            let mut p = &s[..];
            assert_eq!(uint(&mut p), Some(v));
            assert_eq!(pus(&mut p), Some(v), "{v:x}");
            assert!(p.is_empty());
        }
        // Sort order is preserved.
        let enc = |v| {
            let mut s = vec![];
            put_pus(&mut s, v);
            s
        };
        let vs = [0, 5, 0x7fff, 0x8000, 0x12345, 0x3f_ffff, 0x40_0000, 1 << 33];
        for w in vs.windows(2) {
            assert!(enc(w[0]) < enc(w[1]), "{:x} {:x}", w[0], w[1]);
        }
        let mut s = vec![];
        put_sort_string(&mut s, b"a\0b", false);
        assert_eq!(s, b"a\0\xffb\0");
        let mut p = &s[..];
        assert_eq!(sort_string(&mut p), b"a\0b");
        assert!(p.is_empty());
    }
}
