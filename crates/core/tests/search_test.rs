mod common;

use std::path::{Path, PathBuf};

use common::TempDir;
use irondict_core::{DictionaryManager, SearchEngine, SearchMode};

fn mini_path() -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mini/mini.ifo"
    ))
    .to_path_buf()
}

fn engine_over_mini() -> (TempDir, SearchEngine) {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    let dir = TempDir::new().unwrap();
    let engine = SearchEngine::build(dir.path(), &mut mgr).unwrap();
    (dir, engine)
}

#[test]
fn exact_match_is_case_insensitive() {
    let (_dir, engine) = engine_over_mini();
    let hits = engine.search("HELLO", SearchMode::Exact, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].headword, "hello");
    assert_eq!(hits[0].dictionary, "mini");
}

#[test]
fn exact_match_does_not_match_prefix() {
    let (_dir, engine) = engine_over_mini();
    // "hell" is only a prefix of "hello", not an exact headword.
    let hits = engine.search("hell", SearchMode::Exact, 10).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn prefix_match_finds_headword() {
    let (_dir, engine) = engine_over_mini();
    let hits = engine.search("hel", SearchMode::Prefix, 10).unwrap();
    let words: Vec<&str> = hits.iter().map(|h| h.headword.as_str()).collect();
    assert!(words.contains(&"hello"));
}

#[test]
fn fuzzy_match_tolerates_a_typo() {
    let (_dir, engine) = engine_over_mini();
    // "helo" is one edit away from "hello".
    let hits = engine.search("helo", SearchMode::Fuzzy, 10).unwrap();
    let words: Vec<&str> = hits.iter().map(|h| h.headword.as_str()).collect();
    assert!(words.contains(&"hello"));
}

#[test]
fn fuzzy_exact_match_ranks_first_with_top_score() {
    let (_dir, engine) = engine_over_mini();
    // An exact match must rank first and read as a perfect (distance-0) score,
    // even though tantivy's fuzzy query scores every candidate identically.
    let hits = engine.search("hello", SearchMode::Fuzzy, 10).unwrap();
    assert_eq!(hits[0].headword, "hello");
    assert!((hits[0].score - 1.0).abs() < 1e-6);
}

#[test]
fn fuzzy_short_query_still_finds_close_completion() {
    let (_dir, engine) = engine_over_mini();
    // A short query like "ca" still reaches "cat" (within distance 2); the only
    // hard guard is on single characters.
    let hits = engine.search("ca", SearchMode::Fuzzy, 10).unwrap();
    let words: Vec<&str> = hits.iter().map(|h| h.headword.as_str()).collect();
    assert!(words.contains(&"cat"));
}

#[test]
fn fuzzy_single_char_is_exact_only() {
    let (_dir, engine) = engine_over_mini();
    // A lone character is too ambiguous to fuzz, so it only matches exactly
    // (nothing in the fixture is a single character).
    let hits = engine.search("c", SearchMode::Fuzzy, 10).unwrap();
    assert!(hits.iter().all(|h| h.headword != "cat"));
}

#[test]
fn fuzzy_prefix_guard_rejects_first_char_change() {
    let (_dir, engine) = engine_over_mini();
    // "jello" is one edit from "hello" but changes the first character, which the
    // prefix guard rejects.
    let hits = engine.search("jello", SearchMode::Fuzzy, 10).unwrap();
    assert!(hits.iter().all(|h| h.headword != "hello"));
}

#[test]
fn cancelled_build_yields_no_engine() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    let dir = TempDir::new().unwrap();
    // A cancel that fires immediately must abandon the build and commit nothing.
    let outcome =
        SearchEngine::build_cancellable(dir.path(), &mut mgr, || true, |_| {}).unwrap();
    assert!(outcome.is_none());
    // The abandoned index isn't usable; a fresh, uncancelled build still works.
    let engine = SearchEngine::build(dir.path(), &mut mgr).unwrap();
    let hits = engine.search("hello", SearchMode::Exact, 10).unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn full_text_matches_headwords_not_definitions() {
    let (_dir, engine) = engine_over_mini();
    // "furry" appears only in the definition of "cat" ("a furry animal"), not as
    // a headword — search matches headwords only, so it must not surface "cat".
    let hits = engine.search("furry", SearchMode::FullText, 10).unwrap();
    assert!(hits.is_empty());
    // The headword itself still matches, and the definition is still stored for
    // the result snippet even though it isn't searchable.
    let hits = engine.search("cat", SearchMode::FullText, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].headword, "cat");
    assert!(hits[0].snippet.contains("furry"));
}

#[test]
fn empty_query_returns_no_hits() {
    let (_dir, engine) = engine_over_mini();
    assert!(engine
        .search("   ", SearchMode::FullText, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn missing_query_returns_no_hits() {
    let (_dir, engine) = engine_over_mini();
    assert!(engine
        .search("nonexistentxyz", SearchMode::Exact, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn build_is_idempotent_and_reopenable() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    let dir = TempDir::new().unwrap();

    // Building twice into the same directory must not error (rebuild clears it).
    SearchEngine::build(dir.path(), &mut mgr).unwrap();
    SearchEngine::build(dir.path(), &mut mgr).unwrap();

    // The on-disk index can be reopened without rebuilding.
    let engine = SearchEngine::open(dir.path()).unwrap();
    let hits = engine.search("world", SearchMode::Exact, 10).unwrap();
    assert_eq!(hits[0].headword, "world");
}

#[test]
fn disabled_dictionaries_are_not_indexed() {
    let mut mgr = DictionaryManager::new();
    mgr.add(mini_path()).unwrap();
    mgr.set_enabled("mini", false);

    let dir = TempDir::new().unwrap();
    let engine = SearchEngine::build(dir.path(), &mut mgr).unwrap();
    assert!(engine
        .search("hello", SearchMode::Exact, 10)
        .unwrap()
        .is_empty());
}
