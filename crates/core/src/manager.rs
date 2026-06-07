use std::path::{Path, PathBuf};

use crate::config::{Config, DictionaryConfig, Language, Preferences};
use crate::model::{Dictionary, Entry};
use crate::Error;

/// Path to the GCIDE StarDict files bundled with the core crate (Phase 2 asset).
///
/// Resolved relative to the crate source at compile time, which is fine for
/// development; real installs will resolve this from a packaged location in a
/// later phase.
pub fn bundled_gcide_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/gcide/dictd_www.dict.org_gcide.ifo")
}

/// A loaded dictionary together with the bookkeeping the manager needs:
/// where it came from and whether it is enabled for lookups.
#[derive(Debug)]
pub struct ManagedDictionary {
    pub path: PathBuf,
    pub enabled: bool,
    pub language: Language,
    pub dictionary: Dictionary,
}

impl ManagedDictionary {
    /// The dictionary's display name (its StarDict bookname).
    pub fn name(&self) -> &str {
        &self.dictionary.info.name
    }
}

/// A dictionary listed in the config that failed to load. Collected (rather than
/// aborting startup) so one broken or missing file doesn't take down the app.
#[derive(Debug)]
pub struct DictLoadError {
    pub path: PathBuf,
    pub error: Error,
}

/// Results of a lookup against a single dictionary, tagged with that
/// dictionary's name so front-ends can show the source per result.
#[derive(Debug, Clone)]
pub struct LookupResult {
    pub dictionary: String,
    pub entries: Vec<Entry>,
}

/// Owns multiple loaded dictionaries and aggregates lookups across the enabled
/// ones. Supports add/remove/enable and round-tripping to/from [`Config`].
#[derive(Debug, Default)]
pub struct DictionaryManager {
    dicts: Vec<ManagedDictionary>,
    preferences: Preferences,
}

impl DictionaryManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load every dictionary listed in `config`. Returns the manager plus any
    /// per-dictionary load errors so a single failure doesn't prevent startup.
    pub fn from_config(config: &Config) -> (Self, Vec<DictLoadError>) {
        let mut manager = Self::new();
        manager.preferences = config.preferences.clone();
        let mut errors = Vec::new();
        for dc in &config.dictionaries {
            match crate::stardict::load(&dc.path) {
                Ok(dictionary) => manager.dicts.push(ManagedDictionary {
                    path: dc.path.clone(),
                    enabled: dc.enabled,
                    language: dc.language,
                    dictionary,
                }),
                Err(error) => errors.push(DictLoadError {
                    path: dc.path.clone(),
                    error,
                }),
            }
        }
        (manager, errors)
    }

    /// All managed dictionaries, in insertion order.
    pub fn dictionaries(&self) -> &[ManagedDictionary] {
        &self.dicts
    }

    /// Whether a dictionary loaded from `path` is already managed.
    pub fn contains_path(&self, path: &Path) -> bool {
        self.dicts.iter().any(|d| d.path == path)
    }

    /// Load and add a dictionary from `path` (enabled by default). If a
    /// dictionary with the same path is already managed it is returned unchanged.
    pub fn add(&mut self, path: impl AsRef<Path>) -> Result<&ManagedDictionary, Error> {
        let path = path.as_ref();
        if let Some(idx) = self.dicts.iter().position(|d| d.path == path) {
            return Ok(&self.dicts[idx]);
        }
        let dictionary = crate::stardict::load(path)?;
        self.dicts.push(ManagedDictionary {
            path: path.to_path_buf(),
            enabled: true,
            language: Language::Auto,
            dictionary,
        });
        Ok(self.dicts.last().expect("just pushed"))
    }

    /// Load and add the bundled GCIDE dictionary as a preinstalled dictionary.
    pub fn add_bundled_gcide(&mut self) -> Result<&ManagedDictionary, Error> {
        self.add(bundled_gcide_path())
    }

    /// Remove every dictionary with the given name. Returns whether anything was
    /// removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.dicts.len();
        self.dicts.retain(|d| d.name() != name);
        self.dicts.len() != before
    }

    /// Enable or disable the dictionary with the given name. Returns whether a
    /// matching dictionary was found.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> bool {
        match self.dicts.iter_mut().find(|d| d.name() == name) {
            Some(d) => {
                d.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// Pin the language of the dictionary with the given name. Returns whether a
    /// matching dictionary was found.
    pub fn set_language(&mut self, name: &str, language: Language) -> bool {
        match self.dicts.iter_mut().find(|d| d.name() == name) {
            Some(d) => {
                d.language = language;
                true
            }
            None => false,
        }
    }

    /// The current UI preferences.
    pub fn preferences(&self) -> &Preferences {
        &self.preferences
    }

    /// Mutable access to the UI preferences (e.g. to change theme or accent).
    pub fn preferences_mut(&mut self) -> &mut Preferences {
        &mut self.preferences
    }

    /// Look up `word` across all enabled dictionaries, returning one
    /// [`LookupResult`] per dictionary that has a non-empty match.
    pub fn lookup(&mut self, word: &str) -> Result<Vec<LookupResult>, Error> {
        let mut results = Vec::new();
        for d in self.dicts.iter_mut().filter(|d| d.enabled) {
            if let Some(entries) = d.dictionary.lookup(word)? {
                if !entries.is_empty() {
                    results.push(LookupResult {
                        dictionary: d.dictionary.info.name.clone(),
                        entries,
                    });
                }
            }
        }
        Ok(results)
    }

    /// Visit every entry of every enabled dictionary, calling `f` with the
    /// source dictionary's name and the entry. Used to populate the search
    /// index (Phase 5).
    pub fn for_each_enabled_entry(&mut self, mut f: impl FnMut(&str, Entry)) -> Result<(), Error> {
        for d in self.dicts.iter_mut().filter(|d| d.enabled) {
            let name = d.dictionary.info.name.clone();
            d.dictionary.for_each_entry(|entry| f(&name, entry))?;
        }
        Ok(())
    }

    /// Snapshot the current dictionaries and preferences as a persistable
    /// [`Config`], so the whole app state round-trips through disk.
    pub fn config(&self) -> Config {
        Config {
            dictionaries: self
                .dicts
                .iter()
                .map(|d| DictionaryConfig {
                    path: d.path.clone(),
                    enabled: d.enabled,
                    language: d.language,
                })
                .collect(),
            preferences: self.preferences.clone(),
        }
    }
}
