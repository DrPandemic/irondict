<div align="center">

<img src="docs/assets/icon.svg" width="96" alt="irondict icon">

# IronDict

**Fast local multi-dictionary lookup with fuzzy headword search — CLI and GUI.**

[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)

<table>
<tr>
<td><img src="docs/assets/screenshot-light.png" alt="irondict GUI in light mode"></td>
<td><img src="docs/assets/screenshot-dark.png" alt="irondict GUI in dark mode"></td>
</tr>
</table>

</div>

## About

irondict is a desktop dictionary app written in Rust. It loads
[StarDict](https://en.wikipedia.org/wiki/StarDict) dictionaries and gives you
instant fuzzy and prefix search over their headwords across all of them at once.
The same lookup engine powers two front-ends — a native GUI and a CLI — so you
can search from your desktop or your terminal.

The public-domain [GCIDE](docs/gcide.md) dictionary is bundled, so it works out
of the box. Add your own StarDict files, or download monolingual
[Wiktionary](https://www.wiktionary.org/) dictionaries for several languages
directly from the app.

## Features

- **Two front-ends, one engine** — a native [Slint](https://slint.dev) GUI and a
  clap-based CLI share the same core lookup library.
- **Fuzzy, prefix & exact headword search** across every enabled dictionary at once.
- **Downloadable dictionaries** — install monolingual Wiktionary editions for
  seven languages from a built-in catalog, or add any StarDict file you own.
- **Clickable cross-references** — follow links between entries in the GUI.
- **Bundled GCIDE** dictionary, so it works out of the box.
- **System theme aware** (light/dark via the XDG desktop portal).

## Install

Build from source with a recent stable Rust toolchain:

```sh
git clone https://github.com/DrPandemic/irondict
cd irondict
cargo build --release
```

The single binary lands at `target/release/irondict`. It serves both front-ends:
run a subcommand for the CLI, or pass `--gui` to launch the graphical interface.

## Usage

### GUI

```sh
cargo run --release -p irondict -- --gui
```

### CLI

```sh
# Look up a word across all enabled dictionaries
irondict lookup serendipity

# Search headwords (fuzzy by default)
irondict search serendipity

# Download dictionaries from the built-in catalog
irondict catalog            # list what's available
irondict install fr-fr      # download and install by id
irondict uninstall fr-fr    # remove it (files + registration)

# Manage dictionaries
irondict add /path/to/dictionary.ifo
irondict list
irondict remove "Dictionary Name"
```

In the GUI, the same actions live under **Settings → Downloads** (download,
install, delete) and **Settings → Dictionaries** (add, enable, remove).

## Configuration

Dictionaries and preferences are stored in `~/.config/irondict/config.toml`, and
downloaded dictionaries under `~/.local/share/irondict/dictionaries/`. The search
index is cached under `~/.cache/irondict/` and rebuilt automatically when the
dictionary set changes.

Downloaded dictionaries are sourced from
[xxyzz/wiktionary_stardict](https://github.com/xxyzz/wiktionary_stardict) and,
being derived from Wiktionary, are licensed
[CC BY-SA](https://creativecommons.org/licenses/by-sa/4.0/).

## License

[GPL-3.0-or-later](LICENSE).
