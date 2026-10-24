//! Step 5 core tests: the new in-place `.dict`/`.dict.dz` reader must match the
//! `stardict` crate byte for byte, must work from a non-page-aligned range, and
//! must reject a malformed dictzip header without panicking (the last is a unit
//! test in `src/dict.rs`).

use std::fs::File;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use irondict_core::{register_bundled, BundledFiles, DictSource};

fn gcide(ext: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/gcide"))
        .join(format!("dictd_www.dict.org_gcide.{ext}"))
}

/// Compare the new reader against the `stardict` crate over every GCIDE entry.
/// The `stardict` index is keyed by lowercased word; the new reader's
/// `for_each_entry` walks its sorted `order`, so sorting the reference by the
/// same key lets the two be compared one-to-one without materializing both.
#[test]
fn new_dict_reader_matches_stardict_everywhere() {
    let ifo_path = gcide("ifo");
    let ifo = stardict::Ifo::new(ifo_path.clone()).unwrap();
    let old_idx = stardict::idx::Idx::new(gcide("idx"), &ifo, false, None).unwrap();
    let mut old_dict = stardict::dict::Dict::new(gcide("dict.dz"), true).unwrap();

    let mut new_dict = irondict_core::stardict::load(&ifo_path).unwrap();

    let mut reference: Vec<(&String, &stardict::idx::IdxEntry)> = old_idx.items.iter().collect();
    reference.sort_by(|a, b| a.0.cmp(b.0));
    assert!(
        reference.len() > 100_000,
        "expected the full GCIDE set, got {}",
        reference.len()
    );

    let mut i = 0usize;
    let mut mismatch: Option<String> = None;
    new_dict
        .for_each_entry(|entry| {
            let Some((lower, idx_entry)) = reference.get(i) else {
                mismatch = Some(format!("new reader yielded extra entry {}", entry.headword));
                return ControlFlow::Break(());
            };
            let old = old_dict.get_definition(idx_entry, &ifo).unwrap();
            let Some(old) = old else {
                mismatch = Some(format!("old reader had no definition for {lower}"));
                return ControlFlow::Break(());
            };
            if entry.headword != old.word {
                mismatch = Some(format!(
                    "headword mismatch for {lower}: new={} old={}",
                    entry.headword, old.word
                ));
                return ControlFlow::Break(());
            }
            if entry.segments.len() != old.segments.len() {
                mismatch = Some(format!(
                    "segment count mismatch for {lower}: new={} old={}",
                    entry.segments.len(),
                    old.segments.len()
                ));
                return ControlFlow::Break(());
            }
            for (ns, os) in entry.segments.iter().zip(old.segments.iter()) {
                if ns.type_ != os.types || ns.text != os.text {
                    mismatch = Some(format!("segment mismatch for {lower}"));
                    return ControlFlow::Break(());
                }
            }
            i += 1;
            ControlFlow::Continue(())
        })
        .unwrap();

    assert!(mismatch.is_none(), "{}", mismatch.unwrap());
    assert_eq!(
        i,
        reference.len(),
        "new reader yielded {} of {} entries",
        i,
        reference.len()
    );
}

/// Embed a real `.dict` at a non-page-aligned offset inside a larger file (the
/// shape of an APK's `base.apk`), register the dictionary as bundled, and look
/// it up.
#[test]
fn bundled_dictionary_at_non_page_aligned_range() {
    let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mini"));
    let ifo = std::fs::read(dir.join("mini.ifo")).unwrap();
    let idx = std::fs::read(dir.join("mini.idx")).unwrap();
    let dict = std::fs::read(dir.join("mini.dict")).unwrap();

    let offset = 4097u64;
    let mut container = vec![0u8; offset as usize];
    container.extend_from_slice(&dict);
    let tmp = std::env::temp_dir().join(format!(
        "irondict-embed-{}.bin",
        std::process::id()
    ));
    std::fs::write(&tmp, &container).unwrap();
    let file = Arc::new(File::open(&tmp).unwrap());

    // A virtual `.ifo` path that does not exist on disk.
    let virtual_path = PathBuf::from("bundled:tests:mini.ifo");
    register_bundled(
        virtual_path.clone(),
        BundledFiles {
            ifo: DictSource::from_bytes(ifo),
            idx: DictSource::from_bytes(idx),
            idx_gz: false,
            dict: DictSource::Range {
                file,
                offset,
                len: dict.len() as u64,
                name: "tests:mini:dict".to_string(),
            },
            dict_gz: false,
            syn: None,
        },
    );

    let mut d = irondict_core::stardict::load(&virtual_path).unwrap();
    assert_eq!(d.info.name, "mini");
    let entries = d.lookup("hello").unwrap().unwrap();
    assert_eq!(entries[0].headword, "hello");
    assert_eq!(entries[0].segments[0].text, "a greeting");
    assert_eq!(d.lookup("cat").unwrap().unwrap()[0].segments[0].text, "a furry animal");

    let _ = std::fs::remove_file(&tmp);
}
