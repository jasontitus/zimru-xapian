//! Directory entry parsing.
//!
//! There are two kinds of dirents:
//!
//! ## Article entry (mimetype != 0xFFFF)
//! ```text
//! u16  mimetype index   (0xFFFE/0xFFFD reserved for old "linktarget"/"deletedentry")
//! u8   parameter_len    (always 0 in current format; reserved)
//! u8   namespace        (single ASCII char)
//! u32  revision         (always 0; reserved)
//! u32  cluster_number
//! u32  blob_number
//! cstr url
//! cstr title            (empty == reuse url as title)
//! u8[parameter_len] parameter
//! ```
//!
//! ## Redirect entry (mimetype == 0xFFFF)
//! ```text
//! u16  mimetype = 0xFFFF
//! u8   parameter_len
//! u8   namespace
//! u32  revision
//! u32  redirect_index   (index into the URL-pointer list)
//! cstr url
//! cstr title
//! u8[parameter_len] parameter
//! ```

use crate::error::{Error, Result};
use crate::raw;

pub const MIME_REDIRECT: u16 = 0xFFFF;
pub const MIME_LINKTARGET: u16 = 0xFFFE; // deprecated
pub const MIME_DELETED: u16 = 0xFFFD; // deprecated

#[derive(Debug, Clone)]
pub enum Dirent {
    Article(ArticleEntry),
    Redirect(RedirectEntry),
}

/// An ordinal directory row, including obsolete records with no item data.
#[derive(Debug, Clone)]
pub enum DirectoryRecord {
    Entry(Dirent),
    Retired {
        kind: u16,
        namespace: u8,
        url: String,
        title: String,
    },
}

impl DirectoryRecord {
    pub fn namespace(&self) -> u8 {
        match self {
            Self::Entry(entry) => entry.namespace(),
            Self::Retired { namespace, .. } => *namespace,
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::Entry(entry) => entry.url(),
            Self::Retired { url, .. } => url,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Entry(entry) => entry.title(),
            Self::Retired { url, title, .. } => {
                if title.is_empty() {
                    url
                } else {
                    title
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArticleEntry {
    pub mimetype: u16,
    pub namespace: u8,
    pub cluster: u32,
    pub blob: u32,
    pub url: String,
    /// Raw on-disk title — empty when the dirent reuses `url` as its title.
    /// Use [`Dirent::title`] (or [`ArticleEntry::title`]) for the effective title.
    pub title: String,
}

impl ArticleEntry {
    /// Effective title: falls back to `url` when the on-disk title is empty.
    pub fn title(&self) -> &str {
        if self.title.is_empty() {
            &self.url
        } else {
            &self.title
        }
    }
}

#[derive(Debug, Clone)]
pub struct RedirectEntry {
    pub namespace: u8,
    pub redirect_index: u32,
    pub url: String,
    /// Raw on-disk title — empty when the dirent reuses `url` as its title.
    /// Use [`Dirent::title`] (or [`RedirectEntry::title`]) for the effective title.
    pub title: String,
}

impl RedirectEntry {
    /// Effective title: falls back to `url` when the on-disk title is empty.
    pub fn title(&self) -> &str {
        if self.title.is_empty() {
            &self.url
        } else {
            &self.title
        }
    }
}

impl Dirent {
    /// Parse a dirent starting at `off` in the underlying buffer.
    pub fn parse(buf: &[u8], off: usize) -> Result<Self> {
        match Self::parse_record(buf, off)? {
            DirectoryRecord::Entry(record) => Ok(record),
            DirectoryRecord::Retired { kind, .. } => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("unsupported legacy directory entry type {kind:#06x}"),
            )
            .into()),
        }
    }

    pub(crate) fn parse_record(buf: &[u8], off: usize) -> Result<DirectoryRecord> {
        let mimetype = raw::u16_at(buf, off)?;
        let parameter_len = raw::u8_at(buf, off + 2)? as usize;
        let namespace = raw::u8_at(buf, off + 3)?;
        let _revision = raw::u32_at(buf, off + 4)?;

        let (url, after_url) = raw::cstr_at(buf, off + path_offset(mimetype))?;
        let (title, after_title) = raw::cstr_at(buf, after_url)?;
        if parameter_len > buf.len() - after_title {
            return Err(Error::Truncated(after_title as u64));
        }

        match mimetype {
            MIME_REDIRECT => Ok(DirectoryRecord::Entry(Dirent::Redirect(RedirectEntry {
                namespace,
                redirect_index: raw::u32_at(buf, off + 8)?,
                url: url.to_owned(),
                title: title.to_owned(),
            }))),
            MIME_LINKTARGET | MIME_DELETED => Ok(DirectoryRecord::Retired {
                kind: mimetype,
                namespace,
                url: url.to_owned(),
                title: title.to_owned(),
            }),
            _ => Ok(DirectoryRecord::Entry(Dirent::Article(ArticleEntry {
                mimetype,
                namespace,
                cluster: raw::u32_at(buf, off + 8)?,
                blob: raw::u32_at(buf, off + 12)?,
                url: url.to_owned(),
                title: title.to_owned(),
            }))),
        }
    }

    /// Cheap key extraction for binary-search probes: read just the
    /// `(namespace, url)` of the dirent at `off` without allocating. The
    /// returned `&str` borrows from `buf`. Hot-path equivalent of
    /// [`Dirent::parse`] — that fully materializes the dirent and pays
    /// two `String` allocations per call (url + title), which is the
    /// bulk of the `~1.2 µs/op` `entry_by_ns_path` cost on a 17 K-entry
    /// archive (14-probe binary search → 28 String allocs per lookup).
    pub fn key_at(buf: &[u8], off: usize) -> Result<(u8, &str)> {
        let mimetype = raw::u16_at(buf, off)?;
        let namespace = raw::u8_at(buf, off + 3)?;
        let url_off = off + path_offset(mimetype);
        let (url, _) = raw::cstr_at(buf, url_off)?;
        Ok((namespace, url))
    }

    /// Cheap title extraction for binary-search probes — returns
    /// `(namespace, title)` borrowed from `buf`. Title falls back to
    /// `url` when the dirent's title field is empty (matching
    /// [`Dirent::parse`]'s behaviour).
    pub fn title_key_at(buf: &[u8], off: usize) -> Result<(u8, &str)> {
        let mimetype = raw::u16_at(buf, off)?;
        let namespace = raw::u8_at(buf, off + 3)?;
        let url_off = off + path_offset(mimetype);
        let (url, after_url) = raw::cstr_at(buf, url_off)?;
        let (title, _) = raw::cstr_at(buf, after_url)?;
        Ok((namespace, if title.is_empty() { url } else { title }))
    }

    pub fn url(&self) -> &str {
        match self {
            Dirent::Article(a) => &a.url,
            Dirent::Redirect(r) => &r.url,
        }
    }

    /// Effective title: falls back to `url` when the on-disk title is empty
    /// (the common case for content entries), without having allocated a
    /// second copy of the url at parse time.
    pub fn title(&self) -> &str {
        match self {
            Dirent::Article(a) => a.title(),
            Dirent::Redirect(r) => r.title(),
        }
    }

    pub fn namespace(&self) -> u8 {
        match self {
            Dirent::Article(a) => a.namespace,
            Dirent::Redirect(r) => r.namespace,
        }
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self, Dirent::Redirect(_))
    }
}

// Offsets from the start of a directory record to its first string.
fn path_offset(mimetype: u16) -> usize {
    match mimetype {
        MIME_REDIRECT => 12,
        MIME_LINKTARGET | MIME_DELETED => 8,
        _ => 16,
    }
}

/// Compare a (namespace, url) tuple — that's the canonical sort key for the
/// URL pointer list.
pub fn cmp_path(a_ns: u8, a_url: &str, b_ns: u8, b_url: &str) -> std::cmp::Ordering {
    a_ns.cmp(&b_ns).then_with(|| a_url.cmp(b_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_article(
        mime: u16,
        ns: u8,
        cluster: u32,
        blob: u32,
        url: &str,
        title: &str,
    ) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&mime.to_le_bytes());
        v.push(0); // parameter_len
        v.push(ns);
        v.extend_from_slice(&0u32.to_le_bytes()); // revision
        v.extend_from_slice(&cluster.to_le_bytes());
        v.extend_from_slice(&blob.to_le_bytes());
        v.extend_from_slice(url.as_bytes());
        v.push(0);
        v.extend_from_slice(title.as_bytes());
        v.push(0);
        v
    }

    fn build_redirect(ns: u8, idx: u32, url: &str, title: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&MIME_REDIRECT.to_le_bytes());
        v.push(0);
        v.push(ns);
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&idx.to_le_bytes());
        v.extend_from_slice(url.as_bytes());
        v.push(0);
        v.extend_from_slice(title.as_bytes());
        v.push(0);
        v
    }

    #[test]
    fn round_trips_an_article_dirent() {
        let buf = build_article(0, b'C', 5, 3, "home", "Home");
        let d = Dirent::parse(&buf, 0).unwrap();
        match d {
            Dirent::Article(a) => {
                assert_eq!(a.mimetype, 0);
                assert_eq!(a.namespace, b'C');
                assert_eq!(a.cluster, 5);
                assert_eq!(a.blob, 3);
                assert_eq!(a.url, "home");
                assert_eq!(a.title, "Home");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn empty_title_falls_back_to_url() {
        let buf = build_article(2, b'C', 0, 0, "kiwix.png", "");
        let d = Dirent::parse(&buf, 0).unwrap();
        assert_eq!(d.title(), "kiwix.png");
    }

    #[test]
    fn complete_records_include_the_declared_parameters() {
        for mut buf in [
            build_article(0, b'A', 0, 0, "path", ""),
            build_redirect(b'A', 1, "old", ""),
        ] {
            buf[2] = 2;
            buf.extend_from_slice(&[1, 2]);
            assert!(Dirent::parse(&buf, 0).is_ok());
            assert!(matches!(
                Dirent::parse(&buf[..buf.len() - 1], 0),
                Err(Error::Truncated(_))
            ));
        }
    }

    #[test]
    fn obsolete_markers_have_search_keys_and_no_blob_location() {
        for kind in [MIME_LINKTARGET, MIME_DELETED] {
            let mut buf = vec![0; 8];
            buf[..2].copy_from_slice(&kind.to_le_bytes());
            buf[3] = b'A';
            buf.extend_from_slice(b"old page\0Title\0");
            let record = Dirent::parse_record(&buf, 0).unwrap();
            assert_eq!(
                (record.namespace(), record.path(), record.title()),
                (b'A', "old page", "Title")
            );
            assert!(
                matches!(record, DirectoryRecord::Retired { kind: actual, .. } if actual == kind)
            );
            assert_eq!(Dirent::key_at(&buf, 0).unwrap(), (b'A', "old page"));
            assert_eq!(Dirent::title_key_at(&buf, 0).unwrap(), (b'A', "Title"));
            assert!(
                matches!(Dirent::parse(&buf, 0), Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::Unsupported)
            );
        }
    }

    #[test]
    fn parses_redirects() {
        let buf = build_redirect(b'C', 17, "old", "Old");
        let d = Dirent::parse(&buf, 0).unwrap();
        assert!(d.is_redirect());
        if let Dirent::Redirect(r) = d {
            assert_eq!(r.redirect_index, 17);
            assert_eq!(r.namespace, b'C');
            assert_eq!(r.url, "old");
            assert_eq!(r.title, "Old");
        }
    }
}
