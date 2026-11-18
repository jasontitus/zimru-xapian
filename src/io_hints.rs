//! Best-effort read-ahead hints. All callers must work without a hint.

use std::fs::File;

/// Hint that the file will be read sequentially.
pub fn hint_sequential(_file: &File) {
    #[cfg(target_os = "linux")]
    let _ = rustix::fs::fadvise(_file, 0, None, rustix::fs::Advice::Sequential);
    #[cfg(target_os = "macos")]
    hint_will_need(_file);
}

/// Prefetch file pages where the operating system exposes a suitable hint.
pub fn hint_will_need(_file: &File) {
    #[cfg(target_os = "linux")]
    let _ = rustix::fs::fadvise(_file, 0, None, rustix::fs::Advice::WillNeed);
    #[cfg(target_os = "macos")]
    if let Ok(metadata) = _file.metadata() {
        // Apple's advisory read uses a signed 32-bit size. Limit speculative
        // read-ahead to the beginning of the file, even on large archives.
        let bytes = metadata.len().min(1024 * 1024);
        if bytes > 0 {
            let _ = rustix::fs::fcntl_rdadvise(_file, 0, bytes);
        }
    }
}
