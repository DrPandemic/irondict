pub mod config;
pub mod manager;
pub mod model;
pub mod stardict;

pub use config::{Config, DictionaryConfig};
pub use manager::{
    bundled_gcide_path, DictLoadError, DictionaryManager, LookupResult, ManagedDictionary,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("StarDict error: {0}")]
    Stardict(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("config serialize error: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),

    #[error("could not determine the application config directory")]
    NoConfigDir,
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
