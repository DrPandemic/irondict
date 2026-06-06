use std::path::PathBuf;

use irondict_core::{Config, DictionaryConfig};

mod common;
use common::TempDir;

#[test]
fn missing_file_loads_default() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("does-not-exist.toml");
    let config = Config::load_from(&path).unwrap();
    assert_eq!(config, Config::default());
    assert!(config.dictionaries.is_empty());
}

#[test]
fn save_then_load_round_trips() {
    let dir = TempDir::new().unwrap();
    // Nested path so save_to has to create parent directories.
    let path = dir.path().join("nested/config.toml");

    let config = Config {
        dictionaries: vec![
            DictionaryConfig {
                path: PathBuf::from("/dicts/gcide.ifo"),
                enabled: true,
            },
            DictionaryConfig {
                path: PathBuf::from("/dicts/user.ifo"),
                enabled: false,
            },
        ],
    };

    config.save_to(&path).unwrap();
    assert!(path.exists());

    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded, config);
}

#[test]
fn enabled_defaults_to_true_when_omitted() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[[dictionaries]]\npath = \"/dicts/gcide.ifo\"\n").unwrap();

    let loaded = Config::load_from(&path).unwrap();
    assert_eq!(loaded.dictionaries.len(), 1);
    assert!(loaded.dictionaries[0].enabled);
}
