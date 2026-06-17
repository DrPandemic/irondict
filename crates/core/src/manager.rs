use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use crate::config::{Config, DictionaryConfig, Language, Preferences};
use crate::model::{Dictionary, Entry};
use crate::Error;

/// Basename of the bundled GCIDE StarDict header (Phase 2 asset).
const GCIDE_IFO: &str = "dictd_www.dict.org_gcide.ifo";

/// Path to the bundled GCIDE `.ifo`, resolved at runtime so a packaged binary
/// finds the data wherever it was installed while an in-tree build still uses the
/// crate's `assets/`. Search order (first existing wins):
///
/// 1. `$IRONDICT_GCIDE_DIR` — explicit override.
/// 2. `<exe-dir>/../share/irondict/gcide` — relative to the installed binary, so
///    any install `--prefix` works (`/usr/bin` → `/usr/share/irondict/gcide`).
/// 3. system data dirs from `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`),
///    each `<dir>/irondict/gcide`.
/// 4. the compile-time source asset (`CARGO_MANIFEST_DIR/assets/gcide`) — dev fallback.
///
/// If none exist (e.g. an install that didn't ship the data), returns the
/// dev-asset path so callers report a sensible location; loading then fails
/// gracefully as a warning rather than a crash.
pub fn bundled_gcide_path() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(dir) = std::env::var_os("IRONDICT_GCIDE_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            candidates.push(bin_dir.join("../share/irondict/gcide"));
        }
    }
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for dir in data_dirs.split(':').filter(|s| !s.is_empty()) {
        candidates.push(Path::new(dir).join("irondict/gcide"));
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/gcide");
    candidates.push(dev.clone());

    candidates
        .into_iter()
        .map(|dir| dir.join(GCIDE_IFO))
        .find(|ifo| ifo.is_file())
        .unwrap_or_else(|| dev.join(GCIDE_IFO))
}

/// Config entry for the bundled GCIDE, used to seed a fresh config on first run.
/// Pinned to [`Language::English`] (rather than `Auto`) so the launcher
/// integration exposes an English handler out of the box, instead of waiting for
/// the user to pin the language in settings.
pub fn bundled_gcide_config() -> DictionaryConfig {
    DictionaryConfig {
        language: Language::English,
        ..DictionaryConfig::new(bundled_gcide_path())
    }
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

    /// Remove the dictionary loaded from `path`. Returns whether anything was
    /// removed. Preferred over [`remove`](Self::remove) for downloaded
    /// dictionaries, since it targets the exact file rather than a (possibly
    /// shared) display name.
    pub fn remove_path(&mut self, path: &Path) -> bool {
        let before = self.dicts.len();
        self.dicts.retain(|d| d.path != path);
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

    /// Look up `word` in a single enabled dictionary (matched by name), returning
    /// its entries if present. Used to fetch a result-list preview snippet on
    /// demand — the search index no longer stores definitions, so the front-end
    /// reads the snippet straight from the source dictionary for the hit.
    pub fn lookup_in(&mut self, dictionary: &str, word: &str) -> Result<Vec<Entry>, Error> {
        for d in self.dicts.iter_mut().filter(|d| d.enabled) {
            if d.dictionary.info.name == dictionary {
                return Ok(d.dictionary.lookup(word)?.unwrap_or_default());
            }
        }
        Ok(Vec::new())
    }

    /// Visit every entry of every enabled dictionary, calling `f` with the
    /// source dictionary's name and the entry. Used to populate the search
    /// index (Phase 5).
    /// Call `f` for every entry of every enabled dictionary. `f` returns
    /// [`ControlFlow::Break`] to stop early (e.g. a cancelled index build); the
    /// break halts iteration across dictionaries, not just within the current one.
    pub fn for_each_enabled_entry(
        &mut self,
        mut f: impl FnMut(&str, Entry) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        for d in self.dicts.iter_mut().filter(|d| d.enabled) {
            let name = d.dictionary.info.name.clone();
            let mut stopped = false;
            d.dictionary.for_each_entry(|entry| {
                let flow = f(&name, entry);
                stopped = flow.is_break();
                flow
            })?;
            if stopped {
                break;
            }
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
