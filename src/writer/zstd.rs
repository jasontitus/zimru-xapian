//! A single pledged Zstandard frame, including for items larger than the
//! compressor's one-shot input limit. Each job emits blocks of that frame.
//! Each job can match a bounded prefix and starts with fresh entropy tables
//! and explicit offsets. Only the final
//! job sets the last-block bit; native libzim stops after the first frame.

#![forbid(unsafe_code)]

use std::io::{self, Write};

use rayon::prelude::*;
use zstd_rs::{par::Job, CompressionConfig, Compressor, FrameHeader};

const JOB_LOG: u32 = 21;
const JOB_BYTES: usize = 1 << JOB_LOG;
// Ordinary clusters already run in parallel in the archive writer. Large
// individual items may use the shared Rayon pool, in bounded ordered batches.
const MAX_JOB_WORKERS: usize = 4;

struct Worker {
    codec: Compressor,
    encoded: Vec<u8>,
}

pub(super) struct Encoder<W> {
    output: W,
    workers: Vec<Worker>,
    buffer: Vec<u8>,
    job_bytes: usize,
    batch_bytes: usize,
    prefix_bytes: usize,
    history_len: usize,
    remaining: u64,
    first: bool,
    failed: bool,
}

impl<W: Write> Encoder<W> {
    pub(super) fn new(mut output: W, size: u64, level: i32, workers: usize) -> io::Result<Self> {
        // The reference codec interprets explicit 0 as its default level
        // (3), independently of zimru's archive default of 19.
        let level = if level == 0 {
            CompressionConfig::DEFAULT.level
        } else {
            level.clamp(zstd_rs::MIN_LEVEL, zstd_rs::MAX_LEVEL)
        };
        // Larger histories pay off on high-level text archives. Limit the
        // number of these larger encoder workspaces competing for memory.
        // A single encoder can also retain this history at medium levels
        // without multiplying workspaces. Parallel fast encoding uses smaller
        // jobs and shorter overlap, like native multi-worker compression.
        let long_window = (level >= 16 || (level >= 3 && workers <= 1)) && size > JOB_BYTES as u64;
        let job_log = if long_window { 23 } else { JOB_LOG };
        let job_bytes = 1 << job_log;
        let prefix_bytes = if long_window {
            job_bytes / 2
        } else if level >= 3 {
            job_bytes / 4
        } else {
            0
        };
        let max_workers = if long_window { 2 } else { MAX_JOB_WORKERS };
        let config = CompressionConfig {
            level,
            window_log: job_log,
            ..CompressionConfig::DEFAULT
        };
        let worker_count = size
            .div_ceil(job_bytes as u64)
            .clamp(1, workers.clamp(1, max_workers) as u64) as usize;
        let workers = (0..worker_count)
            .map(|_| {
                Ok(Worker {
                    codec: Compressor::new(config).map_err(io::Error::other)?,
                    encoded: Vec::new(),
                })
            })
            .collect::<io::Result<_>>()?;
        let batch_bytes = size.min((job_bytes * worker_count) as u64) as usize;
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(size.min((batch_bytes + prefix_bytes) as u64) as usize)
            .map_err(io::Error::other)?;

        let mut header = Vec::new();
        FrameHeader {
            content_size: Some(size),
            window_size: size.min(job_bytes as u64),
            single_segment: size <= job_bytes as u64,
            dict_id: 0,
            checksum: false,
            header_len: 0,
        }
        .write(job_log, &mut header);
        output.write_all(&header)?;
        Ok(Self {
            output,
            workers,
            buffer,
            job_bytes,
            batch_bytes,
            prefix_bytes,
            history_len: 0,
            remaining: size,
            first: true,
            failed: false,
        })
    }

    pub(super) fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("Zstandard frame already failed"));
        }
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Zstandard input exceeds pledged size",
            ));
        }
        while !bytes.is_empty() {
            let n = bytes
                .len()
                .min(self.batch_bytes - (self.buffer.len() - self.history_len));
            self.buffer.extend_from_slice(&bytes[..n]);
            self.remaining -= n as u64;
            bytes = &bytes[n..];
            if self.buffer.len() - self.history_len == self.batch_bytes || self.remaining == 0 {
                if let Err(error) = self.emit() {
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn emit(&mut self) -> io::Result<()> {
        let input = &self.buffer;
        let jobs = (input.len() - self.history_len)
            .div_ceil(self.job_bytes)
            .max(1);
        let first = self.first;
        let last = self.remaining == 0;
        let encode = |(index, worker): (usize, &mut Worker)| {
            let start = self.history_len + index * self.job_bytes;
            let prefix = start.saturating_sub(self.prefix_bytes);
            let bytes = &input[prefix..(start + self.job_bytes).min(input.len())];
            let job = Job {
                start: start - prefix,
                end: bytes.len(),
                prefix_start: 0,
                first: first && index == 0,
                last: last && index + 1 == jobs,
            };
            worker.encoded.clear();
            worker
                .codec
                .compress_job(bytes, &job, None, &mut worker.encoded)
                .map_err(io::Error::other)
        };
        let workers = &mut self.workers[..jobs];
        if jobs == 1 {
            encode((0, &mut workers[0]))?;
        } else {
            workers.par_iter_mut().enumerate().try_for_each(encode)?;
        }
        for worker in workers {
            self.output.write_all(&worker.encoded)?;
        }
        self.history_len = if last {
            0
        } else {
            self.prefix_bytes.min(input.len())
        };
        let from = input.len() - self.history_len;
        self.buffer.copy_within(from.., 0);
        self.buffer.truncate(self.history_len);
        self.first = false;
        Ok(())
    }

    pub(super) fn finish(mut self) -> io::Result<W> {
        if self.failed {
            return Err(io::Error::other("Zstandard frame already failed"));
        }
        if self.remaining != 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete Zstandard input",
            ));
        }
        // A zero-byte frame still needs a last block.
        if self.first {
            self.emit()?;
        }
        self.output.flush()?;
        Ok(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::zstd::zstd_safe;

    #[test]
    fn native_reader_accepts_one_pledged_frame_across_jobs_and_levels() {
        for (len, level, workers) in [
            (0, 0, 1),
            (1234, i32::MIN, 1),
            (125_000, -16, 1),
            (250_000, i32::MAX, 1),
            (JOB_BYTES, 1, 4),
            (4 * JOB_BYTES + 93, 3, 4),
        ] {
            let input = (0..len)
                .map(|i| (i.wrapping_mul(29) >> 5) as u8)
                .collect::<Vec<_>>();
            let mut writer = Encoder::new(Vec::new(), len as u64, level, workers).unwrap();
            for piece in input.chunks(51_997) {
                writer.write_all(piece).unwrap();
            }
            let output = writer.finish().unwrap();
            assert_eq!(
                zstd_safe::get_frame_content_size(&output).unwrap(),
                Some(len as u64)
            );
            assert_eq!(
                zstd_safe::find_frame_compressed_size(&output).unwrap(),
                output.len()
            );
            assert_eq!(
                ::zstd::stream::decode_all(output.as_slice()).unwrap(),
                input
            );
        }
    }

    #[test]
    fn incomplete_pledge_and_partial_output_errors_cannot_finish() {
        let mut short = Encoder::new(Vec::new(), 3, 3, 1).unwrap();
        assert!(short.write_all(b"four").is_err());
        short.write_all(b"ok").unwrap();
        assert_eq!(
            short.finish().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );

        struct HeaderOnly(bool);
        impl Write for HeaderOnly {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("output closed"));
                }
                self.0 = true;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut cancelled = Encoder::new(HeaderOnly(false), 4, 1, 1).unwrap();
        assert!(cancelled.write_all(b"four").is_err());
        assert!(cancelled.write_all(b"").is_err());
        assert!(cancelled.finish().is_err());
    }
}
