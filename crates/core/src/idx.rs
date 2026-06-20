//! StarDict `.idx` / `.syn` handling, owned by us rather than delegated to the
//! `stardict` crate so that the synonym index doesn't have to be parsed into a
//! `HashMap` at boot.
//!
//! The `.idx` (headword → `.dict` blocks) is parsed eagerly — it is needed for
//! every lookup. Parsing it is allocation-bound (one record per headword, ~1.7M
//! across the installed set), so it is kept cheap three ways: the headwords are
//! stored **columnar** (one shared text buffer plus parallel `Vec`s of offsets,
//! rather than an `IdxEntry`/`Vec` per record), the maps are sized up front from
//! the `.ifo` counts to avoid rehashing, and the parse is **fanned out across
//! cores** with rayon (a cheap sequential boundary scan, then a parallel
//! decode/index per shard, merged in order).
//!
//! The `.syn` file (variant / inflected forms aliasing a headword) is the
//! expensive part to parse into a map: it can hold millions of entries. But it
//! is already sorted by lowercased word on disk, so instead of building a map we
//! keep the file memory-mapped and **binary-search it on demand**. The only
//! preprocessing is a one-off scan that records each entry's byte offset (a
//! `Vec<usize>`, no string allocation), built lazily on the first synonym lookup
//! — so opening a dictionary does no `.syn` work at all.

use std::collections::{HashMap, HashSet};
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

/// Decode a StarDict string field. Valid UTF-8 (essentially every record) is
/// taken verbatim — the common path, avoiding a per-character pass; only invalid
/// bytes fall back to lossy decoding with the replacement character dropped, the
/// way the `stardict` crate decodes, so headwords stay byte-identical to what the
/// rest of the pipeline expects.
fn decode(buf: &[u8]) -> String {
    match std::str::from_utf8(buf) {
        Ok(s) => s.to_owned(),
        Err(_) => String::from_utf8_lossy(buf)
            .chars()
            .filter(|&c| c != '\u{fffd}')
            .collect(),
    }
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
///
/// Headwords are stored columnar in file order (including empty-word records, so
/// a `.syn` entry's stored index — which points into the original order — maps
/// directly to record `i`). `words` is every headword concatenated; record `i`'s
/// headword is `words[word_starts[i]..word_starts[i + 1]]`. `offsets`/`sizes` are
/// its `.dict` block. This avoids the per-record `String`/`Vec` allocations a
/// naive parse pays across ~1.7M records.
pub struct Index {
    words: String,
    /// `n + 1` entries: the start of each record's headword in `words`, plus a
    /// trailing sentinel equal to `words.len()`.
    word_starts: Vec<u32>,
    offsets: Vec<u64>,
    sizes: Vec<u32>,
    /// Lowercased headword → the record positions that share it (StarDict can
    /// list the same word more than once with different blocks), each list in
    /// ascending order. Drives normal lookups; built once at parse time.
    by_word: HashMap<String, Vec<u32>>,
    /// The synonym file, kept mapped and searched on demand. `None` when the
    /// dictionary ships no `.syn`.
    syn: Option<Syn>,
}

/// One worker's slice of the parse, holding record `base..base + len` decoded
/// into local columnar buffers plus a local lowercased index using **global**
/// record positions. Merged into the final [`Index`] in shard order, which keeps
/// every `by_word` list ascending.
struct Shard {
    words: String,
    /// Local start of each record's headword in this shard's `words`.
    starts: Vec<u32>,
    offsets: Vec<u64>,
    sizes: Vec<u32>,
    by_word: HashMap<String, Vec<u32>>,
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
        let n = data.len();

        // Pass 1 (sequential, cheap): record boundaries as `(word_start, null)`
        // byte positions in `data`. The fixed-width offset/size field can itself
        // contain `0`, so we advance past it rather than scanning into it. Empty
        // headwords are kept so `.syn` indices stay valid.
        let mut recs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0usize;
        while i < n {
            let Some(rel) = data[i..].iter().position(|&b| b == 0) else {
                break;
            };
            let zero = i + rel;
            if zero + 1 + 2 * w > n {
                break;
            }
            recs.push((i, zero));
            i = zero + 1 + 2 * w;
        }

        // Pass 2 (parallel): decode each shard's headwords into local columnar
        // buffers and a local lowercased index keyed on global positions.
        let chunk = (recs.len() / (rayon::current_num_threads() * 4)).max(4096);
        let shards: Vec<Shard> = recs
            .par_chunks(chunk)
            .enumerate()
            .map(|(c, chunk_recs)| {
                let base = (c * chunk) as u32;
                let mut shard = Shard {
                    words: String::new(),
                    starts: Vec::with_capacity(chunk_recs.len()),
                    offsets: Vec::with_capacity(chunk_recs.len()),
                    sizes: Vec::with_capacity(chunk_recs.len()),
                    by_word: HashMap::new(),
                };
                for (local, &(ws, we)) in chunk_recs.iter().enumerate() {
                    let start = shard.words.len() as u32;
                    shard.starts.push(start);
                    match std::str::from_utf8(&data[ws..we]) {
                        Ok(s) => shard.words.push_str(s),
                        Err(_) => {
                            let s: String = String::from_utf8_lossy(&data[ws..we])
                                .chars()
                                .filter(|&c| c != '\u{fffd}')
                                .collect();
                            shard.words.push_str(&s);
                        }
                    }
                    let fp = we + 1;
                    shard.offsets.push(read_be(&data[fp..fp + w]) as u64);
                    shard.sizes.push(read_be(&data[fp + w..fp + 2 * w]) as u32);
                    if shard.words.len() as u32 > start {
                        let gi = base + local as u32;
                        let lower = shard.words[start as usize..].to_lowercase();
                        shard.by_word.entry(lower).or_default().push(gi);
                    }
                }
                shard
            })
            .collect();

        // Merge shards in order. Concatenating in shard order keeps each
        // `by_word` list ascending (shard positions are global and a shard's own
        // pushes are in record order). Maps are sized from the `.ifo` counts so
        // they don't rehash while filling.
        let total_words: usize = shards.iter().map(|s| s.words.len()).sum();
        let mut words = String::with_capacity(total_words);
        let mut word_starts: Vec<u32> = Vec::with_capacity(recs.len() + 1);
        let mut offsets: Vec<u64> = Vec::with_capacity(recs.len());
        let mut sizes: Vec<u32> = Vec::with_capacity(recs.len());
        let mut by_word: HashMap<String, Vec<u32>> = HashMap::with_capacity(ifo.wordcount);
        for shard in shards {
            let base = words.len() as u32;
            for &s in &shard.starts {
                word_starts.push(base + s);
            }
            words.push_str(&shard.words);
            offsets.extend(shard.offsets);
            sizes.extend(shard.sizes);
            for (k, v) in shard.by_word {
                by_word.entry(k).or_default().extend(v);
            }
        }
        word_starts.push(words.len() as u32);

        let syn = match syn {
            Some(path) => Some(Syn::open(path)?),
            None => None,
        };
        Ok(Index {
            words,
            word_starts,
            offsets,
            sizes,
            by_word,
            syn,
        })
    }

    /// Number of records (file order), including empty-word ones.
    fn len(&self) -> usize {
        self.word_starts.len().saturating_sub(1)
    }

    /// Record `i`'s original-case headword.
    fn word(&self, i: usize) -> &str {
        let start = self.word_starts[i] as usize;
        let end = self.word_starts[i + 1] as usize;
        &self.words[start..end]
    }

    /// Merge the blocks of every record position sharing a headword into a single
    /// [`IdxEntry`], taking the first record's original-case word. Allocated only
    /// at lookup/iteration time, not for every record at boot.
    fn merge(&self, ids: &[u32]) -> IdxEntry {
        let word = self.word(ids[0] as usize).to_string();
        let blocks = ids
            .iter()
            .map(|&id| IdxEntryBlock {
                offset: self.offsets[id as usize] as usize,
                size: self.sizes[id as usize] as usize,
            })
            .collect();
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
                let raw = raw as usize;
                if raw >= self.len() {
                    continue;
                }
                let canonical = self.word(raw).to_lowercase();
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
            .map(|ids| self.word(ids[0] as usize).to_string())
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
            .field("records", &self.len())
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
