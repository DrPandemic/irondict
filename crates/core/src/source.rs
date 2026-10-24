//! Read-only byte sources for StarDict sub-files.
//!
//! A dictionary sub-file (`.ifo`, `.idx`, `.syn`, `.dict`/`.dict.dz`) can come
//! from one of three places:
//!
//! * a path on the filesystem (desktop, or a dictionary unpacked to disk),
//! * a byte range of an already-open file — this is how a *stored* APK asset is
//!   read in place: the asset's descriptor points at `base.apk`, and the range is
//!   the asset's slice of it,
//! * an owned buffer — for a *compressed* APK asset, which cannot be opened as a
//!   range and so is read out once.
//!
//! All three resolve to a read-only byte slice. Files and ranges are
//! memory-mapped in place (so the 34 MB GCIDE never lands on the heap);
//! in-memory sources are shared behind an `Arc`.

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use memmap2::{Mmap, MmapOptions};

use crate::Error;

/// A read-only byte source.
#[derive(Clone)]
pub enum DictSource {
    /// A path on the filesystem.
    Path(PathBuf),
    /// A byte range `[offset, offset + len)` of an already-open file. `name` is
    /// a stable label used to key the sidecar cache (the underlying file may be
    /// an APK whose descriptor number changes every launch, so it can't be used).
    Range {
        file: Arc<File>,
        offset: u64,
        len: u64,
        name: String,
    },
    /// An owned buffer.
    Bytes(Arc<[u8]>),
}

/// A resolved byte slice, either mapped from a file or shared from memory.
pub enum Bytes {
    Mapped(Mmap),
    Owned(Arc<[u8]>),
}

impl Bytes {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m) => &m[..],
            Bytes::Owned(b) => b,
        }
    }
}

/// Identity of a source for validating the `.idx` sidecar cache.
pub struct CacheKey {
    /// Absolute path (or a stable label for a range) stored in the cache header
    /// to guard against the hash name colliding with another dictionary.
    pub name: String,
    pub len: u64,
    pub mtime: Option<SystemTime>,
}

impl DictSource {
    /// A filesystem source.
    pub fn path(p: impl Into<PathBuf>) -> Self {
        DictSource::Path(p.into())
    }

    /// An owned-buffer source.
    pub fn from_bytes(b: Vec<u8>) -> Self {
        DictSource::Bytes(Arc::from(b))
    }

    /// Resolve to bytes. Paths and ranges are memory-mapped; buffers are shared.
    pub fn open(&self) -> Result<Bytes, Error> {
        match self {
            DictSource::Path(p) => {
                let file = File::open(p)?;
                // SAFETY: a dictionary sub-file is read-only data we never write
                // to; an external truncation could fault, but that is true of
                // every dictionary file the app maps and is outside the threat
                // model.
                Ok(Bytes::Mapped(unsafe { Mmap::map(&file)? }))
            }
            DictSource::Range {
                file,
                offset,
                len,
                ..
            } => {
                // `MmapOptions::offset` accepts offsets that aren't page-aligned.
                // SAFETY: same as above; the range was validated by the caller
                // (an APK asset descriptor).
                let mmap = unsafe {
                    MmapOptions::new()
                        .offset(*offset)
                        .len(*len as usize)
                        .map(&**file)?
                };
                Ok(Bytes::Mapped(mmap))
            }
            DictSource::Bytes(b) => Ok(Bytes::Owned(Arc::clone(b))),
        }
    }

    /// The identity to key the `.idx` sidecar cache on, or `None` for a purely
    /// in-memory source (a compressed APK asset), which has no stable on-disk
    /// identity and simply skips the cache.
    pub fn cache_key(&self) -> Option<CacheKey> {
        match self {
            DictSource::Path(p) => {
                let meta = std::fs::metadata(p).ok()?;
                let name = std::fs::canonicalize(p)
                    .unwrap_or_else(|_| p.clone())
                    .to_string_lossy()
                    .into_owned();
                Some(CacheKey {
                    name,
                    len: meta.len(),
                    mtime: meta.modified().ok(),
                })
            }
            DictSource::Range { file, name, .. } => {
                let meta = file.metadata().ok()?;
                Some(CacheKey {
                    name: name.clone(),
                    len: meta.len(),
                    mtime: meta.modified().ok(),
                })
            }
            DictSource::Bytes(_) => None,
        }
    }

    /// Read the whole source into an owned buffer. Used for a gzipped `.idx`,
    /// which must be decompressed anyway.
    pub fn read_all(&self) -> Result<Vec<u8>, Error> {
        match self {
            DictSource::Path(p) => Ok(std::fs::read(p)?),
            DictSource::Range {
                file,
                offset,
                len,
                ..
            } => {
                use std::os::unix::fs::FileExt;
                let mut buf = vec![0u8; *len as usize];
                file.read_exact_at(&mut buf, *offset)?;
                Ok(buf)
            }
            DictSource::Bytes(b) => Ok(b.to_vec()),
        }
    }
}
