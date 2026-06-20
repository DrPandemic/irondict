use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use stardict::dict::Dict;
use stardict::Ifo;

use crate::idx::Index;
use crate::model::{DefinitionSegment, Dictionary, DictionaryInfo, Entry, SharedDict};

fn to_entry(word: stardict::WordDefinition) -> Entry {
    Entry {
        headword: word.word,
        segments: word
            .segments
            .into_iter()
            .map(|s| DefinitionSegment {
                type_: s.types,
                text: s.text,
            })
            .collect(),
    }
}

/// Locate a StarDict sub-file alongside `prefix` (the `.ifo` path with its
/// extension stripped), preferring the plain form and falling back to the
/// compressed one (`.idx` then `.idx.gz`, `.dict` then `.dict.dz`). Returns the
/// path and whether it is the compressed variant. This mirrors the resolution
/// the `stardict` crate does inside its own loader; we replicate it here so we
/// can build the index and the `.dict` reader separately — the index can then be
/// shared across handles (see [`SharedDict`]) instead of reparsed per handle.
fn sub_file(prefix: &str, name: &str, compressed: &str) -> Result<(PathBuf, bool), crate::Error> {
    let plain = PathBuf::from(format!("{prefix}.{name}"));
    if plain.is_file() {
        return Ok((plain, false));
    }
    let compressed = PathBuf::from(format!("{prefix}.{name}.{compressed}"));
    if compressed.is_file() {
        return Ok((compressed, true));
    }
    Err(crate::Error::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("missing .{name} file for {prefix}"),
    )))
}

pub fn load(path: &Path) -> Result<Dictionary, crate::Error> {
    let prefix = path
        .to_str()
        .and_then(|s| s.strip_suffix(".ifo"))
        .ok_or_else(|| {
            crate::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "dictionary path must be a UTF-8 .ifo file",
            ))
        })?;

    let (idx_path, idx_gz) = sub_file(prefix, "idx", "gz")?;
    let (dict_path, dict_bz) = sub_file(prefix, "dict", "dz")?;
    let syn_path = PathBuf::from(format!("{prefix}.syn"));
    let syn = syn_path.is_file().then_some(syn_path);

    let ifo = Ifo::new(path.to_path_buf()).map_err(|e| crate::Error::Stardict(e.into()))?;
    // Parse the `.idx` (headword → blocks) eagerly; this is needed for every
    // lookup and is cheap. The `.syn` is only memory-mapped here, not parsed —
    // it is binary-searched on demand (see [`crate::idx`]), so a large synonym
    // file no longer costs ~1s of startup. Shared via the returned `Arc`.
    let idx = Index::new(&idx_path, &ifo, idx_gz, syn.as_deref())?;
    let dict =
        Dict::new(dict_path.clone(), dict_bz).map_err(|e| crate::Error::Stardict(e.into()))?;

    let info = DictionaryInfo {
        name: ifo.bookname.clone(),
        word_count: ifo.wordcount,
    };
    Ok(Dictionary {
        info,
        shared: Arc::new(SharedDict {
            ifo,
            idx,
            dict_path,
            dict_bz,
        }),
        dict,
    })
}

impl Dictionary {
    pub fn lookup(&mut self, word: &str) -> Result<Option<Vec<Entry>>, crate::Error> {
        let Some(blocks) = self.shared.idx.lookup_blocks(word) else {
            return Ok(None);
        };
        // Disjoint borrows: the index/`.ifo` are read from the shared (immutable)
        // part while the `.dict` reader is seeked mutably.
        let ifo = &self.shared.ifo;
        let dict = &mut self.dict;
        let mut defs = Vec::with_capacity(blocks.len());
        for block in &blocks {
            if let Some(word) = dict
                .get_definition(block, ifo)
                .map_err(|e| crate::Error::Stardict(e.into()))?
            {
                defs.push(to_entry(word));
            }
        }
        Ok(Some(defs))
    }

    /// The headword at position `n` (modulo the dictionary size) in the index's
    /// internal order. Used to pick a "word of the moment" without materializing
    /// every entry. Returns `None` only for an empty dictionary.
    pub fn nth_headword(&self, n: usize) -> Option<String> {
        self.shared.idx.nth_word(n)
    }

    /// Iterate every entry in the dictionary, calling `f` with each headword and
    /// its full definition. Used to feed the search index (Phase 5).
    ///
    /// Entries are visited in the index's hash order, not sorted. Definitions
    /// that fail to materialize are skipped rather than aborting the whole walk.
    /// Call `f` for every entry. `f` returns [`ControlFlow::Break`] to stop early
    /// (e.g. a cancelled index build) so we don't pay to decode the remaining
    /// entries — decoding each one decompresses its block, which is the bulk of
    /// the cost over a large dictionary.
    pub fn for_each_entry(
        &mut self,
        mut f: impl FnMut(Entry) -> ControlFlow<()>,
    ) -> Result<(), crate::Error> {
        // Borrow the (immutable) shared index/`.ifo` and the (mutable) `.dict`
        // reader as disjoint fields so we can read every block while filling
        // definitions.
        let ifo = &self.shared.ifo;
        let idx = &self.shared.idx;
        let dict = &mut self.dict;
        let mut error = None;
        idx.for_each_word(|idx_entry| {
            match dict.get_definition(&idx_entry, ifo) {
                Ok(Some(word)) => {
                    if f(to_entry(word)).is_break() {
                        return ControlFlow::Break(());
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    error = Some(crate::Error::Stardict(e.into()));
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        });
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
