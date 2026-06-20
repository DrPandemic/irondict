//! StarDict `.idx` / `.syn` handling, owned by us rather than delegated to the
//! `stardict` crate so that the synonym index doesn't have to be parsed into a
//! `HashMap` at boot.
//!
//! The `.idx` (headword → `.dict` blocks) is still parsed eagerly — it is needed
//! for every lookup and is comparatively cheap. The `.syn` file (variant /
//! inflected forms aliasing a headword) is the expensive part: it can hold
//! millions of entries and parsing it into a map dominates startup. But the
//! `.syn` file is already sorted by lowercased word on disk, so instead of
//! building a map we keep the file memory-mapped and **binary-search it on
//! demand**. The only preprocessing is a one-off scan that records each entry's
//! byte offset (a `Vec<usize>`, no string allocation), and even that is built
//! lazily on the first synonym lookup — so opening a dictionary does no `.syn`
//! work at all.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use memmap2::Mmap;
use stardict::idx::{IdxEntry, IdxEntryBlock};
use stardict::ifo::{Ifo, Version};

use crate::Error;

/// Decode a StarDict string field: UTF-8 lossy with the replacement character
/// dropped, matching the `stardict` crate so headwords are byte-identical to
/// what the rest of the pipeline expects.
fn decode(buf: &[u8]) -> String {
    String::from_utf8_lossy(buf)
        .chars()
        .filter(|&c| c != '\u{fffd}')
        .collect()
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

/// A parsed `.idx` plus an optional lazily-searched `.syn`.
pub struct Index {
    /// Every `.idx` record in file order, including empty-word records, so a
    /// `.syn` entry's stored index (which points into this original order) maps
    /// directly to `entries[index]`.
    entries: Vec<IdxEntry>,
    /// Lowercased headword → the `entries` positions that share it (StarDict can
    /// list the same word more than once with different blocks). Drives normal
    /// lookups; built once at parse time.
    by_word: HashMap<String, Vec<u32>>,
    /// The synonym file, kept mapped and searched on demand. `None` when the
    /// dictionary ships no `.syn`.
    syn: Option<Syn>,
}

impl Index {
    /// Parse `idx_path` (gz-decompressing it when `idx_gz`) and remember `syn`
    /// for lazy resolution. Only the `.idx` is read here; the `.syn` is mapped
    /// but not scanned until the first synonym lookup.
    pub fn new(
        idx_path: &Path,
        ifo: &Ifo,
        idx_gz: bool,
        syn: Option<&Path>,
    ) -> Result<Index, Error> {
        let bytes = std::fs::read(idx_path)?;
        let data = if idx_gz {
            let mut out = Vec::new();
            GzDecoder::new(bytes.as_slice()).read_to_end(&mut out)?;
            out
        } else {
            bytes
        };

        let w = idx_field_bytes(ifo);
        let mut entries: Vec<IdxEntry> = Vec::new();
        let mut by_word: HashMap<String, Vec<u32>> = HashMap::new();
        let mut i = 0usize;
        let n = data.len();
        while i < n {
            let Some(rel) = data[i..].iter().position(|&b| b == 0) else {
                break;
            };
            let zero = i + rel;
            let after = zero + 1;
            if after + 2 * w > n {
                break;
            }
            let word = decode(&data[i..zero]);
            let offset = read_be(&data[after..after + w]);
            let size = read_be(&data[after + w..after + 2 * w]);
            i = after + 2 * w;

            let pos = entries.len() as u32;
            if !word.is_empty() {
                by_word.entry(word.to_lowercase()).or_default().push(pos);
            }
            entries.push(IdxEntry {
                word,
                blocks: vec![IdxEntryBlock { offset, size }],
            });
        }

        let syn = match syn {
            Some(path) => Some(Syn::open(path)?),
            None => None,
        };
        Ok(Index {
            entries,
            by_word,
            syn,
        })
    }

    /// Merge the blocks of every `entries` position sharing a headword into a
    /// single [`IdxEntry`], taking the first record's original-case word.
    fn merge(&self, ids: &[u32]) -> IdxEntry {
        let word = self.entries[ids[0] as usize].word.clone();
        let mut blocks = Vec::new();
        for &id in ids {
            blocks.extend(self.entries[id as usize].blocks.iter().cloned());
        }
        IdxEntry { word, blocks }
    }

    /// The entries for `word`: its own definition (if any) followed by the
    /// definitions of any headwords it aliases through `.syn`. Returns `None`
    /// when nothing matches. Mirrors the `stardict` crate's `lookup_blocks`,
    /// resolving synonyms in the forward direction (variant/inflection → entry).
    pub fn lookup_blocks(&self, word: &str) -> Option<Vec<IdxEntry>> {
        let lower = word.to_lowercase();
        let mut out: Vec<IdxEntry> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        if let Some(ids) = self.by_word.get(&lower) {
            let entry = self.merge(ids);
            seen.insert(entry.word.clone());
            out.push(entry);
        }
        if let Some(syn) = &self.syn {
            for raw in syn.resolve(word) {
                let Some(raw_entry) = self.entries.get(raw as usize) else {
                    continue;
                };
                let canonical = raw_entry.word.to_lowercase();
                if let Some(ids) = self.by_word.get(&canonical) {
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

    /// The headword at position `n` modulo the dictionary size, in `by_word`'s
    /// iteration order. Returns `None` only for an empty dictionary.
    pub fn nth_word(&self, n: usize) -> Option<String> {
        let len = self.by_word.len();
        if len == 0 {
            return None;
        }
        self.by_word
            .values()
            .nth(n % len)
            .map(|ids| self.entries[ids[0] as usize].word.clone())
    }

    /// Visit every distinct headword as a merged [`IdxEntry`] (one per word, all
    /// its blocks). Used to feed the search index.
    pub fn for_each_word(&self, mut f: impl FnMut(IdxEntry) -> std::ops::ControlFlow<()>) {
        for ids in self.by_word.values() {
            if f(self.merge(ids)).is_break() {
                break;
            }
        }
    }
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("words", &self.by_word.len())
            .field("records", &self.entries.len())
            .field("has_syn", &self.syn.is_some())
            .finish()
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
