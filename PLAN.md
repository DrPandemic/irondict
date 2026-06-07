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
    `bundled_gcide_path()` (compile-time asset path; real packaging is Phase 9).
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
    `RegexQuery` `^prefix.*`, and fuzzy via `FuzzyTermQuery`),
    `headword` (TEXT|STORED, original case — full-text + display), `definition`
    (TEXT|STORED — full-text + snippet). Full-text uses `QueryParser` (lenient) over
    `headword`+`definition` with BM25.
  - **Fuzzy ranking** (`SearchEngine::fuzzy_search`): tantivy's `FuzzyTermQuery` is
    constant-scored, so a naive top-N can drop a perfect match (it once missed
    `Baba` for query `baba`). Fixed by (1) a mild length guard (single characters
    match exactly; everything longer keeps the full distance-2 budget, so e.g.
    `ba`→`baba` is allowed — just ranked below closer matches); (2) a boosted
    `BooleanQuery` of exact + dist-1 + dist-2
    clauses so closer matches retrieve and rank first; (3) over-fetch + Rust re-rank
    by true edit distance (`edit_distance` — optimal string alignment /
    restricted Damerau–Levenshtein, so an adjacent transposition like
    `recieve`→`receive` costs 1, not 2; char-wise/Unicode-aware) with a
    first-character prefix guard and stable tie-breaks. `score` is `1/(1+distance)`.
  - **Entry iteration:** `Dictionary::for_each_entry` walks `idx.items` and pulls each
    definition (disjoint field borrows of the `stardict` inner); `DictionaryManager::
    for_each_enabled_entry` feeds only enabled dicts into the index.
  - **Caching:** index stored at the OS cache dir (`search::default_index_dir`,
    `~/.cache/irondict/index`). The CLI writes a `manifest` signature (sorted
    name|path|word_count of enabled dicts); a `search` reuses the cached index when
    the signature matches and rebuilds otherwise, so add/remove/enable/disable
    invalidate it automatically. `build` clears any stale index dir to stay idempotent.
  - Verified: `cargo test -p irondict-core` (34 tests, incl. `tests/search_test.rs`
    + `levenshtein` unit tests), `cargo clippy --workspace --all-targets` and
    `cargo fmt --check` clean on 1.96.0. End-to-end against bundled GCIDE in an
    isolated `XDG_CONFIG_HOME`/`XDG_CACHE_HOME`: fuzzy `baba`→`Baba` ranked first,
    `dictionarie`→`Dictionaries`(1 edit) above `Dictionary`(2 edits), prefix
    `diction`→`Diction…`, exact `dictionary`, full-text `lexicographer`
    (definition-only hits), and second run reuses the cache.

- [ ] **Phase 6 — GUI front-end (Slint).** `ui.slint` markup defines a
  search-as-you-type box → results list → definition pane, plus a dictionary
  management panel (add/remove/enable, show counts). `main.rs` wires Slint
  properties/callbacks to `DictionaryManager` + `SearchEngine`. A `build.rs` compiles
  the `.slint` via `slint-build`.

  **Agreed design ("toolbar layout") — decided in look-and-feel discussion:**
  - **Framework:** Slint 1.x under its **GPLv3** option. Do **not** describe the
    design as "Mac"/"Apple"; call it the "toolbar layout" (see memory
    `gui-naming-no-vendor`).
  - **Layout:** top toolbar with an inline `( All · GCIDE )` **segmented scope**
    selector on the **left** and a **search field** on the **right** — **no**
    back/forward history arrows. Body below = a **narrow results column** (shown only
    while searching) + a **definition pane** as the hero/main area.
  - **Empty state:** "Word of the moment" — a **random entry** rendered full in the
    definition pane.
  - **Results:** **card rows** showing the **headword + a greyed one-line snippet**
    (not a flat text list). Selected card = **tinted background + accent left bar**.
  - **Source tag:** a small **colored pill** (e.g. `❲ GCIDE ❳`), not a `── GCIDE`
    footer.
  - **Typography:** **IBM Plex Sans** (SIL OFL), bundled in `crates/gui/assets/`
    (variable font). Scale: headword ~28 semibold · part-of-speech/pron ~13 grey ·
    body ~16 / 1.5 line-height · result title ~14 medium · snippet ~12 grey ·
    pill ~11 medium. Modern sans throughout (no serif).
  - **Theme:** **light by default**; follow the OS light/dark preference live when the
    XDG portal reports one.
  - **Accent color:** blend with the OS. Detect via **`zbus`** in this order, first
    hit wins: XDG portal `org.freedesktop.appearance.accent-color` → GNOME
    `gsettings` → KDE `kdeglobals` → **fallback indigo `#4F46E5`**. Must be
    **blazing fast**: apply a **cached** value (or indigo on first run) for instant
    first paint, then refresh **async** off the UI thread and update a reactive Slint
    accent property; subscribe to the portal `SettingChanged` signal so OS
    accent/theme changes apply live. (Dev box is **sway**, which exposes no portal
    accent and `color-scheme = no preference`, so it lands on indigo + light.)
  - **Accent is used for:** selected result (tint + left bar), the source pill, the
    "word of the moment" label, the search focus ring, and links. Everything else is
    greyscale.
  - **Build order:** (1) static interactive visual prototype with sample data to
    validate the look ("try 1"), then (2) wire `DictionaryManager` + `SearchEngine`
    (live search-as-you-type) and (3) the OS accent/theme detection, then the
    dictionary-management panel.

  Pinned versions at design time: `slint` resolves to **1.16.1**. IBM Plex Sans TTFs
  live under the IBM/plex repo (note: not at `…/fonts/complete/ttf/` on `master` —
  that path 404s; find the current path when bundling).

- [x] **Phase 7 — Settings page (GUI).** A dedicated preferences surface, reachable
  from a **gear button** in the toolbar (overlay/sheet over the definition pane, so
  it doesn't disturb the search-as-you-type flow). This subsumes the
  "dictionary-management panel" promised in Phase 6 and the "settings" item folded
  into the old Polish phase.

  **Sections:**
  - **Dictionaries:** list every loaded dictionary with its **name + word count +
    enabled toggle**; **add** (native file picker for a `.ifo`), **remove**, and
    **enable/disable**. Writes back through `DictionaryManager::config()` →
    `Config::save`, and the **scope control rebuilds** from the new set. Each entry
    also exposes a **per-dictionary language** field (Auto / English / French / …) —
    see the Phase 8 tie-in below.
  - **Appearance:** **theme mode** (System / Light / Dark) overriding OS detection,
    and **accent** (System vs. a custom color). Persisted; applied live to the
    reactive Slint properties added in Phase 6.

  **Decisions:**
  - **File picker:** add **`rfd`** to `irondict-gui`. On Linux it uses the **XDG
    Desktop Portal** file chooser, so it works on the sway/Wayland dev box without a
    GTK dependency. Adding a dictionary therefore goes through the portal, not a
    bespoke path field.
  - **Persistence:** extend core **`Config`** with a `[preferences]` section
    (`theme_mode`, `accent_override`) so the setting lives next to the dictionary
    list and one front-end can't desync the other. `theme::detect_*` consult the
    override before falling back to OS detection (generalizes the temporary
    `IRONDICT_DARK` env override into a real, persisted setting).
  - **Index lifecycle:** add/remove/enable must **invalidate + rebuild** the tantivy
    index. Reuse the Phase 5 manifest-signature check and the Phase 6 background
    build pattern (worker thread + channel + polling `Timer`): keep serving the old
    index, show a "Reindexing…" state, then hot-swap the new `SearchEngine` in.

  **Phase 8 tie-in (per-dictionary language):** the StarDict `.ifo` doesn't reliably
  encode a language, so conjugation's language routing can't always rely on an
  auto-detected hint. This page is where the user pins it: a per-dictionary
  **language** setting (default **Auto**, which falls back to backend detection),
  persisted in the same `[[dictionaries]]` config entry (new optional `language`
  field). The conjugation registry (Phase 8) consults this pinned language first
  before trying backends, which resolves cross-language homographs (e.g. *important*)
  deterministically. Building this field now means Phase 8 has a real source of truth
  instead of guessing.

  **Implemented:**
  - **Core config** (`crates/core/src/config.rs`): new `Language` (`auto`/`en`/`fr`),
    `ThemeMode` (`system`/`light`/`dark`), and `Preferences { theme_mode, accent }`
    enums/struct; `DictionaryConfig` gained a `#[serde(default)] language` field (plus
    a `DictionaryConfig::new(path)` constructor so existing call sites and tests don't
    repeat field defaults). `Config` gained `preferences`. The **manager** now carries
    `Preferences` and per-dictionary `language`, round-tripped through `config()` /
    `from_config` (so a CLI `add` no longer clobbers GUI-set preferences), with
    `set_language` + `preferences()/preferences_mut()` accessors.
  - **Settings overlay** (`crates/gui/ui/app.slint`): gear button in the toolbar opens
    a centered panel over a dimmed backdrop (click-outside / ✕ to close). Dictionaries
    section = a scrollable list of cards (name + grouped word count, a language
    `ComboBox`, an enable `Switch`, and a remove ✕) plus an **Add dictionary…** button;
    Appearance section = a System/Light/Dark segmented control and an accent row (Auto
    + preset swatches). All driven by new properties (`dict-items: [DictRow]`,
    `theme-mode`, `accent-swatches`, `accent-choice`) and callbacks.
  - **Wiring** (`crates/gui/src/main.rs`): add uses the **`rfd`** portal file picker on
    a worker thread (path returned over a channel, applied on the UI thread by a
    polling timer); enable/disable/remove/add mutate the in-memory manager, persist via
    `Config::save`, refresh the scope control + list, and **rebuild the index in the
    background** (reusing the Phase 5 manifest signature + the Phase 6 engine channel,
    hot-swapping the new `SearchEngine` when ready). The scope control now lists only
    **enabled** dictionaries; `scope_filter` indexes into the enabled set. Theme/accent
    changes update `Preferences`, persist, and re-run `apply_appearance` (persisted
    override → else OS detection); `theme.rs` `apply_os_theme` now takes
    `forced_dark`/`forced_accent` overrides (the old `IRONDICT_DARK` env override is
    retained for dev) plus a `parse_hex` helper.
  - **rfd** added to `irondict-gui` with `default-features = false, features =
    ["xdg-portal", "async-std"]` so it uses the XDG portal (no GTK dep) on sway.
  - Verified: `cargo build`, `cargo clippy --all-targets`, `cargo fmt --check`, and the
    full test suite are clean on 1.96.0; the GUI launches and exits cleanly. The
    interactive file-picker dialog itself still wants a hands-on test on the target
    desktop.

- [x] **Phase 8 — Verb conjugation (English + French).** Given a verb headword,
  surface its conjugation. Conjugation is sourced **from the loaded dictionaries**
  (plus in-code grammar rules) — **no verb dataset is bundled** (no Verbiste, no
  irregular-verb table). The core API is **language-aware** so a third language is
  just another backend.

  **Core model** (`crates/core/src/conjugation.rs`) — general enough for both a tiny
  English set and a large French grid:
  - `Conjugation { language, infinitive, sections: Vec<ConjSection> }` where a
    `ConjSection { label, forms: Vec<ConjForm { label, text }> }` is one mood/tense
    (e.g. "Indicatif présent") holding its person-tagged forms. English collapses to
    a single section of principal parts; French expands to many.
  - A `Conjugator` trait (`fn conjugate(&self, headword, definition, force) ->
    Option<Conjugation>`, `fn language(&self) -> Language`) with one implementation
    per language, plus a `ConjugatorRegistry` that routes a lookup to the right
    backend.

  **Language routing:** prefer the **per-dictionary language** pinned in the Phase 7
  settings page (default **Auto**). A pinned language forces a best-effort table
  (`force = true`); under **Auto** every backend is tried with `force = false` and
  the first that recognizes the headword as a verb wins — so English's permissive
  rule generator can't shadow another language. The pinned setting disambiguates
  homographs deterministically.

  **English backend** (`conjugation/english.rs`) — two sources, highest-confidence
  first:
  1. **Parse GCIDE's inflection block.** Verb entries encode principal parts right
     after the POS, e.g. `\Go\ …, v. i. [imp. {Went}; p. p. {Gone}; p. pr. & vb. n.
     {Going}.]` — `imp.` = past, `p. p.` = past participle, `p. pr. & vb. n.` =
     present participle. These authoritative forms win (balance-matched brackets;
     `{…}` alternates joined only by `or`/comma so cross-references like `See {Wend}`
     aren't mistaken for forms).
  2. **Rule-based generator** for regular verbs GCIDE didn't annotate: 3rd-singular
     (`-s` / `-es` after sibilants / `y→ies`), past `-ed`, present participle `-ing`
     (final-consonant doubling, silent-`e` drop). The three suppletive present-tense
     verbs (be→is, have→has, do→does) are handled as a grammar rule, not a data file.

  **French backend** (`conjugation/french.rs`) — **parse from the loaded
  dictionary**, best-effort and conservative: it reads tense headings + person
  pronouns out of whatever conjugation content a French StarDict actually provides,
  and returns nothing for ordinary prose (requires ≥2 sections of ≥3 forms). A
  dictionary that only references a numbered conjugation model yields nothing —
  honest rather than fabricated. (A real generator could be added later, but only
  if it needs no bundled non-OSS data.)

  **Front-ends:**
  - **CLI:** `conjugate [--lang en|fr] <verb>` — looks the word up, then routes its
    definition(s) through the registry; prints the infinitive and each section's
    person-tagged forms. Language auto-detected when `--lang` is omitted.
  - **GUI:** when the displayed entry is a recognized verb, the definition pane shows
    a compact **"Conjugation ▸" button** (not an inline block, so it never crowds the
    body); clicking it opens a centered overlay (same chrome as Settings) with the
    full sections/forms. Hidden for non-verbs; closed by default and on navigation.

  **Implemented:** `crates/core/src/conjugation.rs` (model + `Conjugator` trait +
  `ConjugatorRegistry`), `conjugation/english.rs` (GCIDE block parser + spelling
  rules + suppletive present), `conjugation/french.rs` (conservative table parser),
  `conjugation/tests.rs` (9 tests over GCIDE-style fixtures + a French table). CLI
  `conjugate [--lang en|fr]`; GUI conjugation button + overlay.

- [ ] **Phase 9 — Polish.** Rich definition rendering (HTML/markup data types,
  including GCIDE's markup), search history, packaging of GCIDE.

## Critical files (to be created)

- `Cargo.toml` (root) — convert to `[workspace]`.
- `crates/core/src/{lib.rs, model.rs, stardict.rs, manager.rs, config.rs, search.rs}`
- `crates/core/src/conjugation.rs` + `conjugation/{english,french,tests}.rs` — Phase 8.
  No bundled verb data: English parses GCIDE + spelling rules; French parses the
  loaded dictionary's own conjugation content.
- `crates/cli/src/main.rs` — `clap` CLI.
- `crates/gui/` — `ui/ui.slint` (markup), `src/main.rs` (glue), `build.rs` (slint-build);
  a settings overlay component (Phase 7).
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
- **Phase 7:** open Settings, add a second StarDict via the file picker → it appears in
  the scope control and the index rebuilds, then scope-filtered lookups work across
  both; disable one → excluded; remove → gone; all of it survives a restart. Switch
  the theme between System/Light/Dark and confirm it applies and persists.
- **Phase 8:** English — `conjugate go` → went / gone / going (from GCIDE's
  inflection block), `conjugate walk` → walks / walked / walking (rule-based),
  `conjugate be` → is / was / been (suppletive present). A non-verb headword yields
  no table. French — with a loaded French dictionary that contains a conjugation
  table, `conjugate <verb> --lang fr` parses out its sections; a dictionary that only
  references a numbered model yields nothing (by design — no bundled verb data).
  Auto-detection picks the right language with `--lang` omitted; the GUI shows a
  "Conjugation" button on verb entries that opens the full tables in an overlay.

## Open questions (can defer)

- GCIDE bundling strategy (commit in-repo vs. download on first run) — decided in
  Phase 2 based on file size and GPLv3 redistribution.
- Config format: JSON vs TOML (lean TOML for human-editability).
- Verify the `stardict` crate's license on crates.io before depending on it
  (the GUI/search crates are MIT/Apache; Slint is royalty-free for desktop).
