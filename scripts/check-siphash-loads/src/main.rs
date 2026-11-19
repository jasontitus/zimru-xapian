// SPDX-License-Identifier: GPL-3.0-or-later
use checked::sip128::Hasher128 as _;
use original::sip128::Hasher128 as _;
use std::hash::Hasher;

fn main() {
    let data: Vec<_> = (0u32..288)
        .map(|i| (i.wrapping_mul(3_747_613_931).rotate_left(i % 31) >> 13) as u8)
        .collect();
    macro_rules! compare {
        ($module:ident, $kind:ident, $digest:expr, $finish:ident) => {
            for key in [0, 1, u64::MAX, 0xfedcba9876543210] {
                for start in 0..8 {
                    for len in 0..=272 {
                        let msg = &data[start..start + len];
                        let old = original::$module::$kind::new_with_keys(key, !key);
                        let new = checked::$module::$kind::new_with_keys(key, !key);
                        assert_eq!(($digest)(old.hash(msg)), ($digest)(new.hash(msg)));
                        for step in 1..=17 {
                            let (mut old, mut new) = (old, new);
                            for chunk in msg.chunks(step) {
                                old.write(&[]);
                                new.write(&[]);
                                old.write(chunk);
                                new.write(chunk);
                            }
                            assert_eq!(($digest)(old.$finish()), ($digest)(new.$finish()));
                        }
                    }
                }
            }
        };
    }
    compare!(sip, SipHasher13, u64::from, finish);
    compare!(sip, SipHasher24, u64::from, finish);
    compare!(sip128, SipHasher13, u128::from, finish128);
    compare!(sip128, SipHasher24, u128::from, finish128);
    println!("Compared 34,944 one-shot and 594,048 partitioned hash cases");
}
