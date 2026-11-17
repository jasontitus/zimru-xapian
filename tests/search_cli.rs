// SPDX-License-Identifier: GPL-3.0-or-later
use std::io::Cursor;
use std::path::Path;
use std::process::{Command, Output};
use zimfile_xapian::IndexWriter;
use zimru::writer::{Creator, Item};

fn store(path: &Path, ns: u8, entry: &str, data: Vec<u8>) {
    let mut creator = Creator::new();
    creator.add_item(
        Item::in_namespace(ns, entry, "", "application/octet-stream+xapian", data)
            .with_compress(false),
    );
    creator.write_to(path).unwrap();
}

fn run(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zimsearch"))
        .args(args)
        .arg(path)
        .arg("tectonics")
        .output()
        .unwrap()
}

fn fulltext() -> Vec<u8> {
    let mut writer = IndexWriter::fulltext("eng");
    writer
        .add_html(
            "article",
            "Geology",
            "<p>Plate tectonics explains earthquakes</p>",
        )
        .unwrap();
    let mut bytes = Cursor::new(Vec::new());
    writer.write(&mut bytes, [0; 16]).unwrap();
    bytes.into_inner()
}

#[test]
fn fulltext_hits_use_full_paths_and_respect_limits() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("demo.zim");
    store(&path, b'X', "fulltext/xapian", fulltext());
    let result = run(&path, &["--limit", "1"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let hit: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(hit["path"], "C/article");
    assert_eq!(hit["percent"], 100);
    let empty = run(&path, &["--limit", "0"]);
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    let missing = run(&path, &["--suggest"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("opening title index"));
}

#[test]
fn supports_legacy_fulltext_entry_location_for_glass() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("legacy.zim");
    store(&path, b'Z', "/fulltextIndex/xapian", fulltext());
    let result = run(&path, &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()["path"],
        "C/article"
    );
}

#[test]
fn invalid_glass_and_wrong_index_kind_are_reported() {
    let temp = tempfile::tempdir().unwrap();
    let bad = temp.path().join("bad.zim");
    store(&bad, b'X', "fulltext/xapian", vec![0; 512]);
    let result = run(&bad, &[]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("not a Glass database"));
    let swapped = temp.path().join("swapped.zim");
    store(&swapped, b'X', "title/xapian", fulltext());
    let result = run(&swapped, &["--suggest"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("expected title index"));
}

#[test]
fn valid_index_language_takes_precedence_over_archive_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("language.zim");
    let mut creator = Creator::new();
    creator.add_item(Item::in_namespace(
        b'M',
        "Language",
        "",
        "text/plain",
        vec![255],
    ));
    creator.add_item(Item::in_namespace(
        b'X',
        "fulltext/xapian",
        "",
        "application/octet-stream+xapian",
        fulltext(),
    ));
    creator.write_to(&path).unwrap();
    let result = run(&path, &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()["path"],
        "C/article"
    );
}

fn archive_with_fallback(path: &Path, language: Vec<u8>, compression: zimru::cluster::Compression) {
    use zimru::cluster::Compression;
    let mut writer = IndexWriter::fulltext("");
    writer.set_stemming_language("eng");
    writer.add_text("article", "Article", "running").unwrap();
    let mut bytes = Cursor::new(Vec::new());
    writer.write(&mut bytes, [0; 16]).unwrap();

    let mut creator = Creator::new();
    creator.set_compression(compression);
    creator.add_item(
        Item::in_namespace(b'M', "Language", "", "text/plain", language)
            .with_compress(compression != Compression::None),
    );
    creator.add_item(
        Item::in_namespace(
            b'X',
            "fulltext/xapian",
            "",
            "application/octet-stream+xapian",
            bytes.into_inner(),
        )
        .with_compress(false),
    );
    creator.write_to(path).unwrap();

    let archive = zimru::Archive::open(path).unwrap();
    let item = archive
        .entry_by_ns_path(b'M', "Language")
        .unwrap()
        .get_item(false)
        .unwrap();
    assert_eq!(
        archive.cluster(item.cluster_index()).unwrap().compression(),
        compression
    );
}

#[test]
fn language_fallback_uses_first_field_for_direct_xz_and_zstd_clusters() {
    use zimru::cluster::Compression;
    let dir = tempfile::tempdir().unwrap();
    for compression in [Compression::None, Compression::Xz, Compression::Zstd] {
        let path = dir.path().join(format!("{compression:?}.zim"));
        let mut languages = b"eng,".to_vec();
        languages.extend_from_slice(&[0xff; 2048]);
        archive_with_fallback(&path, languages, compression);
        let result = Command::new(env!("CARGO_BIN_EXE_zimsearch"))
            .arg(&path)
            .arg("runs")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()["path"],
            "C/article"
        );
    }
}

#[test]
fn empty_query_and_zero_limit_skip_oversized_language_field() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("long.zim");
    archive_with_fallback(&path, vec![b'a'; 1025], zimru::cluster::Compression::Zstd);
    let query = |q: &str, n: &str| {
        Command::new(env!("CARGO_BIN_EXE_zimsearch"))
            .args(["--limit", n])
            .arg(&path)
            .arg(q)
            .output()
            .unwrap()
    };
    let failed = query("word", "1");
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("first M/Language field"));
    for output in [query("", "1"), query("  ", "1"), query("word", "0")] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }
}
