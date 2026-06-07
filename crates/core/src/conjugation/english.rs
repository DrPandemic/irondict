//! English conjugation, sourced from GCIDE.
//!
//! GCIDE verb entries annotate their principal parts right after the
//! part-of-speech, in a bracketed inflection block, e.g.
//!
//! ```text
//! Go \Go\, v. i. [imp. {Went}; p. p. {Gone}; p. pr. & vb. n. {Going}. ...]
//! ```
//!
//! where `imp.` is the past, `p. p.` the past participle, and `p. pr. & vb. n.`
//! the present participle. Those authoritative forms win. Verbs GCIDE didn't
//! annotate (regulars like *stop*) fall back to in-code spelling rules. The
//! third-person-singular present is always rule-generated (GCIDE rarely lists it).

use super::{ConjForm, ConjSection, Conjugation, Conjugator};
use crate::config::Language;

/// Conjugates English verbs by reading GCIDE's inflection block, filling any
/// gaps with regular spelling rules.
#[derive(Debug, Default)]
pub struct EnglishConjugator;

impl EnglishConjugator {
    pub fn new() -> Self {
        Self
    }
}

impl Conjugator for EnglishConjugator {
    fn language(&self) -> Language {
        Language::English
    }

    fn conjugate(
        &self,
        headword: &str,
        definition: Option<&str>,
        force: bool,
    ) -> Option<Conjugation> {
        let base = headword.trim().to_lowercase();
        if base.is_empty() || base.contains(' ') {
            return None;
        }

        // Pull authoritative forms from GCIDE's inflection block, if present.
        let parsed = definition.and_then(parse_inflection_block);
        let is_verb = parsed.is_some() || definition.is_some_and(has_verb_pos);

        // During `Auto` routing (force == false) we only claim the word when the
        // dictionary shows it is a verb, so we don't shadow other languages.
        if !force && !is_verb {
            return None;
        }

        let parsed = parsed.unwrap_or_default();
        let third = regular_third_singular(&base);
        let past = parsed.past.unwrap_or_else(|| regular_past(&base));
        let past_part = parsed
            .past_participle
            .or_else(|| Some(regular_past(&base)))
            .unwrap();
        let pres_part = parsed
            .present_participle
            .unwrap_or_else(|| regular_present_participle(&base));

        let forms = vec![
            ConjForm::new("present (he/she/it)", third),
            ConjForm::new("past", past),
            ConjForm::new("past participle", past_part),
            ConjForm::new("present participle", pres_part),
        ];

        Some(Conjugation {
            language: Language::English,
            infinitive: base,
            sections: vec![ConjSection {
                label: "Principal parts".to_string(),
                forms,
            }],
        })
    }
}

/// The principal parts read out of a GCIDE inflection block.
#[derive(Debug, Default)]
struct ParsedParts {
    past: Option<String>,
    past_participle: Option<String>,
    present_participle: Option<String>,
}

/// Whether `definition` carries a verb part-of-speech marker (`v.`, `v. t.`,
/// `v. i.`, …). Used as weak evidence the headword is a verb.
fn has_verb_pos(definition: &str) -> bool {
    find_verb_pos(definition).is_some()
}

/// The byte index just after the first verb part-of-speech marker, if any.
fn find_verb_pos(text: &str) -> Option<usize> {
    const MARKERS: &[&str] = &[
        "v. t. & i.",
        "v. i. & t.",
        "v. t.",
        "v. i.",
        "v. impers.",
        ", v.",
    ];
    MARKERS
        .iter()
        .filter_map(|m| text.find(m).map(|i| i + m.len()))
        .min()
}

/// Parse GCIDE's inflection block for a verb's principal parts. Returns `None`
/// when the entry isn't a verb or has no annotated forms.
fn parse_inflection_block(definition: &str) -> Option<ParsedParts> {
    let pos_end = find_verb_pos(definition)?;
    let after = &definition[pos_end..];

    // Scan the bracketed blocks following the POS; the inflection block is the
    // first one that mentions a principal-part label. (The next block is usually
    // the etymology, which we skip.) Brackets nest — diacritics like `(w[e^]nt)`
    // appear inside — so we balance-match each block.
    let mut rest = after;
    while let Some(open) = rest.find('[') {
        let block_start = open + 1;
        let block = match balanced_bracket(&rest[block_start..]) {
            Some(end) => &rest[block_start..block_start + end],
            None => break,
        };
        // Advance past this block for the next iteration.
        let consumed = block_start + block.len() + 1;
        if block.contains("imp.") || block.contains("p. p.") || block.contains("p. pr.") {
            return Some(parse_parts(block));
        }
        rest = &rest[consumed.min(rest.len())..];
    }
    None
}

/// Given the text just after an opening `[`, return the byte length up to the
/// matching `]`, accounting for nested brackets. `None` if unbalanced.
fn balanced_bracket(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Pull the principal parts out of one inflection block's text.
fn parse_parts(block: &str) -> ParsedParts {
    let mut parts = ParsedParts::default();
    for seg in block.split(';') {
        let label = seg.split('{').next().unwrap_or("").to_lowercase();
        let forms = collect_forms(seg);
        if forms.is_empty() {
            continue;
        }
        let joined = forms.join(" or ");
        // `imp. & p. p.` supplies both past and past participle at once.
        if label.contains("imp.") {
            parts.past.get_or_insert_with(|| joined.clone());
        }
        if label.contains("p. p.") {
            parts.past_participle.get_or_insert_with(|| joined.clone());
        }
        if label.contains("p. pr.") {
            parts.present_participle.get_or_insert(joined);
        }
    }
    parts
}

/// Collect and clean the `{...}` forms at the start of a segment, e.g.
/// `{Ran}or {Run}` → `["ran", "run"]`. Only contiguous "or"/comma-joined
/// alternatives are taken; once the prose continues (as in `{Going}. ... See
/// {Wend}`) collection stops, so cross-references aren't mistaken for forms.
fn collect_forms(seg: &str) -> Vec<String> {
    let mut forms = Vec::new();
    let mut rest = seg;
    while let Some(open) = rest.find('{') {
        // Only the first form may follow arbitrary label text; later forms must
        // be joined to the previous one by an "or"/comma connector.
        if !forms.is_empty() {
            let gap = rest[..open].trim().to_lowercase();
            if !matches!(gap.as_str(), "" | "or" | "," | ", or" | "or,") {
                break;
            }
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        let form = clean_form(&after[..close]);
        if !form.is_empty() {
            forms.push(form);
        }
        rest = &after[close + 1..];
    }
    forms
}

/// Normalize a raw GCIDE form: drop bracketed diacritic codes, collapse
/// whitespace, lowercase.
fn clean_form(raw: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in raw.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

// --- Regular spelling rules ------------------------------------------------

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// Third-person-singular present: `-s`, `-es` after a sibilant/`o`, or `y→ies`.
///
/// English has exactly three suppletive present-tense verbs whose 3sg no
/// dictionary spells out inline; they are handled here as a grammar rule (not a
/// bundled verb list).
fn regular_third_singular(base: &str) -> String {
    match base {
        "be" => return "is".to_string(),
        "have" => return "has".to_string(),
        "do" => return "does".to_string(),
        _ => {}
    }
    let chars: Vec<char> = base.chars().collect();
    let n = chars.len();
    if n >= 2 && chars[n - 1] == 'y' && !is_vowel(chars[n - 2]) {
        return format!("{}ies", &base[..base.len() - 1]);
    }
    if base.ends_with('s')
        || base.ends_with('x')
        || base.ends_with('z')
        || base.ends_with('o')
        || base.ends_with("ch")
        || base.ends_with("sh")
    {
        return format!("{base}es");
    }
    format!("{base}s")
}

/// Regular past / past participle: `-ed`, `-d` after `e`, `y→ied`, or final
/// consonant doubling for monosyllabic CVC stems.
fn regular_past(base: &str) -> String {
    let chars: Vec<char> = base.chars().collect();
    let n = chars.len();
    if base.ends_with('e') {
        return format!("{base}d");
    }
    if n >= 2 && chars[n - 1] == 'y' && !is_vowel(chars[n - 2]) {
        return format!("{}ied", &base[..base.len() - 1]);
    }
    if let Some(doubled) = double_final_consonant(&chars) {
        return format!("{doubled}ed");
    }
    format!("{base}ed")
}

/// Present participle: `-ing`, dropping a silent `e`, `ie→ying`, or doubling a
/// monosyllabic CVC final consonant.
fn regular_present_participle(base: &str) -> String {
    let chars: Vec<char> = base.chars().collect();
    let n = chars.len();
    if let Some(stem) = base.strip_suffix("ie") {
        return format!("{stem}ying");
    }
    if base.ends_with('e') && !base.ends_with("ee") && n > 2 {
        return format!("{}ing", &base[..base.len() - 1]);
    }
    if let Some(doubled) = double_final_consonant(&chars) {
        return format!("{doubled}ing");
    }
    format!("{base}ing")
}

/// For a monosyllabic stem ending in consonant-vowel-consonant (final consonant
/// not `w`/`x`/`y`), return the stem with its final consonant doubled.
fn double_final_consonant(chars: &[char]) -> Option<String> {
    let n = chars.len();
    if n < 3 {
        return None;
    }
    let (c1, c2, c3) = (chars[n - 3], chars[n - 2], chars[n - 1]);
    let cvc = !is_vowel(c1) && is_vowel(c2) && !is_vowel(c3);
    let doublable = !matches!(c3, 'w' | 'x' | 'y');
    if cvc && doublable && is_monosyllabic(chars) {
        let mut s: String = chars.iter().collect();
        s.push(c3);
        Some(s)
    } else {
        None
    }
}

/// Rough syllable count via vowel groups; doubling only applies to monosyllables.
fn is_monosyllabic(chars: &[char]) -> bool {
    let mut groups = 0;
    let mut in_vowel = false;
    for &c in chars {
        if is_vowel(c) {
            if !in_vowel {
                groups += 1;
            }
            in_vowel = true;
        } else {
            in_vowel = false;
        }
    }
    groups <= 1
}
