// SPDX-License-Identifier: GPL-3.0-or-later
use clap::Parser;
use memmap2::Mmap;
use serde_json::json;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use zimfile_xapian::{Index, Source};
use zimru::Archive;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(
    version,
    about = "Search a ZIM archive's embedded Glass index. Prints one JSON object per hit."
)]
struct Args {
    /// Query the title suggestion index
    #[arg(long)]
    suggest: bool,
    /// Maximum results to print
    #[arg(long, default_value_t = 10)]
    limit: usize,
    archive: PathBuf,
    query: String,
}

// File and archive mapping come from one open handle. Offsets exposed to
// the database are relative to the item, even when another blob follows it.
struct IndexRange {
    file: Mutex<File>,
    base: u64,
    length: u64,
}

impl Source for IndexRange {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> zimfile_xapian::Result<()> {
        let length = u64::try_from(buf.len()).map_err(|_| io::Error::other("read too large"))?;
        let pos = offset
            .checked_add(length)
            .filter(|&end| end <= self.length)
            .and_then(|_| self.base.checked_add(offset))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "read outside the embedded index",
                )
            })?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("index file lock poisoned"))?;
        file.seek(SeekFrom::Start(pos))?;
        file.read_exact(buf)?;
        Ok(())
    }
}

fn run(args: Args) -> Result<()> {
    let file = File::open(&args.archive)?;
    // Like zimru::Archive::open, this requires the file to remain unmodified
    // while mapped. Keep the original handle for positional index reads.
    let archive = Archive::from_mmap(unsafe { Mmap::map(&file)? })?;
    let (name, kind) = if args.suggest {
        ("title/xapian", "title")
    } else {
        ("fulltext/xapian", "fulltext")
    };
    let entry = match archive.entry_by_ns_path(b'X', name) {
        Err(zimru::Error::EntryNotFound) if !args.suggest => {
            archive.entry_by_ns_path(b'Z', "/fulltextIndex/xapian")
        }
        result => result,
    }
    .map_err(|error| {
        format!(
            "opening {kind} index in {}: {error}",
            args.archive.display()
        )
    })?;
    let item = entry.get_item(false)?;
    let range = archive.blob_direct_access(item.cluster_index(), item.blob_index())?;
    if !range.is_direct {
        return Err(format!(
            "{kind} index is in a compressed cluster; a directly accessible index is required"
        )
        .into());
    }
    let index = Index::open(Box::new(IndexRange {
        file: Mutex::new(file),
        base: range.file_offset,
        length: range.size,
    }))?;
    if let Some(declared) = index.metadata("kind")?
        && !declared.is_empty()
        && declared != kind.as_bytes()
    {
        return Err(format!(
            "expected {kind} index, found {:?}",
            String::from_utf8_lossy(&declared)
        )
        .into());
    }
    let language = if index
        .metadata("language")?
        .is_none_or(|value| value.is_empty())
    {
        match archive.get_metadata("Language") {
            Ok(bytes) => String::from_utf8(bytes)?,
            Err(zimru::Error::EntryNotFound) => String::new(),
            Err(error) => return Err(error.into()),
        }
    } else {
        String::new()
    };
    let mut out = io::BufWriter::new(io::stdout().lock());
    if args.limit > 0 {
        if args.suggest {
            for path in index.suggest(&args.query, args.limit, language.trim())? {
                writeln!(out, "{}", json!({ "path": path }))?;
            }
        } else {
            for hit in index.search(&args.query, args.limit, language.trim())? {
                writeln!(
                    out,
                    "{}",
                    json!({
                        "path": hit.path, "score": hit.score, "percent": hit.percent, "word_count": hit.word_count
                    })
                )?;
            }
        }
    }
    out.flush()?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e)
            if e.downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) =>
        {
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("zimsearch: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_reads_stay_within_the_blob() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"xxABCDyy").unwrap();
        let source = IndexRange {
            file: Mutex::new(file),
            base: 2,
            length: 4,
        };
        let mut buf = [0; 2];
        source.read_at(&mut buf, 2).unwrap();
        assert_eq!(&buf, b"CD");
        assert!(source.read_at(&mut buf, 3).is_err());
        assert!(source.read_at(&mut buf, u64::MAX).is_err());
    }
}
