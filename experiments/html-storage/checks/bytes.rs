// SPDX-License-Identifier: GPL-3.0-or-later
use std::{
    borrow::Cow,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};
use tendril::{ByteTendril, StrTendril, TendrilSink, fmt, stream::Utf8LossyDecoder};
#[derive(Default)]
struct Capture {
    out: String,
    errors: Vec<(usize, String)>,
}
impl TendrilSink<fmt::UTF8> for Capture {
    type Output = Self;
    fn process(&mut self, v: StrTendril) {
        self.out.push_str(&v)
    }
    fn error(&mut self, v: Cow<'static, str>) {
        self.errors.push((self.out.len(), v.into_owned()));
    }
    fn finish(self) -> Self {
        self
    }
}
fn check(bytes: &[u8], cuts: &[usize], h: &mut DefaultHasher) {
    let mut decoder = Utf8LossyDecoder::new(Capture::default());
    let mut at = 0;
    for &end in cuts {
        decoder.process(ByteTendril::from_slice(&bytes[at..end]));
        at = end;
    }
    decoder.process(ByteTendril::from_slice(&bytes[at..]));
    let v = decoder.finish();
    assert_eq!(v.out, String::from_utf8_lossy(bytes));
    v.out.hash(h);
    v.errors.hash(h);
}
fn main() {
    let mut h = DefaultHasher::new();
    let mut cases = 0;
    for a in 0..=255 {
        for b in 0..=255 {
            for split in [0, 1, 2] {
                check(&[a, b], &[split, split], &mut h);
                cases += 1;
            }
        }
    }
    let alphabet = [
        0, 0x41, 0x7f, 0x80, 0x8f, 0x90, 0x9f, 0xa0, 0xbf, 0xc1, 0xc2, 0xdf, 0xe0, 0xed, 0xef,
        0xf0, 0xf4, 0xf5, 0xff,
    ];
    for a in alphabet {
        for b in alphabet {
            for c in alphabet {
                for d in alphabet {
                    let x = [a, b, c, d];
                    for cuts in [vec![], vec![1, 2, 3], vec![2, 2]] {
                        check(&x, &cuts, &mut h);
                        cases += 1;
                    }
                }
            }
        }
    }
    for x in [
        b"<p>bad \xff &amp; \xe2\x82\xac</p>".as_slice(),
        b"\xf0\x9f\x98\x80\xf0\x80\xf0\x90\x80",
        b"\xc2",
        b"",
    ] {
        for size in 1..8 {
            let mut p =
                html5ever::parse_document(dom_query::Document::default(), Default::default())
                    .from_utf8();
            for chunk in x.chunks(size) {
                p.process(ByteTendril::from_slice(chunk));
            }
            let dom = p.finish();
            dom.html().to_string().hash(&mut h);
            dom.errors.take().hash(&mut h);
            cases += 1;
        }
    }
    println!("{} {:016x}", cases, h.finish());
}
