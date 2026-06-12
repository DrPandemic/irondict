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
        let text = html_to_lines(definition);
        let sections = parse_sections(&text);

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
/// Picks the **longest** matching label to avoid prefix shadowing (e.g.
/// `Indicatif passé` matching before `Indicatif passé composé`).
fn match_tense_label(line: &str) -> Option<String> {
    let lower = strip_accents(&line.to_lowercase());
    TENSE_LABELS
        .iter()
        .filter(|label| {
            let needle = strip_accents(&label.to_lowercase());
            lower.starts_with(&needle)
        })
        .max_by_key(|label| label.len())
        .map(|label| (*label).to_string())
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

/// Normalize the conjugation companion's HTML body into the plain,
/// one-row-per-line text that [`parse_sections`] expects. Plain-text input
/// (e.g. existing line-structured data) is unaffected.
fn html_to_lines(input: &str) -> String {
    if !input.contains('<') {
        return input.to_string();
    }
    let mut result = String::with_capacity(input.len());
    let mut i = 0;
    let bytes = input.as_bytes();
    let len = bytes.len();

    while i < len {
        if bytes[i] == b'<' {
            let tag_end = match input[i..].find('>') {
                Some(pos) => i + pos,
                None => {
                    result.push('<');
                    i += 1;
                    continue;
                }
            };
            let inner = input[i + 1..tag_end].trim();
            if inner.eq_ignore_ascii_case("br") || inner.eq_ignore_ascii_case("br/") || inner.eq_ignore_ascii_case("br /") {
                result.push('\n');
            }
            i = tag_end + 1;
        } else if bytes[i] == b'&' {
            if input[i..].starts_with("&amp;") {
                result.push('&');
                i += 5;
            } else if input[i..].starts_with("&lt;") {
                result.push('<');
                i += 4;
            } else if input[i..].starts_with("&gt;") {
                result.push('>');
                i += 4;
            } else {
                result.push('&');
                i += 1;
            }
        } else {
            let c = input[i..].chars().next().unwrap();
            result.push(c);
            i += c.len_utf8();
        }
    }
    result
}
