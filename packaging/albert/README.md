# Albert launcher integration

A Python plugin for the [Albert](https://albertlauncher.github.io/) launcher
that searches the irondict dictionaries. Type a trigger followed by a word to
list every matching headword; activating a result opens the irondict GUI
straight to that word's definition, scoped to the dictionary it came from.

## Requirements

- Albert with the Python plugin enabled (Python interface **v5.0**, i.e.
  Albert ≥ 0.26 / the v34 series).
- `irondict` on `$PATH`, version **≥ 0.6** (the `--gui --word`/`--dict` and
  `search --dict` flags the plugin uses were added then).

## Install

Copy the plugin into Albert's user plugin directory and enable it:

```sh
cp -r irondict ~/.local/share/albert/python/plugins/
```

Then open Albert's settings → Plugins, enable **Python**, and enable
**IronDict** under it.

## Handlers and triggers

The plugin registers a general handler plus one per dictionary that has a
**pinned language** (set each dictionary's language in irondict's Settings):

- **IronDict** (`d ` by default) — searches every dictionary.
- **IronDict — <name>** — one per language-pinned dictionary, keyed by the
  language code, searching (and opening) only that dictionary. Defaults: `de `
  for English, `df ` for French, `di ` for Italian. A second dictionary in the
  same language gets the full code (e.g. `dfr `) and id suffix (`fr2`).

Each handler has its own trigger, editable under Albert's settings → Triggers,
so you can set whatever per-language shortcuts you like. Dictionaries left on
the **Auto** language, disabled dictionaries, and hidden companions (such as the
conjugation table) don't get a handler.

## Usage

- `d comp` — list headwords matching `comp` across all dictionaries.
- `di cas` — list matches from just the dictionary bound to `di `.
- Enter on a result — open it in the irondict GUI, scoped to its dictionary.
- Alt+Enter (or the action menu) — copy the word to the clipboard.

Enable *fuzzy matching* for a handler in Albert's settings to switch it from
prefix/autocomplete to typo-tolerant matching (each handler toggles
independently).
