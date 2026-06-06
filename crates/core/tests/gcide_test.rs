use std::path::Path;

// Verifies the bundled GCIDE StarDict (Phase 2) loads through the Phase 1
// loader and that a known headword resolves to a definition.

fn gcide_path() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/gcide/dictd_www.dict.org_gcide.ifo"
    ))
}

#[test]
fn load_gcide() {
    let dict = irondict_core::stardict::load(gcide_path()).unwrap();
    assert_eq!(dict.info.name, "dictd_www.dict.org_gcide");
    assert_eq!(dict.info.word_count, 174222);
}

#[test]
fn lookup_dictionary() {
    let mut dict = irondict_core::stardict::load(gcide_path()).unwrap();
    let entries = dict.lookup("dictionary").unwrap().unwrap();
    assert!(!entries.is_empty());
    let text: String = entries[0]
        .segments
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    assert!(
        text.to_lowercase().contains("words"),
        "expected the GCIDE definition of \"dictionary\" to mention words"
    );
}
