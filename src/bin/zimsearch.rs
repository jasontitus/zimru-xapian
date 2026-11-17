// SPDX-License-Identifier: GPL-3.0-or-later
use clap::Parser;
use memmap2::Mmap;
use serde_json::json;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
#[cfg(not(unix))]
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

fn compressed_language(mut input: impl Read, blob_index: u32, output_limit: u64) -> Result<String> {
    let mut info = [0];
    input.read_exact(&mut info)?;
    let decoder: Box<dyn Read + '_> = match info[0] & 0x0f {
        4 => {
            let stream = xz2::stream::Stream::new_stream_decoder(MAX_XZ_DECODER_MEMORY, 0)?;
            Box::new(xz2::read::XzDecoder::new_stream(input, stream))
        }
        5 => {
            let mut decoder = zstd::stream::read::Decoder::new(input)?;
            decoder.window_log_max(ZSTD_WINDOW_LOG)?;
            Box::new(decoder)
        }
        other => return Err(format!("unsupported M/Language cluster compression {other}").into()),
    };
    // Feed zimru's uncompressed Cluster parser the original offset width.
    // Keep at most one extra decoded byte to distinguish exact fits from truncation.
    let mut raw = vec![(info[0] & 0x10) | 1];
    decoder
        .take(output_limit + 1)
        .read_to_end(&mut raw)
        .map_err(|error| format!("decoding M/Language cluster: {error}"))?;
    if raw.len() as u64 - 1 > output_limit {
        return Err(format!("decoded M/Language cluster exceeds {output_limit} bytes").into());
    }
    let cluster = zimru::cluster::Cluster::parse(&raw)?;
    let bytes = cluster.blob(blob_index)?;
    first_language(&bytes[..bytes.len().min(LANGUAGE_PREFIX_BYTES + 1)])
}

fn fallback_language(archive: &Archive, mut file: &File) -> Result<String> {
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
        file.seek(SeekFrom::Start(range.file_offset))?;
        file.read_exact(&mut prefix[..count])?;
        return first_language(&prefix[..count]);
    }
    let region = archive.cluster_byte_range(item.cluster_index())?;
    let size = region.end - region.start;
    if size > MAX_COMPRESSED_LANGUAGE_CLUSTER {
        return Err(format!(
            "compressed M/Language cluster exceeds {MAX_COMPRESSED_LANGUAGE_CLUSTER} bytes"
        )
        .into());
    }
    file.seek(SeekFrom::Start(region.start))?;
    compressed_language(file.take(size), item.blob_index(), MAX_LANGUAGE_CLUSTER)
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
    let index = Index::open(Box::new(IndexRange::new(
        file.try_clone()?,
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
        fallback_language(&archive, &file)?
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
    fn compressed_metadata_honors_output_cap_and_extended_offsets() {
        let mut payload = Vec::from(16u64.to_le_bytes());
        payload.extend_from_slice(&19u64.to_le_bytes());
        payload.extend_from_slice(b"eng");
        let xz = {
            let mut enc = xz2::write::XzEncoder::new(Vec::new(), 1);
            enc.write_all(&payload).unwrap();
            enc.finish().unwrap()
        };
        let zstd = zstd::stream::encode_all(payload.as_slice(), 1).unwrap();
        for (flag, encoded) in [(0x14u8, xz), (0x15, zstd)] {
            let mut raw = vec![flag];
            raw.extend_from_slice(&encoded);
            assert_eq!(compressed_language(raw.as_slice(), 0, 19).unwrap(), "eng");
            let error = compressed_language(raw.as_slice(), 0, 18).unwrap_err();
            assert!(error.to_string().contains("exceeds 18 bytes"));
        }
    }

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
