# Plan — Conjugation companion integration (Phase 4)

Wire the French verb-conjugation StarDict
([DrPandemic/wikitionary-verb-dictionaries](https://github.com/DrPandemic/wikitionary-verb-dictionaries),
tag `v0.1.0`) into irondict so `conjugate manger` and the irregulars render the
full grid in **both** the CLI and the GUI.

The companion dictionary is a normal StarDict (35,468 French verbs,
`sametypesequence=h`), published as two release assets:
`fr-conj-dictzip.tar.zst` (~12 MB, small on disk) and `fr-conj-plain.tar.zst`
(~8.8 MB download, ~105 MB on disk). irondict installs the **dictzip** one.

## Where to download the dictionary

Published as GitHub release assets on
[DrPandemic/wikitionary-verb-dictionaries](https://github.com/DrPandemic/wikitionary-verb-dictionaries/releases)
(tag `v0.1.0`):

- **dictzip (what irondict uses):**
  `https://github.com/DrPandemic/wikitionary-verb-dictionaries/releases/latest/download/fr-conj-dictzip.tar.zst`
- plain `.dict` variant:
  `https://github.com/DrPandemic/wikitionary-verb-dictionaries/releases/latest/download/fr-conj-plain.tar.zst`
- checksums: append `.sha256` to either URL.
- pinned to a tag instead of `latest`: swap `latest/download` for
  `download/v0.1.0`.

The `latest/download/…` form is what the `CatalogEntry.url` in task 1 points at,
mirroring the xxyzz convention already used in `download.rs`. For local dev
before/without a release, the same archives are produced by the build repo at
`data/fr/release/fr-conj-dictzip.tar.zst` (run `verbdict fetch fr && verbdict
build fr && verbdict package fr`).

## Verified facts this plan builds on

- **Entry body shape** (one line, `<br>` between rows, no literal newlines):
  `<b>Indicatif présent</b><br>je mange<br>tu manges<br>…<b>Indicatif imparfait</b><br>…`
- **Both conjugate call sites pass RAW segment text** to `FrenchConjugator`
  with no HTML strip:
  - CLI: `crates/app/src/main.rs` (~line 246, joins segment `.text`).
  - GUI: `crates/app/src/gui.rs` (~line 1411, `compute_conjugation(&raw)`).
- `parse_sections` (`crates/core/src/conjugation/french.rs`) splits on
  `text.lines()` and matches headings with `starts_with`, person rows with
  `find(pronoun)`. With the body on **one line** and tags intact, it matches
  nothing — hence the strip below is required.
- `TENSE_LABELS` currently recognizes only `Indicatif présent` out of the
  companion's 22 labels.
- `CatalogEntry` (`crates/core/src/download.rs`) has fields
  `{ id, label, language, url, approx_size, license, source }`; the `entry!`
  macro hardcodes the xxyzz URL base, so it can't be reused for our asset.
- **GUI gap:** `lookup_raw` returns only the first/displayed dictionary's text,
  and `compute_conjugation` parses just that. The conjugation lives in a
  *separate* `fr-conj` entry, so the GUI won't show it when the main definition
  dict is on screen. The CLI already scans every dictionary via `find_map`.

## Tasks

### 1. Add the catalog entry (`crates/core/src/download.rs`)
- Add a **literal** `CatalogEntry` (the `entry!` macro can't be reused — different
  repo and asset name):
  - `id: "fr-conj"`
  - `label: "Conjugaison — Français"`
  - `language: Language::French` (pin so routing detects French)
  - `url:` `https://github.com/DrPandemic/wikitionary-verb-dictionaries/releases/latest/download/fr-conj-dictzip.tar.zst`
  - `approx_size: 12_000_000`
  - `license: "CC BY-SA 4.0"`
  - `source: "Wiktionary via kaikki.org / wiktextract"`
- No installer change needed: `install_dir`/`find_ifo` already handle arbitrary
  ids, and the `.dict.dz` + `.tar.zst` paths are validated end-to-end in the
  build repo's tests (ruzstd → tar → stardict).

### 2. Strip HTML → line-structured text before parsing (`crates/core`)
- Add a normalization step that turns the companion's HTML into the plain,
  one-row-per-line text `parse_sections` expects:
  - `<br>`, `<br/>`, `<br />` (any case) → `\n`
  - remove remaining tags (`<b>`, `</b>`, …)
  - decode the entities we emit: `&amp;`, `&lt;`, `&gt;`
- Put it in core so CLI and GUI both benefit. Cleanest spot: a helper applied at
  the top of `FrenchConjugator::conjugate` before `parse_sections`. Plain-text
  (GCIDE English) input is unaffected — it has no tags.
- Tests in `crates/core/src/conjugation/tests.rs`: fabricate a small HTML fixture
  (`<b>Indicatif présent</b><br>je mange<br>…`, **invented forms, never copied
  from a proprietary source**) and assert the grid parses.

### 3. Extend `TENSE_LABELS` (`crates/core/src/conjugation/french.rs`)
- Add the 22 companion labels (display order): Indicatif présent / imparfait /
  passé simple / futur simple / passé composé / plus-que-parfait / passé
  antérieur / futur antérieur; Subjonctif présent / imparfait / passé /
  plus-que-parfait; Conditionnel présent / passé; Impératif présent / passé;
  Infinitif présent / passé; Gérondif présent / passé; Participe présent / passé.
- `match_tense_label` returns the **first** `starts_with` hit, which can shadow
  (e.g. `Indicatif passé` is a prefix of three labels). Two safeguards:
  - Order full mood+tense labels **before** the existing bare fallbacks
    (`Présent`, `Subjonctif`, `Conditionnel`, `Futur`, `Impératif`, `Infinitif`).
  - **Recommended:** change `match_tense_label` to pick the **longest** matching
    needle instead of the first — removes prefix shadowing entirely. Guard with a
    test so existing dictionaries still parse.

### 4. GUI: source conjugation across all dictionaries (`crates/app/src/gui.rs`)
- The conjugation must be computed from **any** of the headword's dictionary
  entries, not only the displayed one (mirror the CLI's `find_map`).
- In `compute_page`, after `manager.lookup`, iterate every result's entries, run
  `ConjugatorRegistry::conjugate` per `(entry text, pinned language)`, and take
  the first match — independent of which entry's body is shown. Keep the visible
  body (the definition) unchanged; the conjugation panel is separate.
- Verify the panel appears when the on-screen source is the main French
  definition dict (not `fr-conj`).

### 5. Verify (CLI + GUI)
- Install the `fr-conj` entry via the in-app download flow (or point at the local
  `data/fr/release/fr-conj-dictzip.tar.zst` during dev).
- CLI: `irondict conjugate manger` → full grid. Spot-check irregulars
  (`aller`, `être`, `lire`, `avoir`, `faire`) and a compound (passé composé
  `j'ai mangé`).
- GUI: look up `manger` with the main French dict displayed → conjugation panel
  renders.
- Known, acceptable display nuances (optional polish, do not block): slash-person
  rows (`il/elle/on mange`) parse to person `on`; `j'ai mangé` parses label
  `j'ai`. Improve later if desired by special-casing in `match_person_form`.

### 6. Docs / memory
- Update the `conjugation-companion-html-contract` memory if the strip lands
  somewhere other than `FrenchConjugator::conjugate`.
- README: note the conjugation companion is downloadable.

## Sequencing
Tasks 2 + 3 make the **CLI** work (strip + label coverage). Then 1 (catalog) so
it's installable, then 4 (GUI sourcing) so the panel shows, then 5 (verify).
Tasks 1 and 3 are small and independent.

## Risks
- The `match_tense_label` longest-match change could shift parsing for existing
  dictionaries — cover with tests before/after.
- Install path (ruzstd / `.dict.dz`) is already validated in the build repo, so
  low risk here.

## Out of scope
- Other languages (the build repo is structured for them; the irondict side is
  generic once French works).
- Reflexive / spelling-variant cleanup in the data (deferred by decision — the
  dictionary keeps every verb and form as Wiktionary has them).
