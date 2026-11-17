use std::io::Cursor;
use zimfile_xapian::{Error, Index, IndexWriter, ReaderLimits};

#[test]
fn multi_component_metadata_honors_the_configured_tag_limit() {
    let mut writer = IndexWriter::title("en");
    writer.add_title("outline", "Outline", None).unwrap();
    let mut file = Cursor::new(Vec::new());
    writer.write(&mut file, [0; 16]).unwrap();
    let bytes = file.into_inner();
    let normal = Index::open(Box::new(bytes.clone())).unwrap();
    let words = normal.metadata("stopwords").unwrap().unwrap();
    assert!(words.len() > 4096);

    let open = |limit| {
        Index::open_with_limits(
            Box::new(bytes.clone()),
            ReaderLimits {
                max_tag_bytes: limit,
                ..ReaderLimits::default()
            },
        )
        .unwrap()
    };
    assert_eq!(
        open(words.len()).metadata("stopwords").unwrap().unwrap(),
        words
    );
    assert!(matches!(
        open(words.len() - 1).metadata("stopwords"),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        open(5).metadata("kind").unwrap().as_deref(),
        Some(b"title".as_slice())
    );
}

#[test]
fn fulltext_query_enforces_the_index_stoplist_limit() {
    // The bundled Zulu list is small and exercises real index metadata.
    let mut writer = IndexWriter::fulltext("zu");
    writer.add_text("corals", "Corals", "coral reef").unwrap();
    let mut file = Cursor::new(Vec::new());
    writer.write(&mut file, [0; 16]).unwrap();
    let bytes = file.into_inner();
    let open = |limit| {
        Index::open_with_limits(
            Box::new(bytes.clone()),
            ReaderLimits {
                max_stopwords: limit,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let list = open(0).metadata("stopwords").unwrap().unwrap();
    let distinct = std::str::from_utf8(&list)
        .unwrap()
        .lines()
        .filter(|s| !s.is_empty())
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert_eq!(
        open(distinct).search("coral", 1, "").unwrap()[0].path,
        "C/corals"
    );
    assert!(matches!(
        open(distinct - 1).search("coral", 1, ""),
        Err(Error::Invalid(_))
    ));
}
