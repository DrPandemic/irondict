use std::path::Path;

use stardict::StarDict;

use crate::model::{DefinitionSegment, Dictionary, DictionaryInfo, Entry};

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
        Ok(results.map(|defs| {
            defs.into_iter()
                .map(|d| Entry {
                    headword: d.word,
                    segments: d
                        .segments
                        .into_iter()
                        .map(|s| DefinitionSegment {
                            type_: s.types,
                            text: s.text,
                        })
                        .collect(),
                })
                .collect()
        }))
    }
}
