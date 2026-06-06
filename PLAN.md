# irondict — Project Plan

## Context

`irondict` is a multi-dictionary lookup app written in Rust, exposed through both
a **CLI** and a **GUI**. It ships a few preinstalled dictionaries but is mostly
driven by **user-provided StarDict files**. Lookups must support not just exact
matches but **fuzzy (typo-tolerant) and full-text** search across definitions.
We implement this feature-by-feature; each phase is independently runnable/testable.

## Decisions

- **Data:** preinstalled + mostly user-provided dictionaries. The **first
  preinstalled dictionary is GCIDE** — the GNU Collaborative International
  Dictionary of English (based on Webster's Revised Unabridged 1913 + WordNet
  supplements). A monolingual English dictionary, available in StarDict form.
  Note: the bundled GCIDE build (v0.44) is **GPL-2.0-or-later** (verified in Phase 2,
  see `docs/gcide.md`); newer GNU gcide 0.51+ is GPLv3. Either way it is "or-later",
  so it redistributes cleanly inside this GPL-3.0-or-later project.
- **Format:** StarDict (`.ifo` / `.idx` / `.dict`(`.dz`)).
- **Scope:** multi-dictionary, user can manage/switch/aggregate across them.
- **Matching:** fuzzy + full-text (and exact/prefix as the simpler subset).
- **Structure:** Cargo workspace (core lib + cli bin + gui bin).
- **GUI framework:** **Slint** — chosen for the highest visual polish for the least
  effort (modern look + fast boot were the priorities). UI is written in `.slint`
  declarative markup with built-in Material/Fluent styling and animations; Rust
  provides the glue via properties + callbacks. `iced` is the fallback if we ever
  want to stay 100% Rust with no markup language.
- **Project license: GPLv3** (`GPL-3.0-or-later`, set in `Cargo.toml`). This is
  compatible with bundling GCIDE (GPLv3) and using Slint under its GPLv3 option,
  so the whole stack is consistent. A full `LICENSE` (GPLv3 text) file is added in
  Phase 0.
- **Slint license:** tri-licensed; since the project is GPLv3 we use Slint under its
  **GPLv3** option (no paid license needed for desktop).

## Architecture

Cargo **workspace** with three crates under `crates/`:

```
irondict/
├── Cargo.toml            # [workspace] members
└── crates/
    ├── core/   (irondict-core)  # library: model, StarDict loader, manager, search
    ├── cli/    (irondict-cli)   # binary: clap-based CLI front-end
    └── gui/    (irondict-gui)   # binary: Slint front-end (ui.slint + main.rs)
```

Both front-ends depend only on `irondict-core`, which owns all dictionary logic
(matches the `CLAUDE.md` convention: keep CLI/GUI separate from the core lookup
so both share the same backend).

### Core domain model (`crates/core/src`)

- `Entry { headword: String, definition: Definition }` where `Definition` keeps
  the StarDict data type (plain text / HTML / etc.) so the GUI can render richly.
- `Dictionary` — one loaded dictionary: metadata (name, language pair, word count)
  + lookup over its entries.
- `DictionaryManager` — owns multiple `Dictionary` instances; aggregates searches
  across the enabled ones; add/remove/enable.
- `Config` — persisted list of dictionary paths + enabled state, stored in the OS
  app-data dir via the `directories` crate.
- `SearchEngine` — exact, prefix, fuzzy, and full-text queries (see Phase 4).

### Key crates

- StarDict parsing: **`stardict`** (most maintained, actively updated).
- Full-text + fuzzy: **`tantivy`** (`FuzzyTermQuery` for typo tolerance up to edit
  distance 2; BM25 full-text over headwords + definitions).
- Config/paths: `directories`, `serde` + `serde_json` (or `toml`).
- Errors: `thiserror` (lib) / `anyhow` (binaries).
- CLI: `clap` (derive). GUI: `slint` (+ `slint-build` for compiling `.slint`).

## Build order (one phase at a time)

- [x] **Phase 0 — Workspace scaffold.** Convert the single crate into a workspace;
  create empty `irondict-core`, `irondict-cli`, `irondict-gui`. App compiles & runs
  stub binaries. Add the GPLv3 `LICENSE` file (done) and `Cargo.toml` license field
  (done). Update `CLAUDE.md` with the crate layout.

- [x] **Phase 1 — Core model + StarDict loading.** Define
  `Entry`/`Definition`/`Dictionary`; load a StarDict file via the `stardict` crate;
  exact-match lookup. Unit tests with a small sample dictionary committed as a fixture.

- [x] **Phase 2 — Acquire & convert GCIDE to StarDict.** Sourced a ready-made GCIDE
  StarDict build (`dictd_www.dict.org_gcide`, v0.44) from the StarDict community
  mirror (`download.huzheng.org`) and committed it under
  `crates/core/assets/gcide/`. Verified it loads through the Phase 1 loader
  (`crates/core/tests/gcide_test.rs`: bookname, wordcount 174222, and the headword
  `dictionary` resolve). Full provenance, SHA-256, license, and the GNU-source
  fallback-conversion steps are documented in `docs/gcide.md`.
  - **License finding:** the bundled GCIDE data is **GPL-2.0-or-later** (not GPLv3 as
    assumed below); the embedded notice says "version 2, or (at your option) any later
    version" with "No additional restrictions are claimed." GPL-2.0-or-later content
    redistributes cleanly inside this GPL-3.0-or-later project. (Newer GNU gcide 0.51+
    is GPLv3.)
  - **Bundling strategy decided:** commit the StarDict trio into the repo (offline,
    reproducible) rather than download-on-first-run.

- [x] **Phase 3 — Dictionary manager + config.** `DictionaryManager` over multiple
  dicts; persisted `Config` in the app-data dir; add/remove/enable; load preinstalled
  (GCIDE) + user dictionaries on startup.
  - **Config** (`crates/core/src/config.rs`): TOML `[[dictionaries]] path + enabled`,
    stored at the `directories` app-data path (`~/.config/irondict/config.toml`);
    `load`/`save` (default path) + `load_from`/`save_to` (explicit path, for tests);
    missing file → empty default, omitted `enabled` defaults to `true`.
  - **Manager** (`crates/core/src/manager.rs`): `add`/`remove`/`set_enabled`,
    aggregated `lookup` returning per-dictionary `LookupResult` (tagged with source),
    `from_config` (collects per-dict load errors so one bad file doesn't abort
    startup), `config()` snapshot for round-tripping, and `add_bundled_gcide()` via
    `bundled_gcide_path()` (compile-time asset path; real packaging is Phase 7).
  - Verified: `cargo test -p irondict-core` (20 tests), `cargo clippy`, `cargo fmt`
    on Rust 1.96.0 (stable).

- [x] **Phase 4 — CLI front-end.** `clap` commands implemented: `lookup <word>`,
  `add <path>`, `list`, `remove <name>`, plus `enable <name>`/`disable <name>` (the
  core `set_enabled` is otherwise unreachable from the CLI). `search <query>` is
  deferred to Phase 5, where its `SearchEngine` actually lands. Aggregates results
  across enabled dictionaries and prints the source dictionary per result.
  - **Startup** (`crates/cli/src/main.rs` `load_manager`): loads `Config` from the
    app-data dir and, on first run only (config file absent), seeds + persists the
    bundled GCIDE so it's a normal config entry thereafter (so a later `remove gcide`
    sticks). Per-dictionary load failures are printed as warnings, not fatal.
  - **Persistence:** mutation commands (`add`/`remove`/`enable`/`disable`) save
    `manager.config()` back to disk; `lookup`/`list` are read-only.
  - Verified end-to-end in an isolated `XDG_CONFIG_HOME`: `add` the mini fixture →
    `list` shows GCIDE + mini → `lookup hello` returns hits from both (source-tagged)
    → `disable`/`remove` persist. `cargo test`/`clippy`/`fmt` all clean on 1.96.0.

- [x] **Phase 5 — Search engine (prefix → fuzzy → full-text).** `SearchEngine`
  (`crates/core/src/search.rs`) builds a `tantivy` 0.26 index over headwords +
  definitions and exposes all four query modes via `SearchMode`
  (`Exact`/`Prefix`/`Fuzzy`/`FullText`), wired into the CLI `search` command.
  - **Schema:** `dictionary` (STRING|STORED source name), `key` (lowercased headword
    as a single token — backs case-insensitive exact via `TermQuery`, prefix via
    `RegexQuery` `^prefix.*`, and fuzzy via `FuzzyTermQuery` edit-distance 2),
    `headword` (TEXT|STORED, original case — full-text + display), `definition`
    (TEXT|STORED — full-text + snippet). Full-text uses `QueryParser` (lenient) over
    `headword`+`definition` with BM25.
  - **Entry iteration:** `Dictionary::for_each_entry` walks `idx.items` and pulls each
    definition (disjoint field borrows of the `stardict` inner); `DictionaryManager::
    for_each_enabled_entry` feeds only enabled dicts into the index.
  - **Caching:** index stored at the OS cache dir (`search::default_index_dir`,
    `~/.cache/irondict/index`). The CLI writes a `manifest` signature (sorted
    name|path|word_count of enabled dicts); a `search` reuses the cached index when
    the signature matches and rebuilds otherwise, so add/remove/enable/disable
    invalidate it automatically. `build` clears any stale index dir to stay idempotent.
  - Verified: `cargo test -p irondict-core` (29 tests, incl. `tests/search_test.rs`),
    `cargo clippy --workspace --all-targets` and `cargo fmt --check` clean on 1.96.0.
    End-to-end against bundled GCIDE in an isolated `XDG_CONFIG_HOME`/`XDG_CACHE_HOME`:
    fuzzy `dictionarie`→`Dictionary`, prefix `diction`→`Diction…`, exact `dictionary`,
    full-text `lexicographer` (definition-only hits), and second run reuses the cache.

- [ ] **Phase 6 — GUI front-end (Slint).** `ui.slint` markup defines a
  search-as-you-type box → results list → definition pane, plus a dictionary
  management panel (add/remove/enable, show counts). `main.rs` wires Slint
  properties/callbacks to `DictionaryManager` + `SearchEngine`. A `build.rs` compiles
  the `.slint` via `slint-build`. Use a built-in style (Fluent/Material) for a modern
  look out of the box.

- [ ] **Phase 7 — Polish.** Rich definition rendering (HTML/markup data types,
  including GCIDE's markup), search history, settings, packaging of GCIDE.

## Critical files (to be created)

- `Cargo.toml` (root) — convert to `[workspace]`.
- `crates/core/src/{lib.rs, model.rs, stardict.rs, manager.rs, config.rs, search.rs}`
- `crates/cli/src/main.rs` — `clap` CLI.
- `crates/gui/` — `ui/ui.slint` (markup), `src/main.rs` (glue), `build.rs` (slint-build).
- `crates/core/tests/` + a small StarDict fixture for tests.

## Verification

- **Per phase:** `cargo test`, `cargo clippy`, `cargo fmt --check`.
- **Phase 1:** unit test loads the fixture dictionary and asserts a known
  headword → definition.
- **Phase 2:** the converted GCIDE StarDict loads through the Phase 1 loader and a
  known headword (e.g. "dictionary") resolves to its definition.
- **Phase 4:** `cargo run -p irondict-cli -- add <fixture>` then
  `cargo run -p irondict-cli -- lookup <word>` returns the expected entry.
- **Phase 5:** CLI `search` returns fuzzy hits for a misspelled query and full-text
  hits for a word appearing only inside a definition.
- **Phase 6:** `cargo run -p irondict-gui` — type a prefix, see live results, click an
  entry, see its definition; add/remove a dictionary from the UI.

## Open questions (can defer)

- GCIDE bundling strategy (commit in-repo vs. download on first run) — decided in
  Phase 2 based on file size and GPLv3 redistribution.
- Config format: JSON vs TOML (lean TOML for human-editability).
- Verify the `stardict` crate's license on crates.io before depending on it
  (the GUI/search crates are MIT/Apache; Slint is royalty-free for desktop).
