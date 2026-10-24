use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use stardict::idx::IdxEntry;
use stardict::ifo::Ifo;

use crate::bundled;
use crate::dict::DictReader;
use crate::idx::{decode, Index};
use crate::model::{DefinitionSegment, Dictionary, DictionaryInfo, Entry, SharedDict};
use crate::source::DictSource;
use crate::Error;

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

/// Decode one `.dict` block exactly like the `stardict` crate's `parse_data`:
/// when the `.ifo` declares a `sametypesequence` the whole block is text of
/// those types; otherwise the first byte is the type and the rest is text. Kept
/// byte-identical to the previous implementation (same lossy UTF-8 decode).
fn parse_data(data: &[u8], types: &str) -> Option<(String, String)> {
    let (types, text) = if types.is_empty() {
        if data.len() < 2 {
            return None;
        }
        let mut types = String::new();
        types.push(data[0] as char);
        (types, decode(&data[1..]))
    } else {
        (types.to_owned(), decode(data))
    };
    Some((types, text))
}

/// Materialize one index entry's definition by reading each of its `.dict`
/// blocks. Mirrors `stardict::dict::Dict::get_definition`.
fn get_definition(
    dict: &mut DictReader,
    ifo: &Ifo,
    idx: &IdxEntry,
) -> Result<Option<stardict::WordDefinition>, Error> {
    let mut segments = Vec::new();
    for block in &idx.blocks {
        if let Some(data) = dict.block(block.offset, block.size)? {
            if let Some((types, text)) = parse_data(&data, &ifo.sametypesequence) {
                segments.push(stardict::WordDefinitionSegment { types, text });
            }
        }
    }
    if segments.is_empty() {
        Ok(None)
    } else {
        Ok(Some(stardict::WordDefinition {
            word: idx.word.clone(),
            segments,
        }))
    }
}

/// Locate a StarDict sub-file alongside `prefix` (the `.ifo` path with its
/// extension stripped), preferring the plain form and falling back to the
/// compressed one (`.idx` then `.idx.gz`, `.dict` then `.dict.dz`). Returns the
/// path and whether it is the compressed variant.
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

/// Load a dictionary from its four/five sources. Shared by the filesystem and
/// bundled paths.
fn load_sources(
    ifo_source: &DictSource,
    idx_source: DictSource,
    idx_gz: bool,
    dict_source: DictSource,
    dict_gz: bool,
    syn_source: Option<DictSource>,
) -> Result<Dictionary, crate::Error> {
    let ifo_bytes = ifo_source.open()?;
    let ifo = crate::ifo_bytes::parse_ifo(ifo_bytes.as_slice())?;
    // Parse the `.idx` (headword → blocks) eagerly; this is needed for every
    // lookup and is cheap. The `.syn` is only mapped, not parsed — it is
    // binary-searched on demand (see [`crate::idx`]). Shared via the returned
    // `Arc`.
    let idx = Index::from_source(&idx_source, &ifo, idx_gz, syn_source.as_ref())?;
    let dict_data = Arc::new(dict_source.open()?);
    let dict = DictReader::new_shared(Arc::clone(&dict_data), dict_gz)?;

    let info = DictionaryInfo {
        name: ifo.bookname.clone(),
        word_count: ifo.wordcount,
    };
    Ok(Dictionary {
        info,
        shared: Arc::new(SharedDict {
            ifo,
            idx,
            dict_data,
            dict_gz,
        }),
        dict,
    })
}

/// Load a dictionary. `path` is its `.ifo` file on the filesystem, or a virtual
/// `.ifo` path previously registered as a bundled dictionary
/// (see [`crate::bundled`]).
pub fn load(path: &Path) -> Result<Dictionary, crate::Error> {
    if let Some(files) = bundled::get(path) {
        return load_sources(
            &files.ifo,
            files.idx,
            files.idx_gz,
            files.dict,
            files.dict_gz,
            files.syn,
        );
    }

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
    let (dict_path, dict_gz) = sub_file(prefix, "dict", "dz")?;
    let syn_path = PathBuf::from(format!("{prefix}.syn"));
    let syn = syn_path.is_file().then_some(syn_path);

    load_sources(
        &DictSource::path(path),
        DictSource::path(idx_path),
        idx_gz,
        DictSource::path(dict_path),
        dict_gz,
        syn.map(DictSource::path),
    )
}

impl Dictionary {
    pub fn lookup(&mut self, word: &str) -> Result<Option<Vec<Entry>>, crate::Error> {
        let Some(blocks) = self.shared.idx.lookup_blocks(word) else {
            return Ok(None);
        };
        // Disjoint borrows: the index/`.ifo` are read from the shared (immutable)
        // part while the `.dict` reader is used mutably.
        let ifo = &self.shared.ifo;
        let dict = &mut self.dict;
        let mut defs = Vec::with_capacity(blocks.len());
        for block in &blocks {
            if let Some(word) = get_definition(dict, ifo, block)? {
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
        idx.for_each_word(|idx_entry| match get_definition(dict, ifo, &idx_entry) {
            Ok(Some(word)) => {
                if f(to_entry(word)).is_break() {
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            }
            Ok(None) => ControlFlow::Continue(()),
            Err(e) => {
                error = Some(e);
                ControlFlow::Break(())
            }
        });
        match error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
