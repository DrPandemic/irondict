use std::path::Path;

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
