//! A single XZ stream in a bounded ZIM cluster interval.
//!
//! The raw LZMA2 decoder reads exactly through its end marker. Parsing the
//! XZ container here lets us check each block's actual sizes, preserve all
//! prefilters, and distinguish the stream footer from later archive bytes.
//! Format: <https://tukaani.org/xz/xz-file-format.txt>.

use std::io::{self, Read, Take};

use chksum_hash_sha2_256 as sha256;
use crc::{Crc, CRC_32_ISO_HDLC, CRC_64_XZ};
use lzma_rust2::{filter::StreamFilter, FilterConfig, FilterType, Lzma2Reader};

use crate::{Error, Result};

const CONTAINER_MEMORY: u64 = 64 << 10;

const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);
const CRC64: Crc<u64> = Crc::<u64>::new(&CRC_64_XZ);

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn bytes<const N: usize>(r: &mut impl Read) -> io::Result<[u8; N]> {
    let mut b = [0; N];
    r.read_exact(&mut b)?;
    Ok(b)
}

/// XZ variable-length integers occupy 1 to 9 bytes and use the shortest encoding.
fn vli(r: &mut impl Read) -> io::Result<u64> {
    let mut value = 0;
    for n in 0..9 {
        let [b] = bytes(r)?;
        value |= u64::from(b & 0x7f) << (7 * n);
        if b < 128 {
            return if n != 0 && b == 0 {
                Err(invalid("noncanonical variable-length integer"))
            } else {
                Ok(value)
            };
        }
    }
    Err(invalid("variable-length integer exceeds 63 bits"))
}

fn verify_crc32(body: &[u8], check: &[u8]) -> io::Result<()> {
    if CRC32.checksum(body).to_le_bytes() != check {
        return Err(invalid("CRC32 mismatch"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Check {
    None,
    Crc32,
    Crc64,
    Sha256,
}

impl Check {
    fn size(self) -> usize {
        match self {
            Self::None => 0,
            Self::Crc32 => 4,
            Self::Crc64 => 8,
            Self::Sha256 => 32,
        }
    }

    fn verify(self, data: &[u8], r: &mut impl Read) -> io::Result<()> {
        let matches = match self {
            Self::None => true,
            Self::Crc32 => CRC32.checksum(data).to_le_bytes() == bytes::<4>(r)?,
            Self::Crc64 => CRC64.checksum(data).to_le_bytes() == bytes::<8>(r)?,
            Self::Sha256 => sha256::hash(data).as_ref() == bytes::<32>(r)?,
        };
        if !matches {
            return Err(invalid("block checksum mismatch"));
        }
        Ok(())
    }
}

struct Block {
    compressed: Option<u64>,
    uncompressed: Option<u64>,
    dictionary: u32,
    filters: Vec<FilterConfig>,
}

impl Block {
    fn parse(header: &[u8]) -> io::Result<Self> {
        let (fields, crc) = header.split_at(header.len() - 4);
        verify_crc32(fields, crc)?;
        let flags = fields[1];
        if flags & 0x3c != 0 {
            return Err(invalid("reserved block flags"));
        }
        let mut input = &fields[2..];
        let compressed = (flags & 0x40 != 0).then(|| vli(&mut input)).transpose()?;
        let uncompressed = (flags & 0x80 != 0).then(|| vli(&mut input)).transpose()?;
        let last = flags & 3;
        let mut filters = Vec::new();
        for n in 0..=last {
            let kind = FilterType::try_from(vli(&mut input)?)
                .map_err(|_| invalid("unsupported filter ID"))?;
            let len = usize::try_from(vli(&mut input)?).map_err(|_| invalid("filter length"))?;
            let (props, rest) = input
                .split_at_checked(len)
                .ok_or(invalid("filter length"))?;
            input = rest;
            if n == last {
                let (FilterType::Lzma2, [p @ 0..=40]) = (kind, props) else {
                    return Err(invalid(
                        "expected final LZMA2 filter with dictionary property",
                    ));
                };
                let dictionary = if *p == 40 {
                    u32::MAX
                } else {
                    (2 | u32::from(*p & 1)) << (*p / 2 + 11)
                };
                if input.iter().any(|&b| b != 0) {
                    return Err(invalid("nonzero block header padding"));
                }
                return Ok(Self {
                    compressed,
                    uncompressed,
                    dictionary,
                    filters,
                });
            }
            let property = match (kind, props) {
                (FilterType::Delta, [p]) => u32::from(*p) + 1,
                (FilterType::Delta | FilterType::Lzma2, _) => {
                    return Err(invalid("invalid prefilter"))
                }
                (_, []) => 0,
                (_, [a, b, c, d]) => u32::from_le_bytes([*a, *b, *c, *d]),
                _ => return Err(invalid("invalid BCJ filter properties")),
            };
            let alignment = match kind {
                FilterType::BcjIa64 => 16,
                FilterType::BcjArmThumb | FilterType::BcjRiscv => 2,
                FilterType::BcjArm
                | FilterType::BcjArm64
                | FilterType::BcjPpc
                | FilterType::BcjSparc => 4,
                _ => 1,
            };
            if property % alignment != 0 {
                return Err(invalid("unaligned BCJ start offset"));
            }
            filters.push(FilterConfig {
                filter_type: kind,
                property,
            });
        }
        Err(invalid("missing LZMA2 filter"))
    }

    fn decode(
        &self,
        r: &mut impl Read,
        output: &mut Vec<u8>,
        limit: usize,
        memory: u64,
    ) -> io::Result<()> {
        let start = output.len();
        let remaining = (limit - start) as u64;
        if self.uncompressed.is_some_and(|n| n > remaining) {
            return Err(invalid("declared output exceeds byte limit"));
        }
        let block_limit = self.uncompressed.unwrap_or(remaining) as usize + start;
        // Subtract the sample's 4 KiB dictionary from the library's estimate
        // for model/range-decoder memory. Allow separate space for the parser,
        // buffered input, scratch, and overlapping models on a property reset.
        let overhead =
            u64::from(lzma_rust2::lzma2_get_memory_usage(4096) - 4) * 1024 + CONTAINER_MEMORY;
        let dictionary_budget = memory.saturating_sub(overhead);
        if u64::from(self.dictionary) > dictionary_budget {
            return Err(invalid("declared dictionary exceeds decoder memory limit"));
        }

        // lzma-rust2 grows its history lazily from 64 KiB, doubling and
        // clipping to the dictionary size. A moving reallocation can hold
        // both the previous and next buffers at once. Charge both, including
        // non-power-of-two final windows. If this constrains the block, let
        // short streams with a large declaration succeed; reject before
        // any bytes relying on the shortened history can be returned.
        let power = 1u64 << dictionary_budget.ilog2();
        let history_budget = if dictionary_budget <= 64 << 10 {
            dictionary_budget
        } else {
            (dictionary_budget - power / 2).min(power).max(64 << 10)
        } & !15;
        let possible = u64::from(self.dictionary).min((block_limit - start) as u64);
        let memory_limited = possible > history_budget;
        let read_limit = if memory_limited {
            start + history_budget as usize
        } else {
            block_limit
        };
        // No match in this block can need more history than all its output.
        // This also keeps a 32-bit process's lazy window addressable for
        // small blocks that declare the maximum four-gigabyte dictionary.
        let dictionary = possible.min(history_budget).max(4096) as u32;
        let raw_kib = (memory - CONTAINER_MEMORY) / 1024;
        let mut decoder = Lzma2Reader::new_mem_limit(
            r.take(self.compressed.unwrap_or(u64::MAX)),
            dictionary,
            u32::try_from(raw_kib).unwrap_or(u32::MAX),
            None,
        )?;
        let mut buf = [0; 8192];
        loop {
            // Reading one byte past the permitted output distinguishes an
            // exactly-full stream waiting on its end marker from overflow.
            let cap = (read_limit - output.len() + 1).min(buf.len());
            let n = decoder.read(&mut buf[..cap])?;
            if n == 0 {
                break;
            }
            if n > read_limit - output.len() {
                if memory_limited {
                    return Err(invalid("dictionary growth exceeds decoder memory limit"));
                }
                return Err(invalid("decoded output exceeds byte limit"));
            }
            let needed = output.len() + n;
            if needed > output.capacity() {
                let target = needed.max(output.capacity().saturating_mul(2)).min(limit);
                output
                    .try_reserve_exact(target - output.len())
                    .map_err(io::Error::other)?;
            }
            output.extend_from_slice(&buf[..n]);
        }
        if self.compressed.is_some() && decoder.into_inner().limit() != 0 {
            return Err(invalid("compressed block size mismatch"));
        }
        let data = &mut output[start..];
        if self.uncompressed.is_some_and(|n| n != data.len() as u64) {
            return Err(invalid("decoded block size mismatch"));
        }
        // Encoding applies prefilters in header order. Reverse them in place
        // on this complete block; BCJ's final undecidable bytes stay literal.
        for config in self.filters.iter().rev() {
            let mut filter = StreamFilter::new(config)?;
            filter.decode(data);
            filter.finish();
        }
        Ok(())
    }
}

/// Fingerprint fixed-width ordered (unpadded size, uncompressed size) pairs,
/// so verifying the index does not allocate proportional to the block count.
fn record(hash: &mut sha256::Update, stored: u64, decoded: u64) {
    hash.update(stored.to_le_bytes())
        .update(decoded.to_le_bytes());
}

struct IndexReader<'a, R> {
    input: &'a mut Take<R>,
    crc: crc::Digest<'static, u32>,
}

impl<R: Read> Read for IndexReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(buf)?;
        self.crc.update(&buf[..n]);
        Ok(n)
    }
}

fn index_and_footer(
    r: &mut Take<impl Read>,
    flags: &[u8],
    blocks: u64,
    actual: sha256::Update,
) -> io::Result<()> {
    let remaining = r.limit();
    let mut index = IndexReader {
        input: r,
        crc: CRC32.digest(),
    };
    // The caller already consumed the zero index indicator.
    index.crc.update(&[0]);
    if vli(&mut index)? != blocks {
        return Err(invalid("index block count mismatch"));
    }
    let mut listed = sha256::new();
    for _ in 0..blocks {
        let stored = vli(&mut index)?;
        let decoded = vli(&mut index)?;
        record(&mut listed, stored, decoded);
    }
    if actual.digest() != listed.digest() {
        return Err(invalid("index block size mismatch"));
    }
    let size = 1 + (remaining - index.input.limit());
    let padding = (4 - size % 4) % 4;
    for _ in 0..padding {
        if bytes::<1>(&mut index)? != [0] {
            return Err(invalid("nonzero index padding"));
        }
    }
    let crc = index.crc.finalize();
    if crc.to_le_bytes() != bytes::<4>(r)? {
        return Err(invalid("index CRC32 mismatch"));
    }
    let index_size = size + padding + 4;
    let footer = bytes::<12>(r)?;
    verify_crc32(&footer[4..10], &footer[..4])?;
    let backward = u32::from_le_bytes(footer[4..8].try_into().unwrap());
    if (u64::from(backward) + 1) * 4 != index_size
        || &footer[8..10] != flags
        || &footer[10..] != b"YZ"
    {
        return Err(invalid("invalid stream footer"));
    }
    Ok(())
}

fn stream(r: &mut Take<impl Read>, limit: u64, memory: u64) -> io::Result<Vec<u8>> {
    if memory < CONTAINER_MEMORY {
        return Err(invalid("decoder memory limit is too small"));
    }
    let header = bytes::<12>(r)?;
    if &header[..6] != b"\xFD7zXZ\0" || header[6] != 0 {
        return Err(invalid("bad stream magic or flags"));
    }
    let flags = &header[6..8];
    verify_crc32(flags, &header[8..12])?;
    let check = match flags[1] {
        0 => Check::None,
        1 => Check::Crc32,
        4 => Check::Crc64,
        10 => Check::Sha256,
        _ => return Err(invalid("unsupported stream checksum or flags")),
    };
    let mut output = Vec::new();
    // The raw codec rounds its history size up to a multiple of 16.
    let limit = limit.min((isize::MAX as u64) & !15) as usize;
    let mut blocks = 0;
    let mut actual = sha256::new();
    loop {
        let [first] = bytes(r)?;
        if first == 0 {
            index_and_footer(r, flags, blocks, actual)?;
            return Ok(output);
        }
        let header_size = (usize::from(first) + 1) * 4;
        let mut header = [0; 1024];
        header[0] = first;
        r.read_exact(&mut header[1..header_size])?;
        let block = Block::parse(&header[..header_size])?;
        let compressed_start = r.limit();
        let start = output.len();
        block.decode(r, &mut output, limit, memory)?;
        let compressed = compressed_start - r.limit();
        for _ in 0..((4 - compressed % 4) % 4) {
            if bytes::<1>(r)? != [0] {
                return Err(invalid("nonzero block padding"));
            }
        }
        let data = &output[start..];
        check.verify(data, r)?;
        record(
            &mut actual,
            header_size as u64 + compressed + check.size() as u64,
            data.len() as u64,
        );
        blocks += 1;
    }
}

pub(crate) fn decode(
    r: impl Read,
    size: u64,
    limit: u64,
    memory: Option<u64>,
) -> Result<(Vec<u8>, u64)> {
    let mut input = r.take(size);
    let output = stream(
        &mut input,
        limit,
        memory.unwrap_or(limit.saturating_add(64 << 20)),
    )
    .map_err(|e| Error::Decompression(format!("xz: {e}")))?;
    Ok((output, size - input.limit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Write};
    use xz2::stream::{Check as NativeCheck, Filters, LzmaOptions, MtStreamBuilder, Stream};
    use xz2::write::XzEncoder;

    fn native_stream(data: &[u8], stream: Stream) -> Vec<u8> {
        let mut enc = XzEncoder::new_stream(Vec::new(), stream);
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn liblzma_multiblock_filters_checks_and_dictionary_limit() {
        let data = b"\xe8\x11\x00\x00\x00 article \x48\x00\x03\x01".repeat(971);
        for kind in [
            NativeCheck::None,
            NativeCheck::Crc32,
            NativeCheck::Crc64,
            NativeCheck::Sha256,
        ] {
            let mut filters = Filters::new();
            filters
                .x86()
                .sparc()
                .lzma2(&LzmaOptions::new_preset(1).unwrap());
            let stream = MtStreamBuilder::new()
                .threads(2)
                .block_size(1024)
                .filters(filters)
                .check(kind)
                .encoder()
                .unwrap();
            let bytes = native_stream(&data, stream);
            let (decoded, count) = decode(
                BufReader::with_capacity(7, bytes.as_slice()),
                bytes.len() as u64,
                data.len() as u64,
                Some(8 << 20),
            )
            .unwrap();
            assert_eq!(decoded, data);
            assert_eq!(count, bytes.len() as u64);
            assert!(decode(bytes.as_slice(), count, data.len() as u64, Some(64 << 10)).is_err());
        }
    }

    #[test]
    fn budget_covers_overlapping_dictionary_allocations_during_growth() {
        let data = b"Shared article text. ".repeat(15000);
        let mut options = LzmaOptions::new_preset(1).unwrap();
        options.dict_size(192 << 10);
        let mut filters = Filters::new();
        filters.lzma2(&options);
        let stream = Stream::new_stream_encoder(&filters, NativeCheck::Crc64).unwrap();
        let bytes = native_stream(&data, stream);
        let overhead =
            (u64::from(lzma_rust2::lzma2_get_memory_usage(4096)) - 4) * 1024 + CONTAINER_MEMORY;
        let read = |history| {
            decode(
                bytes.as_slice(),
                bytes.len() as u64,
                data.len() as u64,
                Some(overhead + history),
            )
        };
        assert!(read(192 << 10)
            .unwrap_err()
            .to_string()
            .contains("dictionary growth"));
        let (out, _) = read((192 + 128) << 10).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn a_large_advertised_history_stays_lazy_for_short_blocks() {
        let data = b"Archive with a small public-domain article.";
        let mut bytes = native_stream(
            data,
            Stream::new_easy_encoder(1, NativeCheck::Crc64).unwrap(),
        );
        let block_start = 12;
        let header_len = (usize::from(bytes[block_start]) + 1) * 4;
        assert_eq!(&bytes[block_start + 1..block_start + 4], &[0, 0x21, 1]);
        // A larger declared dictionary is legal for the same LZMA2 data.
        // This creates its standard maximum property without a huge encoder.
        bytes[block_start + 4] = 40;
        let crc_pos = block_start + header_len - 4;
        let crc = CRC32.checksum(&bytes[block_start..crc_pos]).to_le_bytes();
        bytes[crc_pos..crc_pos + 4].copy_from_slice(&crc);
        let size = bytes.len() as u64;
        let limit = u64::from(u32::MAX);
        let (out, used) = decode(bytes.as_slice(), size, limit, None).unwrap();
        assert_eq!(out, data);
        assert_eq!(used, size);
        assert!(decode(bytes.as_slice(), size, limit, Some(64 << 20)).is_err());
    }

    #[test]
    fn verifies_index_records_and_footer_backward_size_independently_of_crcs() {
        let data = b"paper and ink";
        let bytes = native_stream(
            data,
            Stream::new_easy_encoder(1, NativeCheck::Crc64).unwrap(),
        );
        let footer = bytes.len() - 12;
        let backward = u32::from_le_bytes(bytes[footer + 4..footer + 8].try_into().unwrap());
        let index = footer - (backward as usize + 1) * 4;
        assert_eq!(bytes[index..index + 2], [0, 1]);
        // This short one-block stream has single-byte size fields.
        assert_eq!(bytes[index + 3], data.len() as u8);
        let mut wrong_record = bytes.clone();
        wrong_record[index + 3] += 1;
        let crc = CRC32
            .checksum(&wrong_record[index..footer - 4])
            .to_le_bytes();
        wrong_record[footer - 4..footer].copy_from_slice(&crc);
        let mut wrong_footer = bytes.clone();
        wrong_footer[footer + 4..footer + 8].copy_from_slice(&(backward + 1).to_le_bytes());
        let crc = CRC32
            .checksum(&wrong_footer[footer + 4..footer + 10])
            .to_le_bytes();
        wrong_footer[footer..footer + 4].copy_from_slice(&crc);
        for (stream, message) in [
            (&wrong_record, "index block size mismatch"),
            (&wrong_footer, "invalid stream footer"),
        ] {
            assert!(decode(stream.as_slice(), stream.len() as u64, 1024, None)
                .unwrap_err()
                .to_string()
                .contains(message));
        }
    }
}
