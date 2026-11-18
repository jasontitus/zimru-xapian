//! High-level [`Archive`] reader.
//!
//! The API mirrors libzim's user-facing surface (Archive / Entry / Item / Blob)
//! using snake_case as is idiomatic in Rust. Methods are named to match the
//! C++ class methods one-for-one so swapping out a Rust libzim binding for
//! `zimru` should mostly require only a `use` change.

mod storage;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

use lru::LruCache;
#[cfg(feature = "legacy-mmap")]
use memmap2::Mmap;
use rayon::prelude::*;

use crate::cluster::{Cluster, RawOffsets};
use crate::dirent::{ArticleEntry, DirectoryRecord, Dirent};
use crate::error::{Error, Result};
use crate::header::{Header, NO_MAIN_PAGE};
use crate::mime::MimeList;
use crate::raw;
use storage::{Access, Storage};

/// A bounded, random-access byte source. Each read must fill the entire
/// destination from the given logical offset or report an I/O error. Implementors
/// must permit concurrent reads without sharing a seek cursor. As with a file,
/// keep the source bytes stable while the archive retains metadata pages.
pub trait PositionedInput: std::fmt::Debug + Send + Sync {
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()>;
}

/// Maximum redirect chain depth before bailing out.
const MAX_REDIRECTS: usize = 16;

/// Default content namespace for "new namespace" archives.
pub const NS_CONTENT_NEW: u8 = b'C';
/// Metadata namespace (new-style archives).
pub const NS_METADATA: u8 = b'M';
/// Well-known namespace (new-style archives).
pub const NS_WELLKNOWN: u8 = b'W';
/// Index namespace (search indexes etc.).
pub const NS_INDEX: u8 = b'X';
/// Articles namespace (legacy archives).
pub const NS_ARTICLES_LEGACY: u8 = b'A';

/// Default cluster-cache byte budget (sum of decompressed payload bytes
/// held resident at most). Sized for phone-class hosts: 64 MB is enough
/// to keep ~30 typical 2 MB clusters cached for hot-article reuse, but
/// won't dominate a 4 GB-RAM device. Override per-archive with
/// [`Archive::set_cluster_cache_max_bytes`] — bulk-iteration tools that
/// pass over the whole archive once can size up; memory-constrained
/// embedded callers can size down.
pub const DEFAULT_CLUSTER_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Byte-budget LRU cache for decompressed clusters. Wraps `LruCache`
/// (count-based) and adds eviction-by-bytes on every insert: clusters
/// are tracked by their decompressed payload length, and the LRU tail
/// is evicted until the resident-byte total is under budget. Keeps the
/// most-recently-used cluster pinned even if it exceeds the budget on
/// its own (otherwise a single oversized cluster would lock the cache
/// into a permanent miss).
#[derive(Debug)]
struct ClusterByteCache {
    inner: LruCache<u32, Arc<Cluster>>,
    loading: HashMap<u32, Weak<ClusterLoad>>,
    max_bytes: usize,
    current_bytes: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl ClusterByteCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            inner: LruCache::unbounded(),
            loading: HashMap::new(),
            max_bytes,
            current_bytes: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    fn get(&mut self, idx: u32) -> Option<Arc<Cluster>> {
        match self.inner.get(&idx) {
            Some(c) => {
                self.hits += 1;
                Some(c.clone())
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    fn put(&mut self, idx: u32, cluster: Arc<Cluster>) {
        let size = cluster.payload().len();
        if let Some(prev) = self.inner.put(idx, cluster) {
            self.current_bytes = self.current_bytes.saturating_sub(prev.payload().len());
        }
        self.current_bytes = self.current_bytes.saturating_add(size);
        self.evict_to_budget();
    }

    fn resize(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
        self.evict_to_budget();
    }

    fn evict_to_budget(&mut self) {
        while self.current_bytes > self.max_bytes && self.inner.len() > 1 {
            if let Some((_, evicted)) = self.inner.pop_lru() {
                self.current_bytes = self.current_bytes.saturating_sub(evicted.payload().len());
                self.evictions += 1;
            } else {
                break;
            }
        }
    }

    fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn snapshot(&self) -> ClusterCacheStats {
        ClusterCacheStats {
            max_bytes: self.max_bytes as u64,
            current_bytes: self.current_bytes as u64,
            entries: self.inner.len() as u64,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }
}

/// A miss shares one in-flight decode with every concurrent reader of this
/// cluster. The entry is independent of the resident LRU, so eviction before
/// a waiter wakes cannot force that waiter to decompress the same bytes again.
struct ClusterLoad {
    idx: u32,
    owner: Weak<ArchiveCore>,
    result: TableCache<Arc<Cluster>>,
}

impl Drop for ClusterLoad {
    fn drop(&mut self) {
        if let Some(core) = self.owner.upgrade() {
            let mut cache = core.cluster_cache.lock();
            if cache
                .loading
                .get(&self.idx)
                .is_some_and(|slot| slot.strong_count() == 0)
            {
                cache.loading.remove(&self.idx);
            }
        }
    }
}

/// Snapshot of the cluster cache's resident-byte budget, current
/// occupancy, and lifetime hit/miss/eviction counters. Returned by
/// [`Archive::cluster_cache_stats`]. Counters are monotonic; resetting
/// happens only when the archive is closed.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClusterCacheStats {
    pub max_bytes: u64,
    pub current_bytes: u64,
    pub entries: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// OnceLock's stable API lacks fallible initialization. Hold the cold-path
/// lock across the read/allocation, leave it empty on error, and take no lock
/// after publication. Each table has its own lock since loading a title blob
/// can first require the cluster-offset table.
#[derive(Debug)]
struct TableCache<T> {
    value: OnceLock<T>,
    initializing: parking_lot::Mutex<()>,
}

impl<T> Default for TableCache<T> {
    fn default() -> Self {
        Self {
            value: OnceLock::new(),
            initializing: Mutex::new(()),
        }
    }
}

impl<T> TableCache<T> {
    fn get_or_try_init(&self, load: impl FnOnce() -> Result<T>) -> Result<&T> {
        if let Some(value) = self.value.get() {
            return Ok(value);
        }
        let _guard = self.initializing.lock();
        if let Some(value) = self.value.get() {
            return Ok(value);
        }
        let value = load()?;
        Ok(self.value.get_or_init(|| value))
    }
}

/// A title table can be probed in place unless its archive item is compressed.
#[derive(Debug)]
enum TitleOrder {
    Direct { offset: u64, count: u32 },
    Decoded(Arc<[u32]>),
}

impl TitleOrder {
    fn count(&self) -> u32 {
        match self {
            Self::Direct { count, .. } => *count,
            Self::Decoded(ids) => ids.len() as u32,
        }
    }

    fn id(&self, archive: &Archive, row: u32) -> Result<u32> {
        if row >= self.count() {
            return Err(Error::BadTitleIndex(row, self.count()));
        }
        match self {
            Self::Direct { offset, .. } => archive
                .core
                .storage
                .parse(offset + u64::from(row) * 4, |bytes| raw::u32_at(bytes, 0)),
            Self::Decoded(ids) => Ok(ids[row as usize]),
        }
    }

    fn all(&self, archive: &Archive) -> Result<Arc<[u32]>> {
        match self {
            Self::Direct { offset, count } => Ok(archive
                .read_table(*offset, *count, u32::from_le_bytes)?
                .into()),
            Self::Decoded(ids) => Ok(ids.clone()),
        }
    }
}

#[derive(Debug)]
struct ArchiveCore {
    storage: Storage,
    cluster_limits: Option<crate::ClusterReadLimits>,
    header: Header,
    mimes: MimeList,
    /// File length, used as the upper bound when sizing the trailing cluster.
    file_len: u64,
    cluster_cache: Mutex<ClusterByteCache>,
    /// Cached title-order listing: dirent indices sorted by (namespace, title).
    /// Prefer the modern front-article subset at `X/listing/titleOrdered/v1`;
    /// otherwise use the full header/v0 table.
    title_order: TableCache<TitleOrder>,
    title_listing: TableCache<Arc<[u32]>>,
    /// Cached `(article_count, media_count)` for the content namespace,
    /// computed on first request via [`Archive::article_and_media_counts`].
    /// "article" is any item whose mimetype starts with `text/html`;
    /// "media" is any non-redirect, non-article entry. Redirects don't
    /// count toward either total.
    content_counts: OnceLock<(u64, u64)>,
    /// Cluster start offsets in ascending *file* order, built lazily.
    /// The ZIM format does not require cluster index order to match
    /// on-disk order (zimru's own writer emits clusters in completion
    /// order, and spliced streamed clusters land at the tail), so a
    /// cluster's extent must be bounded by the next-greater *offset*,
    /// not by `cluster_pointer(idx + 1)`.
    sorted_cluster_offsets: TableCache<Vec<u64>>,
    // Only one physical cluster interval can straddle the header title pointer.
    title_boundary: OnceLock<(u32, u64)>,
}

/// Admission limits checked before archive tables or decoded clusters are
/// materialized. Simultaneous callers and returned objects use additional memory.
#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    /// Maximum bytes inspected for one metadata record, including the fixed header.
    pub max_metadata_record_bytes: usize,
    pub max_entries: u32,
    pub max_clusters: u32,
    /// Optional per-cluster bounds for all materializing reads on this archive.
    pub cluster: Option<crate::ClusterReadLimits>,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_metadata_record_bytes: storage::DEFAULT_MAX_METADATA_RECORD,
            max_entries: u32::MAX,
            max_clusters: u32::MAX,
            cluster: None,
        }
    }
}

/// A read-only ZIM archive.
#[derive(Clone, Debug)]
pub struct Archive {
    core: Arc<ArchiveCore>,
}

impl Archive {
    /// Open one ZIM file for positioned reads. Split archives must first be joined.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_file(File::open(path)?)
    }

    /// Read an already-open file. All lookups and content reads use this
    /// handle regardless of later renames. The file's contents should remain
    /// stable for the lifetime of the archive so that cached metadata and
    /// subsequently read payloads refer to the same archive revision.
    ///
    /// Metadata probes share at most 1 MiB of cached pages per archive.
    /// A single directory record or MIME list may occupy up to 16 MiB.
    pub fn from_file(file: File) -> Result<Self> {
        Self::from_file_with_limits(file, ArchiveLimits::default())
    }

    /// Open a single file using caller-selected metadata, table and codec limits.
    pub fn from_file_with_limits(file: File, limits: ArchiveLimits) -> Result<Self> {
        Self::from_storage(
            Storage::file(file, limits.max_metadata_record_bytes)?,
            limits,
        )
    }

    /// Open an externally owned address space, such as a sequence of file
    /// segments, with the same metadata cache and limits as a regular file.
    /// `length` is the logical byte length at the time of opening.
    pub fn from_positioned(
        input: Arc<dyn PositionedInput>,
        length: u64,
        limits: ArchiveLimits,
    ) -> Result<Self> {
        Self::from_storage(
            Storage::positioned(input, length, limits.max_metadata_record_bytes),
            limits,
        )
    }

    /// Read an existing mapping whose creator upholds its immutability contract.
    #[cfg(feature = "legacy-mmap")]
    pub fn from_mmap(mmap: Mmap) -> Result<Self> {
        Self::from_storage(Storage::mapped(mmap), ArchiveLimits::default())
    }

    fn from_storage(storage: Storage, limits: ArchiveLimits) -> Result<Self> {
        if limits.max_metadata_record_bytes < crate::header::HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "metadata limit is smaller than the ZIM header",
            )
            .into());
        }
        let file_len = storage.len();
        let header = storage.parse(0, Header::parse)?;
        if header.entry_count > limits.max_entries || header.cluster_count > limits.max_clusters {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive counts exceed configured limits",
            )
            .into());
        }
        let (mimes, _) = storage.parse(header.mime_list_pos, |buf| MimeList::parse(buf, 0))?;
        Ok(Archive {
            core: Arc::new(ArchiveCore {
                storage,
                cluster_limits: limits.cluster,
                header,
                mimes,
                file_len,
                cluster_cache: Mutex::new(ClusterByteCache::new(DEFAULT_CLUSTER_CACHE_MAX_BYTES)),
                title_order: TableCache::default(),
                title_listing: TableCache::default(),
                content_counts: OnceLock::new(),
                sorted_cluster_offsets: TableCache::default(),
                title_boundary: OnceLock::new(),
            }),
        })
    }

    pub fn header(&self) -> &Header {
        &self.core.header
    }

    pub fn uuid(&self) -> crate::Uuid {
        crate::Uuid::from_bytes(self.core.header.uuid)
    }

    /// Hint that subsequent reads will scan the archive sequentially.
    pub fn advise_sequential_scan(&self) {
        self.core.storage.sequential();
    }

    /// Read bytes from the archive at a physical offset, using the same open
    /// source as the metadata lookups. Returns zero at or beyond the archive end.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let end = self.file_len();
        if offset >= end {
            return Ok(0);
        }
        Ok(self.core.storage.reader(offset..end).read(buf)?)
    }

    /// Total number of dirents (articles + redirects + reserved).
    pub fn all_entry_count(&self) -> u32 {
        self.core.header.entry_count
    }

    /// Same as [`Archive::all_entry_count`] — included to match libzim naming.
    pub fn entry_count(&self) -> u32 {
        self.core.header.entry_count
    }

    pub fn cluster_count(&self) -> u32 {
        self.core.header.cluster_count
    }

    pub fn has_main_entry(&self) -> bool {
        self.core.header.has_main_page()
    }

    pub fn main_entry(&self) -> Result<Entry> {
        let idx = self.core.header.main_page;
        if idx == NO_MAIN_PAGE {
            return Err(Error::NoMainEntry);
        }
        self.entry_by_url_index(idx)
    }

    /// URL-pointer index of the archive's main entry, if any. `None`
    /// when the header carries no main-page pointer (the `main_page`
    /// header field is `0xFFFFFFFF`). Cheaper than [`Archive::main_entry`]
    /// — no dirent parse, no clone — for callers that only need the
    /// index (e.g. equality checks against `entry.index()` while
    /// iterating).
    pub fn main_entry_index(&self) -> Option<u32> {
        let idx = self.core.header.main_page;
        if idx == NO_MAIN_PAGE {
            None
        } else {
            Some(idx)
        }
    }

    pub fn has_checksum(&self) -> bool {
        self.core.header.has_checksum()
    }

    /// The 16-byte trailing MD5 checksum. Returns [`Error::NoChecksum`] if the
    /// archive doesn't have one.
    pub fn checksum(&self) -> Result<[u8; 16]> {
        if !self.has_checksum() {
            return Err(Error::NoChecksum);
        }
        let offset = self.core.header.checksum_pos;
        let range = self.core.storage.range(offset, 16)?;
        let mut out = [0; 16];
        self.core.storage.reader(range).read_exact(&mut out)?;
        Ok(out)
    }

    /// Verify the archive's MD5 checksum with bounded sequential reads.
    pub fn check(&self) -> Result<bool> {
        let pos = self.core.header.checksum_pos;
        if pos == 0 || self.core.storage.range(pos, 16).is_err() {
            return Err(Error::NoChecksum);
        }
        let mut h = md5::Context::new();
        let mut reader = self.core.storage.reader(0..pos);
        let mut buf = vec![0; 128 * 1024];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.consume(&buf[..n]);
        }
        Ok(h.finalize().0 == self.checksum()?)
    }

    /// Return the full mime-type list (in declaration order).
    pub fn mime_list(&self) -> &MimeList {
        &self.core.mimes
    }

    /// Look up an entry by index in the URL-pointer list (i.e. the canonical
    /// path order).
    pub fn entry_by_url_index(&self, idx: u32) -> Result<Entry> {
        self.read_entry(idx, Access::Lookup)
    }

    /// Inspect a complete directory record, including deleted or link-target
    /// markers in older archives. These markers have names but no item payload.
    pub fn directory_record(&self, index: u32) -> Result<DirectoryRecord> {
        let offset = self.url_pointer(index)?;
        self.core
            .storage
            .parse(offset, |buf| Dirent::parse_record(buf, 0))
    }

    /// Inspect a directory record by its ordinal in the preferred title listing.
    pub fn title_record(&self, index: u32) -> Result<DirectoryRecord> {
        self.directory_record(self.title_order()?.id(self, index)?)
    }

    fn read_entry(&self, idx: u32, access: Access) -> Result<Entry> {
        let off = self.url_pointer_with(idx, access)?;
        let dirent = self
            .core
            .storage
            .parse_with(off, access, |buf| Dirent::parse(buf, 0))?;
        Ok(Entry {
            archive: self.clone(),
            url_index: idx,
            dirent,
        })
    }

    /// Look up an entry by index in the title-pointer list (title order).
    pub fn entry_by_title_index(&self, idx: u32) -> Result<Entry> {
        self.entry_by_url_index(self.title_order()?.id(self, idx)?)
    }

    /// Number of entries in the preferred title-order listing. Compressed
    /// listings use the same bounded first load as [`Self::title_listing`].
    pub fn title_count(&self) -> Result<u32> {
        Ok(self.title_order()?.count())
    }

    /// Materialize the preferred title-order listing as dirent indices. Prefix
    /// searches and individual title lookups probe directly accessible listings in place.
    /// A compressed listing is decoded once. Its cluster admits up to 65 MiB of
    /// stored data, 64 MiB of decoded data, a 128 MiB XZ decoder and a 64 MiB
    /// Zstandard window. Stricter caller-supplied cluster limits take precedence.
    pub fn title_listing(&self) -> Result<Arc<[u32]>> {
        self.core
            .title_listing
            .get_or_try_init(|| self.title_order()?.all(self))
            .cloned()
    }

    fn title_order(&self) -> Result<&TitleOrder> {
        self.core.title_order.get_or_try_init(|| {
            if self.header().uses_new_namespaces() {
                if let Some(order) = self.named_title_listing("listing/titleOrdered/v1")? {
                    return Ok(order);
                }
            }
            if self.header().title_ptr_pos != u64::MAX {
                return self.header_title_listing();
            }
            if let Some(order) = self.named_title_listing("listing/titleOrdered/v0")? {
                return Ok(order);
            }
            self.named_title_listing("listing/titleOrdered")?
                .ok_or(Error::EntryNotFound)
        })
    }

    fn header_title_listing(&self) -> Result<TitleOrder> {
        let offset = self.header().title_ptr_pos;
        let count = self.entry_count();
        self.table_range(offset, count, 4)?;
        Ok(TitleOrder::Direct { offset, count })
    }

    fn named_title_listing(&self, path: &str) -> Result<Option<TitleOrder>> {
        let entry = match self.entry_by_ns_path(NS_INDEX, path) {
            Ok(entry) => entry,
            Err(Error::EntryNotFound) => return Ok(None),
            Err(err) => return Err(err),
        };
        let item = entry.get_item(true)?;
        let direct = self.blob_direct_access(item.cluster_index(), item.blob_index())?;
        if direct.is_direct {
            if !direct.size.is_multiple_of(4) || direct.size / 4 > u64::from(self.entry_count()) {
                return Err(Error::Truncated(direct.file_offset));
            }
            return Ok(Some(TitleOrder::Direct {
                offset: direct.file_offset,
                count: (direct.size / 4) as u32,
            }));
        }
        // Legacy writers may compress a listing. Apply a cold-load limit even
        // when the caller did not set a general archive decompression policy.
        let cluster = self.cluster_with_limits(
            item.cluster_index(),
            crate::ClusterReadLimits {
                max_decoded_bytes: 64 << 20,
                max_stored_bytes: 65 << 20,
                xz_memory_bytes: 128 << 20,
                zstd_window_log: 26,
            },
        )?;
        let bytes = cluster.blob(item.blob_index())?;
        if !bytes.len().is_multiple_of(4) || bytes.len() / 4 > self.entry_count() as usize {
            return Err(Error::Truncated(bytes.len() as u64));
        }
        Ok(Some(TitleOrder::Decoded(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .copied()
                .map(u32::from_le_bytes)
                .collect(),
        )))
    }

    /// Modeled after libzim's `Archive::getEntryByPath(path)` — for "new
    /// namespace" archives this looks up `C/<path>`. Legacy paths may carry
    /// a namespace prefix such as `A/` or `-/`; unqualified paths use `A/`.
    pub fn get_entry_by_path(&self, path: &str) -> Result<Entry> {
        if self.core.header.uses_new_namespaces() {
            self.entry_by_ns_path(NS_CONTENT_NEW, path)
        } else if let Some((ns, rest)) = split_legacy_path(path) {
            self.entry_by_ns_path(ns, rest)
        } else {
            // Legacy archive without explicit namespace — try articles ns.
            self.entry_by_ns_path(NS_ARTICLES_LEGACY, path)
        }
    }

    pub fn has_entry_by_path(&self, path: &str) -> bool {
        self.get_entry_by_path(path).is_ok()
    }

    /// Look up by title in the content namespace.
    pub fn get_entry_by_title(&self, title: &str) -> Result<Entry> {
        let ns = if self.core.header.uses_new_namespaces() {
            NS_CONTENT_NEW
        } else {
            NS_ARTICLES_LEGACY
        };
        self.entry_by_ns_title(ns, title)
    }

    pub fn has_entry_by_title(&self, title: &str) -> bool {
        self.get_entry_by_title(title).is_ok()
    }

    /// Count entries in a single namespace by binary-searching for the
    /// namespace boundary in the URL pointer list. O(log n) — much faster
    /// than iterating every dirent when the caller only wants a count.
    pub fn entry_count_in_namespace(&self, ns: u8) -> Result<u32> {
        let n = self.core.header.entry_count;
        // Find first index where namespace >= ns.
        let lo_idx = self.lower_bound_ns(ns, n)?;
        // Find first index where namespace > ns (i.e. >= ns+1).
        let hi_idx = self.lower_bound_ns(ns.saturating_add(1), n)?;
        Ok(hi_idx.saturating_sub(lo_idx))
    }

    /// On-disk byte length of the opened file. For
    /// multipart archives this is currently the size of the part we've
    /// opened; multipart-aware sizing is a future TODO.
    pub fn file_len(&self) -> u64 {
        self.core.file_len
    }

    /// Count "articles" (mimetype starts with `text/html`) and "media"
    /// (any other non-redirect item) for catalog purposes. Redirects
    /// don't count toward either total. Result is cached.
    ///
    /// On modern (new-namespace) archives all user content lives under
    /// `C/`, so this walks just that range. On legacy archives the user
    /// content is spread across `A` (articles), `I` (images/files),
    /// `J` (image text), `-` (layout: CSS/JS/fonts), and a few rarer
    /// per-article meta namespaces (`B`, `H`, `U`, `V`, `W`); the only
    /// namespaces explicitly *not* counted are `M` (metadata) and `X`
    /// (search indexes). The walk skips both regardless of namespace
    /// scheme so it stays robust to unusual legacy shapes.
    pub fn article_and_media_counts(&self) -> Result<(u64, u64)> {
        if let Some(&cached) = self.core.content_counts.get() {
            return Ok(cached);
        }
        let new_scheme = self.core.header.uses_new_namespaces();
        let range: Box<dyn Iterator<Item = u32>> = if new_scheme {
            Box::new(self.namespace_range(NS_CONTENT_NEW)?)
        } else {
            Box::new(0..self.all_entry_count())
        };
        let mut articles: u64 = 0;
        let mut media: u64 = 0;
        for idx in range {
            let entry = self.entry_by_url_index(idx)?;
            if !new_scheme {
                let ns = entry.namespace();
                if ns == NS_METADATA || ns == NS_INDEX {
                    continue;
                }
            }
            let mime_idx = match entry.dirent() {
                crate::Dirent::Article(a) => a.mimetype,
                crate::Dirent::Redirect(_) => continue,
            };
            let is_article = self
                .core
                .mimes
                .get(mime_idx)
                .map(|m| m.starts_with("text/html"))
                .unwrap_or(false);
            if is_article {
                articles += 1;
            } else {
                media += 1;
            }
        }
        let _ = self.core.content_counts.set((articles, media));
        Ok((articles, media))
    }

    /// Number of "article" entries (text/html items) in the content
    /// namespace. See [`Archive::article_and_media_counts`].
    pub fn article_count(&self) -> Result<u64> {
        Ok(self.article_and_media_counts()?.0)
    }

    /// Number of "media" entries (non-html, non-redirect items) in the
    /// content namespace. See [`Archive::article_and_media_counts`].
    pub fn media_count(&self) -> Result<u64> {
        Ok(self.article_and_media_counts()?.1)
    }

    /// Pick a uniformly-random entry suitable for serving as a user URL.
    /// Pseudo-random, seeded from the system clock + process id; not
    /// suitable for cryptographic use.
    ///
    /// On modern (new-namespace) archives all user content lives under
    /// `C/`, so this picks uniformly from that range. On legacy archives
    /// user content is spread across many namespaces (`A/I/J/-/B/H/U/V/W`
    /// — Kiwix OSM builds put vector tile data under `I/`); this picks
    /// uniformly from the whole entry space then rejects the metadata
    /// (`M/`) and search-index (`X/`) namespaces, retrying until a
    /// user-namespace entry is found. Probabilistic; converges in O(1)
    /// expected probes because `M/X` are typically << 1 % of entries on
    /// real archives.
    ///
    /// Returns [`Error::EntryNotFound`] when the archive is empty or
    /// every entry is in a non-user namespace.
    pub fn random_content_entry(&self) -> Result<Entry> {
        let mut x = pseudo_random_seed();
        if self.core.header.uses_new_namespaces() {
            let range = self.namespace_range(NS_CONTENT_NEW)?;
            if range.is_empty() {
                return Err(Error::EntryNotFound);
            }
            let span = range.end - range.start;
            let pick = range.start + (x % span as u64) as u32;
            return self.entry_by_url_index(pick);
        }
        // Legacy: walk the full entry space, skip metadata and index
        // namespaces. The mixer's avalanche means each retry probes a
        // different bucket, so even on adversarial archives where M/X
        // occupy a high fraction we converge in O(N) worst case.
        //
        // Retry budget: `max(n, 64)`. On small archives (e.g. n=5 in
        // tests with 2/5 entries in M/X) `x % n` only ranges over 5
        // values, so 5 mixed-state probes can collide and yield false
        // EntryNotFound at ~1 % per call. Bumping the floor to 64
        // drives that to (2/5)^64 ≈ 10⁻²⁵ without affecting large-
        // archive cost (where the M/X fraction is << 1 % and the very
        // first probe almost always succeeds).
        let n = self.core.header.entry_count;
        if n == 0 {
            return Err(Error::EntryNotFound);
        }
        let attempts = (n as u64).max(64);
        for _ in 0..attempts {
            let pick = (x % n as u64) as u32;
            let off = self.url_pointer(pick)?;
            let ns_byte = self.core.storage.parse(off, |buf| raw::u8_at(buf, 3))?;
            if ns_byte != NS_METADATA && ns_byte != NS_INDEX {
                return self.entry_by_url_index(pick);
            }
            // Advance the PRNG state for the next attempt.
            x ^= x >> 30;
            x = x.wrapping_mul(0xBF58476D1CE4E5B9);
            x ^= x >> 27;
        }
        Err(Error::EntryNotFound)
    }

    fn lower_bound_ns(&self, ns: u8, n: u32) -> Result<u32> {
        let mut lo = 0u32;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = self.url_pointer(mid)?;
            // Read the namespace byte directly without parsing the rest of
            // the dirent — it's at a fixed offset (3) inside every dirent.
            let dirent_ns = self.core.storage.parse(off, |buf| raw::u8_at(buf, 3))?;
            if dirent_ns < ns {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// Look up `(namespace, url)` directly via binary search of the URL ptr list.
    pub fn entry_by_ns_path(&self, ns: u8, url: &str) -> Result<Entry> {
        let n = self.core.header.entry_count;
        let mut lo = 0u32;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = self.url_pointer(mid)?;
            // Probe with `key_at` (no allocation) — only materialize the
            // full dirent on the matched leaf.
            let cmp = self.core.storage.parse(off, |buf| {
                let (d_ns, d_key) = Dirent::key_at(buf, 0)?;
                Ok(ns.cmp(&d_ns).then_with(|| url.cmp(d_key)))
            })?;
            match cmp {
                Ordering::Less => hi = mid,
                Ordering::Greater => lo = mid + 1,
                Ordering::Equal => {
                    let d = self.dirent(off)?;
                    return Ok(Entry {
                        archive: self.clone(),
                        url_index: mid,
                        dirent: d,
                    });
                }
            }
        }
        Err(Error::EntryNotFound)
    }

    /// Look up `(namespace, title)`. Search the title listing first, then
    /// fall back to a path-order scan for entries excluded from a modern
    /// front-article subset (including media and non-content namespaces).
    pub fn entry_by_ns_title(&self, ns: u8, title: &str) -> Result<Entry> {
        let listing = self.title_order()?;
        if listing.count() != 0 {
            let mut lo = 0u32;
            let mut hi = listing.count();
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let url_idx = listing.id(self, mid)?;
                let off = self.url_pointer(url_idx)?;
                let cmp = self.core.storage.parse(off, |buf| {
                    let (d_ns, d_key) = Dirent::title_key_at(buf, 0)?;
                    Ok(ns.cmp(&d_ns).then_with(|| title.cmp(d_key)))
                })?;
                match cmp {
                    Ordering::Less => hi = mid,
                    Ordering::Greater => lo = mid + 1,
                    Ordering::Equal => {
                        let d = self.dirent(off)?;
                        return Ok(Entry {
                            archive: self.clone(),
                            url_index: url_idx,
                            dirent: d,
                        });
                    }
                }
            }
        }
        // A full-table listing (header or v0 covering every entry) makes the
        // miss authoritative. Only a v1 front-article subset can hide the
        // requested entry, and only then is the linear scan justified.
        let n = self.core.header.entry_count;
        if listing.count() == n {
            return Err(Error::EntryNotFound);
        }
        for i in 0..n {
            let off = self.url_pointer(i)?;
            let d = self.dirent(off)?;
            if d.namespace() == ns && d.title() == title {
                return Ok(Entry {
                    archive: self.clone(),
                    url_index: i,
                    dirent: d,
                });
            }
        }
        Err(Error::EntryNotFound)
    }

    /// Half-open `[lo, hi)` range of *title-order* indices whose entries are
    /// in namespace `ns` and whose title begins with `prefix`. Returns
    /// `lo == hi` (an empty range) when nothing matches.
    ///
    /// Designed to back `SuggestionSearcher` fallbacks for ZIMs that lack a
    /// Xapian title index: callers iterate `lo..hi` and call
    /// [`Archive::entry_by_title_index`] on each to materialize entries.
    /// Two binary searches over the preferred title listing using
    /// [`Dirent::title_key_at`] perform `O(log N)` title comparisons once the
    /// preferred listing has been located. The first lookup may also prepare
    /// cluster offsets or decode a compressed listing under [`Self::title_listing`]'s limits.
    ///
    /// With a modern v1 listing, only selected C-namespace front articles
    /// are covered. Full header/v0 listings span every namespace.
    pub fn title_prefix_range(&self, ns: u8, prefix: &str) -> Result<std::ops::Range<u32>> {
        let listing = self.title_order()?;
        let lo = self.lower_bound_in_title_listing(listing, ns, prefix.as_bytes())?;
        // The first key strictly greater than every UTF-8 string starting
        // with `prefix` is `prefix` followed by `0xFF` — `0xFF` never appears
        // in valid UTF-8 so this is a safe sentinel.
        let mut succ = Vec::with_capacity(prefix.len() + 1);
        succ.extend_from_slice(prefix.as_bytes());
        succ.push(0xFF);
        let hi = self.lower_bound_in_title_listing(listing, ns, &succ)?;
        Ok(lo..hi)
    }

    /// First index in `listing` where the dirent's `(namespace, title)` is
    /// `>= (ns, key)`. Helper for [`Archive::title_prefix_range`].
    fn lower_bound_in_title_listing(
        &self,
        listing: &TitleOrder,
        ns: u8,
        key: &[u8],
    ) -> Result<u32> {
        let mut lo = 0u32;
        let mut hi = listing.count();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let url_idx = listing.id(self, mid)?;
            let off = self.url_pointer(url_idx)?;
            let cmp = self.core.storage.parse(off, |buf| {
                let (d_ns, title) = Dirent::title_key_at(buf, 0)?;
                Ok(d_ns.cmp(&ns).then_with(|| title.as_bytes().cmp(key)))
            })?;
            if cmp.is_lt() {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// Iterate every entry in URL-pointer (path) order.
    pub fn iter_by_path(&self) -> EntryIter {
        EntryIter::new(self, IterKind::ByPath, Ok(0..self.entry_count()))
    }

    /// Iterate the title listing. A modern v1 listing yields only selected
    /// front articles; full header/v0 listings include every entry.
    pub fn iter_by_title(&self) -> EntryIter {
        EntryIter::new(self, IterKind::ByTitle, self.title_count().map(|n| 0..n))
    }

    /// Get a metadata value by name (e.g. "Title", "Description", "Language").
    /// Looks the entry up in the `M` namespace and returns its content as bytes.
    pub fn get_metadata(&self, name: &str) -> Result<Vec<u8>> {
        let entry = self.entry_by_ns_path(NS_METADATA, name)?;
        let item = entry.get_item(true)?;
        Ok(item.get_data()?.to_vec())
    }

    /// List all metadata keys (entries in the `M` namespace). Binary-
    /// searches for the `M`-namespace range instead of scanning every
    /// dirent, so cost scales with the (small) metadata count, not the
    /// archive's entry count.
    pub fn get_metadata_keys(&self) -> Vec<String> {
        let Ok(range) = self.namespace_range(NS_METADATA) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity((range.end - range.start) as usize);
        for i in range {
            let Ok(off) = self.url_pointer(i) else {
                continue;
            };
            let Ok(url) = self
                .core
                .storage
                .parse(off, |buf| Ok(Dirent::key_at(buf, 0)?.1.to_owned()))
            else {
                continue;
            };
            out.push(url);
        }
        out
    }

    /// Enumerate every cover/thumbnail illustration recorded in the
    /// archive's metadata. ZIM stores these as `M/Illustration_<W>x<H>@<scale>`
    /// entries; this helper parses every such key and returns the
    /// `(width, height, scale)` tuples sorted ascending. Keys that don't
    /// match the pattern are silently skipped — callers get only valid
    /// illustration descriptors.
    pub fn illustrations(&self) -> Vec<(u32, u32, u32)> {
        let mut out: Vec<(u32, u32, u32)> = self
            .get_metadata_keys()
            .into_iter()
            .filter_map(|k| parse_illustration_key(&k))
            .collect();
        out.sort();
        out
    }

    // ============================================================
    // ---- Ergonomic convenience API (idiomatic Rust extras) ----
    // ============================================================
    //
    // Everything below is layered on top of the libzim-mirror methods and
    // designed around common read-only workflows: read one article as text,
    // enumerate all non-redirect content, iterate a path-prefix range,
    // process every blob in parallel, or get a one-shot snapshot of the
    // archive's header/counts. These shapes weren't possible (or were
    // much more verbose) in the original C++ libzim.

    /// One-liner: fetch the decompressed bytes of the entry at `path`,
    /// following any redirects. Equivalent to
    /// `archive.get_entry_by_path(path)?.get_item(true)?.get_data()?.to_vec()`.
    ///
    /// ```no_run
    /// # use zimru::Archive;
    /// let archive = Archive::open("wiki.zim")?;
    /// let bytes = archive.get_bytes("Albert_Einstein")?;
    /// # Ok::<(), zimru::Error>(())
    /// ```
    pub fn get_bytes(&self, path: &str) -> Result<Vec<u8>> {
        let entry = self.get_entry_by_path(path)?;
        let item = entry.get_item(true)?;
        Ok(item.get_data()?.to_vec())
    }

    /// One-liner: fetch the entry at `path` as a UTF-8 [`String`]. Fails
    /// with [`Error::BadUtf8`] if the content isn't valid UTF-8.
    pub fn get_text(&self, path: &str) -> Result<String> {
        let blob = self.get_bytes(path)?;
        String::from_utf8(blob).map_err(|_| Error::BadUtf8(0))
    }

    /// One-liner: fetch the [`Item`] for the entry at `path`, following
    /// any redirects.
    pub fn get_item(&self, path: &str) -> Result<Item> {
        self.get_entry_by_path(path)?.get_item(true)
    }

    /// Resolve the archive's main entry directly to an [`Item`], following
    /// the `W/mainPage` redirect if present.
    pub fn main_item(&self) -> Result<Item> {
        self.main_entry()?.get_item(true)
    }

    /// Just the path of the resolved main page (e.g. `"index"`), skipping
    /// the redirect chain. Useful for logging / info output.
    pub fn main_path(&self) -> Result<String> {
        let mut cur = self.main_entry()?;
        let mut depth = 0;
        while cur.is_redirect() {
            depth += 1;
            if depth > 16 {
                return Err(Error::RedirectLoop);
            }
            cur = cur.get_redirect_entry()?;
        }
        Ok(cur.path().to_string())
    }

    /// Read a metadata entry as a UTF-8 string. Shorthand for
    /// `String::from_utf8(archive.get_metadata(name)?)`.
    pub fn metadata_str(&self, name: &str) -> Result<String> {
        let bytes = self.get_metadata(name)?;
        String::from_utf8(bytes).map_err(|_| Error::BadUtf8(0))
    }

    /// Check whether a metadata key exists without reading its value.
    pub fn has_metadata(&self, name: &str) -> bool {
        self.entry_by_ns_path(NS_METADATA, name).is_ok()
    }

    /// The `(start, end)` URL-pointer range occupied by `namespace`. Entries
    /// `start..end` in URL-pointer order are exactly the ones in `namespace`.
    /// Runs in O(log n) via binary search on the namespace byte.
    pub fn namespace_range(&self, ns: u8) -> Result<std::ops::Range<u32>> {
        let n = self.core.header.entry_count;
        let start = self.lower_bound_ns(ns, n)?;
        let end = self.lower_bound_ns(ns.saturating_add(1), n)?;
        Ok(start..end)
    }

    /// Iterate only the non-redirect entries in path order.
    pub fn articles(&self) -> impl Iterator<Item = Result<Entry>> + '_ {
        self.iter_by_path().filter(|r| match r {
            Ok(e) => !e.is_redirect(),
            Err(_) => true, // surface errors up
        })
    }

    /// Iterate only the redirect entries in path order.
    pub fn redirects(&self) -> impl Iterator<Item = Result<Entry>> + '_ {
        self.iter_by_path().filter(|r| match r {
            Ok(e) => e.is_redirect(),
            Err(_) => true,
        })
    }

    /// Iterate all entries of the content namespace in path order, including
    /// redirects. Legacy archives use the articles namespace.
    ///
    /// ```no_run
    /// # use zimru::Archive;
    /// let archive = Archive::open("wiki.zim")?;
    /// for entry in archive.content_entries() {
    ///     let e = entry?;
    ///     println!("{}", e.path());
    /// }
    /// # Ok::<(), zimru::Error>(())
    /// ```
    pub fn content_entries(&self) -> EntryIter {
        let ns = if self.core.header.uses_new_namespaces() {
            NS_CONTENT_NEW
        } else {
            NS_ARTICLES_LEGACY
        };
        EntryIter::new(self, IterKind::ByPath, self.namespace_range(ns))
    }

    /// Iterate every entry whose (namespace, url) starts with a given
    /// prefix. Binary-searches for the start, then walks forward while the
    /// prefix still matches — O(log n + k) where k is the number of matches.
    ///
    /// Common use: "all pages under `images/`" or "every X/listing/* entry".
    ///
    /// ```no_run
    /// # use zimru::Archive;
    /// let archive = Archive::open("wiki.zim")?;
    /// for entry in archive.by_prefix(b'X', "listing/") {
    ///     let e = entry?;
    ///     println!("{} → {}", e.path(), e.title());
    /// }
    /// # Ok::<(), zimru::Error>(())
    /// ```
    pub fn by_prefix<'a>(
        &'a self,
        ns: u8,
        prefix: &'a str,
    ) -> impl Iterator<Item = Result<Entry>> + 'a {
        let n = self.entry_count();
        let range = self.prefix_start(ns, prefix, n).map(|i| i..n);
        PrefixIter {
            entries: EntryIter::new(self, IterKind::ByPath, range),
            ns,
            prefix: prefix.to_owned(),
        }
    }

    fn prefix_start(&self, ns: u8, prefix: &str, mut hi: u32) -> Result<u32> {
        let mut lo = 0;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = self.url_pointer(mid)?;
            let cmp = self.core.storage.parse(off, |buf| {
                let (dns, url) = Dirent::key_at(buf, 0)?;
                Ok(dns.cmp(&ns).then_with(|| url.cmp(prefix)))
            })?;
            if cmp.is_lt() {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Ok(lo)
    }

    /// Rayon-aware parallel iterator over every entry in URL-pointer order.
    ///
    /// Useful for embarrassingly-parallel scans that touch dirents only
    /// (e.g. counting redirects by namespace). When the inner closure needs
    /// blob content, prefer [`Archive::par_clusters`] instead — it
    /// decompresses each cluster exactly once, which is usually much faster.
    pub fn par_iter_by_path(&self) -> impl ParallelIterator<Item = Result<Entry>> + '_ {
        let n = self.core.header.entry_count;
        (0..n)
            .into_par_iter()
            .map(move |i| self.read_entry(i, Access::Scan))
    }

    /// Run a closure over every decompressed cluster in parallel. Workers
    /// bypass the shared cluster cache, so memory usage stays bounded at
    /// roughly `num_threads × largest_cluster_size`. Used internally by
    /// `zimcheck -A` to hit 8× upstream throughput; exposed here so library
    /// users can build their own parallel content scans (e.g. full-text
    /// indexers, dedup scanners, stats collectors).
    ///
    /// The closure receives `(cluster_index, cluster)` and returns a `T`
    /// per cluster. Results come back in cluster-index order.
    pub fn par_clusters<T, F>(&self, f: F) -> Result<Vec<T>>
    where
        T: Send,
        F: Fn(u32, &Cluster) -> T + Sync + Send,
    {
        let n = self.core.header.cluster_count;
        let results: Vec<Result<T>> = (0..n)
            .into_par_iter()
            .map(|idx| {
                let c = self.cluster_uncached(idx)?;
                Ok(f(idx, &c))
            })
            .collect();
        results.into_iter().collect()
    }

    /// Compact summary of the archive's header + aggregate counts, handy
    /// for `--info` style commands and logging. Single allocation + one
    /// optional main-entry lookup.
    pub fn summary(&self) -> Summary {
        let h = &self.core.header;
        Summary {
            uuid: crate::Uuid::from_bytes(h.uuid),
            major_version: h.major_version,
            minor_version: h.minor_version,
            uses_new_namespaces: h.uses_new_namespaces(),
            entry_count: h.entry_count,
            content_entry_count: self
                .namespace_range(if h.uses_new_namespaces() {
                    NS_CONTENT_NEW
                } else {
                    NS_ARTICLES_LEGACY
                })
                .map(|r| r.end - r.start)
                .unwrap_or(0),
            cluster_count: h.cluster_count,
            mime_type_count: self.core.mimes.len(),
            has_main_entry: h.has_main_page(),
            main_path: self.main_path().ok(),
            has_checksum: h.has_checksum(),
        }
    }

    // ---- internal helpers ----

    fn dirent(&self, pos: u64) -> Result<Dirent> {
        self.core.storage.parse(pos, |buf| Dirent::parse(buf, 0))
    }

    fn table_range(&self, pos: u64, count: u32, width: u64) -> Result<Range<u64>> {
        self.core.storage.range(pos, u64::from(count) * width)
    }

    fn pointer(&self, pos: u64, count: u32, idx: u32, access: Access) -> Result<u64> {
        self.table_range(pos, count, 8)?;
        self.core
            .storage
            .parse_with(pos + u64::from(idx) * 8, access, |buf| raw::u64_at(buf, 0))
    }

    fn read_table<T, const WIDTH: usize>(
        &self,
        pos: u64,
        count: u32,
        decode: fn([u8; WIDTH]) -> T,
    ) -> Result<Vec<T>> {
        let range = self.table_range(pos, count, WIDTH as u64)?;
        let mut reader = self.core.storage.reader(range);
        let mut values = Vec::new();
        values
            .try_reserve_exact(count as usize)
            .map_err(io::Error::other)?;
        let mut block = [0; 8192];
        let mut remaining = count as usize;
        while remaining != 0 {
            let n = remaining.min(block.len() / WIDTH);
            let bytes = &mut block[..n * WIDTH];
            reader.read_exact(bytes)?;
            values.extend(bytes.as_chunks::<WIDTH>().0.iter().copied().map(decode));
            remaining -= n;
        }
        Ok(values)
    }

    fn url_pointer(&self, idx: u32) -> Result<u64> {
        self.url_pointer_with(idx, Access::Lookup)
    }

    fn url_pointer_with(&self, idx: u32, access: Access) -> Result<u64> {
        let n = self.core.header.entry_count;
        if idx >= n {
            return Err(Error::BadUrlIndex(idx, n));
        }
        self.pointer(self.core.header.url_ptr_pos, n, idx, access)
    }

    fn cluster_pointer(&self, idx: u32) -> Result<u64> {
        let n = self.core.header.cluster_count;
        if idx >= n {
            return Err(Error::BadClusterIndex(idx, n));
        }
        self.pointer(self.core.header.cluster_ptr_pos, n, idx, Access::Lookup)
    }

    /// Public access to a decoded cluster by its index. The cluster is cached
    /// inside the archive so repeat calls for the same index are free.
    pub fn cluster(&self, idx: u32) -> Result<Arc<Cluster>> {
        self.load_cluster(idx)
    }

    /// Validate that cluster `idx` decodes and its blob-offset table is
    /// self-consistent, without touching the shared cache. Uncompressed
    /// clusters stream through their offset tables without loading blob data; compressed
    /// clusters are decoded once and every blob range walked.
    pub fn validate_cluster(&self, idx: u32) -> Result<()> {
        self.validate_cluster_references(idx, &[])
    }

    /// Validate a cluster's offset table and every supplied blob reference.
    /// Uncompressed payloads are checked in place; compressed payloads are
    /// decoded once, without populating the archive cache.
    pub fn validate_cluster_references(&self, idx: u32, blobs: &[u32]) -> Result<()> {
        let range = self.cluster_byte_range(idx)?;
        if let Some(offsets) = self.raw_offsets(&range)? {
            let mut reader = self.core.storage.reader(range.start + 1..range.end);
            offsets.validate(&mut reader)?;
            for &blob in blobs {
                offsets.check_index(blob)?;
            }
            return Ok(());
        }
        let c = self.cluster_uncached(idx)?;
        for &blob in blobs {
            c.blob_range(blob)?;
        }
        Ok(())
    }

    /// Direct-access info for a single blob: where its bytes physically
    /// live in the on-disk ZIM file, when the surrounding cluster is
    /// stored uncompressed (compression-id 0 or 1). For compressed
    /// clusters, returns `is_direct = false` and zeroes for offset/size.
    ///
    /// Useful for handing fulltext/Xapian indexes (always stored
    /// uncompressed by convention) to libxapian via `Database(int fd)`
    /// + `lseek` without copying any bytes.
    pub fn blob_direct_access(&self, cluster_idx: u32, blob_idx: u32) -> Result<DirectAccess> {
        let cluster_range = self.cluster_byte_range(cluster_idx)?;
        let offsets = self.raw_offsets(&cluster_range)?;
        match offsets {
            Some(offsets) => {
                let r = offsets.blob_range(blob_idx, |at| {
                    self.offset_at(cluster_range.start + 1 + at, offsets.width())
                })?;
                Ok(DirectAccess {
                    is_direct: true,
                    // The cluster's payload starts immediately after the
                    // 1-byte info byte at the start of the on-disk cluster.
                    file_offset: cluster_range.start + 1 + r.start,
                    size: r.end - r.start,
                })
            }
            None => Ok(DirectAccess {
                is_direct: false,
                file_offset: 0,
                size: 0,
            }),
        }
    }

    /// Absolute on-disk byte offset of cluster `idx` — the byte where
    /// the cluster's leading info-byte begins. Equal to
    /// [`Archive::cluster_byte_range`]`(idx).start`. Exposed as a
    /// standalone primitive for tools that walk the cluster region by
    /// offset alone (`zimsplit` choosing safe split points,
    /// `zimdump --list` printing per-cluster file positions) without
    /// needing the trailing offset.
    pub fn cluster_offset(&self, idx: u32) -> Result<u64> {
        self.cluster_pointer(idx)
    }

    /// Exclusive end of the cluster that starts at `start`: the
    /// smallest cluster offset strictly greater than `start`, or —
    /// for the cluster that comes last in *file* order — the nearest
    /// table/checksum boundary after it.
    ///
    /// Cluster index order is NOT guaranteed to match on-disk order
    /// (zimru's writer emits clusters in encode-completion order, and
    /// streamed huge items splice at the tail), so bounding cluster
    /// `idx` by `cluster_pointer(idx + 1)` would corrupt reads of any
    /// out-of-order cluster. The sorted offset list is built once per
    /// archive and shared.
    fn cluster_slice_end(&self, idx: u32, start: u64) -> Result<u64> {
        let offs = self.core.sorted_cluster_offsets.get_or_try_init(|| {
            let mut offsets = self.read_table(
                self.core.header.cluster_ptr_pos,
                self.cluster_count(),
                u64::from_le_bytes,
            )?;
            offsets.sort_unstable();
            Ok(offsets)
        })?;
        let i = offs.partition_point(|&o| o <= start);
        let boundary = self.cluster_region_boundary_after(start);
        let end = offs
            .get(i)
            .copied()
            .map_or(boundary, |next| next.min(boundary));
        let title = self.header().title_ptr_pos;
        if start >= title || end <= title {
            return Ok(end);
        }
        if let Some(&(cached_idx, end)) = self.core.title_boundary.get() {
            if cached_idx == idx {
                return Ok(end);
            }
        }
        let end = self.title_table_boundary(idx, start..end, title)?;
        let _ = self.core.title_boundary.set((idx, end));
        Ok(end)
    }

    // Some writers expose the full v0 listing both as a blob and through
    // the header. A header pointer to that blob is inside its cluster.
    fn title_table_boundary(&self, idx: u32, range: Range<u64>, title: u64) -> Result<u64> {
        for name in ["listing/titleOrdered/v0", "listing/titleOrdered"] {
            let entry = match self.entry_by_ns_path(NS_INDEX, name) {
                Ok(entry) => entry,
                Err(Error::EntryNotFound) => continue,
                Err(err) => return Err(err),
            };
            let Dirent::Article(article) = entry.dirent() else {
                continue;
            };
            if article.cluster != idx {
                continue;
            }
            let Some(offsets) = self.raw_offsets(&range)? else {
                continue;
            };
            let body = range.start + 1;
            let width = offsets.width();
            let blob = offsets.blob_range(article.blob, |at| self.offset_at(body + at, width))?;
            if body + blob.start == title
                && blob.end - blob.start == u64::from(self.entry_count()) * 4
            {
                // The payload ends at the final value in its offset table.
                let first = self.offset_at(body, width)?;
                if first < width || first > range.end - body || !first.is_multiple_of(width) {
                    return Err(Error::Truncated(first));
                }
                let last = self.offset_at(body + (first - width), width)?;
                if last < blob.end || last > range.end - body {
                    return Err(Error::Truncated(last));
                }
                return Ok(body + last);
            }
        }
        Ok(title)
    }

    /// Nearest header-table / checksum / EOF position after `start` —
    /// the end bound for the cluster that is last in file order.
    fn cluster_region_boundary_after(&self, start: u64) -> u64 {
        let h = &self.core.header;
        let eof = if h.has_checksum() {
            h.checksum_pos.min(self.core.file_len)
        } else {
            self.core.file_len
        };
        [h.url_ptr_pos, h.cluster_ptr_pos, eof]
            .into_iter()
            .filter(|&p| p > start)
            .min()
            .unwrap_or(eof)
    }

    /// On-disk byte range occupied by cluster `idx`, including its info
    /// byte. The returned range's length is the compressed size the cluster
    /// takes up in the file.
    pub fn cluster_byte_range(&self, idx: u32) -> Result<std::ops::Range<u64>> {
        let start = self.cluster_pointer(idx)?;
        let end = self.cluster_slice_end(idx, start)?;
        if end <= start || end > self.core.file_len {
            return Err(Error::Truncated(end));
        }
        Ok(start..end)
    }

    fn offset_at(&self, pos: u64, width: u64) -> Result<u64> {
        self.core.storage.parse(pos, |buf| {
            if width == 8 {
                raw::u64_at(buf, 0)
            } else {
                raw::u32_at(buf, 0).map(u64::from)
            }
        })
    }

    fn raw_offsets(&self, range: &Range<u64>) -> Result<Option<RawOffsets>> {
        let info = self
            .core
            .storage
            .parse(range.start, |buf| raw::u8_at(buf, 0))?;
        RawOffsets::open(info, range.end - range.start - 1, |width| {
            self.offset_at(range.start + 1, width)
        })
    }

    /// Decompress a cluster *without* touching the cache. Useful for parallel
    /// scans where each cluster is read exactly once and caching would only
    /// waste memory + add Mutex contention.
    pub fn cluster_uncached(&self, idx: u32) -> Result<Cluster> {
        self.read_cluster(idx, None)
    }

    /// Read without caching, enforcing the caller's stored, output and codec
    /// limits even if another caller already cached a less restrictive result.
    pub fn cluster_with_limits(
        &self,
        idx: u32,
        limits: crate::ClusterReadLimits,
    ) -> Result<Cluster> {
        self.read_cluster(idx, Some(limits))
    }

    fn read_cluster(&self, idx: u32, limits: Option<crate::ClusterReadLimits>) -> Result<Cluster> {
        let range = self.cluster_byte_range(idx)?;
        let len = range.end - range.start;
        let limits = match (self.core.cluster_limits, limits) {
            (Some(a), Some(b)) => Some(crate::ClusterReadLimits {
                max_stored_bytes: a.max_stored_bytes.min(b.max_stored_bytes),
                max_decoded_bytes: a.max_decoded_bytes.min(b.max_decoded_bytes),
                xz_memory_bytes: a.xz_memory_bytes.min(b.xz_memory_bytes),
                zstd_window_log: a.zstd_window_log.min(b.zstd_window_log),
            }),
            (a, b) => a.or(b),
        };
        Cluster::read(self.core.storage.reader(range), len, limits)
    }

    fn load_cluster(&self, idx: u32) -> Result<Arc<Cluster>> {
        let pending = {
            let mut cache = self.core.cluster_cache.lock();
            if let Some(cluster) = cache.get(idx) {
                return Ok(cluster);
            }
            match cache.loading.get(&idx).and_then(Weak::upgrade) {
                Some(pending) => pending,
                None => {
                    let pending = Arc::new(ClusterLoad {
                        idx,
                        owner: Arc::downgrade(&self.core),
                        result: TableCache::default(),
                    });
                    cache.loading.insert(idx, Arc::downgrade(&pending));
                    pending
                }
            }
        };
        pending
            .result
            .get_or_try_init(|| {
                let cluster = Arc::new(self.cluster_uncached(idx)?);
                self.core.cluster_cache.lock().put(idx, cluster.clone());
                Ok(cluster)
            })
            .cloned()
    }

    /// Reconfigure the cluster cache's byte budget. The cache holds
    /// decompressed clusters; `max_bytes` is the upper bound on the
    /// summed payload sizes resident at once. Default is
    /// [`DEFAULT_CLUSTER_CACHE_MAX_BYTES`]. Existing entries that push
    /// the cache over the new budget are evicted in LRU order; a
    /// single most-recently-used cluster larger than the budget is
    /// kept pinned to avoid permanent misses.
    pub fn set_cluster_cache_max_bytes(&self, max_bytes: usize) {
        self.core.cluster_cache.lock().resize(max_bytes);
    }

    /// Current cluster-cache byte budget. See
    /// [`Archive::set_cluster_cache_max_bytes`].
    pub fn cluster_cache_max_bytes(&self) -> usize {
        self.core.cluster_cache.lock().max_bytes()
    }

    /// Snapshot of cluster-cache occupancy + lifetime counters. Useful
    /// for verifying that the cache is doing its job (hit:miss ratio
    /// for hot-article workloads), tuning [`Archive::set_cluster_cache_max_bytes`]
    /// against actual eviction pressure, or surfacing cache health to
    /// telemetry from a long-running server.
    pub fn cluster_cache_stats(&self) -> ClusterCacheStats {
        self.core.cluster_cache.lock().snapshot()
    }

    // ============================================================
    // ----------- Structural integrity primitives ---------------
    // ============================================================
    //
    // Each `check_*` returns `Ok(true)` for a clean archive, `Ok(false)`
    // for a structural defect, and `Err(_)` only for a read failure
    // that prevents the check from running at all (truncated header,
    // unreadable file, …). `Ok(false)` means "we read the bytes; they
    // don't satisfy this invariant" — actionable for `zimcheck`-style
    // tools. The set is intentionally fine-grained so callers (the
    // shim's libzim-shaped `validate()` entry point being the
    // motivating one) can compose just the checks they need; the
    // archive-wide MD5 check stays separate as [`Archive::check`].

    /// Verify every URL-pointer-list entry resolves to a parseable
    /// dirent within file bounds. Catches a truncated dirent region or
    /// a corrupt url-pointer list entry that points off the end of the
    /// file or into the middle of another structure.
    pub fn check_dirent_ptrs(&self) -> Result<bool> {
        let n = self.core.header.entry_count;
        let file_len = self.core.file_len;
        for i in 0..n {
            let off = self.url_pointer(i)?;
            if off >= file_len {
                return Ok(false);
            }
            if self
                .core
                .storage
                .parse(off, |buf| Dirent::parse_record(buf, 0))
                .is_err()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Verify every dirent appears in `(namespace, url)` order in the
    /// URL-pointer list. `entry_by_ns_path` is a binary search over
    /// this ordering; a `false` here would silently break every path
    /// lookup on the archive.
    pub fn check_dirent_order(&self) -> Result<bool> {
        let n = self.core.header.entry_count;
        let mut previous = None;
        for i in 0..n {
            let off = self.url_pointer(i)?;
            let key = self.core.storage.parse(off, |buf| {
                let (ns, url) = Dirent::key_at(buf, 0)?;
                Ok((ns, url.to_owned()))
            })?;
            if previous.as_ref().is_some_and(|prev| prev > &key) {
                return Ok(false);
            }
            previous = Some(key);
        }
        Ok(true)
    }

    /// Validate every available title table independently: the header and
    /// v0 tables must be full permutations of the URL indices, while v1 is
    /// a unique C-namespace subset. All tables must be title-sorted.
    pub fn check_title_index(&self) -> Result<bool> {
        let mut found = false;
        if self.core.header.title_ptr_pos != u64::MAX {
            found = true;
            if !self.valid_title_listing(&self.header_title_listing()?.all(self)?, false)? {
                return Ok(false);
            }
        }
        for (path, subset) in [
            ("listing/titleOrdered/v0", false),
            ("listing/titleOrdered", false),
            ("listing/titleOrdered/v1", true),
        ] {
            if let Some(listing) = self.named_title_listing(path)? {
                found = true;
                if !self.valid_title_listing(&listing.all(self)?, subset)? {
                    return Ok(false);
                }
            }
        }
        Ok(found)
    }

    fn valid_title_listing(&self, listing: &[u32], subset: bool) -> Result<bool> {
        if !subset && listing.len() != self.entry_count() as usize {
            return Ok(false);
        }
        // Bound allocation by the real URL table, not an unchecked header count.
        self.table_range(self.core.header.url_ptr_pos, self.entry_count(), 8)?;
        let mut seen = vec![false; self.entry_count() as usize];
        let mut prev = None;
        for &url_idx in listing {
            let Some(seen) = seen.get_mut(url_idx as usize) else {
                return Ok(false);
            };
            if std::mem::replace(seen, true) {
                return Ok(false);
            }
            let off = self.url_pointer(url_idx)?;
            let key = self.core.storage.parse(off, |buf| {
                let (ns, title) = Dirent::title_key_at(buf, 0)?;
                Ok((ns, title.to_owned()))
            })?;
            if (subset && key.0 != NS_CONTENT_NEW) || prev.as_ref().is_some_and(|prev| prev > &key)
            {
                return Ok(false);
            }
            prev = Some(key);
        }
        Ok(true)
    }

    /// Verify every cluster-pointer-list entry is in-bounds for the
    /// file. Catches truncation between the end of the dirent region
    /// and the trailing checksum.
    pub fn check_cluster_ptrs(&self) -> Result<bool> {
        let n = self.core.header.cluster_count;
        let file_len = self.core.file_len;
        for i in 0..n {
            let off = self.cluster_pointer(i)?;
            // Full payload validation remains separate in `validate_cluster`.
            if off >= file_len {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Verify every article-dirent's mimetype index resolves inside
    /// the on-file mime-type list. A `false` here means a dirent will
    /// hand back `application/octet-stream` (or something stranger,
    /// depending on the consumer) instead of its true mimetype.
    pub fn check_mimetypes(&self) -> Result<bool> {
        let n = self.core.header.entry_count;
        let mime_count = self.core.mimes.len();
        for i in 0..n {
            if let DirectoryRecord::Entry(Dirent::Article(a)) = self.directory_record(i)? {
                if (a.mimetype as usize) >= mime_count {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }
}

/// Cheap PRNG seed: nanoseconds + pid run through SplitMix64's mixer.
/// Adequate for "give me a random article" — not for security.
fn pseudo_random_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut x = nanos.wrapping_mul(0x9E3779B97F4A7C15)
        ^ (std::process::id() as u64).wrapping_mul(0xBF58476D1CE4E5B9);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58476D1CE4E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D049BB133111EB);
    x ^= x >> 31;
    x
}

/// Parse a `Illustration_<W>x<H>@<scale>` metadata key into its
/// `(width, height, scale)` components. Returns `None` if the key
/// doesn't match the pattern or any of the dimensions fail to parse
/// as a `u32`.
fn parse_illustration_key(name: &str) -> Option<(u32, u32, u32)> {
    let rest = name.strip_prefix("Illustration_")?;
    let (dims, scale) = rest.split_once('@')?;
    let (w, h) = dims.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?, scale.parse().ok()?))
}

fn split_legacy_path(path: &str) -> Option<(u8, &str)> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b'/' && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'-')
    {
        Some((bytes[0], &path[2..]))
    } else {
        None
    }
}

// ---- Entry / Item / Blob ----

#[derive(Clone, Debug)]
pub struct Entry {
    archive: Archive,
    url_index: u32,
    dirent: Dirent,
}

impl Entry {
    pub fn path(&self) -> &str {
        self.dirent.url()
    }

    pub fn title(&self) -> &str {
        self.dirent.title()
    }

    pub fn namespace(&self) -> u8 {
        self.dirent.namespace()
    }

    pub fn is_redirect(&self) -> bool {
        self.dirent.is_redirect()
    }

    pub fn index(&self) -> u32 {
        self.url_index
    }

    /// Borrow the raw [`Dirent`] backing this entry. Useful for callers that
    /// want to inspect the article's cluster/blob/mimetype without paying
    /// for the [`Item`] resolution path.
    pub fn dirent(&self) -> &Dirent {
        &self.dirent
    }

    /// Shortcut: `get_item(true)`, matching the most common workflow where
    /// users want the item and don't care about redirects.
    pub fn item(&self) -> Result<Item> {
        self.get_item(true)
    }

    /// Walk redirects to the final content entry (no-op if non-redirect).
    pub fn resolve(&self) -> Result<Entry> {
        let mut cur = self.clone();
        let mut hops = 0;
        while cur.is_redirect() {
            hops += 1;
            if hops > MAX_REDIRECTS {
                return Err(Error::RedirectLoop);
            }
            cur = cur.get_redirect_entry()?;
        }
        Ok(cur)
    }

    /// libzim parity: returns the [`Item`] for this entry. If `follow` is
    /// true, redirect chains are resolved transparently; otherwise calling
    /// this on a redirect returns [`Error::NotAnItem`].
    pub fn get_item(&self, follow: bool) -> Result<Item> {
        let mut entry = self.clone();
        let mut hops = 0;
        while entry.is_redirect() {
            if !follow {
                return Err(Error::NotAnItem);
            }
            hops += 1;
            if hops > MAX_REDIRECTS {
                return Err(Error::RedirectLoop);
            }
            entry = entry.get_redirect_entry()?;
        }
        let article = match entry.dirent {
            Dirent::Article(a) => a,
            Dirent::Redirect(_) => unreachable!(),
        };
        Ok(Item {
            archive: entry.archive,
            url_index: entry.url_index,
            article,
        })
    }

    /// Resolve a single redirect step. Returns Self on a non-redirect.
    pub fn get_redirect_entry(&self) -> Result<Entry> {
        if let Dirent::Redirect(r) = &self.dirent {
            self.archive.entry_by_url_index(r.redirect_index)
        } else {
            Ok(self.clone())
        }
    }
}

impl std::fmt::Display for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}/{} \"{}\"",
            if self.is_redirect() { "↪ " } else { "" },
            char::from(self.namespace()),
            self.path(),
            self.title()
        )
    }
}

/// Where an item's bytes physically live in the on-disk ZIM file,
/// returned by [`Archive::blob_direct_access`]. When `is_direct` is
/// `false` the item is stored in a compressed cluster and must be
/// fetched via the normal `get_data` path; offset/size are zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectAccess {
    /// `true` iff the item is in an uncompressed cluster and its
    /// bytes can be `pread`/`mmap`'d straight from the ZIM file.
    pub is_direct: bool,
    /// Absolute byte offset in the ZIM file at which the item's bytes
    /// begin. Only meaningful if `is_direct` is `true`.
    pub file_offset: u64,
    /// Length of the item's bytes on disk. Equals the decompressed
    /// size for uncompressed clusters. Only meaningful if `is_direct`
    /// is `true`.
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct Item {
    archive: Archive,
    url_index: u32,
    article: ArticleEntry,
}

impl Item {
    pub fn path(&self) -> &str {
        &self.article.url
    }

    pub fn title(&self) -> &str {
        self.article.title()
    }

    /// Single-byte namespace this item belongs to (e.g. `b'C'`, `b'A'`).
    /// On items reached via redirect-following lookups this is the
    /// resolved entry's namespace, which may differ from the source
    /// entry's namespace on legacy cross-namespace redirects.
    pub fn namespace(&self) -> u8 {
        self.article.namespace
    }

    pub fn mimetype(&self) -> &str {
        self.archive
            .core
            .mimes
            .get(self.article.mimetype)
            .unwrap_or("application/octet-stream")
    }

    pub fn index(&self) -> u32 {
        self.url_index
    }

    pub fn cluster_index(&self) -> u32 {
        self.article.cluster
    }

    pub fn blob_index(&self) -> u32 {
        self.article.blob
    }

    /// Borrow the archive this item came from. Useful for callers that
    /// want to call archive-level helpers (e.g. `blob_direct_access`)
    /// without having to thread the archive through separately.
    pub fn archive(&self) -> &Archive {
        &self.archive
    }

    /// Total decompressed size of the underlying blob in bytes.
    pub fn size(&self) -> Result<u64> {
        let direct = self
            .archive
            .blob_direct_access(self.cluster_index(), self.blob_index())?;
        if direct.is_direct {
            return Ok(direct.size);
        }
        let blob = self.get_data()?;
        Ok(blob.size() as u64)
    }

    /// Read at a byte offset within the item, returning zero at or beyond
    /// its end. Uncompressed items read directly from the open archive file.
    /// Compressed items use the shared decompressed cluster cache.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let direct = self
            .archive
            .blob_direct_access(self.cluster_index(), self.blob_index())?;
        if direct.is_direct {
            if offset >= direct.size {
                return Ok(0);
            }
            let length = (direct.size - offset).min(buf.len() as u64) as usize;
            return self
                .archive
                .read_at(direct.file_offset + offset, &mut buf[..length]);
        }
        let blob = self.get_data()?;
        if offset >= blob.size() as u64 {
            return Ok(0);
        }
        Ok((&blob.data()[offset as usize..]).read(buf)?)
    }

    // ---- ergonomic content accessors ----

    /// Fetch the item's decompressed bytes. Shortcut for
    /// `item.get_data()?.to_vec()`.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(self.get_data()?.to_vec())
    }

    /// Fetch the item's content as a UTF-8 [`String`]. Returns
    /// [`Error::BadUtf8`] if the content is not valid UTF-8.
    pub fn text(&self) -> Result<String> {
        let bytes = self.get_data()?;
        std::str::from_utf8(bytes.data())
            .map(str::to_owned)
            .map_err(|_| Error::BadUtf8(0))
    }

    /// True if the mimetype is `text/html` (with any charset parameter).
    pub fn is_html(&self) -> bool {
        let m = self.mimetype();
        m.starts_with("text/html") || m.starts_with("application/xhtml")
    }

    /// True if the mimetype starts with `text/`.
    pub fn is_text(&self) -> bool {
        self.mimetype().starts_with("text/")
    }

    /// True if the mimetype starts with `image/`.
    pub fn is_image(&self) -> bool {
        self.mimetype().starts_with("image/")
    }

    /// Fetch the blob bytes (decompressing the cluster if needed).
    pub fn get_data(&self) -> Result<Blob> {
        let (cluster, range) = self.pinned_blob()?;
        Ok(Blob {
            payload: cluster.payload().clone(),
            range,
        })
    }

    /// Warm the OS page cache by reading this uncompressed item's byte
    /// range sequentially with a bounded buffer. Compressed items are skipped.
    /// The operating system determines how long warmed pages stay resident.
    pub fn warmup(&self) -> Result<()> {
        let direct = self
            .archive
            .blob_direct_access(self.cluster_index(), self.blob_index())?;
        if !direct.is_direct {
            return Ok(());
        }
        let range = self
            .archive
            .core
            .storage
            .range(direct.file_offset, direct.size)?;
        io::copy(
            &mut self.archive.core.storage.reader(range),
            &mut io::sink(),
        )?;
        Ok(())
    }

    /// Resolve the underlying cluster (kept alive by an `Arc`) and the
    /// byte range of this item within the decompressed payload, with
    /// no per-call wrapper allocation. Designed for zero-allocation
    /// FFI consumers — the C ABI's `zimru_item_blob_view` calls this
    /// to skip the per-call `Box::new(zimru_blob_t)` that the
    /// `get_data → zimru_blob_t` path pays.
    ///
    /// Pinning the cluster `Arc` keeps the decompressed payload alive
    /// even if the cluster cache evicts the entry mid-use, so callers
    /// can hold the returned slice across LRU pressure.
    pub fn pinned_blob(&self) -> Result<(Arc<Cluster>, std::ops::Range<usize>)> {
        let cluster = self.archive.load_cluster(self.article.cluster)?;
        let range = cluster.blob_range(self.article.blob).map_err(|e| match e {
            Error::BadBlobIndex { blob, count, .. } => Error::BadBlobIndex {
                cluster: self.article.cluster,
                blob,
                count,
            },
            other => other,
        })?;
        Ok((cluster, range))
    }
}

#[derive(Clone, Debug)]
pub struct Blob {
    payload: Arc<[u8]>,
    range: std::ops::Range<usize>,
}

impl Blob {
    pub fn data(&self) -> &[u8] {
        &self.payload[self.range.clone()]
    }

    pub fn size(&self) -> usize {
        self.range.len()
    }

    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.data().to_vec()
    }

    /// Borrow the blob's bytes as `&str` if they're valid UTF-8. Zero-copy.
    pub fn as_str(&self) -> Result<&str> {
        std::str::from_utf8(self.data()).map_err(|_| Error::BadUtf8(0))
    }

    /// A `std::io::Read` cursor over the blob that starts at byte 0.
    /// Useful for passing blob content to streaming parsers (e.g. gzip
    /// inflaters, serde_json readers) without materializing a `Vec<u8>`.
    pub fn reader(&self) -> std::io::Cursor<&[u8]> {
        std::io::Cursor::new(self.data())
    }
}

impl std::ops::Deref for Blob {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.data()
    }
}

impl AsRef<[u8]> for Blob {
    fn as_ref(&self) -> &[u8] {
        self.data()
    }
}

#[derive(Clone, Copy, Debug)]
enum IterKind {
    ByPath,
    ByTitle,
}

pub struct EntryIter {
    archive: Archive,
    kind: IterKind,
    indices: Range<u32>,
    init_error: Option<Error>,
}

impl EntryIter {
    fn new(archive: &Archive, kind: IterKind, range: Result<Range<u32>>) -> Self {
        let (indices, init_error) = match range {
            Ok(indices) => (indices, None),
            Err(e) => (0..0, Some(e)),
        };
        Self {
            archive: archive.clone(),
            kind,
            indices,
            init_error,
        }
    }
}

impl Iterator for EntryIter {
    type Item = Result<Entry>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(e) = self.init_error.take() {
            return Some(Err(e));
        }
        let idx = self.indices.next()?;
        Some(match self.kind {
            IterKind::ByPath => self.archive.read_entry(idx, Access::Scan),
            IterKind::ByTitle => self.archive.entry_by_title_index(idx),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.indices.len() + usize::from(self.init_error.is_some());
        (n, Some(n))
    }
}

impl ExactSizeIterator for EntryIter {}
impl std::iter::FusedIterator for EntryIter {}

/// Iterator returned by [`Archive::by_prefix`] — yields every dirent whose
/// `(namespace, url)` starts with the given prefix, in URL-pointer order.
pub struct PrefixIter {
    entries: EntryIter,
    ns: u8,
    prefix: String,
}

impl Iterator for PrefixIter {
    type Item = Result<Entry>;
    fn next(&mut self) -> Option<Self::Item> {
        let entry = match self.entries.next()? {
            Ok(e) => e,
            Err(e) => return Some(Err(e)),
        };
        // First (namespace, url) outside the prefix range ends iteration.
        if entry.namespace() != self.ns || !entry.path().starts_with(&self.prefix) {
            self.entries.indices = 0..0;
            return None;
        }
        Some(Ok(entry))
    }
}

impl std::iter::FusedIterator for PrefixIter {}

/// Snapshot of an archive's header + aggregate counts, returned by
/// [`Archive::summary`].
#[derive(Debug, Clone)]
pub struct Summary {
    pub uuid: crate::Uuid,
    pub major_version: u16,
    pub minor_version: u16,
    pub uses_new_namespaces: bool,
    pub entry_count: u32,
    pub content_entry_count: u32,
    pub cluster_count: u32,
    pub mime_type_count: usize,
    pub has_main_entry: bool,
    pub main_path: Option<String>,
    pub has_checksum: bool,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "uuid            {}", self.uuid)?;
        writeln!(
            f,
            "version         {}.{}{}",
            self.major_version,
            self.minor_version,
            if self.uses_new_namespaces {
                " (new namespaces)"
            } else {
                " (legacy)"
            }
        )?;
        writeln!(
            f,
            "entries         {} total ({} content)",
            self.entry_count, self.content_entry_count
        )?;
        writeln!(f, "clusters        {}", self.cluster_count)?;
        writeln!(f, "mime types      {}", self.mime_type_count)?;
        if let Some(p) = &self.main_path {
            writeln!(f, "main page       {}", p)?;
        }
        writeln!(
            f,
            "checksum        {}",
            if self.has_checksum { "yes" } else { "no" }
        )?;
        Ok(())
    }
}
