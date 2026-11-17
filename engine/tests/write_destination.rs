// SPDX-License-Identifier: GPL-3.0-or-later
use std::io::{self, Cursor, Seek, SeekFrom, Write};
use zimfile_xapian::{Error, IndexWriter};

#[test]
fn nonempty_destination_retains_its_data_and_position() {
    let original = vec![42; 64 * 1024];
    let mut sink = Cursor::new(original.clone());
    sink.set_position(17);
    let result = IndexWriter::title("eng").write(&mut sink, [0; 16]);
    assert!(matches!(result, Err(Error::Invalid(_))));
    assert_eq!(sink.position(), 17);
    assert_eq!(sink.into_inner(), original);
}

#[test]
fn io_failure_after_the_first_write_is_returned_to_the_caller() {
    struct FailingSink(Cursor<Vec<u8>>);

    impl Write for FailingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.0.get_ref().is_empty() {
                self.0.write(buf)
            } else {
                Err(io::Error::other("storage unavailable"))
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    impl Seek for FailingSink {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.0.seek(pos)
        }
    }

    let mut sink = FailingSink(Cursor::new(Vec::new()));
    let result = IndexWriter::title("eng").write(&mut sink, [0; 16]);
    assert!(matches!(result, Err(Error::Io(_))));
    assert!(!sink.0.get_ref().is_empty());
}
