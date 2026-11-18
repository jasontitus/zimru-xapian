// SPDX-License-Identifier: GPL-3.0-or-later
#![forbid(unsafe_code)]

use clap::Parser;

use serde_json::json;
use std::fs::File;
use std::io::{self, Write};
#[cfg(not(unix))]
use std::io::{Read, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
#[cfg(not(unix))]
use std::sync::Mutex;
use zimfile_xapian::{Index, Source};
use zimru::{Archive, ClusterReadLimits};

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

// The reader and archive share one open file identity. Offsets exposed to
// the database are relative to the item, even when another blob follows it.
struct IndexRange {
    #[cfg(unix)]
    file: File,
    #[cfg(not(unix))]
    file: Mutex<File>,
    base: u64,
    length: u64,
}

impl IndexRange {
    fn new(file: File, base: u64, length: u64) -> Self {
        #[cfg(not(unix))]
        let file = Mutex::new(file);
        Self { file, base, length }
    }
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
        #[cfg(unix)]
        self.file.read_exact_at(buf, pos)?;
        #[cfg(not(unix))]
        {
            let mut file = self
                .file
                .lock()
                .map_err(|_| io::Error::other("index file lock poisoned"))?;
            file.seek(SeekFrom::Start(pos))?;
            file.read_exact(buf)?;
        }
        Ok(())
    }
}

// The engine only needs the first language tag. Real lists can contain many
// languages. Bound the first field, leaving the rest unread on direct items.
const LANGUAGE_PREFIX_BYTES: usize = 1024;

fn first_language(bytes: &[u8]) -> Result<String> {
    let first = bytes.split(|&b| b == b',').next().unwrap_or_default();
    if first.len() > LANGUAGE_PREFIX_BYTES {
        return Err("first M/Language field exceeds 1024 bytes".into());
    }
    Ok(std::str::from_utf8(first)?.trim().to_owned())
}

// The fallback is needed only for an index without a language of its own.
const MAX_LANGUAGE_CLUSTER: u64 = 64 << 20;
const MAX_COMPRESSED_LANGUAGE_CLUSTER: u64 = 65 << 20;
const MAX_XZ_DECODER_MEMORY: u64 = 128 << 20;
const ZSTD_WINDOW_LOG: u32 = 26; // 64 MiB

fn fallback_language(archive: &Archive) -> Result<String> {
    let entry = match archive.entry_by_ns_path(b'M', "Language") {
        Ok(entry) => entry,
        Err(zimru::Error::EntryNotFound) => return Ok(String::new()),
        Err(error) => return Err(error.into()),
    };
    let item = entry.get_item(true)?;
    let range = archive.blob_direct_access(item.cluster_index(), item.blob_index())?;
    if range.is_direct {
        let count = range.size.min((LANGUAGE_PREFIX_BYTES + 1) as u64) as usize;
        let mut prefix = [0; LANGUAGE_PREFIX_BYTES + 1];
        if archive.read_at(range.file_offset, &mut prefix[..count])? != count {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        return first_language(&prefix[..count]);
    }
    let limits = ClusterReadLimits {
        max_stored_bytes: MAX_COMPRESSED_LANGUAGE_CLUSTER,
        max_decoded_bytes: MAX_LANGUAGE_CLUSTER,
        xz_memory_bytes: MAX_XZ_DECODER_MEMORY,
        zstd_window_log: ZSTD_WINDOW_LOG,
    };
    let cluster = archive
        .cluster_with_limits(item.cluster_index(), limits)
        .map_err(|e| format!("decoding M/Language cluster: {e}"))?;
    let bytes = cluster.blob(item.blob_index())?;
    first_language(&bytes[..bytes.len().min(LANGUAGE_PREFIX_BYTES + 1)])
}

fn run(args: Args) -> Result<()> {
    let file = File::open(&args.archive)?;
    let archive = Archive::from_file(file.try_clone()?)?;
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
    let index = Index::open(Box::new(IndexRange::new(
        file,
        range.file_offset,
        range.size,
    )))?;
    if let Some(declared) = index.metadata("kind")?
        && !declared.is_empty()
        && declared != kind.as_bytes()
    {
        return Err(format!(
            "expected {kind} index, found {:?}{}",
            String::from_utf8_lossy(&declared[..declared.len().min(64)]),
            if declared.len() > 64 { "…" } else { "" }
        )
        .into());
    }
    if args.limit == 0 || args.query.trim().is_empty() {
        return Ok(());
    }
    let language = if index
        .metadata("language")?
        .is_none_or(|value| value.is_empty())
    {
        fallback_language(&archive)?
    } else {
        String::new()
    };
    let mut out = io::BufWriter::new(io::stdout().lock());
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
        let source = IndexRange::new(file, 2, 4);
        let mut buf = [0; 2];
        source.read_at(&mut buf, 2).unwrap();
        assert_eq!(&buf, b"CD");
        assert!(source.read_at(&mut buf, 3).is_err());
        assert!(source.read_at(&mut buf, u64::MAX).is_err());
    }
}
