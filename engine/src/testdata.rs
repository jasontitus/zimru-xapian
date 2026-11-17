//! The real Kiwix archives the tests check against (`just testdata`), read
//! through zimfile's public API: only bytes and strings cross over.

use crate::glass::Db;
use std::path::PathBuf;
pub use zimfile::Archive;

fn open(rel: &str) -> Archive {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    Archive::open(&p).expect("test data missing: run `just testdata`")
}

pub fn climate() -> Archive {
    open("../zimfile/tests/data/nons/wikipedia_en_climate_change_mini_2024-06.zim")
}

/// Archives with libzim-built indexes: climate and zim-mcp's nollywood.
pub fn archives() -> Vec<Archive> {
    vec![
        climate(),
        open("../zim-mcp/tests/data/wikipedia_en_nollywood_maxi_2026-10.zim"),
    ]
}

/// The archive's embedded full-text or title index, opened from its bytes.
pub fn index(a: &Archive, fulltext: bool) -> Db {
    let path = if fulltext {
        "fulltext/xapian"
    } else {
        "title/xapian"
    };
    let e = a.entry_in('X', path).expect("no Xapian index");
    Db::open(Box::new(a.content(&e).unwrap())).unwrap()
}
