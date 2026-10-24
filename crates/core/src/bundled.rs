//! Registry for dictionaries bundled inside the application (the APK's assets)
//! rather than stored on the filesystem.
//!
//! Core stays free of Android APIs. The Android entry point resolves each APK
//! asset to a [`DictSource`] (a memory-mapped range, or an owned buffer for a
//! compressed asset) and registers the set here under the same virtual `.ifo`
//! path the config already uses. [`crate::stardict::load`] then resolves that
//! path through this registry instead of the filesystem, so the config entry and
//! the search-index signature stay stable across launches.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::source::DictSource;

/// The sub-files of one bundled dictionary.
#[derive(Clone)]
pub struct BundledFiles {
    pub ifo: DictSource,
    pub idx: DictSource,
    /// The `.idx` is gzipped (decompressed into memory on load).
    pub idx_gz: bool,
    pub dict: DictSource,
    /// The `.dict` is dictzip-compressed (`.dict.dz`).
    pub dict_gz: bool,
    pub syn: Option<DictSource>,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, BundledFiles>> {
    static REG: OnceLock<Mutex<HashMap<PathBuf, BundledFiles>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register `files` under the virtual `.ifo` path `path`. The path need not
/// exist on disk; it is only an identity for the config and the loader.
pub fn register(path: impl Into<PathBuf>, files: BundledFiles) {
    registry().lock().unwrap().insert(path.into(), files);
}

/// The bundled files registered under `path`, if any.
pub(crate) fn get(path: &Path) -> Option<BundledFiles> {
    registry().lock().unwrap().get(path).cloned()
}
