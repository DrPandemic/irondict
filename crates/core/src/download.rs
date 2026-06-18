//! Downloading and installing dictionaries from a catalog of releases.
//!
//! The catalog is currently built in: the monolingual (same-language)
//! Wiktionary StarDict editions published by the `xxyzz/wiktionary_stardict`
//! project. Each entry points at that project's `releases/latest` asset, so a
//! download always fetches the newest snapshot. Installed dictionaries live
//! under the OS data dir and are handed to the [`DictionaryManager`] by `.ifo`
//! path, exactly like a manually added dictionary.
//!
//! The content is Wiktionary-derived and licensed CC BY-SA; attribution and
//! license are carried on each [`CatalogEntry`] so front-ends can surface them.
//!
//! [`DictionaryManager`]: crate::DictionaryManager

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::config::Language;
use crate::Error;

/// One dictionary offered for in-app download.
#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    /// Stable id, also the on-disk install directory name (e.g. `"en-en"`).
    pub id: &'static str,
    /// Human-readable label shown before the dictionary is installed (the real
    /// name comes from the `.ifo` bookname once loaded).
    pub label: &'static str,
    /// Language to pin on the dictionary so verb conjugation routes correctly.
    pub language: Language,
    /// URL of the `.tar.zst` release asset.
    pub url: &'static str,
    /// Approximate compressed download size in bytes, for display.
    pub approx_size: u64,
    /// Content license (all entries are Wiktionary-derived).
    pub license: &'static str,
    /// Upstream source, surfaced for attribution.
    pub source: &'static str,
}

const SOURCE: &str = "Wiktionary via xxyzz/wiktionary_stardict";
const LICENSE: &str = "CC BY-SA 4.0";

macro_rules! entry {
    ($id:literal, $label:literal, $lang:expr, $size:literal) => {
        CatalogEntry {
            id: $id,
            label: $label,
            language: $lang,
            url: concat!(
                "https://github.com/xxyzz/wiktionary_stardict/releases/latest/download/",
                $id,
                ".tar.zst"
            ),
            approx_size: $size,
            license: LICENSE,
            source: SOURCE,
        }
    };
}

/// The built-in catalog of downloadable dictionaries: the monolingual
/// Wiktionary editions published by xxyzz, plus the French conjugation
/// companion. Sizes are the compressed download as of the 2026-06-08 snapshot
/// and are approximate.
pub fn catalog() -> &'static [CatalogEntry] {
    use Language::{Auto, English, French};
    &[
        entry!("en-en", "Wiktionary — English", English, 98_000_000),
        entry!("fr-fr", "Wiktionnaire — Français", French, 106_000_000),
        entry!("de-de", "Wiktionary — Deutsch", Auto, 22_000_000),
        entry!("es-es", "Wikcionario — Español", Auto, 18_000_000),
        entry!("ru-ru", "Викисловарь — Русский", Auto, 116_000_000),
        entry!("fi-fi", "Wikisanakirja — Suomi", Auto, 14_000_000),
        entry!("sv-sv", "Wiktionary — Svenska", Auto, 10_000_000),
        CATALOG_IT_IT,
        CATALOG_FR_CONJ,
    ]
}

const CATALOG_IT_IT: CatalogEntry = CatalogEntry {
    id: "it-it",
    label: "Wiktionary — Italiano",
    language: Language::Italian,
    url: "https://github.com/DrPandemic/wikitionary-dictionaries/releases/latest/download/it-it-dictzip.tar.zst",
    approx_size: 5_100_000,
    license: LICENSE,
    source: "Wiktionary via kaikki.org / wiktextract",
};

const CATALOG_FR_CONJ: CatalogEntry = CatalogEntry {
    id: "fr-conj",
    label: "Conjugaison — Français",
    language: Language::French,
    url: "https://github.com/DrPandemic/wikitionary-dictionaries/releases/latest/download/fr-conj-dictzip.tar.zst",
    approx_size: 12_000_000,
    license: LICENSE,
    source: "Wiktionary via kaikki.org / wiktextract",
};

/// The catalog entry with the given id, if any.
pub fn find(id: &str) -> Option<&'static CatalogEntry> {
    catalog().iter().find(|e| e.id == id)
}

fn project_dirs() -> Result<ProjectDirs, Error> {
    ProjectDirs::from("", "", "irondict").ok_or(Error::NoConfigDir)
}

/// Directory under the OS data dir where downloaded dictionaries are installed
/// (e.g. `~/.local/share/irondict/dictionaries` on Linux).
pub fn dictionaries_dir() -> Result<PathBuf, Error> {
    Ok(project_dirs()?.data_dir().join("dictionaries"))
}

/// The install directory for a given catalog id.
pub fn install_dir(id: &str) -> Result<PathBuf, Error> {
    Ok(dictionaries_dir()?.join(id))
}

/// The installed `.ifo` path for `id`, if the dictionary is present on disk.
pub fn installed_ifo(id: &str) -> Option<PathBuf> {
    find_ifo(&install_dir(id).ok()?)
}

/// Whether the dictionary with the given catalog id is installed.
pub fn is_installed(id: &str) -> bool {
    installed_ifo(id).is_some()
}

/// Delete an installed dictionary's files from disk. A no-op (returns `Ok`) if
/// the dictionary isn't installed. Callers should unregister it from the
/// [`DictionaryManager`](crate::DictionaryManager) first.
pub fn uninstall(id: &str) -> Result<(), Error> {
    let dir = install_dir(id)?;
    match fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Conjugation companions: each primary dictionary id paired with its hidden
/// conjugation-companion id and the language the two share. Drives companion
/// auto-install/uninstall (the companion follows its primary) and conjugation
/// text sourcing ([`DictionaryManager::companion_text`]).
///
/// A companion may be listed here before its catalog entry / upstream release
/// asset exists (English and Italian land their parsers first — see PLAN.md
/// Phases 1–2); call sites guard on [`find`] so a not-yet-published companion is
/// a no-op rather than a failed download.
///
/// [`DictionaryManager::companion_text`]: crate::DictionaryManager::companion_text
const COMPANIONS: &[(&str, &str, Language)] = &[
    ("fr-fr", "fr-conj", Language::French),
    ("en-en", "en-conj", Language::English),
    ("it-it", "it-conj", Language::Italian),
];

/// The conjugation-companion id for a primary dictionary id, if any
/// (e.g. `"fr-fr"` → `"fr-conj"`).
pub fn companion_for(id: &str) -> Option<&'static str> {
    COMPANIONS
        .iter()
        .find(|(primary, _, _)| *primary == id)
        .map(|(_, companion, _)| *companion)
}

/// The primary dictionary id for a conjugation-companion id, if any
/// (e.g. `"fr-conj"` → `"fr-fr"`).
pub fn primary_for(id: &str) -> Option<&'static str> {
    COMPANIONS
        .iter()
        .find(|(_, companion, _)| *companion == id)
        .map(|(primary, _, _)| *primary)
}

/// The conjugation-companion id for a language, if one is defined
/// (e.g. [`Language::French`] → `"fr-conj"`).
pub fn companion_for_language(language: Language) -> Option<&'static str> {
    COMPANIONS
        .iter()
        .find(|(_, _, lang)| *lang == language)
        .map(|(_, companion, _)| *companion)
}

/// Whether `id` is a conjugation companion (a hidden, auto-paired dictionary).
pub fn is_companion(id: &str) -> bool {
    COMPANIONS.iter().any(|(_, companion, _)| *companion == id)
}

/// Whether `path` points at an installed conjugation companion, matched by the
/// companion id segment in its install path (e.g. `.../fr-conj/...`). Installed
/// dictionaries live under `dictionaries_dir()/<id>/`, so the id is always a
/// path segment.
pub fn path_is_companion(path: &Path) -> bool {
    let path = path.to_string_lossy();
    COMPANIONS
        .iter()
        .any(|(_, companion, _)| path.contains(&format!("/{companion}/")))
}

fn find_ifo(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "ifo"))
}

/// Progress reported during [`install`].
#[derive(Debug, Clone, Copy)]
pub enum Progress {
    /// Bytes of the compressed download received so far, and the total if the
    /// server reported a `Content-Length`.
    Downloading { received: u64, total: Option<u64> },
}

/// A reader that reports how many bytes have passed through it. Wrapping the
/// HTTP body lets us emit download progress while the bytes stream straight on
/// into the zstd/tar pipeline, without buffering the whole archive.
struct ProgressReader<R, F> {
    inner: R,
    received: u64,
    total: Option<u64>,
    callback: F,
}

impl<R: Read, F: FnMut(Progress)> Read for ProgressReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.received += n as u64;
            (self.callback)(Progress::Downloading {
                received: self.received,
                total: self.total,
            });
        }
        Ok(n)
    }
}

/// Download and install the dictionary described by `entry`, reporting download
/// progress through `progress`. Returns the path to the installed `.ifo` file,
/// ready to hand to [`DictionaryManager::add`](crate::DictionaryManager::add).
///
/// The archive is streamed straight through zstd + tar into a temporary
/// directory that is swapped into place only on success, so an interrupted or
/// failed download never leaves a half-installed dictionary behind.
pub fn install(entry: &CatalogEntry, progress: impl FnMut(Progress)) -> Result<PathBuf, Error> {
    let dir = install_dir(entry.id)?;
    let parent = dir
        .parent()
        .expect("install dir is dictionaries_dir/<id>, so it has a parent");
    fs::create_dir_all(parent)?;

    let tmp = parent.join(format!(".{}.tmp", entry.id));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;

    // Run the fallible download/extract in a closure so we always clean up the
    // temp dir on the way out, whatever fails.
    let extracted = (|| {
        let resp = ureq::get(entry.url)
            .call()
            .map_err(|e| Error::Download(format!("requesting {}: {e}", entry.url)))?;
        let total = resp
            .header("Content-Length")
            .and_then(|s| s.parse::<u64>().ok());
        let reader = ProgressReader {
            inner: resp.into_reader(),
            received: 0,
            total,
            callback: progress,
        };
        let decoder = ruzstd::decoding::StreamingDecoder::new(reader)
            .map_err(|e| Error::Download(format!("decoding zstd stream: {e}")))?;
        tar::Archive::new(decoder)
            .unpack(&tmp)
            .map_err(|e| Error::Download(format!("extracting archive: {e}")))?;
        find_ifo(&tmp).ok_or_else(|| Error::Download("archive contains no .ifo file".to_string()))
    })();

    let ifo_in_tmp = match extracted {
        Ok(p) => p,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };
    let ifo_name = ifo_in_tmp
        .file_name()
        .expect("find_ifo returns a path with a filename")
        .to_owned();

    // Swap the freshly extracted dir into place, replacing any prior install.
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::rename(&tmp, &dir)?;

    Ok(dir.join(ifo_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_mapping_round_trips_for_all_languages() {
        for (primary, companion, language) in
            [("fr-fr", "fr-conj", Language::French), ("en-en", "en-conj", Language::English),
             ("it-it", "it-conj", Language::Italian)]
        {
            assert_eq!(companion_for(primary), Some(companion));
            assert_eq!(primary_for(companion), Some(primary));
            assert_eq!(companion_for_language(language), Some(companion));
            assert!(is_companion(companion));
            assert!(!is_companion(primary));
        }
    }

    #[test]
    fn non_companion_ids_have_no_mapping() {
        assert_eq!(companion_for("de-de"), None);
        assert_eq!(primary_for("de-de"), None);
        assert_eq!(companion_for_language(Language::Auto), None);
        assert!(!is_companion("en-en"));
    }

    #[test]
    fn path_is_companion_matches_install_segment() {
        assert!(path_is_companion(Path::new("/data/dictionaries/fr-conj/x.ifo")));
        assert!(path_is_companion(Path::new("/data/dictionaries/it-conj/x.ifo")));
        assert!(!path_is_companion(Path::new("/data/dictionaries/fr-fr/x.ifo")));
    }
}
