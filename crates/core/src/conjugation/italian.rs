//! Italian conjugation, parsed from the loaded dictionary.
//!
//! Like French (see [`super::french`]), Italian conjugation is whatever the
//! loaded dictionary spells out — there is no bundled verb dataset. The
//! `it-conj` companion lists the full mood × tense × person grid (rendered by
//! the `wiktionary-dictionaries` pipeline as `<b>Indicativo presente</b><br>io
//! parlo<br>…`); a general definition dictionary yields nothing here.
//!
//! The parser recognizes Italian tense/mood headings each followed by
//! pronoun-tagged forms, and only returns a result when it finds at least two
//! such blocks with several persons each, so ordinary prose isn't mistaken for a
//! table.

use super::table::{is_solid_table, TableSpec};
use super::{Conjugation, Conjugator};
use crate::config::Language;

/// Italian tense/mood headings we recognize. The full `Mood tense` labels the
/// `it-conj` companion emits come first; bare fallbacks follow for any other
/// Italian dictionary that lists a table. Matched longest-first.
const TENSE_LABELS: &[&str] = &[
    "Indicativo presente",
    "Indicativo imperfetto",
    "Indicativo passato remoto",
    "Indicativo futuro semplice",
    "Congiuntivo presente",
    "Congiuntivo imperfetto",
    "Condizionale presente",
    "Imperativo",
    "Infinito",
    "Gerundio",
    "Participio presente",
    "Participio passato",
    // Bare fallbacks (non-companion dictionaries):
    "Indicativo",
    "Congiuntivo",
    "Condizionale",
    "Presente",
    "Imperfetto",
    "Passato remoto",
    "Futuro",
];

/// Italian subject pronouns that introduce a conjugated form on a line. Includes
/// the alternants a dictionary might use for the third persons.
const PRONOUNS: &[&str] = &[
    "io ", "tu ", "egli ", "ella ", "lui ", "lei ", "esso ", "essa ", "noi ", "voi ", "essi ",
    "esse ", "loro ",
];

/// Parses Italian conjugation out of a dictionary entry's text.
#[derive(Debug, Default)]
pub struct ItalianConjugator;

impl ItalianConjugator {
    pub fn new() -> Self {
        Self
    }
}

impl Conjugator for ItalianConjugator {
    fn language(&self) -> Language {
        Language::Italian
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

        // `_force` is irrelevant: we can only show what the dictionary contains,
        // so the solid-table guard decides either way.
        if !is_solid_table(&sections) {
            return None;
        }

        Some(Conjugation {
            language: Language::Italian,
            infinitive: base,
            sections,
        })
    }
}

/// Lowercase ASCII-fold Italian accents so headings match regardless of how the
/// dictionary cased or accented them. Input may be NFC or NFD; combining marks
/// are stripped so `e\u{0301}` and `\u{00E8}` both fold to `e`.
fn strip_accents(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(*c, '\u{0300}'..='\u{036F}'))
        .map(|c| match c {
            'à' | 'á' => 'a',
            'è' | 'é' => 'e',
            'ì' | 'í' => 'i',
            'ò' | 'ó' => 'o',
            'ù' | 'ú' => 'u',
            other => other,
        })
        .collect()
}
