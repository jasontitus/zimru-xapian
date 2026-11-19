// SPDX-License-Identifier: GPL-3.0-or-later
use dom_query::Document;
use html5ever::tendril::TendrilSink;
use std::{hint::black_box, time::Instant};
fn chunk_end(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    let end = 256.min(bytes.len());
    let mut end = (1..end)
        .find(|&i| {
            bytes[i] == b'<'
                && bytes
                    .get(i + 1)
                    .is_none_or(|c| b"abcefinstu".contains(&c.to_ascii_lowercase()))
        })
        .unwrap_or(end);
    while !rest.is_char_boundary(end) {
        end += 1;
    }
    end
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let source = std::fs::read_to_string(&args[1]).unwrap();
    let parse = || {
        let mut opts = html5ever::ParseOpts::default();
        opts.tokenizer.discard_bom = false;
        let mut parser = html5ever::parse_document(Document::default(), opts);
        let mut rest = source.strip_prefix('\u{feff}').unwrap_or(&source);
        while !rest.is_empty() {
            let (head, tail) = rest.split_at(chunk_end(rest));
            parser.process(head.into());
            rest = tail
        }
        parser.finish()
    };
    std::fs::write(&args[2], parse().html().as_bytes()).unwrap();
    for _ in 0..10 {
        black_box(parse());
    }
    let t = Instant::now();
    for _ in 0..200 {
        black_box(parse());
    }
    println!("{}", t.elapsed().as_nanos() / 200);
}
