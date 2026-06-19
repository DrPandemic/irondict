//! English conjugation — generated periphrastic grid from a bundled
//! irregular-verb table and regular spelling rules.
//!
//! English has only four inflected principal parts: 3sg present (`goes`),
//! simple past (`went`), past participle (`gone`), present participle
//! (`going`).  The entire rest of the grid (`I have gone`, `he would have
//! been working`) is **periphrastic** — mechanical combinations of auxiliary
//! + non-finite form. This module generates the full grid in-code from the
//! principal parts.

use super::{ConjForm, ConjSection, Conjugation, Conjugator};
use crate::config::Language;

/// Conjugates English verbs from an in-code irregular table and regular
/// spelling rules, producing the full periphrastic grid.
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

        let in_irregular = irregular_verb(&base).is_some();
        let has_pos = definition.is_some_and(has_verb_pos);
        let is_verb = in_irregular || has_pos;

        if !force && !is_verb {
            return None;
        }

        let (irr_past, irr_past_part) = irregular_verb(&base).unwrap_or(("", ""));
        let is_be = base == "be";

        let third = regular_third_singular(&base);
        let pres_part = regular_present_participle(&base);

        let past = if irr_past.is_empty() {
            regular_past(&base)
        } else {
            irr_past.to_string()
        };

        // An empty irregular past participle means it coincides with the past
        // (e.g. `bend` → bent/bent, `buy` → bought/bought); regular verbs reach
        // the same fall-through since their `past` is already the regular form.
        let past_part = if irr_past_part.is_empty() {
            past.clone()
        } else {
            irr_past_part.to_string()
        };

        let sections = build_grid(&base, &third, &past, &past_part, &pres_part, is_be);

        Some(Conjugation {
            language: Language::English,
            infinitive: base,
            sections,
        })
    }
}

// --- Grid builder ------------------------------------------------------------

const PERSONS: &[&str] = &["I", "you", "he/she/it", "we", "they"];

fn build_grid(
    base: &str,
    third: &str,
    past: &str,
    past_part: &str,
    pres_part: &str,
    is_be: bool,
) -> Vec<ConjSection> {
    let mut s = Vec::with_capacity(17);

    // -- Indicative (12 sections) ------------------------------------------

    // Simple present
    s.push(person_section("Indicative present", |p| {
        if is_be {
            aux_be_present(p).to_string()
        } else if p == "he/she/it" {
            third.to_string()
        } else {
            base.to_string()
        }
    }));

    // Simple past
    s.push(person_section("Indicative past", |p| {
        if is_be {
            aux_be_past(p).to_string()
        } else {
            past.to_string()
        }
    }));

    // Simple future
    s.push(fixed_aux_section("Indicative future", "will", base));

    // Present continuous
    s.push(varying_aux_section(
        "Indicative present continuous",
        aux_be_present,
        pres_part,
    ));

    // Past continuous
    s.push(varying_aux_section(
        "Indicative past continuous",
        aux_be_past,
        pres_part,
    ));

    // Future continuous
    s.push(fixed_aux_section(
        "Indicative future continuous",
        "will be",
        pres_part,
    ));

    // Present perfect
    s.push(varying_aux_section(
        "Indicative present perfect",
        aux_have,
        past_part,
    ));

    // Past perfect
    s.push(fixed_aux_section(
        "Indicative past perfect",
        "had",
        past_part,
    ));

    // Future perfect
    s.push(fixed_aux_section(
        "Indicative future perfect",
        "will have",
        past_part,
    ));

    // Present perfect continuous
    s.push(varying_aux_section(
        "Indicative present perfect continuous",
        |p| format!("{} been", aux_have(p)),
        pres_part,
    ));

    // Past perfect continuous
    s.push(fixed_aux_section(
        "Indicative past perfect continuous",
        "had been",
        pres_part,
    ));

    // Future perfect continuous
    s.push(fixed_aux_section(
        "Indicative future perfect continuous",
        "will have been",
        pres_part,
    ));

    // -- Conditional (4 sections) -------------------------------------------

    s.push(fixed_aux_section("Conditional", "would", base));
    s.push(fixed_aux_section(
        "Conditional continuous",
        "would be",
        pres_part,
    ));
    s.push(fixed_aux_section(
        "Conditional perfect",
        "would have",
        past_part,
    ));
    s.push(fixed_aux_section(
        "Conditional perfect continuous",
        "would have been",
        pres_part,
    ));

    // -- Non-finite ---------------------------------------------------------

    s.push(ConjSection {
        label: "Non-finite".to_string(),
        forms: vec![
            ConjForm::new("infinitive", format!("to {base}")),
            ConjForm::new("present participle", pres_part.to_string()),
            ConjForm::new("past participle", past_part.to_string()),
        ],
    });

    s
}

fn person_section(label: &str, text_for: impl Fn(&str) -> String) -> ConjSection {
    ConjSection {
        label: label.to_string(),
        forms: PERSONS
            .iter()
            .map(|&p| ConjForm::new(p, text_for(p)))
            .collect(),
    }
}

fn varying_aux_section<S: AsRef<str>>(
    label: &str,
    aux: impl Fn(&str) -> S,
    main: &str,
) -> ConjSection {
    person_section(label, |p| format!("{} {main}", aux(p).as_ref()))
}

fn fixed_aux_section(label: &str, aux: &str, main: &str) -> ConjSection {
    ConjSection {
        label: label.to_string(),
        forms: PERSONS
            .iter()
            .map(|&p| ConjForm::new(p, format!("{aux} {main}")))
            .collect(),
    }
}

// --- Auxiliary tables -------------------------------------------------------

fn aux_be_present(person: &str) -> &'static str {
    match person {
        "I" => "am",
        "he/she/it" => "is",
        _ => "are",
    }
}

fn aux_be_past(person: &str) -> &'static str {
    match person {
        "I" | "he/she/it" => "was",
        _ => "were",
    }
}

fn aux_have(person: &str) -> &'static str {
    match person {
        "he/she/it" => "has",
        _ => "have",
    }
}

// --- Irregular-verb table ---------------------------------------------------
//
// Maps base → (past, past_participle).  Only ~200 irregulars; everything else
// is covered by the regular spelling rules.  The past participle may be empty
// when it coincides with the past form — the caller falls back to the regular
// past (which is also the regular past participle).

fn irregular_verb(base: &str) -> Option<(&'static str, &'static str)> {
    let v = match base {
        "arise" => ("arose", "arisen"),
        "awake" => ("awoke", "awoken"),
        "be" => ("was", "been"),
        "bear" => ("bore", "borne"),
        "beat" => ("beat", "beaten"),
        "become" => ("became", "become"),
        "begin" => ("began", "begun"),
        "bend" => ("bent", ""),
        "bet" => ("bet", ""),
        "bid" => ("bid", ""),
        "bind" => ("bound", ""),
        "bite" => ("bit", "bitten"),
        "bleed" => ("bled", ""),
        "blow" => ("blew", "blown"),
        "break" => ("broke", "broken"),
        "breed" => ("bred", ""),
        "bring" => ("brought", ""),
        "broadcast" => ("broadcast", ""),
        "build" => ("built", ""),
        "burn" => ("burnt", ""),
        "burst" => ("burst", ""),
        "buy" => ("bought", ""),
        "cast" => ("cast", ""),
        "catch" => ("caught", ""),
        "choose" => ("chose", "chosen"),
        "cling" => ("clung", ""),
        "come" => ("came", "come"),
        "cost" => ("cost", ""),
        "creep" => ("crept", ""),
        "cut" => ("cut", ""),
        "deal" => ("dealt", ""),
        "dig" => ("dug", ""),
        "dive" => ("dove", ""),
        "do" => ("did", "done"),
        "draw" => ("drew", "drawn"),
        "dream" => ("dreamt", ""),
        "drink" => ("drank", "drunk"),
        "drive" => ("drove", "driven"),
        "dwell" => ("dwelt", ""),
        "eat" => ("ate", "eaten"),
        "fall" => ("fell", "fallen"),
        "feed" => ("fed", ""),
        "feel" => ("felt", ""),
        "fight" => ("fought", ""),
        "find" => ("found", ""),
        "flee" => ("fled", ""),
        "fling" => ("flung", ""),
        "fly" => ("flew", "flown"),
        "forbid" => ("forbade", "forbidden"),
        "forecast" => ("forecast", ""),
        "foresee" => ("foresaw", "foreseen"),
        "forget" => ("forgot", "forgotten"),
        "forgive" => ("forgave", "forgiven"),
        "forsake" => ("forsook", "forsaken"),
        "freeze" => ("froze", "frozen"),
        "get" => ("got", "gotten"),
        "give" => ("gave", "given"),
        "go" => ("went", "gone"),
        "grind" => ("ground", ""),
        "grow" => ("grew", "grown"),
        "hang" => ("hung", ""),
        "have" => ("had", ""),
        "hear" => ("heard", ""),
        "hide" => ("hid", "hidden"),
        "hit" => ("hit", ""),
        "hold" => ("held", ""),
        "hurt" => ("hurt", ""),
        "keep" => ("kept", ""),
        "kneel" => ("knelt", ""),
        "knit" => ("knit", ""),
        "know" => ("knew", "known"),
        "lay" => ("laid", ""),
        "lead" => ("led", ""),
        "lean" => ("leant", ""),
        "leap" => ("leapt", ""),
        "learn" => ("learnt", ""),
        "leave" => ("left", ""),
        "lend" => ("lent", ""),
        "let" => ("let", ""),
        "lie" => ("lay", "lain"),
        "light" => ("lit", ""),
        "lose" => ("lost", ""),
        "make" => ("made", ""),
        "mean" => ("meant", ""),
        "meet" => ("met", ""),
        "mistake" => ("mistook", "mistaken"),
        "misunderstand" => ("misunderstood", ""),
        "overcome" => ("overcame", "overcome"),
        "overtake" => ("overtook", "overtaken"),
        "pay" => ("paid", ""),
        "plead" => ("pled", ""),
        "prove" => ("proved", "proven"),
        "put" => ("put", ""),
        "quit" => ("quit", ""),
        "read" => ("read", ""),
        "repay" => ("repaid", ""),
        "ride" => ("rode", "ridden"),
        "ring" => ("rang", "rung"),
        "rise" => ("rose", "risen"),
        "run" => ("ran", "run"),
        "saw" => ("sawed", "sawn"),
        "say" => ("said", ""),
        "see" => ("saw", "seen"),
        "seek" => ("sought", ""),
        "sell" => ("sold", ""),
        "send" => ("sent", ""),
        "set" => ("set", ""),
        "sew" => ("sewed", "sewn"),
        "shake" => ("shook", "shaken"),
        "shear" => ("sheared", "shorn"),
        "shed" => ("shed", ""),
        "shine" => ("shone", ""),
        "shoot" => ("shot", ""),
        "show" => ("showed", "shown"),
        "shrink" => ("shrank", "shrunk"),
        "shut" => ("shut", ""),
        "sing" => ("sang", "sung"),
        "sink" => ("sank", "sunk"),
        "sit" => ("sat", ""),
        "slay" => ("slew", "slain"),
        "sleep" => ("slept", ""),
        "slide" => ("slid", ""),
        "sling" => ("slung", ""),
        "slit" => ("slit", ""),
        "smell" => ("smelt", ""),
        "sow" => ("sowed", "sown"),
        "speak" => ("spoke", "spoken"),
        "speed" => ("sped", ""),
        "spell" => ("spelt", ""),
        "spend" => ("spent", ""),
        "spill" => ("spilt", ""),
        "spin" => ("spun", ""),
        "spit" => ("spat", ""),
        "split" => ("split", ""),
        "spoil" => ("spoilt", ""),
        "spread" => ("spread", ""),
        "spring" => ("sprang", "sprung"),
        "stand" => ("stood", ""),
        "steal" => ("stole", "stolen"),
        "stick" => ("stuck", ""),
        "sting" => ("stung", ""),
        "stink" => ("stank", "stunk"),
        "stride" => ("strode", "stridden"),
        "strike" => ("struck", ""),
        "string" => ("strung", ""),
        "strive" => ("strove", "striven"),
        "swear" => ("swore", "sworn"),
        "sweep" => ("swept", ""),
        "swell" => ("swelled", "swollen"),
        "swim" => ("swam", "swum"),
        "swing" => ("swung", ""),
        "take" => ("took", "taken"),
        "teach" => ("taught", ""),
        "tear" => ("tore", "torn"),
        "tell" => ("told", ""),
        "think" => ("thought", ""),
        "throw" => ("threw", "thrown"),
        "thrust" => ("thrust", ""),
        "tread" => ("trod", "trodden"),
        "undergo" => ("underwent", "undergone"),
        "understand" => ("understood", ""),
        "undertake" => ("undertook", "undertaken"),
        "undo" => ("undid", "undone"),
        "upset" => ("upset", ""),
        "wake" => ("woke", "woken"),
        "wear" => ("wore", "worn"),
        "weave" => ("wove", "woven"),
        "wed" => ("wed", ""),
        "weep" => ("wept", ""),
        "wet" => ("wet", ""),
        "win" => ("won", ""),
        "wind" => ("wound", ""),
        "withdraw" => ("withdrew", "withdrawn"),
        "withhold" => ("withheld", ""),
        "withstand" => ("withstood", ""),
        "wring" => ("wrung", ""),
        "write" => ("wrote", "written"),
        _ => return None,
    };
    Some(v)
}

// --- Verb part-of-speech detection ------------------------------------------

/// Whether `definition` carries a verb part-of-speech marker (`v.`, `v. t.`,
/// `v. i.`, …).  Used as weak evidence the headword is a verb during `Auto`
/// routing, so English doesn't shadow other languages for non-verbs.
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

// --- Regular spelling rules -------------------------------------------------

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// Third-person-singular present: `-s`, `-es` after a sibilant/`o`, or `y→ies`.
///
/// English has exactly three suppletive present-tense verbs whose 3sg no
/// dictionary spells out inline; they are handled here as a grammar rule.
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
