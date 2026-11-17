// SPDX-License-Identifier: GPL-3.0-or-later
use std::io::Cursor;
use zimfile_xapian::{Index, IndexWriter};

fn open(writer: IndexWriter) -> Index {
    let mut bytes = Cursor::new(Vec::new());
    writer.write(&mut bytes, [0; 16]).unwrap();
    Index::open(Box::new(bytes.into_inner())).unwrap()
}

#[test]
fn ranked_limits_preserve_weights_percentages_and_docid_ties() {
    let mut writer = IndexWriter::fulltext("eng");
    for (path, body) in [
        ("first", "rock salt"),
        ("second", "rock salt"),
        ("skipped", "rock paper"),
        ("best", "rock salt rock salt"),
        ("absent", "paper"),
    ] {
        writer.add_text(path, "", body).unwrap();
    }
    let index = open(writer);
    let all = index.search("rock salt", usize::MAX, "eng").unwrap();
    assert_eq!(
        all.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
        ["C/best", "C/first", "C/second"]
    );
    for n in 0..=all.len() {
        assert_eq!(index.search("rock salt", n, "eng").unwrap(), all[..n]);
    }
}

#[test]
fn collapse_keeps_the_best_alias_even_after_its_first_hit_was_evicted() {
    let mut writer = IndexWriter::title("eng");
    for (path, title, target) in [
        ("zulu", "Token Zulu", "shared"),
        ("charlie", "Token Charlie", "charlie"),
        ("bravo", "Token Bravo", "bravo"),
        ("alpha", "Token Alpha", "shared"),
        ("alpha-copy", "Token Alpha", "shared"),
    ] {
        writer.add_title(path, title, Some(target)).unwrap();
    }
    let index = open(writer);
    let all = index.suggest("token", usize::MAX, "eng").unwrap();
    assert_eq!(all, ["C/alpha", "C/bravo", "C/charlie"]);
    for n in 0..=all.len() {
        assert_eq!(index.suggest("token", n, "eng").unwrap(), all[..n]);
    }
}

#[test]
fn query_budgets_report_excess_work_without_truncating_results() {
    use zimfile_xapian::ReaderLimits;
    let mut writer = IndexWriter::title("");
    for term in ["?", "??", "???", "apple", "appleton", "applewood"] {
        writer.add_title(term, term, None).unwrap();
    }
    let mut output = Cursor::new(Vec::new());
    writer.write(&mut output, [0; 16]).unwrap();
    let bytes = output.into_inner();
    let with = |limits| Index::open_with_limits(Box::new(bytes.clone()), limits).unwrap();
    let small = with(ReaderLimits {
        max_query_terms: 2,
        ..Default::default()
    });
    for prefix in ["?", "app"] {
        let err = small.suggest(prefix, 1, "").unwrap_err();
        assert!(err.to_string().contains("term limit"), "{err}");
    }
    let small = with(ReaderLimits {
        max_query_bytes: 3,
        ..Default::default()
    });
    for err in [
        small.suggest("apple", 1, "").unwrap_err(),
        small.search("apple", 1, "").unwrap_err(),
    ] {
        assert!(err.to_string().contains("byte limit"), "{err}");
    }
    assert_eq!(
        with(Default::default()).suggest("?", 3, "").unwrap().len(),
        3
    );
    assert!(small.suggest("apple", 0, "").unwrap().is_empty());
}
