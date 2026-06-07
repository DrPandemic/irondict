use std::path::{Path, PathBuf};

use irondict_core::{Config, DictionaryConfig, DictionaryManager};

fn mini_path() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mini/mini.ifo"
    ))
    .to_path_buf()
}

fn gcide_path() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/gcide/dictd_www.dict.org_gcide.ifo"
    ))
    .to_path_buf()
}

#[test]
fn add_and_lookup_single_dictionary() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();

    let results = mgr.lookup("hello").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].dictionary, "mini");
    assert_eq!(results[0].entries[0].headword, "hello");
}

#[test]
fn add_is_idempotent_by_path() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    mgr.add(mini_path()).unwrap();
    assert_eq!(mgr.dictionaries().len(), 1);
}

#[test]
fn lookup_aggregates_across_dictionaries() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    mgr.add(gcide_path()).unwrap();

    // "dictionary" only exists in GCIDE.
    let results = mgr.lookup("dictionary").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].dictionary, "dictd_www.dict.org_gcide");

    // "hello" exists in both fixtures (GCIDE has a "hello" headword too).
    let results = mgr.lookup("hello").unwrap();
    let names: Vec<&str> = results.iter().map(|r| r.dictionary.as_str()).collect();
    assert!(names.contains(&"mini"));
}

#[test]
fn disabled_dictionary_is_excluded_from_lookup() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();

    assert!(mgr.set_enabled("mini", false));
    let results = mgr.lookup("hello").unwrap();
    assert!(results.is_empty());

    assert!(mgr.set_enabled("mini", true));
    let results = mgr.lookup("hello").unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn set_enabled_unknown_dictionary_returns_false() {
    let mut mgr = DictionaryManager::new();
    assert!(!mgr.set_enabled("nope", false));
}

#[test]
fn remove_dictionary() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    assert!(mgr.remove("mini"));
    assert!(mgr.dictionaries().is_empty());
    assert!(!mgr.remove("mini"));
}

#[test]
fn from_config_loads_listed_dictionaries() {
    let config = Config {
        dictionaries: vec![DictionaryConfig::new(mini_path())],
        ..Default::default()
    };
    let (mut mgr, errors) = DictionaryManager::from_config(&config);
    assert!(errors.is_empty());
    assert_eq!(mgr.dictionaries().len(), 1);

    let results = mgr.lookup("world").unwrap();
    assert_eq!(results[0].entries[0].headword, "world");
}

#[test]
fn from_config_collects_load_errors_for_missing_files() {
    let config = Config {
        dictionaries: vec![
            DictionaryConfig::new(mini_path()),
            DictionaryConfig::new(PathBuf::from("/no/such/dictionary.ifo")),
        ],
        ..Default::default()
    };
    let (mgr, errors) = DictionaryManager::from_config(&config);
    assert_eq!(mgr.dictionaries().len(), 1);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, PathBuf::from("/no/such/dictionary.ifo"));
}

#[test]
fn config_round_trips_through_manager() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    mgr.set_enabled("mini", false);

    let config = mgr.config();
    assert_eq!(config.dictionaries.len(), 1);
    assert_eq!(config.dictionaries[0].path, mini_path());
    assert!(!config.dictionaries[0].enabled);

    let (mgr2, errors) = DictionaryManager::from_config(&config);
    assert!(errors.is_empty());
    assert_eq!(mgr2.dictionaries().len(), 1);
    assert!(!mgr2.dictionaries()[0].enabled);
}

#[test]
fn add_bundled_gcide() {
    let mut mgr = DictionaryManager::new();
    mgr.add_bundled_gcide().unwrap();
    assert_eq!(mgr.dictionaries().len(), 1);
    let results = mgr.lookup("dictionary").unwrap();
    assert_eq!(results[0].dictionary, "dictd_www.dict.org_gcide");
}
