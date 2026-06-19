//! Shared parsing for companion conjugation tables.
//!
//! The French and Italian companions both render the same shape — a bold
//! tense/mood heading followed by `<br>`-separated, subject-pronoun-prefixed
//! person forms. Only the recognized headings, the pronoun set, and the accent
//! folding differ per language, so the parsing lives here and each language
//! supplies a [`TableSpec`].

use super::{ConjForm, ConjSection};

/// A language's companion-table shape: the tense/mood headings it recognizes
/// (matched longest-first to avoid prefix shadowing), the subject pronouns that
/// introduce a person form, and an accent/case folder used so headings match
/// regardless of how the dictionary wrote them.
pub(super) struct TableSpec {
    pub tense_labels: &'static [&'static str],
    pub pronouns: &'static [&'static str],
    pub fold: fn(&str) -> String,
}

impl TableSpec {
    /// Parse a companion entry's `definition` into ordered tense sections,
    /// keeping only the sections that collected at least one person form.
    pub fn parse(&self, definition: &str) -> Vec<ConjSection> {
        let text = html_to_lines(definition);
        let mut sections = Vec::new();
        let mut current: Option<ConjSection> = None;

        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(label) = self.match_tense_label(line) {
                if let Some(sec) = current.take() {
                    push_section(&mut sections, sec);
                }
                current = Some(ConjSection {
                    label,
                    forms: Vec::new(),
                });
                // A heading may share its line with the first form ("Présent: je …").
                if let Some(form) = self.match_person_form(line) {
                    if let Some(sec) = current.as_mut() {
                        sec.forms.push(form);
                    }
                }
            } else if let Some(form) = self.match_person_form(line) {
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

    /// If `line` begins with a known tense heading, return its display label.
    /// Picks the **longest** matching label so e.g. `Indicatif passé composé`
    /// wins over `Indicatif passé`.
    fn match_tense_label(&self, line: &str) -> Option<String> {
        let lower = (self.fold)(&line.to_lowercase());
        self.tense_labels
            .iter()
            .filter(|label| lower.starts_with(&(self.fold)(&label.to_lowercase())))
            .max_by_key(|label| label.len())
            .map(|label| (*label).to_string())
    }

    /// If `line` contains a subject pronoun + form, return it as a [`ConjForm`].
    fn match_person_form(&self, line: &str) -> Option<ConjForm> {
        let lower = line.to_lowercase();
        for pron in self.pronouns {
            if let Some(pos) = lower.find(pron) {
                let rest = line[pos..].trim();
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
}

/// Guard against false positives: a real conjugation table has several tenses,
/// each with multiple person forms. Returns true when at least two sections have
/// three or more forms.
pub(super) fn is_solid_table(sections: &[ConjSection]) -> bool {
    sections
        .iter()
        .filter(|s| s.forms.len() >= 3)
        .take(2)
        .count()
        >= 2
}

fn push_section(sections: &mut Vec<ConjSection>, sec: ConjSection) {
    if !sec.forms.is_empty() {
        sections.push(sec);
    }
}

/// Normalize a companion's HTML body into the plain, one-row-per-line text the
/// parser expects. Plain-text input (no `<`) is returned unchanged.
pub(super) fn html_to_lines(input: &str) -> String {
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
            if inner.eq_ignore_ascii_case("br")
                || inner.eq_ignore_ascii_case("br/")
                || inner.eq_ignore_ascii_case("br /")
            {
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
