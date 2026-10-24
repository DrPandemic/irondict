# Packaging irondict (Linux)

This documents how to produce and install a distributable build of the single
`irondict` binary, which serves both front-ends: subcommands run the CLI, and
`--gui` (used by the desktop entry) launches the graphical interface.

## Release build

```sh
cargo build --release -p irondict
```

The workspace `[profile.release]` (root `Cargo.toml`) enables `lto = "thin"`,
`codegen-units = 1`, and `strip = "symbols"` so the shipped binary is optimized
and free of debug symbols. The binary lands at `target/release/irondict`.

The default **GCIDE** dictionary is shipped as a **data directory**, not embedded
in the binary. `bundled_gcide_path()` (`irondict-core`) resolves it at runtime, so
an installed binary finds the data wherever it was installed while an in-tree
build still uses `crates/core/assets/gcide/`. Search order (first existing wins):

1. `$IRONDICT_GCIDE_DIR` — explicit override.
2. `<exe-dir>/../share/irondict/gcide` — relative to the installed binary, so any
   `--prefix` works (`/usr/bin/irondict` → `/usr/share/irondict/gcide`).
3. system data dirs from `$XDG_DATA_DIRS` (default `/usr/local/share:/usr/share`),
   each `<dir>/irondict/gcide`.
4. the compile-time source asset (`CARGO_MANIFEST_DIR/assets/gcide`) — dev fallback.

So a package **must install the GCIDE trio** (`*.ifo` / `*.idx` / `*.dict.dz`) to
`<prefix>/share/irondict/gcide/` (see the install recipe below). User-added
dictionaries live in the config dir (`~/.config/irondict/`), and the search index
is cached in `~/.cache/irondict/index`.

## Install layout (FHS / XDG)

| Artifact | Source | Installed path (prefix `/usr`) |
|----------|--------|-------------------------------|
| Binary | `target/release/irondict` | `/usr/bin/irondict` |
| Desktop entry | `packaging/irondict.desktop` | `/usr/share/applications/irondict.desktop` |
| Icons (PNG) | `crates/app/assets/icons/hicolor/<size>/apps/irondict.png` | `/usr/share/icons/hicolor/<size>/apps/irondict.png` |
| Scalable icon | `crates/app/assets/icons/irondict.svg` | `/usr/share/icons/hicolor/scalable/apps/irondict.svg` |
| GCIDE data | `crates/core/assets/gcide/*` | `/usr/share/irondict/gcide/` |

The `.desktop` `Exec=irondict --gui` and `Icon=irondict` match the installed
binary name and the icon basename. `StartupWMClass=irondict` lets the compositor pair the
window with the launcher entry. The window also sets its own icon at runtime (via
the Slint `icon:` property, embedded at compile time) so it shows correctly even
when run uninstalled.

### Example install (staged into `$DESTDIR`)

```sh
prefix=/usr
install -Dm755 target/release/irondict        "$DESTDIR$prefix/bin/irondict"
install -Dm644 packaging/irondict.desktop      "$DESTDIR$prefix/share/applications/irondict.desktop"
install -Dm644 crates/app/assets/icons/irondict.svg \
    "$DESTDIR$prefix/share/icons/hicolor/scalable/apps/irondict.svg"
# default GCIDE dictionary data (resolved at runtime by bundled_gcide_path())
for f in crates/core/assets/gcide/*; do
    install -Dm644 "$f" "$DESTDIR$prefix/share/irondict/gcide/$(basename "$f")"
done
for s in 16 32 48 64 128 256 512; do
    install -Dm644 "crates/app/assets/icons/hicolor/${s}x${s}/apps/irondict.png" \
        "$DESTDIR$prefix/share/icons/hicolor/${s}x${s}/apps/irondict.png"
done
```

After a system install, refresh the caches:

```sh
update-desktop-database -q /usr/share/applications || true
gtk-update-icon-cache -q /usr/share/icons/hicolor || true
```

## Icon

`crates/app/assets/icons/irondict.svg` is the source of truth (an open book on the
indigo accent tile, matching the GUI fallback accent `#4F46E5`). The PNG raster
sizes are generated from it:

```sh
cd crates/app/assets/icons
for s in 16 32 48 64 128 256 512; do
    rsvg-convert -w $s -h $s irondict.svg -o "hicolor/${s}x${s}/apps/irondict.png"
done
```

## Licensing & provenance

- **irondict** is `GPL-3.0-or-later` (see the top-level `LICENSE`).
- The bundled **GCIDE** StarDict data is **GPL-2.0-or-later** — full provenance,
  SHA-256, upstream source, and the license notice are recorded in
  [`docs/gcide.md`](gcide.md). GPL-2.0-or-later content redistributes cleanly
  inside this GPL-3.0-or-later project.
- **IBM Plex Sans** (bundled under `crates/app/assets/fonts/`) is SIL OFL 1.1.
- **Slint** is used under its GPLv3 option (no paid license needed for desktop).

A binary distribution must therefore carry the irondict `LICENSE`, the GCIDE
notice (via `docs/gcide.md`), and the SIL OFL for IBM Plex Sans.

## Android (arm64 APK)

The GUI also ships as an Android APK built with
[`cargo-apk`](https://github.com/rust-mobile/cargo-apk). A single `cdylib`,
`crates/android` (`irondict-android`), provides the `android_main` entry point and
the packaging metadata; the window and all behaviour are the same code as the
desktop GUI (`crates/app`, linked as a library). The APK works on plain AOSP — no
Google Play services or libraries.

### Toolchain

- `rustup target add aarch64-linux-android x86_64-linux-android`
  (x86_64 is for the emulator).
- `cargo install cargo-apk`.
- A JDK plus the Android SDK/NDK. The Slint Android backend's `build.rs` compiles
a Java helper with `javac`/`d8`, so `d8` from build-tools < 35 breaks on JDK ≥ 21;
keep build-tools ≥ 35 installed. NDK r28+ links for 16 KB pages by default
(NDK 30 is used here).
- Environment: `ANDROID_HOME`, `ANDROID_NDK_ROOT`, `ANDROID_PLATFORM=android-35`,
  `JAVA_HOME` (Android Studio's `jbr` works).

### Build

```sh
ANDROID_HOME=$ANDROID_HOME \
ANDROID_NDK_ROOT=$ANDROID_HOME/ndk/30.0.16248370 \
ANDROID_PLATFORM=android-35 \
JAVA_HOME=/opt/android-studio/jbr \
CARGO_APK_RELEASE_KEYSTORE=$HOME/.android/debug.keystore \
CARGO_APK_RELEASE_KEYSTORE_PASSWORD=android \
cargo apk build -p irondict-android --lib --release --target x86_64-linux-android
```

The signed APK lands at `target/release/apk/irondict-android.apk`. `cargo-apk`
derives `versionName` from the workspace `package.version` (`version_code` from its
semver), so the single workspace version drives both desktop and Android.

`build_targets` in `crates/android/Cargo.toml` lists `aarch64-linux-android`
(the shipping target) and `x86_64-linux-android` (the emulator); drop x86_64 for a
release APK. Release keystores are supplied through the
`CARGO_APK_RELEASE_KEYSTORE[_PASSWORD]` environment variables and must be kept
**outside the repository**; the debug keystore above is for local testing only.

### Bundled GCIDE

The SDK's `assets` metadata points at `crates/core/assets`, so the APK carries
`assets/gcide/`. Core reads the StarDict files through a source abstraction
(`irondict_core::DictSource`): a filesystem path, a byte range of an open file, or
an owned buffer. On Android the entry point opens each asset:

- `.dict.dz` is stored uncompressed by `aapt`, so `AAsset_openFileDescriptor64`
succeeds and it is memory-mapped **in place** from `base.apk` (no 34 MB copy, no
extraction to disk).
- `.idx` and `.ifo` come out deflated, so they are read into memory.

Run the guard after a build to assert the mapping precondition still holds:

```sh
crates/android/verify-apk-assets.sh target/release/apk/irondict-android.apk
```

### On-device notes

- Verify on a `default` (AOSP) system image, not a `google_apis*` one.
- `directories` can't find a home on Android, so `android_main` sets `HOME` to
the app's internal data directory before the backend starts; config, data and the
search-index cache then land under it.
- `eprintln!` is invisible on Android, so logs and panics are routed to logcat
under the `irondict` tag.
- The narrow (phone) layout is selected at runtime (`root.width < 600px`); the same
build is the desktop layout when the window is wide.
- Android builds the UI with Slint's **material** style (`crates/app/build.rs`);
desktop keeps `fluent`. Fluent's `ScrollView` sets `flickable.interactive: false`,
which disables touch drag-panning, so definitions/results wouldn't scroll on a
phone. Material also gives the Settings widgets a native feel; its ComboBox is
wider, so the dictionary row pins its width and disables the horizontal
scrollbar.
- The app theme (`crates/android/res/values/styles.xml`) forces
`windowLightStatusBar`/`windowLightNavigationBar`, so the system clock and status
icons stay dark and readable over the light UI (Android 15+ draws apps
edge-to-edge).
- The definition body is a `ScrollView` over a `VerticalLayout`, not a `ListView`.
`ListView` re-estimates item heights during a fling, which makes long entries
jump ~200px mid-scroll; realizing every block keeps heights exact and scrolling
smooth. Two details matter: the inner layout uses `alignment: start` (the default
`stretch` spreads the viewport's spare space between rows, opening big gaps on
short entries), and its `width` is bound to `def-scroll.visible-width` so the body
can't pan sideways by the vertical scrollbar's reserve.
- On phones the settings panel and the conjugation overlay are finger-following
pagers. Both render their tab pages side by side and translate them with a
`SwipePanel` (a `SwipeGestureHandler` that hosts the content, with a background
`TouchArea {}` so nested vertical `ScrollView`s keep scrolling). `moved` tracks
the finger, `swiped` snaps to the nearest page (a drag past ~1/3 of the width
switches, otherwise it snaps back), and `cancelled` restores the current page. A
`Flickable` was tried first but its fling animation kept overriding the snap and
left the page between tabs. The page offset is an `in-out` root property whose
`animate` duration is `0` while dragging (1:1 tracking) and animated for tab taps
and the snap.
- The launcher icon is an **adaptive icon**
(`res/mipmap-anydpi-v26/ic_launcher.xml` + `res/drawable/ic_launcher_foreground.xml`,
background `@color/ic_launcher_background` `#4F46E5`). Without it Android treats
the legacy `mipmap-*/ic_launcher.png` as a pre-8.0 icon and draws it as a small
tile inside a white circle. The full-bleed tile colour is also painted in the
foreground so launchers that tint/override the background still show the indigo
square; the artwork matches `crates/app/assets/icons/irondict.svg` used on the
desktop.

