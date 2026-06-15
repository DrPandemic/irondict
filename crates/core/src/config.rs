use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::Error;

/// The language of a dictionary's headwords, used to route verb conjugation to
/// the right backend (Phase 8). `Auto` defers to detection; the user can pin it
/// per dictionary from the settings page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Auto,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "fr")]
    French,
    #[serde(rename = "it")]
    Italian,
}

impl Language {
    /// The short code used in the config, on the command line, and by the
    /// launcher integration (`auto` when the language is unpinned).
    pub fn code(self) -> &'static str {
        match self {
            Language::Auto => "auto",
            Language::English => "en",
            Language::French => "fr",
            Language::Italian => "it",
        }
    }
}

/// Whether the UI follows the OS light/dark setting or is pinned by the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// User-facing application preferences (everything that isn't the dictionary
/// list). Stored alongside the dictionaries in the same config file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    pub theme_mode: ThemeMode,
    /// Accent color override as a `#rrggbb` string; `None` means "follow the OS".
    #[serde(default)]
    pub accent: Option<String>,
    /// The dictionary scope selected last, by dictionary name; `None` means the
    /// "All" scope. Restored on launch so the app reopens where you left off.
    #[serde(default)]
    pub last_scope: Option<String>,
}

/// One dictionary as recorded in the persisted config: where it lives on disk,
/// whether it participates in lookups, and its (optional) pinned language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryConfig {
    pub path: PathBuf,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub language: Language,
}

fn default_enabled() -> bool {
    true
}

impl DictionaryConfig {
    /// A config entry for a dictionary at `path`, enabled with language `Auto`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            enabled: true,
            language: Language::Auto,
        }
    }
}

/// The persisted application config: the set of dictionaries the user has added
/// (plus their enabled state and language) and the UI preferences. Stored as
/// TOML in the OS app-data dir.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub dictionaries: Vec<DictionaryConfig>,
    #[serde(default)]
    pub preferences: Preferences,
}

impl Config {
    /// Path to the config file in the OS app-data dir
    /// (e.g. `~/.config/irondict/config.toml` on Linux).
    pub fn default_path() -> Result<PathBuf, Error> {
        let dirs = ProjectDirs::from("", "", "irondict").ok_or(Error::NoConfigDir)?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Load the config from the default app-data path, returning an empty config
    /// if the file does not exist yet.
    pub fn load() -> Result<Config, Error> {
        Self::load_from(&Self::default_path()?)
    }

    /// Load the config from a specific path, returning an empty config if the
    /// file does not exist yet.
    pub fn load_from(path: &Path) -> Result<Config, Error> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(toml::from_str(&contents)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Save the config to the default app-data path, creating parent
    /// directories as needed.
    pub fn save(&self) -> Result<(), Error> {
        self.save_to(&Self::default_path()?)
    }

    /// Save the config to a specific path, creating parent directories as needed.
    pub fn save_to(&self, path: &Path) -> Result<(), Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}
