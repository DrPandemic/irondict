use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::Error;

/// One dictionary as recorded in the persisted config: where it lives on disk
/// and whether it participates in lookups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictionaryConfig {
    pub path: PathBuf,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// The persisted application config: the set of dictionaries the user has added
/// plus their enabled state. Stored as TOML in the OS app-data dir.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub dictionaries: Vec<DictionaryConfig>,
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
