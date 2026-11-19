// SPDX-License-Identifier: GPL-3.0-or-later
fn main() {
    let mut bounds = html::ARTICLE_BOUNDS;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    for source_file in std::env::args().skip(1) {
        let s = std::fs::read_to_string(source_file).unwrap();
        for chunk in [1, 15, 16, 17, 256, 4096] {
            let (_, stats) =
                Html::parse_chunked(&s, &bounds, None, chunk, &mut html::Spent::default()).unwrap();
            println!("{chunk} {stats:?}");
        }
        assert!(matches!(
            Html::parse_document_bounded(&s, &bounds, Some(&cancelled)),
            Err(html::Refused::Cancelled)
        ));
        let ordinary = bounds;
        bounds.nodes = (1, usize::MAX);
        assert!(matches!(
            Html::parse_document_bounded(&s, &bounds, None),
            Err(html::Refused::TooComplex)
        ));
        bounds = ordinary;
        bounds.work = (1, 0);
        assert!(matches!(
            Html::parse_document_bounded(&s, &bounds, None),
            Err(html::Refused::TooComplex)
        ));
        bounds = ordinary;
    }
}
