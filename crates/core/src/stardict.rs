use std::path::Path;

use stardict::StarDict;

use crate::model::{DefinitionSegment, Dictionary, DictionaryInfo, Entry};

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

pub fn load(path: &Path) -> Result<Dictionary, crate::Error> {
    let inner = stardict::no_cache(path).map_err(|e| crate::Error::Stardict(e.into()))?;
    let ifo = inner.ifo();
    let info = DictionaryInfo {
        name: ifo.bookname.clone(),
        word_count: ifo.wordcount,
    };
    Ok(Dictionary { info, inner })
}

impl Dictionary {
    pub fn lookup(&mut self, word: &str) -> Result<Option<Vec<Entry>>, crate::Error> {
        let results = self
            .inner
            .lookup(word)
            .map_err(|e| crate::Error::Stardict(e.into()))?;
        Ok(results.map(|defs| defs.into_iter().map(to_entry).collect()))
    }

    /// Iterate every entry in the dictionary, calling `f` with each headword and
    /// its full definition. Used to feed the search index (Phase 5).
    ///
    /// Entries are visited in the index's hash order, not sorted. Definitions
    /// that fail to materialize are skipped rather than aborting the whole walk.
    pub fn for_each_entry(&mut self, mut f: impl FnMut(Entry)) -> Result<(), crate::Error> {
        // Borrow the (immutable) index and (mutable) dict store as disjoint
        // fields so we can read every block while filling definitions.
        let ifo = &self.inner.ifo;
        let dict = &mut self.inner.dict;
        for idx_entry in self.inner.idx.items.values() {
            if let Some(word) = dict
                .get_definition(idx_entry, ifo)
                .map_err(|e| crate::Error::Stardict(e.into()))?
            {
                f(to_entry(word));
            }
        }
        Ok(())
    }
}
