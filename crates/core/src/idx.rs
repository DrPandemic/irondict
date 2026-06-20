//! StarDict `.idx` / `.syn` handling, owned by us rather than delegated to the
//! `stardict` crate so that neither file has to be parsed into a `HashMap` at
//! boot.
//!
//! The `.idx` maps each headword to its `.dict` block(s). Rather than decode it
//! into owned strings/vectors on every launch (allocation-bound across the ~1.7M
//! records of the installed set), we keep the `.idx` **memory-mapped** and store
//! only what a lookup actually needs, as two small `u32` tables:
//!
//! * `rec_offsets` — the byte offset of every record's start in the mapped file,
//!   so a record's headword and `.dict` block can be read straight from the map.
//!   It covers *all* records in file order (including empty-word ones) so a
//!   `.syn` index, which points into that order, maps directly to a record.
//! * `order` — the record indices of the non-empty headwords, sorted by their
//!   lowercased word, so a lookup is an `O(log n)` binary search over the map
//!   instead of an `O(1)` hash probe into a parsed table.
//!
//! Those two tables are the only derived data, and they are persisted to a
//! **sidecar cache** in the OS cache dir (see [`cache`]). On the first launch
//! after a dictionary is added or changed we scan the `.idx` once (a cheap
//! sequential boundary scan plus a parallel sort) and write the cache; every
//! later launch just reads the cache back and re-maps the `.idx`, so opening a
//! dictionary does no parsing at all. The cache is validated against the
//! `.idx` file's length and mtime and silently rebuilt on any mismatch, and all
//! cache I/O is best-effort — a read-only or missing cache dir just falls back
//! to scanning.
//!
//! The `.syn` file (variant / inflected forms aliasing a headword) is handled
//! the same way it always has been: kept memory-mapped and **binary-searched on
//! demand** (it is already sorted by lowercased word on disk), with a one-off
//! lazy offset scan on the first synonym lookup — so opening a dictionary does
//! no `.syn` work either.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use memmap2::Mmap;
use rayon::prelude::*;
use stardict::idx::{IdxEntry, IdxEntryBlock};
use stardict::ifo::{Ifo, Version};

use crate::Error;

/// Decode a StarDict string field, borrowing the mapped bytes when they are
/// valid UTF-8 (essentially every record) and only allocating on the rare
/// invalid record, where it falls back to lossy decoding with the replacement
/// character dropped — the way the `stardict` crate decodes, so headwords stay
/// byte-identical to what the rest of the pipeline expects.
fn decode_cow(buf: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(buf) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => Cow::Owned(
            String::from_utf8_lossy(buf)
                .chars()
                .filter(|&c| c != '\u{fffd}')
                .collect(),
        ),
    }
}

/// [`decode_cow`] as an owned `String`, used where a borrow can't be held.
fn decode(buf: &[u8]) -> String {
    decode_cow(buf).into_owned()
}

/// Width in bytes of the `.idx` offset and size fields. StarDict v2.4.2 uses
/// 32-bit fields; v3.0.0 uses 64-bit only when `idxoffsetbits=64`. (The `.syn`
/// index is always a 32-bit field regardless — see [`Syn`].)
fn idx_field_bytes(ifo: &Ifo) -> usize {
    match ifo.version {
        Version::V300 if ifo.idxoffsetbits == 64 => 8,
        _ => 4,
    }
}

fn read_be(buf: &[u8]) -> usize {
    buf.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize)
}

/// Headword count (from the `.ifo`) below which the sidecar cache isn't used:
/// scanning a small `.idx` costs well under a millisecond, so a cache would only
/// add I/O and clutter the cache dir (notably with throwaway test fixtures). The
/// cache earns its keep on the large dictionaries the boot cost actually comes
/// from.
const CACHE_MIN_WORDS: usize = 10_000;

/// The `.idx` bytes the [`Index`] reads from: memory-mapped for a plain `.idx`,
/// or owned for a gzipped one (which has to be decompressed into memory). The
/// record-offset table indexes into whichever this is.
enum IdxData {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl IdxData {
    fn as_bytes(&self) -> &[u8] {
        match self {
            IdxData::Mapped(m) => &m[..],
            IdxData::Owned(v) => v,
        }
    }

    /// Map a plain `.idx`, or read + decompress a gzipped one.
    fn open(idx_path: &Path, idx_gz: bool) -> Result<IdxData, Error> {
        if idx_gz {
            let bytes = std::fs::read(idx_path)?;
            let mut out = Vec::new();
            GzDecoder::new(bytes.as_slice()).read_to_end(&mut out)?;
            Ok(IdxData::Owned(out))
        } else {
            let file = File::open(idx_path)?;
            // SAFETY: the `.idx` is a read-only data file we never write to; a
            // concurrent external truncation could fault, but that is true of
            // every dictionary file the app maps and is outside the threat model.
            let mmap = unsafe { Mmap::map(&file)? };
            Ok(IdxData::Mapped(mmap))
        }
    }
}

/// Read a record's original-case headword from the mapped `.idx` at byte
/// `off` (a record start): the bytes up to the terminating `\0`.
fn read_word(data: &[u8], off: usize) -> Cow<'_, str> {
    let rel = data[off..]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(data.len() - off);
    decode_cow(&data[off..off + rel])
}

/// A mapped `.idx` plus the small derived tables that drive lookups, and an
/// optional lazily-searched `.syn`. None of the headwords are copied into owned
/// strings at boot; they are read from [`data`](Self::data) on demand.
pub struct Index {
    data: IdxData,
    /// Width of the `.idx` offset/size fields (4 or 8), from the `.ifo`.
    field_bytes: usize,
    /// Byte offset of every record's start in `data`, in file order (including
    /// empty-word records, so a `.syn` index maps directly to record `i`).
    rec_offsets: Vec<u32>,
    /// Record indices (into `rec_offsets`) of the non-empty headwords, sorted by
    /// lowercased word then by record index. Binary-searched for lookups; equal
    /// keys form one contiguous run, in ascending record order.
    order: Vec<u32>,
    /// The synonym file, kept mapped and searched on demand. `None` when the
    /// dictionary ships no `.syn`.
    syn: Option<Syn>,
}

impl Index {
    /// Open `idx_path` (gz-decompressing when `idx_gz`) and remember `syn` for
    /// lazy resolution. The derived tables are loaded from the sidecar cache when
    /// it is present and valid, otherwise built by one scan of the `.idx` and
    /// written back. Only the `.idx` is touched here; the `.syn` is mapped but
    /// not scanned until the first synonym lookup.
    pub fn new(
        idx_path: &Path,
        ifo: &Ifo,
        idx_gz: bool,
        syn: Option<&Path>,
    ) -> Result<Index, Error> {
        let data = IdxData::open(idx_path, idx_gz)?;
        let w = idx_field_bytes(ifo);

        // Validate/load the cache against the source file's length and mtime
        // (not the decompressed bytes), so a gzipped `.idx` is keyed correctly.
        let (src_len, src_mtime) = match std::fs::metadata(idx_path) {
            Ok(m) => (m.len(), m.modified().ok()),
            Err(_) => (0, None),
        };

        // Only large dictionaries go through the cache; small ones are cheap to
        // scan and aren't worth a cache file (see [`CACHE_MIN_WORDS`]).
        let (rec_offsets, order) = if ifo.wordcount >= CACHE_MIN_WORDS {
            match cache::load(idx_path, src_len, src_mtime) {
                Some(tables) => tables,
                None => {
                    let tables = build(data.as_bytes(), w);
                    cache::store(idx_path, src_len, src_mtime, &tables.0, &tables.1);
                    tables
                }
            }
        } else {
            build(data.as_bytes(), w)
        };

        let syn = match syn {
            Some(path) => Some(Syn::open(path)?),
            None => None,
        };
        Ok(Index {
            data,
            field_bytes: w,
            rec_offsets,
            order,
            syn,
        })
    }

    /// Number of records (file order), including empty-word ones.
    fn len(&self) -> usize {
        self.rec_offsets.len()
    }

    /// Record `i`'s original-case headword, read from the mapped `.idx`.
    fn word(&self, i: usize) -> Cow<'_, str> {
        read_word(self.data.as_bytes(), self.rec_offsets[i] as usize)
    }

    /// Record `i`'s `.dict` block `(offset, size)`, read from the mapped `.idx`.
    fn fields(&self, i: usize) -> (usize, usize) {
        let data = self.data.as_bytes();
        let off = self.rec_offsets[i] as usize;
        let rel = data[off..].iter().position(|&b| b == 0).unwrap_or(0);
        let fp = off + rel + 1;
        let w = self.field_bytes;
        (read_be(&data[fp..fp + w]), read_be(&data[fp + w..fp + 2 * w]))
    }

    /// Merge the blocks of every record sharing a headword into a single
    /// [`IdxEntry`], taking the first record's original-case word. `ids` are
    /// record indices (a run from `order`); allocated only at lookup/iteration
    /// time, not for every record at boot.
    fn merge(&self, ids: &[u32]) -> IdxEntry {
        let word = self.word(ids[0] as usize).into_owned();
        let blocks = ids
            .iter()
            .map(|&id| {
                let (offset, size) = self.fields(id as usize);
                IdxEntryBlock { offset, size }
            })
            .collect();
        IdxEntry { word, blocks }
    }

    /// The contiguous run of `order` whose lowercased headword equals `lower`,
    /// found by binary search. Returns the record indices, or `None` for a miss.
    fn find_run(&self, lower: &str) -> Option<&[u32]> {
        let order = &self.order;
        let n = order.len();
        // Lower bound: the first record whose key is not less than `lower`.
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.word(order[mid] as usize).to_lowercase().as_str() < lower {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let start = lo;
        let mut k = lo;
        while k < n && self.word(order[k] as usize).to_lowercase() == lower {
            k += 1;
        }
        if k > start {
            Some(&order[start..k])
        } else {
            None
        }
    }

    /// The entries for `word`: its own definition (if any) followed by the
    /// definitions of any headwords it aliases through `.syn`. Returns `None`
    /// when nothing matches. Mirrors the `stardict` crate's `lookup_blocks`,
    /// resolving synonyms in the forward direction (variant/inflection → entry).
    pub fn lookup_blocks(&self, word: &str) -> Option<Vec<IdxEntry>> {
        let lower = word.to_lowercase();
        let mut out: Vec<IdxEntry> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        if let Some(ids) = self.find_run(&lower) {
            let entry = self.merge(ids);
            seen.insert(entry.word.clone());
            out.push(entry);
        }
        if let Some(syn) = &self.syn {
            for raw in syn.resolve(word) {
                let raw = raw as usize;
                if raw >= self.len() {
                    continue;
                }
                let canonical = self.word(raw).to_lowercase();
                if let Some(ids) = self.find_run(&canonical) {
                    let entry = self.merge(ids);
                    if seen.insert(entry.word.clone()) {
                        out.push(entry);
                    }
                }
            }
        }

        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// A headword at position `n` modulo the searchable set. Used to pick a
    /// "word of the moment" without materializing every entry. Returns `None`
    /// only for an empty dictionary.
    pub fn nth_word(&self, n: usize) -> Option<String> {
        if self.order.is_empty() {
            return None;
        }
        let id = self.order[n % self.order.len()];
        Some(self.word(id as usize).into_owned())
    }

    /// Visit every distinct headword as a merged [`IdxEntry`] (one per word, all
    /// its blocks). Used to feed the search index. Walks `order`, grouping the
    /// contiguous run of each lowercased word.
    pub fn for_each_word(&self, mut f: impl FnMut(IdxEntry) -> std::ops::ControlFlow<()>) {
        let n = self.order.len();
        let mut k = 0usize;
        while k < n {
            let key = self.word(self.order[k] as usize).to_lowercase();
            let start = k;
            k += 1;
            while k < n && self.word(self.order[k] as usize).to_lowercase() == key {
                k += 1;
            }
            if f(self.merge(&self.order[start..k])).is_break() {
                break;
            }
        }
    }
}

/// Build the derived tables by scanning the `.idx` bytes: a cheap sequential
/// boundary scan for `rec_offsets`, then a parallel sort of the non-empty
/// records by lowercased word (tie-broken by record index, so equal-word runs
/// stay in ascending order). Run only on a cache miss.
fn build(data: &[u8], w: usize) -> (Vec<u32>, Vec<u32>) {
    let n = data.len();

    // The fixed-width offset/size field can itself contain `0`, so advance past
    // it rather than scanning into it. Empty headwords are kept so `.syn`
    // indices stay valid.
    let mut rec_offsets: Vec<u32> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let Some(rel) = data[i..].iter().position(|&b| b == 0) else {
            break;
        };
        let zero = i + rel;
        if zero + 1 + 2 * w > n {
            break;
        }
        rec_offsets.push(i as u32);
        i = zero + 1 + 2 * w;
    }

    // Searchable subset: records whose headword is non-empty (the byte at the
    // record start isn't the terminating `\0`).
    let mut order: Vec<u32> = rec_offsets
        .iter()
        .enumerate()
        .filter(|&(_, &off)| data.get(off as usize) != Some(&0))
        .map(|(i, _)| i as u32)
        .collect();
    order.par_sort_by_cached_key(|&i| {
        let off = rec_offsets[i as usize] as usize;
        (read_word(data, off).to_lowercase(), i)
    });

    (rec_offsets, order)
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("records", &self.len())
            .field("searchable", &self.order.len())
            .field("has_syn", &self.syn.is_some())
            .finish()
    }
}

/// On-disk sidecar cache of an `.idx`'s derived lookup tables (`rec_offsets` and
/// `order`), so a launch after the first skips scanning and sorting the `.idx`.
///
/// One file per dictionary lives in the OS cache dir, named by a hash of the
/// `.idx`'s absolute path. The header stores the source `.idx`'s length and
/// mtime plus its absolute path; a load is accepted only when all three match
/// the file on disk (length + mtime guard against an updated dictionary, the
/// stored path guards against a hash collision). Everything is little-endian.
/// All I/O here is best-effort: any failure to read or write the cache falls
/// back to building the tables from the `.idx`, so a read-only or absent cache
/// dir is harmless.
mod cache {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    const MAGIC: &[u8; 4] = b"IDXC";
    const VERSION: u32 = 1;

    /// The `.idx` absolute path as a string, used both to name the cache file
    /// and, stored in the header, to guard against hash collisions.
    fn abs_path(idx_path: &Path) -> String {
        std::fs::canonicalize(idx_path)
            .unwrap_or_else(|_| idx_path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }

    fn cache_path(abs: &str) -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "irondict")?;
        let mut h = DefaultHasher::new();
        abs.hash(&mut h);
        let name = format!("{:016x}.idxcache", h.finish());
        Some(dirs.cache_dir().join("idx").join(name))
    }

    fn mtime_nanos(t: Option<SystemTime>) -> i64 {
        t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0)
    }

    fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
        Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
    }

    fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
        Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
    }

    fn rd_i64(b: &[u8], o: usize) -> Option<i64> {
        Some(i64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
    }

    fn read_u32_vec(bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// Load and validate the cache for `idx_path`. Returns the
    /// `(rec_offsets, order)` tables, or `None` on any miss/mismatch/error.
    pub fn load(
        idx_path: &Path,
        src_len: u64,
        src_mtime: Option<SystemTime>,
    ) -> Option<(Vec<u32>, Vec<u32>)> {
        let abs = abs_path(idx_path);
        let path = cache_path(&abs)?;
        let buf = std::fs::read(&path).ok()?;
        parse(&buf, &abs, src_len, src_mtime)
    }

    fn parse(
        buf: &[u8],
        abs: &str,
        src_len: u64,
        src_mtime: Option<SystemTime>,
    ) -> Option<(Vec<u32>, Vec<u32>)> {
        if buf.get(0..4)? != MAGIC || rd_u32(buf, 4)? != VERSION {
            return None;
        }
        if rd_u64(buf, 8)? != src_len {
            return None;
        }
        // Validate mtime only when both sides have one; otherwise length and the
        // stored path carry the check.
        let stored_mtime = rd_i64(buf, 16)?;
        let cur_mtime = mtime_nanos(src_mtime);
        if stored_mtime != 0 && cur_mtime != 0 && stored_mtime != cur_mtime {
            return None;
        }

        let path_len = rd_u32(buf, 24)? as usize;
        let mut p = 28usize;
        if buf.get(p..p + path_len)? != abs.as_bytes() {
            return None;
        }
        p += path_len;

        let n = rd_u64(buf, p)? as usize;
        p += 8;
        let rec_offsets = read_u32_vec(buf.get(p..p + n * 4)?);
        p += n * 4;

        let m = rd_u64(buf, p)? as usize;
        p += 8;
        let order = read_u32_vec(buf.get(p..p + m * 4)?);

        Some((rec_offsets, order))
    }

    /// Write the cache for `idx_path`. Best-effort: any error is ignored, so the
    /// next launch simply rebuilds the tables.
    pub fn store(
        idx_path: &Path,
        src_len: u64,
        src_mtime: Option<SystemTime>,
        rec_offsets: &[u32],
        order: &[u32],
    ) {
        let abs = abs_path(idx_path);
        let Some(path) = cache_path(&abs) else {
            return;
        };

        let mut buf =
            Vec::with_capacity(28 + abs.len() + 16 + rec_offsets.len() * 4 + order.len() * 4);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&src_len.to_le_bytes());
        buf.extend_from_slice(&mtime_nanos(src_mtime).to_le_bytes());
        buf.extend_from_slice(&(abs.len() as u32).to_le_bytes());
        buf.extend_from_slice(abs.as_bytes());
        buf.extend_from_slice(&(rec_offsets.len() as u64).to_le_bytes());
        for &x in rec_offsets {
            buf.extend_from_slice(&x.to_le_bytes());
        }
        buf.extend_from_slice(&(order.len() as u64).to_le_bytes());
        for &x in order {
            buf.extend_from_slice(&x.to_le_bytes());
        }

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Write to a per-process temp file then rename, so a concurrent launch
        // never observes a half-written cache.
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, &buf).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// A memory-mapped `.syn` file, searched on demand instead of parsed up front.
///
/// Layout (StarDict): a sequence of records, each `word\0` followed by a 4-byte
/// big-endian index into the `.idx` order. Records are sorted by their
/// lowercased word — the monolingual Wiktionary dumps this app consumes sort by
/// full Unicode lowercase (the same fold the original `HashMap`-based lookup
/// applied), which is what makes the binary search valid. The 4-byte index can
/// itself contain a `0` byte, so record boundaries can't be recovered by
/// scanning for `\0` from an arbitrary position — hence the
/// [`offsets`](Self::offsets) table, built by one forward scan from the start
/// (where boundaries are unambiguous) and cached. The offsets table is built
/// lazily on the first synonym lookup, so opening the dictionary does no `.syn`
/// work at all.
struct Syn {
    mmap: Mmap,
    offsets: OnceLock<Vec<usize>>,
}

impl Syn {
    fn open(path: &Path) -> Result<Syn, Error> {
        let file = File::open(path)?;
        // SAFETY: the `.syn` file is a read-only data file we never write to; a
        // concurrent external truncation could fault, but that is true of every
        // dictionary file the app maps/reads and is outside the threat model.
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(Syn {
            mmap,
            offsets: OnceLock::new(),
        })
    }

    /// Byte offsets of each record's start, built lazily on first use.
    fn offsets(&self) -> &[usize] {
        self.offsets.get_or_init(|| {
            let data = &self.mmap[..];
            let n = data.len();
            let mut offs = Vec::new();
            let mut i = 0usize;
            while i < n {
                offs.push(i);
                let Some(rel) = data[i..].iter().position(|&b| b == 0) else {
                    break;
                };
                // word\0 + 4-byte index.
                i += rel + 1 + 4;
            }
            offs
        })
    }

    /// The lowercased word of record `k`, the key the file is sorted on.
    fn key_at(&self, k: usize) -> String {
        let data = &self.mmap[..];
        let start = self.offsets()[k];
        let rel = data[start..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(data.len() - start);
        decode(&data[start..start + rel]).to_lowercase()
    }

    /// The `.idx` index stored in record `k` (the 4 bytes after its `\0`).
    fn value_at(&self, k: usize) -> u32 {
        let data = &self.mmap[..];
        let start = self.offsets()[k];
        let rel = data[start..].iter().position(|&b| b == 0).unwrap_or(0);
        let p = start + rel + 1;
        u32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]])
    }

    /// Every `.idx` index whose synonym matches `word` case-insensitively, found
    /// by binary search over the lowercase-sorted records.
    fn resolve(&self, word: &str) -> Vec<u32> {
        let lower = word.to_lowercase();
        let count = self.offsets().len();
        // Lower bound: the first record whose key is not less than `lower`.
        // Equal keys are contiguous (the file is sorted by this key), so they
        // form one run starting here.
        let mut lo = 0usize;
        let mut hi = count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.key_at(mid) < lower {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut out = Vec::new();
        let mut k = lo;
        while k < count && self.key_at(k) == lower {
            out.push(self.value_at(k));
            k += 1;
        }
        out
    }
}
