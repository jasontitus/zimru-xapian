use super::error::{set_err, zimru_error_t};
use crate::{Error, Result};
use std::io;

/// Copy through initialized Rust storage. A C caller may provide writable
/// but uninitialized bytes, which cannot form a Rust `&mut [u8]`.
///
/// # Safety
/// A non-null source must be a live handle. The destination must be writable
/// for `len` bytes. All non-null writable arguments must be disjoint, valid
/// for their types and exclusively available to this call.
pub(super) unsafe fn copy_read_at<T>(
    source: *const T,
    mut offset: u64,
    dst: *mut u8,
    len: usize,
    out_read: *mut usize,
    err: *mut *mut zimru_error_t,
    read: impl Fn(&T, u64, &mut [u8]) -> Result<usize>,
) -> bool {
    if !out_read.is_null() {
        out_read.write(0);
    }
    if source.is_null()
        || out_read.is_null()
        || (dst.is_null() && len != 0)
        || len > isize::MAX as usize
    {
        set_err(
            err,
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid positional read arguments",
            )),
        );
        return false;
    }
    let source = &*source;
    let mut scratch = vec![0u8; len.min(64 * 1024)];
    let mut copied = 0;
    while copied < len {
        let chunk = (len - copied).min(scratch.len());
        let n = match read(source, offset, &mut scratch[..chunk]) {
            Ok(n) => n,
            Err(e) => {
                set_err(err, e);
                return false;
            }
        };
        if n > chunk {
            set_err(
                err,
                Error::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "reader returned more bytes than requested",
                )),
            );
            return false;
        }
        std::ptr::copy_nonoverlapping(scratch.as_ptr(), dst.add(copied), n);
        copied += n;
        out_read.write(copied);
        if n < chunk || copied == len {
            break;
        }
        let Some(next) = offset.checked_add(n as u64) else {
            set_err(
                err,
                Error::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "read offset overflow",
                )),
            );
            return false;
        };
        offset = next;
    }
    true
}
