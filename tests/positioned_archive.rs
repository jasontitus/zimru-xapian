//! Positioned access to a single extended cluster with a sparse multi-GiB blob.
use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

use zimru::{Archive, ArchiveLimits, ClusterReadLimits};

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "zimru-archive-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_archive(mut f: File, blob_size: u64) -> u64 {
    let mut header = [0u8; 80];
    header[..4].copy_from_slice(&0x044d495au32.to_le_bytes());
    header[4..6].copy_from_slice(&6u16.to_le_bytes());
    header[6..8].copy_from_slice(&1u16.to_le_bytes());
    header[24..28].copy_from_slice(&1u32.to_le_bytes());
    header[28..32].copy_from_slice(&1u32.to_le_bytes());
    for (i, ptr) in [112u64, 120, 124, 80].into_iter().enumerate() {
        header[32 + i * 8..40 + i * 8].copy_from_slice(&ptr.to_le_bytes());
    }
    header[64..72].fill(0xff);
    f.write_all(&header).unwrap();
    f.write_all(b"application/octet-stream\0\0").unwrap();
    f.seek(SeekFrom::Start(112)).unwrap();
    f.write_all(&4092u64.to_le_bytes()).unwrap(); // dirent spans pages
    f.write_all(&0u32.to_le_bytes()).unwrap();
    f.write_all(&16384u64.to_le_bytes()).unwrap();
    f.seek(SeekFrom::Start(4092)).unwrap();
    let mut dirent = [0u8; 16];
    dirent[3] = b'C';
    f.write_all(&dirent).unwrap();
    f.write_all(b"index\0").unwrap();
    f.write_all(&vec![b't'; 9000]).unwrap();
    f.write_all(&[0]).unwrap();
    f.seek(SeekFrom::Start(16384)).unwrap();
    f.write_all(&[0x11]).unwrap(); // uncompressed extended offsets
    f.write_all(&16u64.to_le_bytes()).unwrap();
    f.write_all(&(16 + blob_size).to_le_bytes()).unwrap();
    let end = 16384 + 1 + 16 + blob_size;
    f.seek(SeekFrom::Start(end - 4)).unwrap();
    f.write_all(b"tail").unwrap();
    end
}

#[test]
#[cfg(unix)] // sparse file semantics
fn large_blob_tail_reads_use_the_original_handle_and_skip_the_cluster_cache() {
    let tmp = TempDir::new();
    let path = tmp.0.join("sparse.zim");
    let size = (1 << 32) + 81;
    let end = write_archive(File::create(&path).unwrap(), size);
    let archive = Archive::from_file(File::open(&path).unwrap()).unwrap();
    fs::rename(&path, tmp.0.join("old.zim")).unwrap();
    fs::write(&path, b"independent file").unwrap();
    let item = archive.get_item("index").unwrap();
    assert_eq!(item.title().len(), 9000);
    assert_eq!(archive.file_len(), end);
    assert_eq!(item.size().unwrap(), size);
    archive.validate_cluster_references(0, &[0]).unwrap();

    let mut buf = [0; 8];
    assert_eq!(item.read_at(size - 4, &mut buf).unwrap(), 4);
    assert_eq!(&buf[..4], b"tail");
    assert_eq!(item.read_at(size, &mut buf).unwrap(), 0);
    assert_eq!(archive.read_at(end - 4, &mut buf).unwrap(), 4);
    assert_eq!(&buf[..4], b"tail");
    assert_eq!(archive.read_at(u64::MAX, &mut buf).unwrap(), 0);
    assert_eq!(archive.cluster_cache_stats().entries, 0);
}

#[test]
fn caller_limits_apply_to_counts_metadata_and_cluster_materialization() {
    let tmp = TempDir::new();
    let path = tmp.0.join("limits.zim");
    write_archive(File::create(&path).unwrap(), 1024);
    let open = |limits| Archive::from_file_with_limits(File::open(&path).unwrap(), limits);
    let cluster = ClusterReadLimits {
        max_stored_bytes: 64,
        max_decoded_bytes: 2048,
        xz_memory_bytes: 128 << 20,
        zstd_window_log: 26,
    };
    let policy = ArchiveLimits {
        max_entries: 1,
        max_clusters: 1,
        cluster: Some(cluster),
        ..ArchiveLimits::default()
    };
    assert!(open(ArchiveLimits {
        max_entries: 0,
        ..policy
    })
    .is_err());
    assert!(open(ArchiveLimits {
        max_clusters: 0,
        ..policy
    })
    .is_err());
    assert!(open(ArchiveLimits {
        max_metadata_record_bytes: 79,
        ..policy
    })
    .is_err());
    let small_metadata = open(ArchiveLimits {
        max_metadata_record_bytes: 80,
        ..policy
    })
    .unwrap();
    assert!(small_metadata
        .get_item("index")
        .unwrap_err()
        .to_string()
        .contains("metadata record exceeds"));
    let archive = open(policy).unwrap();
    let item = archive.get_item("index").unwrap();
    let mut tail = [0; 4];
    assert_eq!(item.read_at(1020, &mut tail).unwrap(), 4);
    assert_eq!(&tail, b"tail");
    assert!(item.get_data().is_err());
    assert!(archive.cluster(0).is_err());
    assert!(archive
        .cluster_with_limits(
            0,
            ClusterReadLimits {
                max_stored_bytes: 2048,
                ..cluster
            }
        )
        .is_err());
    assert_eq!(archive.cluster_cache_stats().entries, 0);
    let unrestricted = open(ArchiveLimits::default()).unwrap();
    assert_eq!(
        unrestricted.cluster(0).unwrap().blob(0).unwrap().len(),
        1024
    );
    assert!(unrestricted.cluster_with_limits(0, cluster).is_err());
}

#[cfg(feature = "legacy-mmap")]
#[test]
fn legacy_map_and_positioned_reader_agree() {
    use std::io::Read;
    let tmp = TempDir::new();
    let path = tmp.0.join("mapped.zim");
    let end = write_archive(File::create(&path).unwrap(), 120);
    let mut map = memmap2::MmapMut::map_anon(end as usize).unwrap();
    File::open(&path).unwrap().read_exact(&mut map).unwrap();
    let mapped = Archive::from_mmap(map.make_read_only().unwrap()).unwrap();
    let file = Archive::open(path).unwrap();
    assert_eq!(
        mapped.get_bytes("index").unwrap(),
        file.get_bytes("index").unwrap()
    );
    assert_eq!(
        mapped.get_item("index").unwrap().title(),
        file.get_item("index").unwrap().title()
    );
}

#[test]
fn header_title_table_can_alias_an_uncompressed_listing_blob() {
    let tmp = TempDir::new();
    let path = tmp.0.join("embedded-titles.zim");
    let mut buf = [0u8; 512];
    buf[..4].copy_from_slice(&0x044d495au32.to_le_bytes());
    buf[4..6].copy_from_slice(&6u16.to_le_bytes());
    buf[6..8].copy_from_slice(&1u16.to_le_bytes());
    buf[24..28].copy_from_slice(&2u32.to_le_bytes());
    buf[28..32].copy_from_slice(&1u32.to_le_bytes());
    let cluster = 256usize;
    let title = cluster + 1 + 16; // blob one, after four content bytes
    for (i, ptr) in [112u64, title as u64, 128, 80].into_iter().enumerate() {
        buf[32 + i * 8..40 + i * 8].copy_from_slice(&ptr.to_le_bytes());
    }
    buf[64..72].fill(0xff);
    buf[80..92].copy_from_slice(b"text/plain\0\0");
    buf[112..120].copy_from_slice(&136u64.to_le_bytes());
    buf[120..128].copy_from_slice(&168u64.to_le_bytes());
    buf[128..136].copy_from_slice(&(cluster as u64).to_le_bytes());
    buf[139] = b'C';
    buf[152..158].copy_from_slice(b"home\0\0");
    buf[171] = b'X';
    buf[180..184].copy_from_slice(&1u32.to_le_bytes());
    let name = b"listing/titleOrdered/v0\0\0";
    buf[184..184 + name.len()].copy_from_slice(name);
    buf[cluster] = 1;
    for (i, offset) in [12u32, 16, 24].into_iter().enumerate() {
        buf[cluster + 1 + i * 4..cluster + 5 + i * 4].copy_from_slice(&offset.to_le_bytes());
    }
    buf[cluster + 13..cluster + 17].copy_from_slice(b"home");
    buf[title + 4..title + 8].copy_from_slice(&1u32.to_le_bytes());
    fs::write(&path, &buf[..title + 8]).unwrap();
    let archive = Archive::open(path).unwrap();
    assert_eq!(
        archive.cluster_byte_range(0).unwrap(),
        cluster as u64..(title + 8) as u64
    );
    assert_eq!(&*archive.title_listing().unwrap(), &[0, 1]);
    assert!(archive.check_title_index().unwrap());
    archive.validate_cluster_references(0, &[0, 1]).unwrap();
    assert_eq!(archive.cluster_cache_stats().entries, 0);
}

#[test]
fn iterator_setup_errors_are_yielded_once_with_an_exact_size_hint() {
    let tmp = TempDir::new();
    let path = tmp.0.join("long-url.zim");
    write_archive(File::create(&path).unwrap(), 4);
    // Reuse the long title as part of a long URL, leaving an empty title.
    let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(4092 + 16 + 5)).unwrap();
    file.write_all(b"-").unwrap();
    drop(file);
    let limits = ArchiveLimits {
        max_metadata_record_bytes: 80,
        ..Default::default()
    };
    let archive = Archive::from_file_with_limits(File::open(path).unwrap(), limits).unwrap();
    let mut titles = archive.iter_by_title();
    assert_eq!(titles.size_hint(), (1, Some(1)));
    assert_eq!(titles.len(), 1);
    assert!(titles.next().unwrap().is_err());
    assert_eq!(titles.size_hint(), (0, Some(0)));
    assert!(titles.next().is_none());
    assert!(titles.next().is_none());

    let mut prefix = archive.by_prefix(b'C', "not present");
    assert!(prefix.next().unwrap().is_err());
    assert!(prefix.next().is_none());
    assert!(prefix.next().is_none());
}

#[test]
fn simultaneous_readers_share_the_same_cached_payload() {
    use std::sync::{Arc, Barrier};
    let tmp = TempDir::new();
    let path = tmp.0.join("shared-cluster.zim");
    write_archive(File::create(&path).unwrap(), 2 << 20);
    let archive = Archive::open(path).unwrap();
    let gate = Barrier::new(12);
    let clusters = std::thread::scope(|scope| {
        let workers = (0..12)
            .map(|_| {
                scope.spawn(|| {
                    gate.wait();
                    archive.cluster(0).unwrap()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(clusters.iter().all(|c| Arc::ptr_eq(&clusters[0], c)));
    let stats = archive.cluster_cache_stats();
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.hits + stats.misses, 12);
}

#[test]
fn caller_supplied_source_serves_metadata_and_content_without_a_path() {
    use std::io;
    use std::sync::Arc;
    #[derive(Debug)]
    struct Bytes(Vec<u8>);
    impl zimru::archive::PositionedInput for Bytes {
        fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
            let start = usize::try_from(offset).map_err(io::Error::other)?;
            let input = self
                .0
                .get(start..)
                .and_then(|s| s.get(..destination.len()))
                .ok_or(io::ErrorKind::UnexpectedEof)?;
            destination.copy_from_slice(input);
            Ok(())
        }
    }
    let tmp = TempDir::new();
    let path = tmp.0.join("virtual.zim");
    let size = write_archive(File::create(&path).unwrap(), 32);
    let bytes = Arc::new(Bytes(fs::read(&path).unwrap()));
    fs::remove_file(path).unwrap();
    let archive = Archive::from_positioned(bytes.clone(), size, ArchiveLimits::default()).unwrap();
    let mut tail = [0; 4];
    assert_eq!(
        archive
            .get_item("index")
            .unwrap()
            .read_at(28, &mut tail)
            .unwrap(),
        4
    );
    assert_eq!(&tail, b"tail");
    assert!(Archive::from_positioned(bytes, 4, ArchiveLimits::default()).is_err());
}
