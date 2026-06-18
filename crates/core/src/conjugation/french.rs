//! French conjugation, parsed from the loaded dictionary.
//!
//! We do **not** bundle a verb dataset or a generator: French conjugation is
//! whatever the loaded French dictionary itself spells out. General dictionaries
//! (e.g. Le Petit Robert) only reference a numbered conjugation *model* in their
//! appendix, so they yield nothing here; a dedicated conjugation StarDict that
//! lists the full grid does.
//!
//! The parser looks for known tense headings (`Présent`, `Imparfait`, `Futur`,
//! …) each followed by person-tagged forms (`je …`, `tu …`, `il …`). To avoid
//! mistaking ordinary prose for a conjugation table it only returns a result
//! when it finds at least two such tense blocks with several persons each.

use super::table::{is_solid_table, TableSpec};
use super::{Conjugation, Conjugator};
use crate::config::Language;

/// French tense/mood headings we recognize, normalized to lowercase without
/// accents for matching. The display label keeps the proper accented form.
/// Full mood+tense labels from the conjugation companion come first (24: the
/// 22 companion labels + the 2 already-here `Indicatif présent` and
/// `Subjonctif imparfait`), followed by bare fallbacks for non-companion
/// dictionaries.
const TENSE_LABELS: &[&str] = &[
    "Indicatif présent",
    "Indicatif imparfait",
    "Indicatif passé simple",
    "Indicatif futur simple",
    "Indicatif passé composé",
    "Indicatif plus-que-parfait",
    "Indicatif passé antérieur",
    "Indicatif futur antérieur",
    "Subjonctif présent",
    "Subjonctif imparfait",
    "Subjonctif passé",
    "Subjonctif plus-que-parfait",
    "Conditionnel présent",
    "Conditionnel passé",
    "Impératif présent",
    "Impératif passé",
    "Infinitif présent",
    "Infinitif passé",
    "Gérondif présent",
    "Gérondif passé",
    "Participe présent",
    "Participe passé",
    // Bare fallbacks (non-companion dictionaries):
    "Présent",
    "Imparfait",
    "Passé simple",
    "Passé composé",
    "Futur simple",
    "Futur",
    "Conditionnel",
    "Subjonctif",
    "Impératif",
    "Infinitif",
];

/// French subject pronouns that introduce a conjugated form on a line.
const PRONOUNS: &[&str] = &[
    "je ", "j'", "tu ", "il ", "elle ", "on ", "nous ", "vous ", "ils ", "elles ",
];

/// Parses French conjugation out of a dictionary entry's text.
#[derive(Debug, Default)]
pub struct FrenchConjugator;

impl FrenchConjugator {
    pub fn new() -> Self {
        Self
    }
}

impl Conjugator for FrenchConjugator {
    fn language(&self) -> Language {
        Language::French
    }

    fn conjugate(
        &self,
        headword: &str,
        definition: Option<&str>,
        _force: bool,
    ) -> Option<Conjugation> {
        let base = headword.trim().to_lowercase();
        let definition = definition?;
        let spec = TableSpec {
            tense_labels: TENSE_LABELS,
            pronouns: PRONOUNS,
            fold: strip_accents,
        };
        let sections = spec.parse(definition);

        // `_force` is irrelevant here — we can only show what the dictionary
        // actually contains, so the solid-table guard decides either way.
        if !is_solid_table(&sections) {
            return None;
        }

        Some(Conjugation {
            language: Language::French,
            infinitive: base,
            sections,
        })
    }
}

/// Lowercase ASCII-fold common French accents so headings match regardless of
/// how the dictionary cased or accented them. The input may be in either NFC
/// (precomposed) or NFD (decomposed) form; combining marks are stripped so
/// that `e\u{0301}` and `\u{00E9}` both fold to `e`.
fn strip_accents(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(*c, '\u{0300}'..='\u{036F}'))
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'û' | 'ü' | 'ù' => 'u',
            'ç' => 'c',
            other => other,
        })
        .collect()
}
