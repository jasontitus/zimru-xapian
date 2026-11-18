//! Decompression: frames, blocks, and the public `Decompressor`.
mod execute;
mod literals;
mod seqtable;
mod sequences;
mod state;
pub mod tables;

use crate::dict::Dictionary;
use crate::error::{Error, Result};
use crate::frame::{parse_block_header, BlockType, FrameFormat, FrameHeader, BLOCK_MAX, SKIPPABLE_BASE, SKIPPABLE_MASK, WINDOW_LOG_MAX};
use crate::huf::dtable::HufDTable;
use crate::xxh64::xxh64;
use alloc::vec::Vec;
use execute::Exec;
use seqtable::{Kind, SeqTable};
use state::{DecState, Predefined};

/// A dictionary prepared for decoding (tables built once, shared by any number of frames).
pub struct DecoderDictionary {
    pub(crate) dict: Dictionary,
    pub(crate) huf: Option<HufDTable>,
    pub(crate) seq: Option<[SeqTable; 3]>,
}

impl DecoderDictionary {
    /// Builds the decoding tables for `dict` once.
    pub fn new(dict: Dictionary) -> Result<DecoderDictionary> {
        let (huf, seq) = match &dict.tables {
            Some(t) => {
                let mut s = [SeqTable::default(), SeqTable::default(), SeqTable::default()];
                s[0].build(&t.ll, Kind::LiteralLength)?;
                s[1].build(&t.of, Kind::Offset)?;
                s[2].build(&t.ml, Kind::MatchLength)?;
                (Some(HufDTable::new(&t.huf)), Some(s))
            }
            None => (None, None),
        };
        Ok(DecoderDictionary { dict, huf, seq })
    }

    /// The underlying dictionary.
    pub fn dictionary(&self) -> &Dictionary {
        &self.dict
    }
}

/// Reusable decompression context. Holds no configuration: every call states its limit.
pub struct Decompressor {
    st: DecState,
    pre: Predefined,
}

impl Decompressor {
    /// Creates a decompression context (its predefined tables are built here).
    pub fn new() -> Decompressor {
        let pre = match Predefined::new() {
            Ok(p) => p,
            Err(_) => unreachable!("predefined distributions are valid constants"),
        };
        Decompressor { st: DecState::new(), pre }
    }

    /// Decodes every frame in `src` (skipping skippable frames), appending to `out`.
    /// Fails with `OutputLimit` before `out` would grow past `max_output` bytes in total.
    /// Returns the number of bytes appended. Like the reference, empty input is zero frames.
    pub fn decompress(&mut self, src: &[u8], dict: Option<&DecoderDictionary>, max_output: usize, out: &mut Vec<u8>) -> Result<usize> {
        self.decompress_format(FrameFormat::Standard, src, dict, max_output, out)
    }

    /// Like [`Decompressor::decompress`] for frames in `format` (magicless frames
    /// are concatenated without magic numbers and cannot include skippable frames).
    pub fn decompress_format(
        &mut self,
        format: FrameFormat,
        src: &[u8],
        dict: Option<&DecoderDictionary>,
        max_output: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        let before = out.len();
        let limit = before.checked_add(max_output).ok_or(Error::Parameter("output limit overflows"))?;
        let mut at = 0;
        while at < src.len() {
            at += self.frame(format, &src[at..], dict, limit, out)?;
        }
        Ok(out.len() - before)
    }

    /// Decodes one frame (or skips one skippable frame) at the start of `src`,
    /// appending to `out` without letting it exceed `limit` bytes. Returns bytes consumed.
    pub fn decompress_frame(&mut self, src: &[u8], dict: Option<&DecoderDictionary>, limit: usize, out: &mut Vec<u8>) -> Result<usize> {
        self.frame(FrameFormat::Standard, src, dict, limit, out)
    }

    fn frame(
        &mut self,
        format: FrameFormat,
        src: &[u8],
        dict: Option<&DecoderDictionary>,
        limit: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        if format == FrameFormat::Magicless {
            let h = FrameHeader::parse_format(src, format)?;
            return self.frame_with(src, &h, dict, limit, out);
        }
        let magic = u32::from_le_bytes(src.get(..4).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?);
        if magic & SKIPPABLE_MASK == SKIPPABLE_BASE {
            let len = u32::from_le_bytes(src.get(4..8).ok_or(Error::Truncated)?.try_into().map_err(|_| Error::Truncated)?);
            let end = 8usize.checked_add(len as usize).ok_or(Error::Truncated)?;
            return if end <= src.len() { Ok(end) } else { Err(Error::Truncated) };
        }
        let h = FrameHeader::parse(src)?;
        self.frame_with(src, &h, dict, limit, out)
    }

    /// Decodes the frame whose header `h` was parsed from the start of `src`.
    fn frame_with(
        &mut self,
        src: &[u8],
        h: &FrameHeader,
        dict: Option<&DecoderDictionary>,
        limit: usize,
        out: &mut Vec<u8>,
    ) -> Result<usize> {
        let mut frame = self.begin_frame(*h, dict, limit, 1u64 << WINDOW_LOG_MAX, out)?;
        let mut at = h.header_len;
        loop {
            let block = frame.block_header(src.get(at..).ok_or(Error::Truncated)?)?;
            at += 3;
            let end = at.checked_add(block.stored_size()).ok_or(Error::Truncated)?;
            frame.decode_block(block, src.get(at..end).ok_or(Error::Truncated)?)?;
            at = end;
            if block.is_last() {
                break;
            }
        }
        let checksum = if h.checksum {
            let bytes = src.get(at..).and_then(|tail| tail.get(..4)).ok_or(Error::Truncated)?;
            at += 4;
            Some(u32::from_le_bytes(bytes.try_into().map_err(|_| Error::Truncated)?))
        } else {
            None
        };
        frame.finish(checksum)?;
        Ok(at)
    }

    /// Start a frame for block-at-a-time decoding, appending to `out`.
    /// Both the total output cap and the permitted declared history are in bytes.
    pub fn begin_frame<'a>(
        &'a mut self,
        h: FrameHeader,
        dict: Option<&'a DecoderDictionary>,
        output_limit: usize,
        max_window: u64,
        out: &'a mut Vec<u8>,
    ) -> Result<FrameBlocks<'a>> {
        if h.window_size > max_window {
            return Err(Error::WindowTooLarge);
        }
        let dict = match (h.dict_id, dict) {
            (0, d) => d,
            (id, Some(d)) if d.dict.id == id => Some(d),
            (_, Some(_)) => return Err(Error::WrongDictionary),
            (_, None) => return Err(Error::MissingDictionary),
        };
        let start = out.len();
        let max_len = output_limit.min(isize::MAX as usize);
        let room = max_len.checked_sub(start).ok_or(Error::OutputLimit)?;
        let limit = if let Some(size) = h.content_size {
            let size = usize::try_from(size).map_err(|_| Error::OutputLimit)?;
            if size > room {
                return Err(Error::OutputLimit);
            }
            // Avoid committing memory for a large pledge before seeing blocks.
            if size <= 8 << 20 {
                reserve_output(out, (start + size).saturating_add(execute::SLACK).min(max_len), max_len)?;
            }
            start + size
        } else {
            max_len
        };
        self.st.reset(dict);
        let bounds = Bounds {
            frame_start: start,
            allocation_limit: max_len,
            limit,
            block_max: h.window_size.min(BLOCK_MAX as u64) as usize,
            window: usize::try_from(h.window_size).unwrap_or(usize::MAX),
        };
        Ok(FrameBlocks { decoder: self, dict, header: h, bounds, out, last: false })
    }

    /// Decodes one compressed block at `pos`; returns the new logical end.
    fn block(&mut self, body: &[u8], dict: Option<&DecoderDictionary>, b: &Bounds, pos: usize, out: &mut Vec<u8>) -> Result<usize> {
        let (lits, used) = literals::decode(body, &mut self.st, dict, b.block_max)?;
        let history: &[u8] = dict.map(|d| d.dict.content.as_slice()).unwrap_or(&[]);
        let mut ex = Exec::new(out, pos, b.frame_start, history, b.limit, b.block_max, b.window, b.allocation_limit);
        let r = sequences::decode(body, used, lits, &mut self.st, &self.pre, dict, &mut ex);
        let end = ex.pos;
        r.map(|_| end)
    }
}

impl Default for Decompressor {
    fn default() -> Self {
        Self::new()
    }
}

/// Checks the output limit and makes `out[pos..pos + n]` writable.
fn make_room(out: &mut Vec<u8>, pos: usize, n: usize, b: &Bounds) -> Result<()> {
    let end = pos.checked_add(n).filter(|&e| e <= b.limit).ok_or(Error::OutputLimit)?;
    if out.len() < end {
        reserve_output(out, end, b.allocation_limit)?;
        out.resize(end, 0);
    }
    Ok(())
}

// Vec::reserve may double past the caller's cap. Grow geometrically ourselves,
// using the total output policy so concatenated frames also grow amortized.
fn reserve_output(out: &mut Vec<u8>, needed: usize, cap: usize) -> Result<()> {
    if needed > cap {
        return Err(Error::OutputLimit);
    }
    if needed > out.capacity() {
        let target = out.capacity().saturating_mul(2).clamp(needed, cap);
        out.try_reserve_exact(target - out.len()).map_err(|_| Error::OutputLimit)?;
    }
    Ok(())
}

/// Per-frame bounds every block is checked against.
struct Bounds {
    /// Index in the output where the frame starts.
    frame_start: usize,
    /// Total requested output capacity across all frames.
    allocation_limit: usize,
    /// Caller's cap on the output length, further restricted by the pledge.
    limit: usize,
    /// Block_Maximum_Size: min(window, 128 KiB).
    block_max: usize,
    window: usize,
}

/// Parsed block header. Its size has been checked against its frame window.
#[derive(Clone, Copy)]
pub struct BlockHeader {
    last: bool,
    ty: BlockType,
    size: usize,
}

impl BlockHeader {
    /// Bytes the stream must supply (one byte for a run-length block).
    pub fn stored_size(self) -> usize {
        if self.ty == BlockType::Rle { 1 } else { self.size }
    }

    /// Whether this block ends the frame.
    pub fn is_last(self) -> bool { self.last }
}

/// A single frame's decoder state and output. Supply blocks in order and
/// finish with the optional frame checksum.
pub struct FrameBlocks<'a> {
    decoder: &'a mut Decompressor,
    dict: Option<&'a DecoderDictionary>,
    header: FrameHeader,
    bounds: Bounds,
    out: &'a mut Vec<u8>,
    last: bool,
}

impl FrameBlocks<'_> {
    /// Read the next three-byte block header before allocating its input.
    pub fn block_header(&self, bytes: &[u8]) -> Result<BlockHeader> {
        let (last, ty, size) = parse_block_header(bytes)?;
        if self.last || size > self.bounds.block_max {
            return Err(Error::Corrupt("block exceeds frame bounds"));
        }
        Ok(BlockHeader { last, ty, size })
    }

    /// Consume one block; no whole-frame input buffer is required.
    pub fn decode_block(&mut self, block: BlockHeader, body: &[u8]) -> Result<()> {
        if self.last || block.size > self.bounds.block_max || body.len() != block.stored_size() {
            return Err(Error::Corrupt("unexpected block length or sequence"));
        }
        let start = self.out.len();
        match block.ty {
            BlockType::Rle => {
                make_room(self.out, start, block.size, &self.bounds)?;
                self.out[start..].fill(body[0]);
            }
            BlockType::Raw => {
                make_room(self.out, start, block.size, &self.bounds)?;
                self.out[start..].copy_from_slice(body);
            }
            BlockType::Compressed => {
                let end = self.decoder.block(body, self.dict, &self.bounds, start, self.out);
                self.out.truncate(end.as_ref().copied().unwrap_or(start));
                end?;
            }
        }
        self.last = block.last;
        Ok(())
    }

    /// Verify the final block, declared content size and checksum, if present.
    pub fn finish(self, checksum: Option<u32>) -> Result<()> {
        let produced = &self.out[self.bounds.frame_start..];
        if !self.last { return Err(Error::Truncated); }
        if self.header.content_size.is_some_and(|size| size != produced.len() as u64) {
            return Err(Error::ContentSize);
        }
        if self.header.checksum != checksum.is_some()
            || checksum.is_some_and(|expected| xxh64(produced, 0) as u32 != expected) {
            return Err(Error::Checksum);
        }
        Ok(())
    }
}
