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

        // Defective modals have no participle or infinitive and don't fit the
        // periphrastic grid; never emit a (wrong) conjugation for them, even
        // when forced.  Modals that double as ordinary lexical verbs (`can`
        // "to can food", `will` "to bequeath") are intentionally excluded.
        if is_defective_modal(&base) {
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
// Maps base → (past, past_participle).  Everything not listed is covered by the
// regular spelling rules.  The past participle may be empty when it coincides
// with the past form — the caller falls back to the regular past (which is also
// the regular past participle).
//
// The irregular forms are derived from the English Wiktionary "English irregular
// verbs" category via kaikki.org / wiktextract, and are therefore licensed
// CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/) —
// © Wiktionary contributors.  Regular twins, defective modals, and archaic-only
// entries were filtered out; see the project's data-attribution notes.

fn irregular_verb(base: &str) -> Option<(&'static str, &'static str)> {
    let v = match base {
        "abide" => ("abode", ""),
        "arise" => ("arose", "arisen"),
        "awake" => ("awoke", "awoken"),
        "babysit" => ("babysat", ""),
        "backslide" => ("backslid", "backslidden"),
        "be" => ("was", "been"),
        "bear" => ("bore", "borne"),
        "beat" => ("beat", "beaten"),
        "become" => ("became", "become"),
        "befall" => ("befell", "befallen"),
        "beget" => ("begot", "begotten"),
        "begin" => ("began", "begun"),
        "behold" => ("beheld", ""),
        "bend" => ("bent", ""),
        "beset" => ("beset", ""),
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
        "browbeat" => ("browbeat", "browbeaten"),
        "build" => ("built", ""),
        "burn" => ("burnt", ""),
        "burst" => ("burst", ""),
        "buy" => ("bought", ""),
        "cast" => ("cast", ""),
        "catch" => ("caught", ""),
        "chide" => ("chid", ""),
        "choose" => ("chose", "chosen"),
        "clad" => ("clad", ""),
        "cleave" => ("cleft", ""),
        "cling" => ("clung", ""),
        "come" => ("came", "come"),
        "cost" => ("cost", ""),
        "creep" => ("crept", ""),
        "crossbreed" => ("crossbred", ""),
        "cut" => ("cut", ""),
        "deal" => ("dealt", ""),
        "dig" => ("dug", ""),
        "dive" => ("dove", ""),
        "do" => ("did", "done"),
        "downtrod" => ("downtrod", "downtrodden"),
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
        "forego" => ("forewent", "foregone"),
        "foresee" => ("foresaw", "foreseen"),
        "foretell" => ("foretold", ""),
        "forget" => ("forgot", "forgotten"),
        "forgive" => ("forgave", "forgiven"),
        "forgo" => ("forwent", "forgone"),
        "forsake" => ("forsook", "forsaken"),
        "forswear" => ("forswore", "forsworn"),
        "freeze" => ("froze", "frozen"),
        "gainsay" => ("gainsaid", ""),
        "get" => ("got", "gotten"),
        "give" => ("gave", "given"),
        "go" => ("went", "gone"),
        "grind" => ("ground", ""),
        "grow" => ("grew", "grown"),
        "hang" => ("hung", ""),
        "have" => ("had", ""),
        "hear" => ("heard", ""),
        "hew" => ("hewed", "hewn"),
        "hide" => ("hid", "hidden"),
        "hit" => ("hit", ""),
        "hold" => ("held", ""),
        "housebreak" => ("housebroke", "housebroken"),
        "hurt" => ("hurt", ""),
        "input" => ("input", ""),
        "inset" => ("inset", ""),
        "interbreed" => ("interbred", ""),
        "interweave" => ("interwove", "interwoven"),
        "jailbreak" => ("jailbroke", "jailbroken"),
        "keep" => ("kept", ""),
        "kneel" => ("knelt", ""),
        "knit" => ("knit", ""),
        "know" => ("knew", "known"),
        "lade" => ("laded", "laden"),
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
        "lipread" => ("lipread", ""),
        "lose" => ("lost", ""),
        "make" => ("made", ""),
        "mean" => ("meant", ""),
        "meet" => ("met", ""),
        "mic" => ("miced", ""),
        "misdo" => ("misdid", "misdone"),
        "mishear" => ("misheard", ""),
        "mislay" => ("mislaid", ""),
        "mislead" => ("misled", ""),
        "misread" => ("misread", ""),
        "mistake" => ("mistook", "mistaken"),
        "misunderstand" => ("misunderstood", ""),
        "offset" => ("offset", ""),
        "outbid" => ("outbid", ""),
        "outdo" => ("outdid", "outdone"),
        "outgo" => ("outwent", "outgone"),
        "outgrow" => ("outgrew", "outgrown"),
        "outrun" => ("outran", "outrun"),
        "outsell" => ("outsold", ""),
        "overbear" => ("overbore", "overborne"),
        "overcast" => ("overcast", ""),
        "overcome" => ("overcame", "overcome"),
        "overdo" => ("overdid", "overdone"),
        "overdraw" => ("overdrew", "overdrawn"),
        "overgo" => ("overwent", "overgone"),
        "overgrow" => ("overgrew", "overgrown"),
        "overtake" => ("overtook", "overtaken"),
        "overthink" => ("overthought", ""),
        "overthrow" => ("overthrew", "overthrown"),
        "partake" => ("partook", "partaken"),
        "pay" => ("paid", ""),
        "plead" => ("pled", ""),
        "proofread" => ("proofread", ""),
        "prove" => ("proved", "proven"),
        "put" => ("put", ""),
        "quit" => ("quit", ""),
        "read" => ("read", ""),
        "rebuild" => ("rebuilt", ""),
        "rebuy" => ("rebought", ""),
        "recast" => ("recast", ""),
        "redo" => ("redid", "redone"),
        "redraw" => ("redrew", "redrawn"),
        "relay" => ("relaid", ""),
        "rend" => ("rent", ""),
        "repay" => ("repaid", ""),
        "reset" => ("reset", ""),
        "resing" => ("resang", "resung"),
        "resit" => ("resat", ""),
        "restrike" => ("restruck", ""),
        "rethink" => ("rethought", ""),
        "rewrite" => ("rewrote", "rewritten"),
        "rid" => ("rid", ""),
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
        "shit" => ("shat", ""),
        "shoe" => ("shod", ""),
        "shoot" => ("shot", ""),
        "show" => ("showed", "shown"),
        "shrink" => ("shrank", "shrunk"),
        "shrive" => ("shrove", "shriven"),
        "shut" => ("shut", ""),
        "sightsee" => ("sightsaw", "sightseen"),
        "sing" => ("sang", "sung"),
        "sink" => ("sank", "sunk"),
        "sit" => ("sat", ""),
        "skywrite" => ("skywrote", "skywritten"),
        "slay" => ("slew", "slain"),
        "sleep" => ("slept", ""),
        "slide" => ("slid", ""),
        "sling" => ("slung", ""),
        "slink" => ("slunk", ""),
        "slit" => ("slit", ""),
        "smell" => ("smelt", ""),
        "sow" => ("sowed", "sown"),
        "speak" => ("spoke", "spoken"),
        "speed" => ("sped", ""),
        "spell" => ("spelt", ""),
        "spellbind" => ("spellbound", ""),
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
        "strew" => ("strewed", "strewn"),
        "stride" => ("strode", "stridden"),
        "strike" => ("struck", ""),
        "strikethrough" => ("struckthrough", ""),
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
        "thrive" => ("throve", "thriven"),
        "throw" => ("threw", "thrown"),
        "thrust" => ("thrust", ""),
        "tread" => ("trod", "trodden"),
        "typeset" => ("typeset", ""),
        "unbind" => ("unbound", ""),
        "undercut" => ("undercut", ""),
        "underdo" => ("underdid", "underdone"),
        "undergo" => ("underwent", "undergone"),
        "underlie" => ("underlay", "underlain"),
        "understand" => ("understood", ""),
        "undertake" => ("undertook", "undertaken"),
        "underwrite" => ("underwrote", "underwritten"),
        "undo" => ("undid", "undone"),
        "unwind" => ("unwound", ""),
        "upset" => ("upset", ""),
        "wake" => ("woke", "woken"),
        "waylay" => ("waylaid", ""),
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

/// Whether `definition` carries a verb part-of-speech marker.  Used as evidence
/// the headword is a verb so non-verbs (e.g. the noun "serendipity") don't get a
/// conjugation, both during `Auto` routing and when a specific English source is
/// asked unforced.  Recognizes GCIDE's `v. t.`/`v. i.` abbreviations as well as
/// the HTML POS heading and inflection line that Wiktionary entries carry.
fn has_verb_pos(definition: &str) -> bool {
    find_verb_pos(definition).is_some()
}

/// The byte index just after the first verb part-of-speech marker, if any.
fn find_verb_pos(text: &str) -> Option<usize> {
    const MARKERS: &[&str] = &[
        // GCIDE inline abbreviations.
        "v. t. & i.",
        "v. i. & t.",
        "v. t.",
        "v. i.",
        "v. impers.",
        ", v.",
        // HTML dictionaries: a "Verb" POS heading (`<h3>Verb</h3>`, …) or the
        // inflection line every verb headword carries.  `>Verb<` avoids matching
        // "Adverb"/"Proverb".
        ">Verb<",
        "present participle",
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

/// Regular past / past participle: `-ed`, `-d` after `e`, `y→ied`, `c→cked`
/// after a vowel, or final consonant doubling.
fn regular_past(base: &str) -> String {
    let chars: Vec<char> = base.chars().collect();
    let n = chars.len();
    if base.ends_with('e') {
        return format!("{base}d");
    }
    if n >= 2 && chars[n - 1] == 'y' && !is_vowel(chars[n - 2]) {
        return format!("{}ied", &base[..base.len() - 1]);
    }
    if base.ends_with("ic") {
        return format!("{base}ked");
    }
    if let Some(doubled) = double_final_consonant(base, &chars) {
        return format!("{doubled}ed");
    }
    format!("{base}ed")
}

/// Present participle: `-ing`, dropping a silent `e`, `ie→ying`, `c→cking`
/// after a vowel, or doubling a final consonant.
fn regular_present_participle(base: &str) -> String {
    let chars: Vec<char> = base.chars().collect();
    let n = chars.len();
    if let Some(stem) = base.strip_suffix("ie") {
        return format!("{stem}ying");
    }
    if base.ends_with('e') && !base.ends_with("ee") && n > 2 {
        return format!("{}ing", &base[..base.len() - 1]);
    }
    if base.ends_with("ic") {
        return format!("{base}king");
    }
    if let Some(doubled) = double_final_consonant(base, &chars) {
        return format!("{doubled}ing");
    }
    format!("{base}ing")
}

/// For a stem ending in a doublable final consonant (not `w`/`x`/`y`), return
/// the stem with its final consonant doubled.  Monosyllabic consonant-vowel-
/// consonant stems always double; for polysyllables, doubling keys on
/// final-syllable stress, which can't be read off the spelling, so the common
/// stress-final doublers are consulted from a curated list.
fn double_final_consonant(base: &str, chars: &[char]) -> Option<String> {
    let n = chars.len();
    if n < 3 {
        return None;
    }
    let c3 = chars[n - 1];
    if matches!(c3, 'w' | 'x' | 'y') || is_vowel(c3) {
        return None;
    }
    let (c1, c2) = (chars[n - 3], chars[n - 2]);
    let monosyllabic_cvc = !is_vowel(c1) && is_vowel(c2) && is_monosyllabic(chars);
    if monosyllabic_cvc || POLYSYLLABIC_DOUBLERS.contains(&base) {
        let mut s: String = chars.iter().collect();
        s.push(c3);
        Some(s)
    } else {
        None
    }
}

/// Common polysyllabic verbs that double their final consonant before `-ed`/
/// `-ing` because their final syllable is stressed — a fact not derivable from
/// spelling (cf. `prefer`→`preferred` vs `offer`→`offered`).  Listing the
/// frequent stress-final doublers avoids both the under-generation of a
/// monosyllable-only rule and the over-generation (`visit`→`*visitted`) of a
/// blanket consonant-vowel-consonant rule.
static POLYSYLLABIC_DOUBLERS: &[&str] = &[
    "abet", "abhor", "acquit", "admit", "allot", "babysit", "befit", "beget", "begin", "beset",
    "commit", "compel", "concur", "confer", "control", "defer", "demur", "deter", "dispel", "emit",
    "enrol", "entrap", "equip", "excel", "expel", "extol", "forbid", "forget", "format",
    "handicap", "impel", "incur", "infer", "input", "inset", "inter", "kidnap", "occur", "offset",
    "omit", "outbid", "outfit", "outrun", "outwit", "overlap", "patrol", "permit", "prefer",
    "program", "propel", "rebel", "rebut", "recap", "recur", "refer", "regret", "remit", "repel",
    "reset", "resit", "retrofit", "submit", "transfer", "transmit", "unban", "unwrap", "upset",
];

/// Defective modals with no participle/infinitive that don't fit the
/// periphrastic grid.  `can` and `will` are deliberately absent: they double as
/// ordinary lexical verbs (to can food, to will a bequest) and conjugate fine.
fn is_defective_modal(base: &str) -> bool {
    matches!(
        base,
        "may" | "might" | "shall" | "should" | "must" | "would" | "could" | "ought"
    )
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
