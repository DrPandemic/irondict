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
