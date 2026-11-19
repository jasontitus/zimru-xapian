// SPDX-License-Identifier: GPL-3.0-or-later
use dom_query::Document;
use html5ever::tendril::{ByteTendril, TendrilSink};
use html5ever::tokenizer::{
    BufferQueue, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use std::cell::RefCell;
use std::fmt::Write as FmtWrite;
use std::io::{BufWriter, Write};

#[derive(Default)]
struct Sink(RefCell<String>);
impl TokenSink for Sink {
    type Handle = ();
    fn process_token(&self, token: Token, line: u64) -> TokenSinkResult<()> {
        writeln!(self.0.borrow_mut(), "{line} {token:?}").unwrap();
        TokenSinkResult::Continue
    }
}
fn measure(content: &str, cuts: &[usize], id: usize, exact: bool, out: &mut impl Write) {
    let mut opts = html5ever::ParseOpts::default();
    opts.tokenizer.exact_errors = exact;
    let mut parser = html5ever::parse_document(Document::default(), opts).from_utf8();
    let tok = Tokenizer::new(
        Sink::default(),
        TokenizerOpts {
            exact_errors: exact,
            ..Default::default()
        },
    );
    let buffer = BufferQueue::default();
    let mut prev = 0;
    let mut prev_char = 0;
    for &end in cuts {
        parser.process(ByteTendril::from_slice(&content.as_bytes()[prev..end]));
        prev = end;
        // The string-level tokenizer only accepts whole UTF-8 code points.
        let mut char_end = end;
        while !content.is_char_boundary(char_end) {
            char_end -= 1;
        }
        buffer.push_back(content[prev_char..char_end].into());
        let _ = tok.feed(&buffer);
        prev_char = char_end;
    }
    assert_eq!(prev, content.len());
    tok.end();
    let doc = parser.finish();
    writeln!(
        out,
        "{id} {:?} {:?} {:?} {:?}",
        doc.html(),
        doc.errors.take(),
        doc.quirks_mode.get(),
        tok.sink.0.take()
    )
    .unwrap();
}
fn main() {
    let files: Vec<_> = std::env::args().skip(1).collect();
    let documents = [
        "",
        "\u{feff}",
        "\r\n\r\n\r\0\t\u{80}終",
        "A\nβ\r\n😅&amp;\r&#x1f63a;\0<kbd>éé\nB</kbd>",
        "<!DOCTYPE html PUBLIC \"article\"><!--page\r\n--><table>n<tr><td>é<li>一<li>二</table><b><i>tail</b></i>",
        "<noscript>a<b></noscript><template><textarea>&amp;\r\nテキスト</textarea></template><pre>\r\n①</pre>",
        "<svg viewBox=\"0 0 1 1\"><![CDATA[my\nfigure]]><foreignObject><p>a</svg><math><mi>β</mi></math>",
        "<title>&copy; 🗺️</title><style>p::before {content:'&';}</style><script type='application/json'>{\"label\":\"<&\"}</script><p title='x\n' title=y>a",
        "新".repeat(17).leak(),
        "a&am".repeat(21).leak(),
    ];
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut id = 0;
    for (data, all_splits) in documents.iter().map(|v| (v.to_string(), true)).chain(
        files
            .into_iter()
            .map(|path| (std::fs::read_to_string(path).unwrap(), false)),
    ) {
        for exact in [false, true] {
            let mut cases = vec![vec![0, 0, data.len(), data.len()]];
            for step in [1, 2, 3, 7, 15, 16, 17, 31, 64, 257, 4096] {
                let mut v: Vec<usize> = (0..data.len()).step_by(step).collect();
                v.extend([data.len(), data.len()]);
                cases.push(v);
            }
            if all_splits {
                for cut in 0..=data.len() {
                    cases.push(vec![0, cut, cut, data.len()]);
                }
            }
            for cuts in cases {
                measure(&data, &cuts, id, exact, &mut out);
                id += 1;
            }
        }
    }
    eprintln!("Compared {id} parse/token runs");
}
