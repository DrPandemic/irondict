use std::path::{Path, PathBuf};

mod common;
use common::TempDir;

/// Write a minimal `sametypesequence=m` StarDict dictionary plus a `.syn`
/// synonym file into `dir`, returning the `.ifo` path. `entries` are written in
/// order (so a `.syn` index refers to a position here); each `syns` pair maps a
/// synonym word to the entry index it aliases. Mirrors the on-disk layout the
/// loader expects: `.idx` is `word\0` + BE u32 offset + BE u32 size; `.syn` is
/// `word\0` + BE u32 entry-index.
fn build_dict_with_syn(
    dir: &Path,
    name: &str,
    entries: &[(&str, &str)],
    syns: &[(&str, u32)],
) -> PathBuf {
    let mut idx = Vec::new();
    let mut dict = Vec::new();
    for (word, def) in entries {
        let offset = dict.len() as u32;
        dict.extend_from_slice(def.as_bytes());
        idx.extend_from_slice(word.as_bytes());
        idx.push(0);
        idx.extend_from_slice(&offset.to_be_bytes());
        idx.extend_from_slice(&(def.len() as u32).to_be_bytes());
    }
    let mut syn = Vec::new();
    for (word, index) in syns {
        syn.extend_from_slice(word.as_bytes());
        syn.push(0);
        syn.extend_from_slice(&index.to_be_bytes());
    }
    std::fs::write(dir.join(format!("{name}.idx")), &idx).unwrap();
    std::fs::write(dir.join(format!("{name}.dict")), &dict).unwrap();
    std::fs::write(dir.join(format!("{name}.syn")), &syn).unwrap();
    let ifo = format!(
        "StarDict's dict ifo file\nversion=2.4.2\nbookname={name}\nwordcount={}\nsynwordcount={}\nidxfilesize={}\nsametypesequence=m\n",
        entries.len(),
        syns.len(),
        idx.len()
    );
    let ifo_path = dir.join(format!("{name}.ifo"));
    std::fs::write(&ifo_path, ifo).unwrap();
    ifo_path
}

#[test]
fn synonym_resolves_to_entry() {
    let dir = TempDir::new().unwrap();
    let entries = [("color", "a hue"), ("apple", "a fruit")];
    // "colour" and "colors" both alias entry 0 ("color"); the second sorts after
    // the first, exercising the lazily-built, binary-searched `.syn` path.
    let syns = [("colors", 0u32), ("colour", 0u32)];
    let path = build_dict_with_syn(dir.path(), "syndict", &entries, &syns);

    let mut dict = irondict_core::stardict::load(&path).unwrap();

    // The variant spelling resolves to the real entry.
    let entries = dict.lookup("colour").unwrap().unwrap();
    assert_eq!(entries[0].headword, "color");
    assert_eq!(entries[0].segments[0].text, "a hue");

    // Lookup is case-insensitive on the synonym, like a direct headword.
    let entries = dict.lookup("COLORS").unwrap().unwrap();
    assert_eq!(entries[0].headword, "color");

    // A direct headword still works alongside synonyms.
    assert_eq!(dict.lookup("apple").unwrap().unwrap()[0].headword, "apple");

    // A word that is neither a headword nor a synonym misses.
    assert!(dict.lookup("banana").unwrap().is_none());
}

#[test]
fn load_mini_fixture() {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mini/mini.ifo"
    ));
    let dict = irondict_core::stardict::load(path).unwrap();
    assert_eq!(dict.info.name, "mini");
    assert_eq!(dict.info.word_count, 3);
}

#[test]
fn exact_lookup_hello() {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mini/mini.ifo"
    ));
    let mut dict = irondict_core::stardict::load(path).unwrap();
    let entries = dict.lookup("hello").unwrap().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].headword, "hello");
    assert_eq!(entries[0].segments.len(), 1);
    assert_eq!(entries[0].segments[0].text, "a greeting");
}

#[test]
fn exact_lookup_world() {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mini/mini.ifo"
    ));
    let mut dict = irondict_core::stardict::load(path).unwrap();
    let entries = dict.lookup("world").unwrap().unwrap();
    assert_eq!(entries[0].headword, "world");
    assert_eq!(entries[0].segments[0].text, "the earth");
}

#[test]
fn exact_lookup_cat() {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mini/mini.ifo"
    ));
    let mut dict = irondict_core::stardict::load(path).unwrap();
    let entries = dict.lookup("cat").unwrap().unwrap();
    assert_eq!(entries[0].headword, "cat");
    assert_eq!(entries[0].segments[0].text, "a furry animal");
}

#[test]
fn lookup_missing_returns_none() {
    let path = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mini/mini.ifo"
    ));
    let mut dict = irondict_core::stardict::load(path).unwrap();
    let result = dict.lookup("nonexistent").unwrap();
    assert!(result.is_none());
}
