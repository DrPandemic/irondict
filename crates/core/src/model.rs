use std::sync::Arc;

use stardict::Ifo;

use crate::dict::DictReader;
use crate::idx::Index;
use crate::source::Bytes;

#[derive(Debug, Clone)]
pub struct DefinitionSegment {
    pub type_: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub headword: String,
    pub segments: Vec<DefinitionSegment>,
}

#[derive(Debug, Clone)]
pub struct DictionaryInfo {
    pub name: String,
    pub word_count: usize,
}

/// The immutable, expensive-to-parse part of a loaded dictionary: the headword
/// index (with its synonym map) and the `.ifo` metadata, plus the shared `.dict`
/// mapping. Parsing these from the `.idx`/`.syn` files is by far the dominant
/// cost of opening a dictionary (multiple seconds across a large set), so it is
/// done once and shared (behind an `Arc`) by every handle to the dictionary —
/// letting the UI thread and the search worker each hold a dictionary without
/// reparsing. The `.dict` bytes are shared too, so a second handle reuses the
/// mapping rather than reopening the file.
pub(crate) struct SharedDict {
    pub(crate) ifo: Ifo,
    pub(crate) idx: Index,
    pub(crate) dict_data: Arc<Bytes>,
    pub(crate) dict_gz: bool,
}

pub struct Dictionary {
    pub info: DictionaryInfo,
    pub(crate) shared: Arc<SharedDict>,
    /// The `.dict` reader for this handle. It tracks a per-handle chunk cache,
    /// so it needs `&mut`; each handle therefore owns its own (the mapping above
    /// and the parsed index are the parts that are shared and read-only).
    pub(crate) dict: DictReader,
}

impl Dictionary {
    /// Open a second handle to the same dictionary, sharing the already-parsed
    /// index and `.ifo` and opening a fresh `.dict` reader. This is cheap (an
    /// `Arc` clone plus opening one file) and, crucially, skips the multi-second
    /// `.idx`/`.syn` parse — so handing the search worker its own copy at launch
    /// no longer reparses every dictionary.
    pub fn reopen(&self) -> Result<Dictionary, crate::Error> {
        let dict = DictReader::new_shared(Arc::clone(&self.shared.dict_data), self.shared.dict_gz)?;
        Ok(Dictionary {
            info: self.info.clone(),
            shared: Arc::clone(&self.shared),
            dict,
        })
    }
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}
