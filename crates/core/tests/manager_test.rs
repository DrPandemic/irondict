mod common;

use std::path::{Path, PathBuf};

use common::TempDir;
use irondict_core::{bundled_gcide_config, Config, DictionaryConfig, DictionaryManager, Language};

/// Write a one-entry StarDict (`sametypesequence=m`) into `dir` and return the
/// `.ifo` path. Mirrors the helper in `search_test.rs`.
fn build_stardict(dir: &Path, name: &str, word: &str, def: &str) -> PathBuf {
    let mut idx = Vec::new();
    idx.extend_from_slice(word.as_bytes());
    idx.push(0);
    idx.extend_from_slice(&0u32.to_be_bytes());
    idx.extend_from_slice(&(def.len() as u32).to_be_bytes());
    let ifo = format!(
        "version=3.0.0\nbookname={name}\nwordcount=1\nidxfilesize={}\nsametypesequence=m\n",
        idx.len()
    );
    std::fs::write(dir.join(format!("{name}.idx")), &idx).unwrap();
    std::fs::write(dir.join(format!("{name}.dict")), def.as_bytes()).unwrap();
    let ifo_path = dir.join(format!("{name}.ifo"));
    std::fs::write(&ifo_path, ifo).unwrap();
    ifo_path
}

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
fn bundled_gcide_seeds_as_english() {
    // The first-run seed pins English (not Auto) so the launcher integration
    // exposes an English handler without the user touching settings.
    let config = bundled_gcide_config();
    assert_eq!(config.language, Language::English);
    assert!(config.enabled);
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

#[test]
fn companion_text_sources_the_installed_companion() {
    // The companion is identified by its install-dir id segment, so the
    // dictionary must live under a `fr-conj/` directory.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("fr-conj");
    std::fs::create_dir_all(&dir).unwrap();
    let ifo = build_stardict(&dir, "Conjugaison", "parler", "je parle, tu parles");

    let mut mgr = DictionaryManager::new();
    mgr.add(&ifo).unwrap();
    mgr.set_language("Conjugaison", Language::French);

    // Installed + enabled + matching language: returns the entry text.
    assert_eq!(
        mgr.companion_text("parler", Language::French).as_deref(),
        Some("je parle, tu parles")
    );
    // No companion for a language without one.
    assert_eq!(mgr.companion_text("parler", Language::English), None);
    // No entry for an unknown headword.
    assert_eq!(mgr.companion_text("manger", Language::French), None);
}

#[test]
fn companion_text_is_none_when_no_companion_installed() {
    // A plain (non-companion) dictionary path never matches.
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    mgr.set_language("mini", Language::French);
    assert_eq!(mgr.companion_text("hello", Language::French), None);
}
