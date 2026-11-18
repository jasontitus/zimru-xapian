//! The writer emits one LZMA2 block with CRC64 per ZIM XZ cluster. The
//! container fields follow the .xz File Format specification (sections 2–4).
//! The LZMA2 codec consumes the offset table and blob slices incrementally.
use std::io::{self, Write};

use crc::{Crc, CRC_32_ISO_HDLC, CRC_64_XZ};
use lzma_rust2::{Lzma2Options, Lzma2Writer};

const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);
const CRC64: Crc<u64> = Crc::<u64>::new(&CRC_64_XZ);
const FLAGS: [u8; 2] = [0, 4]; // XZ check ID 4 selects CRC64.

fn vli(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        bytes.push(value as u8 | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8);
}

pub(super) fn encode(
    mut out: Vec<u8>,
    level: u32,
    offsets: &[u8],
    blobs: &[Vec<u8>],
    payload_size: usize,
) -> io::Result<Vec<u8>> {
    let mut options = Lzma2Options::with_preset(level);
    // History beyond the complete input cannot contribute a match. The XZ
    // LZMA2 property encodes dictionary sizes in alternating powers and halves.
    let needed = (payload_size as u64).min(u64::from(options.lzma_options.dict_size));
    let (prop, dictionary) = (0u8..=39)
        .map(|p| (p, (2u32 + u32::from(p & 1)) << (11 + p / 2)))
        .find(|&(_, bytes)| u64::from(bytes) >= needed)
        .ok_or_else(|| io::Error::other("XZ dictionary exceeds the supported presets"))?;
    options.lzma_options.dict_size = dictionary;

    out.extend_from_slice(b"\xFD7zXZ\0");
    out.extend_from_slice(&FLAGS);
    out.extend_from_slice(&CRC32.checksum(&FLAGS).to_le_bytes());

    // Twelve-byte block header, one LZMA2 filter, no optional size fields.
    let header = [2, 0, 0x21, 1, prop, 0, 0, 0];
    out.extend_from_slice(&header);
    out.extend_from_slice(&CRC32.checksum(&header).to_le_bytes());
    let start = out.len();
    let mut digest = CRC64.digest();
    let mut writer = Lzma2Writer::new(&mut out, options);
    for bytes in std::iter::once(offsets).chain(blobs.iter().map(Vec::as_slice)) {
        writer.write_all(bytes)?;
        digest.update(bytes);
    }
    writer.finish()?;
    let compressed = out.len() - start;
    out.extend_from_slice(&[0; 3][..compressed.wrapping_neg() % 4]);
    out.extend_from_slice(&digest.finalize().to_le_bytes());

    let index_start = out.len();
    out.extend_from_slice(&[0, 1]); // Index indicator and one block record.
    vli(&mut out, 12 + compressed as u64 + 8);
    vli(&mut out, payload_size as u64);
    let index_bytes = out.len() - index_start;
    out.extend_from_slice(&[0; 3][..index_bytes.wrapping_neg() % 4]);
    let crc = CRC32.checksum(&out[index_start..]);
    out.extend_from_slice(&crc.to_le_bytes());

    // With one record and two VLIs, the Index is at most 24 bytes.
    let backward_size = ((out.len() - index_start) / 4 - 1) as u32;
    let mut footer = [0; 6];
    footer[..4].copy_from_slice(&backward_size.to_le_bytes());
    footer[4..].copy_from_slice(&FLAGS);
    out.extend_from_slice(&CRC32.checksum(&footer).to_le_bytes());
    out.extend_from_slice(&footer);
    out.extend_from_slice(b"YZ");
    Ok(out)
}
