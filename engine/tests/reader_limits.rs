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

fn three_titles(limits: ReaderLimits) -> Index {
    let mut writer = IndexWriter::title("eng");
    for (path, title, target) in [
        ("earth", "Earth", None),
        ("terra", "Earth alias", Some("earth")),
        ("mars", "Earth to Mars", None),
    ] {
        writer.add_title(path, title, target).unwrap();
    }
    let mut file = Cursor::new(Vec::new());
    writer.write(&mut file, [0; 16]).unwrap();
    Index::open_with_limits(Box::new(file.into_inner()), limits).unwrap()
}

#[test]
fn ranked_pages_keep_docdata_order_after_alias_collapse() {
    let index = three_titles(ReaderLimits::default());
    let all = index.suggest("earth", 10, "").unwrap();
    assert_eq!(all, ["C/earth", "C/mars"]);
    for offset in 0..=3 {
        let page = index
            .suggest_page("earth", offset, 1, "", &|| false)
            .unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(
            page.hits,
            all.iter().skip(offset).take(1).cloned().collect::<Vec<_>>()
        );
    }
    let count = index.suggest_page("earth", 0, 0, "", &|| false).unwrap();
    assert!(count.hits.is_empty());
    assert_eq!(count.total, 3);
    assert!(matches!(
        index.suggest_page("earth", usize::MAX, 2, "", &|| false),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn totals_only_and_small_pages_observe_independent_bounds() {
    let index = three_titles(ReaderLimits {
        max_ranked_hits: 1,
        ..Default::default()
    });
    assert!(index.suggest("earth", 1, "").is_ok());
    assert!(matches!(
        index.suggest("earth", 2, ""),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        index
            .suggest_page("earth", 0, 0, "", &|| false)
            .unwrap()
            .total,
        3
    );

    // Returning only the first title still requires retaining its sort key
    // and alias-collapse key for ranking; output has a separate limit.
    let tight = three_titles(ReaderLimits {
        max_result_bytes: 5,
        ..Default::default()
    });
    assert!(matches!(
        tight.suggest("earth", 1, ""),
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        tight
            .suggest_page("earth", 0, 0, "", &|| false)
            .unwrap()
            .total,
        3
    );
}

#[test]
fn fulltext_pages_preserve_score_and_return_total_without_storing_prior_paths() {
    // The long path is ranked first; seeking past it requires only its rank.
    let mut writer = IndexWriter::fulltext("eng");
    for path in ["this-path-is-intentionally-too-long", "b", "c"] {
        writer.add_text(path, "Earth", "planet earth").unwrap();
    }
    let mut file = Cursor::new(Vec::new());
    writer.write(&mut file, [0; 16]).unwrap();
    let bytes = file.into_inner();
    let all = Index::open(Box::new(bytes.clone()))
        .unwrap()
        .search("earth", 3, "")
        .unwrap();
    let index = Index::open_with_limits(
        Box::new(bytes),
        ReaderLimits {
            max_result_field_bytes: 7,
            max_result_bytes: 5,
            ..Default::default()
        },
    )
    .unwrap();
    let second = index
        .search_page("earth", 1, 1, "", false, &|| false)
        .unwrap();
    assert_eq!(second.total, 3);
    assert_eq!(second.hits[0].hit, all[1]);
    assert_eq!(second.hits[0].snippet, None);
    assert!(
        index
            .search_page("earth", 1, 2, "", false, &|| false)
            .is_err()
    );
    assert!(index.search("earth", 1, "").is_err());
    let count = index
        .search_page("earth", usize::MAX, 0, "", false, &|| false)
        .unwrap();
    assert_eq!(count.total, 3);
    assert!(count.hits.is_empty());
}

#[test]
fn broad_query_can_cancel_during_matching_and_retry_on_the_same_index() {
    let mut writer = IndexWriter::fulltext("eng");
    for i in 0..300 {
        writer
            .add_text(&format!("p{i}"), "Earth", "planet earth")
            .unwrap();
    }
    let mut file = Cursor::new(Vec::new());
    writer.write(&mut file, [0; 16]).unwrap();
    let index = Index::open(Box::new(file.into_inner())).unwrap();
    let checks = std::cell::Cell::new(0);
    let error = index
        .search_page("planet", 0, 5, "", false, &|| {
            checks.set(checks.get() + 1);
            checks.get() > 40
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "Search cancelled");
    assert_eq!(
        index
            .search_page("planet", 0, 5, "", false, &|| false)
            .unwrap()
            .total,
        300
    );
}
