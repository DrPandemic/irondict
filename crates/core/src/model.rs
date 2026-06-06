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

pub struct Dictionary {
    pub info: DictionaryInfo,
    pub(crate) inner: stardict::StarDictStd,
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}
