# GCIDE — bundled dictionary (Phase 2)

The first preinstalled dictionary is **GCIDE**, the GNU Collaborative
International Dictionary of English (Webster's Revised Unabridged 1913 +
WordNet supplements). It ships in StarDict form under
`crates/core/assets/gcide/`.

## What is committed

```
crates/core/assets/gcide/
├── dictd_www.dict.org_gcide.ifo      # StarDict metadata
├── dictd_www.dict.org_gcide.idx      # headword index
└── dictd_www.dict.org_gcide.dict.dz  # dictzip-compressed definitions
```

- bookname: `dictd_www.dict.org_gcide`
- wordcount: 174222
- `sametypesequence=m` (plain-text definitions)
- StarDict `version=2.4.2`, source data dated 2003.05.13

## Provenance

We used a ready-made StarDict build rather than converting from source
(decided in Phase 2 — see `PLAN.md`).

- Source archive: `stardict-dictd_www.dict.org_gcide-2.4.2.tar.bz2`
- Mirror: `http://download.huzheng.org/dict.org/` (the StarDict community
  dictionary mirror)
- Downloaded: 2026-06-06
- Archive SHA-256:
  `7ce3072763a896897f2c4d23db88619ddf11f043b1f4ae58e764f1a861690537`

The build is derived from the `dictd` GCIDE database published by
`www.dict.org`, which in turn is the GNU GCIDE data (originally from
`ftp://ftp.gnu.org/gnu/dictionary`). The embedded `00-database-short`
entry identifies it as **"The Collaborative International Dictionary of
English v.0.44"**, prepared by MICRA, Inc.

To re-create the bundled files:

```sh
curl -O http://download.huzheng.org/dict.org/stardict-dictd_www.dict.org_gcide-2.4.2.tar.bz2
# verify the SHA-256 above, then:
tar xjf stardict-dictd_www.dict.org_gcide-2.4.2.tar.bz2
cp stardict-dictd_www.dict.org_gcide-2.4.2/dictd_www.dict.org_gcide.* \
   crates/core/assets/gcide/
```

### Authoritative source / fallback conversion

The authoritative GCIDE source lives at `https://ftp.gnu.org/gnu/gcide/`
(latest `gcide-0.54`, SGML/CIDE format). Converting it to StarDict
ourselves is the documented fallback if we ever need a newer version: use
the `dictd` toolchain (`dictfmt`/`dictzip`) to produce a dictd database,
then `stardict-tools` (`dictd2dic`) to produce the StarDict trio. That
toolchain is not currently installed and was not needed for this build.

## License

**GCIDE is licensed GPL-2.0-or-later.** The license notice embedded in the
data states:

> GCIDE is free software; you can redistribute it and/or modify it under
> the terms of the GNU General Public License as published by the Free
> Software Foundation; either version 2, or (at your option) any later
> version.

and the conversion notice adds: *"No additional restrictions are claimed.
Please redistribute this changed version under the same conditions and
restriction that apply to the original version."*

Note: this corrects the earlier assumption in `PLAN.md` that GCIDE is
GPLv3. The bundled v0.44 data is **GPL-2.0-or-later**. (Newer GNU `gcide`
releases, 0.51+, are GPLv3.) Because the data is GPL-2.0-**or-later**, it
may be redistributed under GPLv3, so bundling it inside this
GPL-3.0-or-later project is license-compatible.

## Verification

`crates/core/tests/gcide_test.rs` asserts the bundled dictionary loads
through the Phase 1 loader (`irondict_core::stardict::load`), reports the
expected bookname and wordcount, and resolves the headword `dictionary`.

```sh
cargo test -p irondict-core --test gcide_test
```
