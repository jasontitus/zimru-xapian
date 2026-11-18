//! Incremental RFC 8878 input for ZIM clusters. The materialized cluster is
//! also the decoder's history. Stored input is buffered one block at a time.
#![forbid(unsafe_code)]

use std::io::{self, BufRead, BufReader, Read};
use zstd_rs::{Decompressor, FrameHeader};

use crate::error::{Error, Result};

const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const BLOCK_BYTES: usize = 128 << 10;
const DEFAULT_WINDOW_LOG: u32 = 27;

fn codec_error(err: zstd_rs::Error) -> Error {
    Error::Decompression(format!("zstd: {err}"))
}

pub(crate) fn exact(bytes: &[u8], output_limit: u64) -> Result<Vec<u8>> {
    decode(bytes, output_limit, None, false)
}

pub(crate) fn region(
    reader: impl Read,
    size: u64,
    output_limit: u64,
    window_log: Option<u32>,
) -> Result<Vec<u8>> {
    let capacity = size.min(BLOCK_BYTES as u64 + 3) as usize;
    let input = BufReader::with_capacity(capacity, reader.take(size));
    decode(input, output_limit, window_log, true)
}

fn decode(
    mut input: impl BufRead,
    output_limit: u64,
    window_log: Option<u32>,
    physical: bool,
) -> Result<Vec<u8>> {
    let mut decoder = Decompressor::new();
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let limit = usize::try_from(output_limit).unwrap_or(usize::MAX);
    let mut saw_frame = false;
    loop {
        let mut magic = [0; 4];
        let mut n = 0;
        while n < magic.len() {
            match input.read(&mut magic[n..]) {
                Ok(0) => break,
                Ok(read) => n += read,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        if n == 0 && saw_frame {
            return Ok(out);
        }
        // Interpret padding only after a complete frame. A truncated standard
        // or skippable magic still signals an incomplete next frame.
        let skippable = (0x50..=0x5f).contains(&magic[0]);
        let expected = if skippable {
            [magic[0], 0x2a, 0x4d, 0x18]
        } else {
            MAGIC
        };
        if magic[..n] != expected[..n] {
            if saw_frame && physical {
                return Ok(out);
            }
            return Err(codec_error(zstd_rs::Error::BadMagic));
        }
        if n < 4 {
            return Err(codec_error(zstd_rs::Error::Truncated));
        }
        if skippable {
            let length = u64::from(read_u32(&mut input)?);
            let skipped = io::copy(&mut input.by_ref().take(length), &mut io::sink())?;
            if skipped != length {
                return Err(codec_error(zstd_rs::Error::Truncated));
            }
        } else {
            let header = read_header(&mut input)?;
            // libzstd's streaming default is 128 MiB. Its small pledged bulk
            // path also handled larger declared windows. These frames can
            // produce at most their small pledge; neither path allocates a
            // separate history based on the declaration.
            let max_window = match window_log {
                Some(log) => 1u64 << log,
                None if header.content_size.is_some_and(|s| s <= 8 << 20) => 1u64 << 31,
                None => 1u64 << DEFAULT_WINDOW_LOG,
            };
            let mut frame = decoder
                .begin_frame(header, None, limit, max_window, &mut out)
                .map_err(codec_error)?;
            loop {
                let mut block_header = [0; 3];
                input.read_exact(&mut block_header)?;
                let block = frame.block_header(&block_header).map_err(codec_error)?;
                let size = block.stored_size();
                let available = input.fill_buf()?;
                if available.len() >= size {
                    frame
                        .decode_block(block, &available[..size])
                        .map_err(codec_error)?;
                    input.consume(size);
                } else {
                    // block_header bounded this by min(frame window, 128 KiB).
                    scratch.resize(size, 0);
                    input.read_exact(&mut scratch)?;
                    frame.decode_block(block, &scratch).map_err(codec_error)?;
                }
                if block.is_last() {
                    break;
                }
            }
            let checksum = if header.checksum {
                Some(read_u32(&mut input)?)
            } else {
                None
            };
            frame.finish(checksum).map_err(codec_error)?;
        }
        saw_frame = true;
    }
}

fn read_u32(input: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_header(input: &mut impl Read) -> Result<FrameHeader> {
    // Largest standard header: magic, descriptor, window descriptor, 4-byte
    // dictionary ID and 8-byte content size. Reuse the codec's sole parser.
    let mut bytes = [0; 18];
    bytes[..4].copy_from_slice(&MAGIC);
    for n in 5..=bytes.len() {
        input.read_exact(&mut bytes[n - 1..n])?;
        match FrameHeader::parse(&bytes[..n]) {
            Err(zstd_rs::Error::Truncated) => continue,
            result => return result.map_err(codec_error),
        }
    }
    Err(codec_error(zstd_rs::Error::Truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct ShortReads<'a>(&'a [u8]);
    impl Read for ShortReads<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(7);
            self.0.read(&mut out[..n])
        }
    }

    #[test]
    fn checksum_window_and_aggregate_output_on_fragmented_native_frames() {
        let data = b"Article content and dictionary-independent match history.\n".repeat(10_000);
        let mut encoder = zstd::Encoder::new(Vec::new(), 5).unwrap();
        encoder.window_log(18).unwrap();
        encoder.include_checksum(true).unwrap();
        encoder.write_all(&data).unwrap();
        let encoded = encoder.finish().unwrap();
        let header = FrameHeader::parse(&encoded).unwrap();
        assert!(header.checksum);
        assert!(header.content_size.is_none());
        assert_eq!(header.window_size, 1 << 18);
        let len = data.len() as u64;
        let read = |src: &[u8], max, log| region(ShortReads(src), src.len() as u64, max, Some(log));
        assert_eq!(read(&encoded, len, 18).unwrap(), data);
        assert!(read(&encoded, len, 17).is_err());
        let mut both = encoded.clone();
        both.extend_from_slice(&encoded);
        assert_eq!(read(&both, len * 2, 18).unwrap(), data.repeat(2));
        assert!(read(&both, len * 2 - 1, 18).is_err());
        let mut incorrect = encoded;
        *incorrect.last_mut().unwrap() ^= 1;
        assert!(read(&incorrect, len, 18)
            .unwrap_err()
            .to_string()
            .contains("checksum"));
    }

    #[test]
    fn exact_output_cap_across_many_small_frames() {
        let text = b"repeated article text ".repeat(360);
        let encoded = zstd::bulk::compress(&text, 4).unwrap();
        let frames = encoded.repeat(61);
        let expected = text.repeat(61);
        let result = exact(&frames, expected.len() as u64).unwrap();
        assert_eq!(result, expected);
        assert!(result.capacity() <= expected.len());
        assert!(exact(&frames, expected.len() as u64 - 1).is_err());
    }

    #[test]
    fn dictionary_ids_require_external_dictionary() {
        let dictionary = include_bytes!(
            "../vendor/zstd-rs/tests/golden/golden-dictionaries/http-dict-missing-symbols"
        );
        let mut encoder = zstd::Encoder::with_dictionary(Vec::new(), 3, dictionary).unwrap();
        encoder.write_all(b"HTTP/1.1 200 OK\r\n").unwrap();
        let encoded = encoder.finish().unwrap();
        assert_ne!(FrameHeader::parse(&encoded).unwrap().dict_id, 0);
        let e = exact(&encoded, 65536).unwrap_err();
        assert!(e.to_string().contains("dictionary"), "{e}");
    }
}
