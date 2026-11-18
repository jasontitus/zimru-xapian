//! RFC 1952 gzip members used by the HTML importer.
use std::io;

use crc::{Crc, CRC_32_ISO_HDLC};
use miniz_oxide::inflate::stream::{inflate, InflateState};
use miniz_oxide::{DataFormat, MZFlush, MZStatus};

const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> io::Result<&'a [u8]> {
    let (field, rest) = input
        .split_at_checked(len)
        .ok_or(io::ErrorKind::UnexpectedEof)?;
    *input = rest;
    Ok(field)
}

fn header(input: &mut &[u8]) -> io::Result<()> {
    let start = *input;
    let fixed = take(input, 10)?;
    let flags = fixed[3];
    if fixed[..3] != [0x1f, 0x8b, 8] || flags & 0xe0 != 0 {
        return Err(invalid("invalid gzip header"));
    }

    if flags & 4 != 0 {
        let len = take(input, 2)?;
        take(input, u16::from_le_bytes([len[0], len[1]]) as usize)?;
    }
    for flag in [8, 16] {
        if flags & flag != 0 {
            // Preserve flate2's limit for each NUL-terminated filename or comment.
            let field = input.iter().take(65536).position(|&byte| byte == 0);
            match field {
                Some(len) => {
                    take(input, len + 1)?;
                }
                None if input.len() >= 65536 => return Err(invalid("gzip header field too long")),
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            }
        }
    }
    if flags & 2 != 0 {
        let covered = &start[..start.len() - input.len()];
        let checksum = (CRC32.checksum(covered) as u16).to_le_bytes();
        if take(input, 2)? != checksum {
            return Err(invalid("gzip header checksum mismatch"));
        }
    }
    Ok(())
}

pub(super) fn inflate_gzip(mut input: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut scratch = [0u8; 8192];
    let mut state = InflateState::new_boxed(DataFormat::Raw);
    loop {
        header(&mut input)?;
        let member_start = out.len();
        let mut checksum = CRC32.digest();
        loop {
            let result = inflate(&mut state, input, &mut scratch, MZFlush::None);
            let status = result
                .status
                .map_err(|_| invalid("invalid gzip deflate stream"))?;
            input = &input[result.bytes_consumed..];
            out.try_reserve(result.bytes_written)
                .map_err(io::Error::other)?;
            let bytes = &scratch[..result.bytes_written];
            checksum.update(bytes);
            out.extend_from_slice(bytes);
            match status {
                MZStatus::StreamEnd => break,
                MZStatus::Ok if result.bytes_consumed != 0 || result.bytes_written != 0 => {}
                _ => return Err(invalid("incomplete gzip deflate stream")),
            }
        }
        let size = (out.len() - member_start) as u32;
        let trailer = take(&mut input, 8)?;
        if trailer[..4] != checksum.finalize().to_le_bytes() || trailer[4..] != size.to_le_bytes() {
            return Err(invalid("gzip member checksum or size mismatch"));
        }
        if input.is_empty() {
            return Ok(out);
        }
        state.reset(DataFormat::Raw);
    }
}
