# Albert launcher integration

A Python plugin for the [Albert](https://albertlauncher.github.io/) launcher
that searches the irondict dictionaries. Type the trigger (`d ` by default)
followed by a word to list every matching headword; activating a result opens
the irondict GUI straight to that word's definition.

## Requirements

- Albert with the Python plugin enabled (Python interface **v5.0**, i.e.
  Albert ≥ 0.26 / the v34 series).
- `irondict` on `$PATH`, version **≥ 0.5** (the `--gui --word` flag the "Open"
  action uses was added then).

## Install

Copy the plugin into Albert's user plugin directory and enable it:

```sh
cp -r irondict ~/.local/share/albert/python/plugins/
```

Then open Albert's settings → Plugins, enable **Python**, and enable
**IronDict** under it. The trigger and prefix/fuzzy matching can be changed
there.

## Usage

- `d comp` — list headwords matching `comp` (prefix/autocomplete by default).
- Enter on a result — open it in the irondict GUI.
- Alt+Enter (or the action menu) — copy the word to the clipboard.

Enable *fuzzy matching* for the plugin in Albert's settings to switch from
prefix to typo-tolerant matching.
