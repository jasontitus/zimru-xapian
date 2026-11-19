//! Cluster reader.
//!
//! On-disk layout of a cluster:
//!
//! ```text
//! u8  info_byte
//!     bits 0..3 : compression type
//!                 0 = none (legacy default)
//!                 1 = none
//!                 2 = zlib   (deprecated, not supported)
//!                 3 = bzip2  (deprecated, not supported)
//!                 4 = xz / lzma2
//!                 5 = zstd
//!     bit  4    : extended flag
//!                 if set, blob offsets in the payload are u64 (8 bytes)
//!                 otherwise they are u32 (4 bytes).
//!
//! ── decompressed payload ──
//! offsets: [P; N+1]   where P = u32 or u64 depending on extended flag
//! blob[0] data
//! blob[1] data
//! ...
//! blob[N-1] data
//! ```
//!
//! `offsets[0]` always equals `(N+1) * sizeof(P)` so the blob count `N`
//! can be derived from the first offset alone.

use std::io::{BufReader, Read};
use std::ops::Range;
use std::sync::Arc;

use crate::error::{Error, Result};

const COMPRESSION_NONE_LEGACY: u8 = 0;
const COMPRESSION_NONE: u8 = 1;
const COMPRESSION_ZLIB: u8 = 2;
const COMPRESSION_BZIP2: u8 = 3;
const COMPRESSION_XZ: u8 = 4;
const COMPRESSION_ZSTD: u8 = 5;

const EXTENDED_FLAG: u8 = 0x10;

/// Decompressed-size ceiling for standard (u32 blob offsets) clusters.
/// The format cannot address payload bytes past `u32::MAX` with 4-byte
/// offsets, so anything larger is corrupt input or a decompression bomb.
pub const MAX_STANDARD_CLUSTER_BYTES: u64 = u32::MAX as u64;

/// Decompressed-size ceiling for extended (u64 blob offsets) clusters —
/// a defensive bound so a crafted cluster in an untrusted archive cannot
/// grow the decode buffer without limit and OOM the process. 16 GiB,
/// spelled as a decimal literal so the cbindgen-exported C macro stays a
/// well-defined `long long` constant (a `16 << 30` would overflow `int`).
pub const MAX_EXTENDED_CLUSTER_BYTES: u64 = 17_179_869_184;

/// Bounds for materializing a single archive cluster. The stored byte count
/// includes the info byte and any padding in the physical cluster interval.
/// Decoder working memory is additional to the returned
/// payload. All fields must be supplied by the caller.
#[derive(Clone, Copy, Debug)]
pub struct ClusterReadLimits {
    pub max_stored_bytes: u64,
    pub max_decoded_bytes: u64,
    pub xz_memory_bytes: u64,
    /// Base-two log of the largest permitted Zstandard history window (10..=31).
    pub zstd_window_log: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Xz,
    Zstd,
}

#[cfg_attr(feature = "retained-cluster-buffers", path = "cluster/retained.rs")]
#[cfg_attr(not(feature = "retained-cluster-buffers"), path = "cluster/inline.rs")]
mod payload;
use payload::Payload;
pub(crate) use payload::PayloadPin;

#[derive(Debug)]
pub struct Cluster {
    /// Decompressed (or pass-through) payload, NOT including the info byte.
    payload: Payload,
    extended: bool,
    blob_count: u32,
    compression: Compression,
}

impl Cluster {
    /// Decode a cluster from its on-disk bytes (including the info byte).
    pub fn parse(raw: &[u8]) -> Result<Self> {
        if raw.is_empty() {
            return Err(Error::Truncated(0));
        }
        let info = raw[0];
        let compression_id = info & 0x0F;
        let extended = info & EXTENDED_FLAG != 0;
        let body = &raw[1..];

        let limit = if extended {
            MAX_EXTENDED_CLUSTER_BYTES
        } else {
            MAX_STANDARD_CLUSTER_BYTES
        };
        let (payload, compression): (Arc<[u8]>, Compression) = match compression_id {
            COMPRESSION_NONE_LEGACY | COMPRESSION_NONE => {
                validate_offsets(body, extended)?;
                (Arc::from(body), Compression::None)
            }
            COMPRESSION_XZ => (Arc::from(decode_xz(body, limit)?), Compression::Xz),
            COMPRESSION_ZSTD => (
                Arc::from(crate::codec_zstd::exact(body, limit)?),
                Compression::Zstd,
            ),
            COMPRESSION_ZLIB | COMPRESSION_BZIP2 => {
                return Err(Error::UnsupportedCompression(compression_id));
            }
            other => return Err(Error::UnsupportedCompression(other)),
        };

        let blob_count = if compression == Compression::None {
            offset_count(&payload, extended)?
        } else {
            validate_offsets(&payload, extended)?
        };

        Ok(Cluster {
            payload: payload.into(),
            extended,
            blob_count,
            compression,
        })
    }

    /// Decode a bounded on-disk region. The distance to the next cluster is
    /// only an upper bound, so use the offset table's final value to determine
    /// how much uncompressed payload to retain.
    pub(crate) fn read(
        mut input: impl Read,
        len: u64,
        limits: Option<ClusterReadLimits>,
    ) -> Result<Self> {
        if let Some(policy) = limits {
            if len > policy.max_stored_bytes {
                return Err(Error::Decompression(
                    "stored cluster exceeds byte limit".into(),
                ));
            }
            if !(10..=31).contains(&policy.zstd_window_log) || policy.xz_memory_bytes == 0 {
                return Err(Error::Decompression("invalid decoder memory limits".into()));
            }
        }
        let mut info = [0];
        input.read_exact(&mut info)?;
        let info = info[0];
        let extended = info & EXTENDED_FLAG != 0;
        let limit = if extended {
            MAX_EXTENDED_CLUSTER_BYTES
        } else {
            MAX_STANDARD_CLUSTER_BYTES
        };
        let limit = limits.map_or(limit, |p| limit.min(p.max_decoded_bytes));
        let body_len = len.checked_sub(1).ok_or(Error::Truncated(0))?;
        let id = info & 0x0f;
        if matches!(id, COMPRESSION_NONE | COMPRESSION_NONE_LEGACY) {
            let width = if extended { 8 } else { 4 };
            let mut head = [0u8; 8];
            input.read_exact(&mut head[..width])?;
            let first = read_off(&head, 0, extended)?;
            let offsets = RawOffsets::from_first(width as u64, first, body_len)?;
            if first > limit {
                return Err(Error::Truncated(first));
            }
            let mut payload = head[..width].to_vec();
            read_exact_append(&mut input, first - width as u64, &mut payload)?;
            let last = offsets.validate(&mut &payload[..])?;
            if last > limit {
                return Err(Error::Truncated(last));
            }
            read_exact_append(&mut input, last - first, &mut payload)?;
            return Ok(Self {
                payload: payload.into(),
                extended,
                blob_count: offsets.count,
                compression: Compression::None,
            });
        }
        let (payload, compression) = match id {
            COMPRESSION_XZ => {
                let (payload, _) = crate::codec_xz::decode(
                    BufReader::new(input.take(body_len)),
                    body_len,
                    limit,
                    limits.map(|p| p.xz_memory_bytes),
                )?;
                (payload, Compression::Xz)
            }
            COMPRESSION_ZSTD => (
                crate::codec_zstd::region(
                    input,
                    body_len,
                    limit,
                    limits.map(|p| p.zstd_window_log),
                )?,
                Compression::Zstd,
            ),
            other => return Err(Error::UnsupportedCompression(other)),
        };
        let blob_count = validate_offsets(&payload, extended)?;
        Ok(Self {
            payload: payload.into(),
            extended,
            blob_count,
            compression,
        })
    }

    pub fn compression(&self) -> Compression {
        self.compression
    }

    pub fn blob_count(&self) -> u32 {
        self.blob_count
    }

    /// Returns a shareable slice of the whole payload. With retained cluster
    /// buffers enabled, the first call may convert a decoded buffer to inline
    /// Arc storage. Existing item blobs stay valid until their pins are dropped.
    pub fn payload(&self) -> &Arc<[u8]> {
        self.payload.shared()
    }

    pub(crate) fn pin_payload(&self) -> PayloadPin {
        self.payload.pin()
    }

    pub(crate) fn payload_len(&self) -> usize {
        self.payload.len()
    }

    pub fn blob(&self, idx: u32) -> Result<&[u8]> {
        let r = self.blob_range(idx)?;
        Ok(&self.payload()[r])
    }

    pub fn blob_range(&self, idx: u32) -> Result<Range<usize>> {
        self.payload.with_bytes(|bytes| self.range_in(idx, bytes))
    }

    fn range_in(&self, idx: u32, payload: &[u8]) -> Result<Range<usize>> {
        if idx >= self.blob_count {
            return Err(Error::BadBlobIndex {
                cluster: u32::MAX,
                blob: idx,
                count: self.blob_count,
            });
        }
        let ptr_size = if self.extended { 8 } else { 4 };
        let off_pos = idx as usize * ptr_size;
        let start = read_off(payload, off_pos, self.extended)?;
        let end = read_off(payload, off_pos + ptr_size, self.extended)?;
        let first = (self.blob_count as u64 + 1) * ptr_size as u64;
        if start < first || end < start || end > payload.len() as u64 {
            return Err(Error::Truncated(end));
        }
        let (start, end) = (start as usize, end as usize);
        Ok(start..end)
    }
}

/// Shape of an uncompressed blob-offset table. All positions here are
/// relative to the first byte following the cluster's info byte.
pub(crate) struct RawOffsets {
    count: u32,
    width: u64,
    first: u64,
    body_len: u64,
}

impl RawOffsets {
    pub(crate) fn open(
        info: u8,
        body_len: u64,
        read_first: impl FnOnce(u64) -> Result<u64>,
    ) -> Result<Option<Self>> {
        match info & 0x0f {
            COMPRESSION_NONE | COMPRESSION_NONE_LEGACY => {}
            COMPRESSION_XZ | COMPRESSION_ZSTD => return Ok(None),
            id => return Err(Error::UnsupportedCompression(id)),
        }
        let width = if info & EXTENDED_FLAG != 0 { 8 } else { 4 };
        if body_len < width {
            return Err(Error::Truncated(body_len));
        }
        Self::from_first(width, read_first(width)?, body_len).map(Some)
    }

    fn from_first(width: u64, first: u64, body_len: u64) -> Result<Self> {
        if first < width || !first.is_multiple_of(width) || first > body_len {
            return Err(Error::Truncated(first));
        }
        let count = u32::try_from(first / width - 1).map_err(|_| Error::Truncated(first))?;
        Ok(Self {
            count,
            width,
            first,
            body_len,
        })
    }

    pub(crate) fn width(&self) -> u64 {
        self.width
    }

    pub(crate) fn check_index(&self, blob: u32) -> Result<()> {
        if blob < self.count {
            return Ok(());
        }
        Err(Error::BadBlobIndex {
            cluster: u32::MAX,
            blob,
            count: self.count,
        })
    }

    pub(crate) fn blob_range(
        &self,
        blob: u32,
        mut read: impl FnMut(u64) -> Result<u64>,
    ) -> Result<Range<u64>> {
        self.check_index(blob)?;
        let pos = u64::from(blob) * self.width;
        let start = read(pos)?;
        let end = read(pos + self.width)?;
        if start < self.first || start > end || end > self.body_len {
            return Err(Error::Truncated(end));
        }
        Ok(start..end)
    }

    /// Validate offsets in fixed-size batches, without reading blob contents.
    pub(crate) fn validate(&self, input: &mut impl Read) -> Result<u64> {
        let mut remaining = self.first;
        let mut block = [0; 8192];
        let mut previous = self.first;
        while remaining != 0 {
            let n = remaining.min(block.len() as u64) as usize;
            input.read_exact(&mut block[..n])?;
            for (i, ptr) in block[..n].chunks_exact(self.width as usize).enumerate() {
                let next = read_off(ptr, 0, self.width == 8)?;
                if next < previous
                    || next > self.body_len
                    || (remaining == self.first && i == 0 && next != self.first)
                {
                    return Err(Error::Truncated(next));
                }
                previous = next;
            }
            remaining -= n as u64;
        }
        Ok(previous)
    }
}

fn read_exact_append(input: &mut impl Read, len: u64, out: &mut Vec<u8>) -> Result<()> {
    let n = input.take(len).read_to_end(out)?;
    if n as u64 != len {
        return Err(Error::Truncated(n as u64));
    }
    Ok(())
}

fn offset_count(body: &[u8], extended: bool) -> Result<u32> {
    let width = if extended { 8 } else { 4 };
    let first = read_off(body, 0, extended)?;
    Ok(RawOffsets::from_first(width, first, body.len() as u64)?.count)
}

fn validate_offsets(body: &[u8], extended: bool) -> Result<u32> {
    let count = offset_count(body, extended)?;
    let width = if extended { 8 } else { 4 };
    let mut prev = (count as u64 + 1) * width as u64;
    for i in 1..=count as usize {
        let next = read_off(body, i * width, extended)?;
        if next < prev || next > body.len() as u64 {
            return Err(Error::Truncated(next));
        }
        prev = next;
    }
    Ok(count)
}

fn read_off(buf: &[u8], off: usize, extended: bool) -> Result<u64> {
    if extended {
        crate::raw::u64_at(buf, off)
    } else {
        crate::raw::u32_at(buf, off).map(u64::from)
    }
}

fn decode_xz(body: &[u8], limit: u64) -> Result<Vec<u8>> {
    let (out, consumed) = crate::codec_xz::decode(body, body.len() as u64, limit, None)?;
    if consumed != body.len() as u64 {
        return Err(Error::Decompression(
            "xz: trailing data after stream".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_uncompressed_cluster(blobs: &[&[u8]], extended: bool) -> Vec<u8> {
        let n = blobs.len();
        let ptr_size = if extended { 8 } else { 4 };
        let header_len = (n + 1) * ptr_size;
        let mut out = Vec::new();
        out.push(if extended { 0x11 } else { 0x01 });
        let mut cursor = header_len as u64;
        for blob in blobs {
            push_off(&mut out, cursor, extended);
            cursor += blob.len() as u64;
        }
        push_off(&mut out, cursor, extended);
        for blob in blobs {
            out.extend_from_slice(blob);
        }
        out
    }

    fn push_off(v: &mut Vec<u8>, x: u64, extended: bool) {
        if extended {
            v.extend_from_slice(&x.to_le_bytes());
        } else {
            v.extend_from_slice(&(x as u32).to_le_bytes());
        }
    }

    #[test]
    fn guarded_decode_accepts_exact_limits_for_each_codec_and_extended_offsets() {
        use std::io::Write;
        let raw = build_uncompressed_cluster(&[b"eng"], true);
        let body = &raw[1..];
        let mut xz = xz2::write::XzEncoder::new(Vec::new(), 1);
        xz.write_all(body).unwrap();
        for (id, encoded) in [
            (1, body.to_vec()),
            (4, xz.finish().unwrap()),
            (5, zstd::stream::encode_all(body, 1).unwrap()),
        ] {
            let mut bytes = vec![id | EXTENDED_FLAG];
            bytes.extend(encoded);
            let len = bytes.len() as u64;
            let limits = ClusterReadLimits {
                max_stored_bytes: len,
                max_decoded_bytes: body.len() as u64,
                xz_memory_bytes: 128 << 20,
                zstd_window_log: 26,
            };
            let decode = |p| Cluster::read(&bytes[..], len, Some(p));
            assert_eq!(decode(limits).unwrap().blob(0).unwrap(), b"eng");
            assert!(decode(ClusterReadLimits {
                max_stored_bytes: len - 1,
                ..limits
            })
            .is_err());
            assert!(decode(ClusterReadLimits {
                max_decoded_bytes: body.len() as u64 - 1,
                ..limits
            })
            .is_err());
        }
    }

    #[test]
    fn zstd_physical_ranges_allow_padding_and_continue_complete_frames() {
        let body = &build_uncompressed_cluster(&[b"language"], true)[1..];
        let first = zstd::stream::encode_all(&body[..7], 1).unwrap();
        let second = zstd::stream::encode_all(&body[7..], 1).unwrap();
        let skip = [0x5f, 0x2a, 0x4d, 0x18, 2, 0, 0, 0, 1, 2];
        let mut bytes = vec![COMPRESSION_ZSTD | EXTENDED_FLAG];
        bytes.extend(first);
        bytes.extend(skip);
        bytes.extend(second);
        let end = bytes.len();
        assert_eq!(
            Cluster::parse(&bytes).unwrap().blob(0).unwrap(),
            b"language"
        );
        bytes.resize(end + 71, 0);
        let limits = ClusterReadLimits {
            max_stored_bytes: bytes.len() as u64,
            max_decoded_bytes: body.len() as u64,
            xz_memory_bytes: 1 << 26,
            zstd_window_log: 26,
        };
        for policy in [None, Some(limits)] {
            let read = |buf: &[u8]| Cluster::read(buf, buf.len() as u64, policy);
            assert_eq!(read(&bytes).unwrap().blob(0).unwrap(), b"language");
            assert!(read(&bytes[..end - 1]).is_err());
            let mut partial = bytes[..end].to_vec();
            partial.extend([0x28, 0xb5]);
            assert!(read(&partial).is_err());
        }
        assert!(Cluster::parse(&bytes).is_err());
        let stored = ClusterReadLimits {
            max_stored_bytes: end as u64,
            ..limits
        };
        assert!(Cluster::read(&bytes[..], bytes.len() as u64, Some(stored)).is_err());
        let decoded = ClusterReadLimits {
            max_decoded_bytes: body.len() as u64 - 1,
            ..limits
        };
        assert!(Cluster::read(&bytes[..], bytes.len() as u64, Some(decoded)).is_err());
    }

    #[test]
    fn zstd_physical_range_allows_padding_after_pledged_frame() {
        let body = &build_uncompressed_cluster(&[b"language"], false)[1..];
        let mut raw = vec![COMPRESSION_ZSTD];
        raw.extend(zstd::bulk::compress(body, 1).unwrap());
        raw.extend([0; 32]);
        let c = Cluster::read(&raw[..], raw.len() as u64, None).unwrap();
        assert_eq!(c.blob(0).unwrap(), b"language");
        assert!(Cluster::parse(&raw).is_err());
    }

    #[test]
    fn xz_physical_range_ends_at_verified_footer() {
        use std::io::Write;
        let raw = build_uncompressed_cluster(&[b"book"], false);
        let mut encoder = xz2::write::XzEncoder::new(vec![COMPRESSION_XZ], 1);
        encoder.write_all(&raw[1..]).unwrap();
        let mut bytes = encoder.finish().unwrap();
        let end = bytes.len();
        let limits = ClusterReadLimits {
            max_stored_bytes: end as u64 + 17,
            max_decoded_bytes: raw.len() as u64 - 1,
            xz_memory_bytes: 64 << 20,
            zstd_window_log: 26,
        };
        bytes.resize(end + 17, 0);
        for policy in [None, Some(limits)] {
            let read = |buf: &[u8]| Cluster::read(buf, buf.len() as u64, policy);
            assert_eq!(read(&bytes).unwrap().blob(0).unwrap(), b"book");
            assert!(read(&bytes[..end - 1]).is_err());
        }
        assert!(Cluster::parse(&bytes).is_err());
        assert_eq!(
            Cluster::parse(&bytes[..end]).unwrap().blob(0).unwrap(),
            b"book"
        );
    }

    #[test]
    fn exact_zstd_input_supports_concatenated_pledged_frames() {
        let raw = build_uncompressed_cluster(&[b"book"], false);
        let body = &raw[1..];
        let mut bytes = vec![COMPRESSION_ZSTD];
        bytes.extend(zstd::bulk::compress(&body[..8], 1).unwrap());
        bytes.extend(zstd::bulk::compress(&body[8..], 1).unwrap());
        let cluster = Cluster::parse(&bytes).unwrap();
        assert_eq!(cluster.blob(0).unwrap(), b"book");
        assert!(crate::codec_zstd::exact(&bytes[1..], body.len() as u64 - 1).is_err());
        assert!(Cluster::parse(&bytes[..bytes.len() - 1]).is_err());
        bytes.push(0);
        assert!(Cluster::parse(&bytes).is_err());
    }

    #[cfg(feature = "retained-cluster-buffers")]
    #[test]
    fn existing_blob_pins_survive_concurrent_shared_payload_requests() {
        let raw = build_uncompressed_cluster(&[b"abc", b"defg"], false);
        let cluster = Cluster::read(&raw[..], raw.len() as u64, None).unwrap();
        let pin = cluster.pin_payload();
        assert!(matches!(pin, PayloadPin::Decoded(_)));
        assert!(cluster.blob(u32::MAX).is_err());
        assert!(matches!(cluster.pin_payload(), PayloadPin::Decoded(_)));
        let range = cluster.blob_range(1).unwrap();
        assert_eq!(&pin.as_ref()[range.clone()], b"defg");
        std::thread::scope(|scope| {
            let first = scope.spawn(|| cluster.payload().clone());
            let second = scope.spawn(|| cluster.payload().clone());
            assert!(Arc::ptr_eq(&first.join().unwrap(), &second.join().unwrap()));
        });
        if let PayloadPin::Decoded(bytes) = &pin {
            assert_eq!(Arc::strong_count(bytes), 1);
        }
        assert!(matches!(cluster.pin_payload(), PayloadPin::Shared(_)));
        assert_eq!(cluster.blob(1).unwrap(), &pin.as_ref()[range]);
    }

    #[test]
    fn reads_uncompressed_cluster_4byte_offsets() {
        let raw = build_uncompressed_cluster(&[b"hello", b"world", b"!!"], false);
        let c = Cluster::parse(&raw).unwrap();
        assert_eq!(c.compression(), Compression::None);
        assert_eq!(c.blob_count(), 3);
        assert_eq!(c.blob(0).unwrap(), b"hello");
        assert_eq!(c.blob(1).unwrap(), b"world");
        assert_eq!(c.blob(2).unwrap(), b"!!");
        assert!(c.blob(3).is_err());
    }

    #[test]
    fn reads_uncompressed_cluster_8byte_offsets() {
        let raw = build_uncompressed_cluster(&[b"a", b"bb", b"ccc", b"dddd"], true);
        let c = Cluster::parse(&raw).unwrap();
        assert_eq!(c.blob_count(), 4);
        assert_eq!(c.blob(0).unwrap(), b"a");
        assert_eq!(c.blob(3).unwrap(), b"dddd");
    }

    #[test]
    fn reads_zstd_cluster() {
        let inner = build_uncompressed_cluster(&[b"foo", b"barbaz"], false);
        let payload = &inner[1..];
        let compressed = zstd::stream::encode_all(payload, 3).unwrap();
        let mut raw = Vec::with_capacity(1 + compressed.len());
        raw.push(0x05);
        raw.extend_from_slice(&compressed);
        let c = Cluster::parse(&raw).unwrap();
        assert_eq!(c.compression(), Compression::Zstd);
        assert_eq!(c.blob_count(), 2);
        assert_eq!(c.blob(0).unwrap(), b"foo");
        assert_eq!(c.blob(1).unwrap(), b"barbaz");
    }

    #[test]
    fn reads_xz_cluster() {
        let inner = build_uncompressed_cluster(&[b"alpha", b"beta", b"gamma"], false);
        let payload = &inner[1..];
        let compressed = {
            use std::io::Write;
            let mut enc = xz2::write::XzEncoder::new(Vec::new(), 3);
            enc.write_all(payload).unwrap();
            enc.finish().unwrap()
        };
        let mut raw = Vec::with_capacity(1 + compressed.len());
        raw.push(0x04);
        raw.extend_from_slice(&compressed);
        let c = Cluster::parse(&raw).unwrap();
        assert_eq!(c.compression(), Compression::Xz);
        assert_eq!(c.blob_count(), 3);
        assert_eq!(c.blob(0).unwrap(), b"alpha");
        assert_eq!(c.blob(2).unwrap(), b"gamma");
    }

    #[test]
    fn rejects_zstd_declared_size_over_limit() {
        let payload = vec![0u8; 1024];
        let compressed = zstd::stream::encode_all(&payload[..], 3).unwrap();
        match crate::codec_zstd::exact(&compressed, 100) {
            Err(Error::Decompression(msg)) => assert!(msg.contains("limit"), "{msg}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_xz_stream_over_limit() {
        let payload = vec![0u8; 1024];
        let compressed = {
            use std::io::Write;
            let mut enc = xz2::write::XzEncoder::new(Vec::new(), 3);
            enc.write_all(&payload).unwrap();
            enc.finish().unwrap()
        };
        match decode_xz(&compressed, 100) {
            Err(Error::Decompression(msg)) => assert!(msg.contains("limit"), "{msg}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_compression() {
        let raw = vec![0x02u8, 0, 0, 0, 0];
        match Cluster::parse(&raw) {
            Err(Error::UnsupportedCompression(2)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
