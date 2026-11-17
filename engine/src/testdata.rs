// Modified for the standalone zimru-xapian tools on 2026-11-17. See PROVENANCE.md.
//! Real Kiwix archives used for the upstream conformance tests.
//! Run `python3 tests/real_archives.py` from the workspace root.

use crate::glass::Db;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

pub struct Archive {
    pub zim: zimru::Archive,
    path: PathBuf,
}

fn open(name: &str) -> Archive {
    let directory = std::env::var_os("ZIM_TESTDATA").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/real-archives"),
        PathBuf::from,
    );
    let path = directory.join(name);
    let zim = zimru::Archive::open(&path).expect("download fixtures with tests/real_archives.py");
    Archive { zim, path }
}

pub fn climate() -> Archive {
    open("wikipedia_en_climate_change_mini_2024-06.zim")
}

pub fn archives() -> Vec<Archive> {
    vec![climate(), open("wikipedia_en_nollywood_maxi_2026-10.zim")]
}

pub fn index(archive: &Archive, fulltext: bool) -> Db {
    let path = if fulltext {
        "fulltext/xapian"
    } else {
        "title/xapian"
    };
    let entry = archive
        .zim
        .entry_by_ns_path(b'X', path)
        .expect("no Xapian index");
    let item = entry.get_item(false).unwrap();
    let range = archive
        .zim
        .blob_direct_access(item.cluster_index(), item.blob_index())
        .unwrap();
    assert!(range.is_direct);
    let mut file = File::open(&archive.path).unwrap();
    file.seek(SeekFrom::Start(range.file_offset)).unwrap();
    let mut bytes = vec![0; usize::try_from(range.size).unwrap()];
    file.read_exact(&mut bytes).unwrap();
    Db::open(Box::new(bytes)).unwrap()
}
