// SPDX-License-Identifier: GPL-3.0-or-later
#![forbid(unsafe_code)]
use std::{env, hint::black_box, time::Instant};
use tendril::StrTendril;

fn main() {
    let kind = env::args().nth(1).unwrap();
    let expected = ["a", "ñ", "界", "🦊", "abcdñ12", "01234567"];
    let mut values = Vec::with_capacity(65536);
    let parent: StrTendril = "0123456789abcdef".repeat(512).into();
    let started = Instant::now();
    match kind.as_str() {
        "compact" => {
            for i in 0..65536 {
                let value = StrTendril::from_slice(black_box(expected[i % expected.len()]));
                let mut copy = value.clone();
                copy.push_char('x');
                assert!(copy.ends_with('x'));
                assert_eq!(&*value, expected[i % expected.len()]);
                values.push(value);
            }
        }
        "shared" => {
            for i in 0..65536 {
                let offset = i % 128;
                let mut view = parent.subtendril(offset, 12);
                let base = parent.get(offset as usize..offset as usize + 12).unwrap();
                assert_eq!(view.as_ptr(), base.as_ptr());
                view.reserve(0);
                assert_eq!(view.as_ptr(), base.as_ptr());
                let mut changed = view.clone();
                changed.push_char('🦊');
                assert!(changed.ends_with('🦊'));
                assert_eq!(&*view, base);
                values.push(view);
            }
        }
        "sliding" => {
            let mut value = parent.clone();
            for i in 0..65536 {
                value.pop_front(16);
                value.push_slice("0123456789abcdef");
                assert_eq!(value.len32(), 8192);
                if i % 64 == 0 {
                    values.push(value.subtendril(32, 12));
                }
            }
            assert_eq!(value, parent);
        }
        _ => panic!("unknown workload"),
    }
    drop(parent);
    let checksum = values.iter().map(|v| v.len() as u64 + u64::from(v.as_bytes()[0])).sum::<u64>();
    let nanos = started.elapsed().as_nanos();
    println!("{} {} {} {}", std::mem::size_of::<StrTendril>(), black_box(values.len()), checksum, nanos);
}
