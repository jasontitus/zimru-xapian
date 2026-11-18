//! Positioned reads on a single open file. Small metadata probes share a
//! bounded page cache; sequential cluster/checksum reads bypass that cache.
use std::fs::File;
use std::io::{self, Read};
use std::num::NonZeroUsize;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use lru::LruCache;
use parking_lot::Mutex;

use crate::error::{Error, Result};

const PAGE: usize = 4096;
const SHARDS: usize = 16;
const PINNED_PAGES: usize = 128;
const PAGES_PER_SHARD: usize = 8;
// Directory strings and the whole MIME declaration are variable-length. This
// ceiling applies to one metadata record, never to the archive or blob size.
pub(super) const DEFAULT_MAX_METADATA_RECORD: usize = 16 << 20;

#[derive(Clone, Copy)]
pub(super) enum Access {
    /// Allow a point lookup to populate fixed slots.
    Lookup,
    /// Use existing fixed pages, keeping empty slots available for later lookups.
    Scan,
}

#[derive(Debug)]
enum Source {
    File(File),
    Positioned(Arc<dyn super::PositionedInput>),
    #[cfg(feature = "legacy-mmap")]
    Mapped(memmap2::Mmap),
}

#[derive(Debug)]
pub(super) struct Storage {
    source: Source,
    len: u64,
    metadata_limit: usize,
    // Stable references allow common metadata pages to be read without taking an
    // LRU lock or updating a shared reference count. Collisions use the LRU.
    pinned: [OnceLock<(u64, Box<[u8]>)>; PINNED_PAGES],
    pages: [Mutex<LruCache<u64, Arc<[u8]>>>; SHARDS],
    #[cfg(not(any(unix, windows)))]
    seek_lock: Mutex<()>,
}

impl Storage {
    pub(super) fn file(file: File, metadata_limit: usize) -> io::Result<Self> {
        let len = file.metadata()?.len();
        Ok(Self::new(Source::File(file), len, metadata_limit))
    }

    pub(super) fn positioned(
        input: Arc<dyn super::PositionedInput>,
        len: u64,
        metadata_limit: usize,
    ) -> Self {
        Self::new(Source::Positioned(input), len, metadata_limit)
    }

    #[cfg(feature = "legacy-mmap")]
    pub(super) fn mapped(map: memmap2::Mmap) -> Self {
        let len = map.len() as u64;
        Self::new(Source::Mapped(map), len, DEFAULT_MAX_METADATA_RECORD)
    }

    fn new(source: Source, len: u64, metadata_limit: usize) -> Self {
        Self {
            source,
            len,
            metadata_limit,
            pinned: std::array::from_fn(|_| OnceLock::new()),
            pages: std::array::from_fn(|_| {
                Mutex::new(LruCache::new(NonZeroUsize::new(PAGES_PER_SHARD).unwrap()))
            }),
            #[cfg(not(any(unix, windows)))]
            seek_lock: Mutex::new(()),
        }
    }

    pub(super) fn len(&self) -> u64 {
        self.len
    }

    pub(super) fn range(&self, start: u64, length: u64) -> Result<Range<u64>> {
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.len)
            .ok_or(Error::Truncated(start))?;
        Ok(start..end)
    }

    fn read_exact(&self, at: u64, out: &mut [u8]) -> io::Result<()> {
        match &self.source {
            Source::Positioned(input) => input.read_exact_at(at, out),
            Source::File(file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileExt;
                    file.read_exact_at(out, at)
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::FileExt;
                    let mut pos = at;
                    let mut rest = out;
                    while !rest.is_empty() {
                        match file.seek_read(rest, pos) {
                            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                            Ok(n) => {
                                rest = &mut rest[n..];
                                pos += n as u64;
                            }
                            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                            Err(err) => return Err(err),
                        }
                    }
                    Ok(())
                }
                #[cfg(not(any(unix, windows)))]
                {
                    use std::io::{Seek, SeekFrom};
                    let _guard = self.seek_lock.lock();
                    let mut fd = file;
                    fd.seek(SeekFrom::Start(at))?;
                    fd.read_exact(out)
                }
            }
            #[cfg(feature = "legacy-mmap")]
            Source::Mapped(map) => {
                let pos = usize::try_from(at).map_err(|_| io::ErrorKind::UnexpectedEof)?;
                let end = pos
                    .checked_add(out.len())
                    .ok_or(io::ErrorKind::UnexpectedEof)?;
                out.copy_from_slice(map.get(pos..end).ok_or(io::ErrorKind::UnexpectedEof)?);
                Ok(())
            }
        }
    }

    fn read_page(&self, number: u64) -> Result<Box<[u8]>> {
        let start = number * PAGE as u64;
        let len = (self.len - start).min(PAGE as u64) as usize;
        let mut bytes = vec![0; len].into_boxed_slice();
        self.read_exact(start, &mut bytes)?;
        Ok(bytes)
    }

    fn pinned_page(&self, number: u64, access: Access) -> Result<Option<&[u8]>> {
        let slot = &self.pinned[number as usize % PINNED_PAGES];
        if matches!(access, Access::Lookup) && slot.get().is_none() {
            let page = self.read_page(number)?;
            // Another reader may have filled the slot while this one did I/O.
            // Only complete pages are published. An I/O error can be retried.
            let _ = slot.set((number, page));
        }
        Ok(slot
            .get()
            .and_then(|(stored, bytes)| (*stored == number).then_some(bytes.as_ref())))
    }

    fn recent_page(&self, number: u64) -> Result<Arc<[u8]>> {
        let shard = &self.pages[number as usize % SHARDS];
        if let Some(page) = shard.lock().get(&number).cloned() {
            return Ok(page);
        }
        let page: Arc<[u8]> = self.read_page(number)?.into();
        shard.lock().put(number, page.clone());
        Ok(page)
    }

    /// Try the page containing the metadata record first. Records crossing a
    /// page boundary are retried in successively larger bounded buffers.
    pub(super) fn parse<T>(&self, at: u64, parse: impl Fn(&[u8]) -> Result<T>) -> Result<T> {
        self.parse_with(at, Access::Lookup, parse)
    }

    pub(super) fn parse_with<T>(
        &self,
        at: u64,
        access: Access,
        parse: impl Fn(&[u8]) -> Result<T>,
    ) -> Result<T> {
        self.range(at, 1)?;
        let max = (self.len - at).min(self.metadata_limit as u64) as usize;
        let parse_prefix = |bytes: &[u8]| match parse(bytes) {
            Err(Error::Truncated(_)) if bytes.len() == max && self.len - at > max as u64 => {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "ZIM metadata record exceeds configured byte limit",
                )
                .into())
            }
            Err(Error::Truncated(relative)) => Err(Error::Truncated(at.saturating_add(relative))),
            Err(Error::BadUtf8(relative)) => Err(Error::BadUtf8(at.saturating_add(relative))),
            result => result,
        };
        #[cfg(feature = "legacy-mmap")]
        if let Source::Mapped(map) = &self.source {
            let pos = at as usize;
            return parse_prefix(&map[pos..pos + max]);
        }
        let number = at / PAGE as u64;
        let recent;
        let page = match self.pinned_page(number, access)? {
            Some(bytes) => bytes,
            None => {
                recent = self.recent_page(number)?;
                &recent
            }
        };
        let tail = &page[at as usize % PAGE..];
        let tail = &tail[..tail.len().min(max)];
        match parse_prefix(tail) {
            Err(Error::Truncated(_)) if tail.len() < max => {}
            result => return result,
        }
        let mut size = (2 * PAGE).min(max);
        let mut buf = tail.to_vec();
        loop {
            let have = buf.len();
            buf.resize(size, 0);
            self.read_exact(at + have as u64, &mut buf[have..])?;
            match parse_prefix(&buf) {
                Err(Error::Truncated(_)) if size < max => size = size.saturating_mul(2).min(max),
                result => return result,
            }
        }
    }

    pub(super) fn reader(&self, range: Range<u64>) -> StorageReader<'_> {
        StorageReader {
            storage: self,
            remaining: range,
        }
    }

    pub(super) fn sequential(&self) {
        match &self.source {
            Source::Positioned(_) => {}
            Source::File(file) => crate::io_hints::hint_sequential(file),
            #[cfg(all(feature = "legacy-mmap", unix))]
            Source::Mapped(map) => {
                let _ = map.advise(memmap2::Advice::Sequential);
            }
            #[cfg(all(feature = "legacy-mmap", not(unix)))]
            Source::Mapped(_) => {}
        }
    }
}

pub(super) struct StorageReader<'a> {
    storage: &'a Storage,
    remaining: Range<u64>,
}

impl Read for StorageReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let len = (out.len() as u64).min(self.remaining.end - self.remaining.start) as usize;
        if len != 0 {
            self.storage
                .read_exact(self.remaining.start, &mut out[..len])?;
            self.remaining.start += len as u64;
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::super::TableCache;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    #[test]
    fn colliding_pages_keep_the_original_bytes_and_the_last_short_page() {
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom, Write};

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("zimru-pages-{}-{stamp}", std::process::id()));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let stride = (PINNED_PAGES * PAGE) as u64;
        for i in 0..12u8 {
            file.seek(SeekFrom::Start(u64::from(i) * stride)).unwrap();
            file.write_all(&[i]).unwrap();
        }
        let storage = Storage::file(file, DEFAULT_MAX_METADATA_RECORD).unwrap();
        // A full scan may fill the rotating cache before any point query runs.
        for i in 0..12u8 {
            let offset = u64::from(i) * stride;
            assert_eq!(
                storage
                    .parse_with(offset, Access::Scan, |buf| crate::raw::u8_at(buf, 0))
                    .unwrap(),
                i
            );
        }
        assert!(storage.pinned.iter().all(|slot| slot.get().is_none()));
        for i in (0..12u8).chain((0..12).rev()) {
            assert_eq!(
                storage
                    .parse(u64::from(i) * stride, |buf| crate::raw::u8_at(buf, 0))
                    .unwrap(),
                i
            );
        }
        drop(storage);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn small_metadata_reads_report_physical_offsets() {
        let storage =
            Storage::file(File::open(file!()).unwrap(), DEFAULT_MAX_METADATA_RECORD).unwrap();
        let last = storage.len() - 1;
        let result = storage.parse(last, |buf| crate::raw::u64_at(buf, 0));
        assert!(matches!(result, Err(Error::Truncated(off)) if off == last));
    }

    #[test]
    fn concurrent_table_first_reads_publish_one_value_and_retry_after_error() {
        let cache = TableCache::default();
        let starts = AtomicUsize::new(0);
        let gate = Barrier::new(12);
        assert!(cache.get_or_try_init(|| Err(Error::EntryNotFound)).is_err());
        std::thread::scope(|scope| {
            for _ in 0..12 {
                scope.spawn(|| {
                    gate.wait();
                    assert_eq!(
                        *cache
                            .get_or_try_init(|| {
                                starts.fetch_add(1, Ordering::Relaxed);
                                Ok(17)
                            })
                            .unwrap(),
                        17
                    );
                });
            }
        });
        assert_eq!(starts.load(Ordering::Relaxed), 1);
    }
}
