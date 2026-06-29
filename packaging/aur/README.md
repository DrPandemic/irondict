# Arch Linux packaging

Files for building IronDict as an Arch package and publishing it to the
[AUR](https://aur.archlinux.org/).

- `PKGBUILD` — the build recipe. Downloads the tagged release tarball from
  GitHub, builds the workspace with `cargo`, and installs the single `irondict`
  binary plus the desktop entry, icons, and bundled GCIDE dictionary.
- `.SRCINFO` — generated metadata the AUR reads. Must be kept in sync with the
  `PKGBUILD` (see below).
- `LICENSE` — 0BSD license for the packaging sources (required by AUR).
- `REUSE.toml` — REUSE compliance file for the packaging sources (required by AUR).

## Build

Run as a normal user (not root) with `base-devel` and `cargo`/`rust` installed:

```sh
cd packaging/aur
makepkg -f          # build (skips check); produces irondict-<ver>-1-x86_64.pkg.tar.zst
makepkg -f -i       # build and install
```

Or build, test, and install in one step:

```sh
makepkg -si         # -s pulls missing deps, runs check(), then installs
```

Useful flags:

- `-c` — remove the `src/`/`pkg/` work dirs afterward.
- `--nocheck` — skip the `check()` step (`cargo test --workspace`).
- `makepkg --verifysource` — only download + checksum-verify the source.

The build fetches the **released tag** named by `pkgver`, not the local working
tree, so local uncommitted changes are not included.

## Releasing a new version

After tagging a release (e.g. `vX.Y.Z`):

1. Set `pkgver=X.Y.Z` (and reset `pkgrel=1`) in `PKGBUILD`.
2. Update `sha256sums` for the new tarball:

   ```sh
   curl -sL "https://github.com/DrPandemic/irondict/archive/refs/tags/vX.Y.Z.tar.gz" | sha256sum
   ```

   (or run `updpkgsums`, from the `pacman-contrib` package).
3. Regenerate the metadata:

   ```sh
   makepkg --printsrcinfo > .SRCINFO
   ```

4. Verify it builds cleanly, then commit all changed files (`PKGBUILD`,
   `.SRCINFO`, and `REUSE.toml` if version changed).
