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

use super::{ConjForm, ConjSection, Conjugation, Conjugator};
use crate::config::Language;

/// French tense/mood headings we recognize, normalized to lowercase without
/// accents for matching. The display label keeps the proper accented form.
const TENSE_LABELS: &[&str] = &[
    "Indicatif présent",
    "Présent",
    "Imparfait",
    "Passé simple",
    "Passé composé",
    "Futur simple",
    "Futur",
    "Conditionnel présent",
    "Conditionnel",
    "Subjonctif présent",
    "Subjonctif imparfait",
    "Subjonctif",
    "Impératif",
    "Participe présent",
    "Participe passé",
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
        let sections = parse_sections(definition);

        // Guard against false positives: a real conjugation table has several
        // tenses, each with multiple person forms. `_force` is irrelevant here —
        // we can only show what the dictionary actually contains.
        let solid = sections
            .iter()
            .filter(|s| s.forms.len() >= 3)
            .take(2)
            .count();
        if solid < 2 {
            return None;
        }

        Some(Conjugation {
            language: Language::French,
            infinitive: base,
            sections,
        })
    }
}

/// Split `text` into tense sections at recognized headings, collecting the
/// person-tagged forms under each.
fn parse_sections(text: &str) -> Vec<ConjSection> {
    let mut sections = Vec::new();
    let mut current: Option<ConjSection> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(label) = match_tense_label(line) {
            if let Some(sec) = current.take() {
                push_section(&mut sections, sec);
            }
            current = Some(ConjSection {
                label,
                forms: Vec::new(),
            });
            // A heading may share its line with the first form ("Présent: je …").
            if let Some(form) = match_person_form(line) {
                if let Some(sec) = current.as_mut() {
                    sec.forms.push(form);
                }
            }
        } else if let Some(form) = match_person_form(line) {
            if let Some(sec) = current.as_mut() {
                sec.forms.push(form);
            }
        }
    }
    if let Some(sec) = current.take() {
        push_section(&mut sections, sec);
    }
    sections
}

fn push_section(sections: &mut Vec<ConjSection>, sec: ConjSection) {
    if !sec.forms.is_empty() {
        sections.push(sec);
    }
}

/// If `line` begins with a known tense heading, return its display label.
fn match_tense_label(line: &str) -> Option<String> {
    let lower = strip_accents(&line.to_lowercase());
    for label in TENSE_LABELS {
        let needle = strip_accents(&label.to_lowercase());
        if lower.starts_with(&needle) {
            return Some((*label).to_string());
        }
    }
    None
}

/// If `line` contains a subject pronoun + form, return it as a [`ConjForm`].
fn match_person_form(line: &str) -> Option<ConjForm> {
    let lower = line.to_lowercase();
    for pron in PRONOUNS {
        if let Some(pos) = lower.find(pron) {
            let rest = line[pos..].trim();
            // Need an actual form after the pronoun.
            let words: Vec<&str> = rest.split_whitespace().collect();
            if words.len() >= 2 {
                let label = words[0].trim_end_matches('\'').to_string();
                let text = words[1..].join(" ");
                return Some(ConjForm::new(label, text));
            }
        }
    }
    None
}

/// Lowercase ASCII-fold common French accents so headings match regardless of
/// how the dictionary cased or accented them.
fn strip_accents(s: &str) -> String {
    s.chars()
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
