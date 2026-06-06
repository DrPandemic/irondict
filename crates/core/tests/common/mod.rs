use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A throwaway directory under the OS temp dir, removed on drop. A minimal
/// stand-in for the `tempfile` crate (whose recent versions require a newer
/// Cargo than this project targets).
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new() -> std::io::Result<Self> {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = format!(
            "irondict-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
