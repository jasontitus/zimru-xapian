use std::io::{self, Cursor};
use zimfile_xapian::{Error, Index, IndexWriter, Result, Source};

/// Simulate a file that opened successfully but has since become unreadable.
struct HeaderOnly(Vec<u8>);

impl Source for HeaderOnly {
    fn read_at(&self, buf: &mut [u8], off: u64) -> Result<()> {
        if off == 0 {
            return self.0.read_at(buf, off);
        }
        Err(io::Error::other("test read failure").into())
    }
}

#[test]
fn read_failures_propagate_before_query_evaluation() {
    let mut writer = IndexWriter::title("eng");
    writer.add_title("A", "A", None).unwrap();
    let mut file = Cursor::new(Vec::new());
    writer.write(&mut file, [0; 16]).unwrap();
    let index = Index::open(Box::new(HeaderOnly(file.into_inner()))).unwrap();

    assert!(matches!(index.metadata("kind"), Err(Error::Io(_))));
    assert!(matches!(index.search("c++", 1, "eng"), Err(Error::Io(_))));
    assert!(matches!(index.suggest("c++", 1, "eng"), Err(Error::Io(_))));
}
