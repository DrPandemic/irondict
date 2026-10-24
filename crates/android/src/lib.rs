//! Android entry point for the irondict GUI.
//!
//! This crate only supplies the `android_main` symbol and the packaging
//! metadata; the window and all behaviour live in the `irondict` crate. On
//! non-Android targets it compiles to an empty `cdylib` so `--workspace`
//! commands still succeed.

#[cfg(target_os = "android")]
mod android {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::Read;
    use std::sync::Arc;

    use irondict_core::{register_bundled, BundledFiles, DictSource};

    const IFO: &str = "gcide/dictd_www.dict.org_gcide.ifo";
    const IDX: &str = "gcide/dictd_www.dict.org_gcide.idx";
    const DICT: &str = "gcide/dictd_www.dict.org_gcide.dict.dz";

    /// Resolve one APK asset to a [`DictSource`]: a mapped range when the asset
    /// is stored uncompressed (a `base.apk` descriptor succeeds), otherwise an
    /// owned buffer read out of the APK. `allow_owned` is false for the 34 MB
    /// `.dict.dz`, which must take the range path or GCIDE is skipped rather than
    /// copied into RAM.
    fn asset_source(
        assets: &slint::android::android_activity::ndk::asset::AssetManager,
        asset_name: &str,
        label: &str,
        allow_owned: bool,
    ) -> Option<DictSource> {
        let cstr = CString::new(asset_name).ok()?;
        let asset = assets.open(&cstr)?;
        match asset.open_file_descriptor() {
            Ok(opened) => {
                log::info!(
                    "bundled {asset_name}: range {} bytes at offset {}",
                    opened.size,
                    opened.offset
                );
                let file = Arc::new(File::from(opened.fd));
                Some(DictSource::Range {
                    file,
                    offset: opened.offset as u64,
                    len: opened.size as u64,
                    name: label.to_owned(),
                })
            }
            Err(e) => {
                if !allow_owned {
                    log::error!("bundled {asset_name} is compressed ({e}); skipping GCIDE");
                    return None;
                }
                let mut buf = Vec::with_capacity(asset.length());
                let mut asset = asset;
                match asset.read_to_end(&mut buf) {
                    Ok(_) => {
                        log::info!("bundled {asset_name}: read {} bytes", buf.len());
                        Some(DictSource::from_bytes(buf))
                    }
                    Err(e) => {
                        log::error!("reading bundled {asset_name}: {e}");
                        None
                    }
                }
            }
        }
    }

    /// Register the GCIDE bundled in the APK under the same virtual path the
    /// config already uses, so it resolves through `irondict-core`'s registry.
    fn register_gcide(app: &slint::android::AndroidApp) {
        let assets = app.asset_manager();
        let Some(ifo) = asset_source(&assets, IFO, "bundled:gcide:ifo", true) else {
            log::error!("bundled GCIDE .ifo missing; not registering GCIDE");
            return;
        };
        let Some(idx) = asset_source(&assets, IDX, "bundled:gcide:idx", true) else {
            log::error!("bundled GCIDE .idx missing; not registering GCIDE");
            return;
        };
        let Some(dict) = asset_source(&assets, DICT, "bundled:gcide:dict", false) else {
            return;
        };

        let path = irondict_core::bundled_gcide_path();
        register_bundled(
            path.clone(),
            BundledFiles {
                ifo,
                idx,
                idx_gz: false,
                dict,
                dict_gz: true,
                syn: None,
            },
        );
        log::info!("registered bundled GCIDE at {}", path.display());
    }

    /// Route diagnostics to logcat. `eprintln!` goes nowhere on Android, so the
    /// logger plus a panic hook are the only way to see what happened.
    fn init_logging() {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag("irondict"),
        );

        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log::error!("panic: {info}");
            default_hook(info);
        }));
    }

    #[unsafe(no_mangle)]
    fn android_main(app: slint::android::AndroidApp) {
        init_logging();

        // `directories` finds no config/data/cache dir on Android: `$HOME` is
        // unset and there is no `getpwuid` fallback there. Pointing `HOME` at
        // the app's internal data dir restores all three without touching core.
        match app.internal_data_path() {
            Some(dir) => std::env::set_var("HOME", dir),
            None => log::error!("AndroidApp::internal_data_path() returned None"),
        }

        // The bundled dictionary lives in the APK; register it before the GUI
        // loads the config that references it.
        register_gcide(&app);

        // Must run before any Slint call that needs a backend.
        if let Err(e) = slint::android::init(app) {
            log::error!("slint::android::init failed: {e}");
            return;
        }

        if let Err(e) = irondict::gui::run(None, None, None) {
            log::error!("GUI exited with error: {e}");
        }
    }
}
