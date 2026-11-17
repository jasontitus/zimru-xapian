// SPDX-License-Identifier: GPL-3.0-or-later
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use zimfile_xapian::Index;

fn run(input: &Path, db: &Path, mode: &str, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xapianbuilder"))
        .arg(mode)
        .arg("--input")
        .arg(input)
        .arg("--output")
        .arg(db)
        .arg("--quiet")
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn malformed_late_record_and_existing_output_are_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("input");
    let db = tmp.path().join("output");
    fs::write(&input, "{\"path\":\"a\",\"title\":\"First\"}\n{\"path\":").unwrap();
    let failed = run(&input, &db, "title", &[]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("line 2"));
    assert!(!db.exists());

    fs::write(&db, b"previous index").unwrap();
    fs::write(&input, "{\"path\":\"a\",\"title\":\"First\"}\n").unwrap();
    assert!(!run(&input, &db, "title", &[]).status.success());
    assert_eq!(fs::read(db).unwrap(), b"previous index");
}

#[test]
fn skipped_documents_and_empty_database() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("input");
    let db = tmp.path().join("output");
    fs::write(&input, r#"{"path":"r","title":"Redirect","target_path":"a"}
{"path":"x","title":"Hidden","mimetype":"text/html","body":"<meta name='robots' content='noindex'><p>hidden</p>"}
"#).unwrap();
    assert!(
        run(&input, &db, "fulltext", &["--skip-if-empty"])
            .status
            .success()
    );
    assert!(!db.exists());
    assert!(run(&input, &db, "fulltext", &[]).status.success());
    let idx = Index::open(Box::new(fs::read(db).unwrap())).unwrap();
    assert!(idx.search("hidden", 10, "").unwrap().is_empty());
}

#[test]
fn reads_literal_text_and_resets_stemmer_between_records() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("input");
    let db = tmp.path().join("output");
    fs::write(
        &input,
        r#"{"path":"a","title":"A","body":"<strong>running dogs</strong>","language":"english"}
{"path":"b","title":"B","body":"running dogs"}

{"path":"bad\u0000path","title":"skip","body":"running"}
"#,
    )
    .unwrap();
    let result = run(&input, &db, "fulltext", &["--jobs", "1"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let idx = Index::open(Box::new(fs::read(db).unwrap())).unwrap();
    assert_eq!(idx.search("run", 10, "").unwrap()[0].path, "C/a");
    assert_eq!(idx.search("running", 10, "").unwrap()[0].path, "C/b");
    let hits = idx.search("strong", 10, "").unwrap();
    assert_eq!(hits.len(), 1, "literal text includes angle-bracketed words");
    assert_eq!(hits[0].word_count, Some(2));
}

#[test]
fn long_title_token_errors_before_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("input");
    let db = tmp.path().join("output");
    let valid = serde_json::json!({"path": "edge", "title": "a".repeat(240)});
    let invalid = serde_json::json!({"path": "lost", "title": "a".repeat(241)});
    fs::write(&input, format!("{valid}\n{invalid}\n")).unwrap();
    let result = run(&input, &db, "title", &[]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("line 2"));
    assert!(!db.exists());
}
