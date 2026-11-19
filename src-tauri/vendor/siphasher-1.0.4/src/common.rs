/// Compression round for SipHash algorithm.
macro_rules! compress {
    ($state:expr) => {{
        compress!($state.v0, $state.v1, $state.v2, $state.v3)
    }};
    ($v0:expr, $v1:expr, $v2:expr, $v3:expr) => {{
        $v0 = $v0.wrapping_add($v1);
        $v1 = $v1.rotate_left(13);
        $v1 ^= $v0;
        $v0 = $v0.rotate_left(32);
        $v2 = $v2.wrapping_add($v3);
        $v3 = $v3.rotate_left(16);
        $v3 ^= $v2;
        $v0 = $v0.wrapping_add($v3);
        $v3 = $v3.rotate_left(21);
        $v3 ^= $v0;
        $v2 = $v2.wrapping_add($v1);
        $v1 = $v1.rotate_left(17);
        $v1 ^= $v2;
        $v2 = $v2.rotate_left(32);
    }};
}

/// Loads an integer of the desired type from a byte stream, in LE order.
macro_rules! load_int_le {
    ($buf:expr, $i:expr, $int_ty:ident) => {{
        const SIZE: usize = ::core::mem::size_of::<$int_ty>();
        let bytes = &$buf[$i..][..SIZE];
        $int_ty::from_le_bytes(
            ::core::convert::TryInto::try_into(bytes).expect("exact-width SipHash word"),
        )
    }};
}

/// Loads a u64 using up to seven bytes of a byte slice.
#[inline]
pub(crate) fn u8to64_le(buf: &[u8], start: usize, len: usize) -> u64 {
    assert!(len < 8);
    let tail = &buf[start..][..len];
    let mut bytes = [0; 8];
    bytes[..len].copy_from_slice(tail);
    u64::from_le_bytes(bytes)
}

pub(crate) use compress;
pub(crate) use load_int_le;
