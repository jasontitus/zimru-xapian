// SPDX-License-Identifier: GPL-3.0-or-later
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tendril::stream::Utf8LossyDecoder;
use tendril::{ByteTendril, StrTendril, TendrilSink, fmt};

struct Sink {
    text: String,
    errors: Vec<(usize, String)>,
}
impl TendrilSink<fmt::UTF8> for Sink {
    type Output = (String, Vec<(usize, String)>);
    fn process(&mut self, t: StrTendril) {
        self.text.push_str(&t);
    }
    fn error(&mut self, e: Cow<'static, str>) {
        self.errors.push((self.text.len(), e.into_owned()));
    }
    fn finish(self) -> Self::Output {
        (self.text, self.errors)
    }
}
fn decode(bytes: &[u8], cut: usize, h: &mut DefaultHasher) {
    let mut decoder = Utf8LossyDecoder::new(Sink {
        text: String::new(),
        errors: vec![],
    });
    for q in [&bytes[..cut], &[][..], &bytes[cut..]] {
        decoder.process(ByteTendril::from_slice(q));
    }
    let (s, e) = decoder.finish();
    assert_eq!(s, String::from_utf8_lossy(bytes));
    (s, e).hash(h);
}
fn main() {
    let mut h = DefaultHasher::new();
    let choices = [
        0, 0x41, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc0, 0xc2, 0xdf, 0xe0, 0xe1, 0xed,
        0xef, 0xf0, 0xf4, 0xf5, 0xff,
    ];
    for a in choices {
        for b in choices {
            for c in choices {
                for d in choices {
                    let bytes = [a, b, c, d];
                    for i in 0..=4 {
                        decode(&bytes, i, &mut h);
                    }
                }
            }
        }
    }
    println!("UTF-8 carry and errors {:016x}", h.finish());
    let mut xs: Vec<StrTendril> = (0..8)
        .map(|_| StrTendril::from_slice("📚α123456789é🦀Z"))
        .collect();
    let mut state = 1u64;
    fn rand(s: &mut u64) -> usize {
        *s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*s >> 32) as usize
    }
    for _ in 0..200_000 {
        let i = rand(&mut state) % xs.len();
        let j = rand(&mut state) % xs.len();
        let n = xs[i].len32();
        let a = (rand(&mut state) % (n as usize + 3)) as u32;
        let l = (rand(&mut state) % (n as usize + 3)) as u32;
        match rand(&mut state) % 10 {
            0 => {
                xs[i] = xs[j].clone();
            }
            1 => xs[i].push_slice("abcéß0123456789"),
            2 => {
                let x = xs[j].clone();
                xs[i].push_tendril(&x);
            }
            3 => match xs[i].try_subtendril(a, l) {
                Ok(t) => {
                    t.len32().hash(&mut h);
                    xs[j] = t;
                }
                Err(e) => format!("{:?}", e).hash(&mut h),
            },
            4 => format!("{:?}", xs[i].try_pop_front(a)).hash(&mut h),
            5 => format!("{:?}", xs[i].try_pop_back(a)).hash(&mut h),
            6 => xs[i].reserve(a),
            7 => xs[i].push_char('📝'),
            8 => {
                xs[i].pop_front_char().hash(&mut h);
            }
            _ => xs[i].clear(),
        }
        (&*xs[i]).as_bytes().hash(&mut h);
        if xs[i].len32() > 4096 {
            xs[i].clear();
        }
    }
    println!("Mutable UTF-8 ranges {:016x}", h.finish());
}
