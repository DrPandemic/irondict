use super::*;
use crate::config::Language;

// --- English (periphrastic grid from bundled irregular table) ----------------

#[test]
fn regular_walk_grid() {
    let c = EnglishConjugator::new();
    let conj = c.conjugate("walk", None, true).unwrap();
    assert_eq!(conj.language, Language::English);
    assert_eq!(conj.infinitive, "walk");

    // Spot-check a few cells across moods and tenses.
    let pres = &find_section(&conj.sections, "Indicative present").forms;
    assert_eq!(by_label(pres, "I"), "walk");
    assert_eq!(by_label(pres, "he/she/it"), "walks");
    assert_eq!(by_label(pres, "they"), "walk");

    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "walked");
    assert_eq!(by_label(past, "you"), "walked");

    let fut = &find_section(&conj.sections, "Indicative future").forms;
    assert_eq!(by_label(fut, "we"), "will walk");

    let pres_cont = &find_section(&conj.sections, "Indicative present continuous").forms;
    assert_eq!(by_label(pres_cont, "I"), "am walking");
    assert_eq!(by_label(pres_cont, "he/she/it"), "is walking");

    let pf = &find_section(&conj.sections, "Indicative present perfect").forms;
    assert_eq!(by_label(pf, "I"), "have walked");
    assert_eq!(by_label(pf, "he/she/it"), "has walked");

    let fut_pf_cont = &find_section(&conj.sections, "Indicative future perfect continuous").forms;
    assert_eq!(by_label(fut_pf_cont, "they"), "will have been walking");

    let cond = &find_section(&conj.sections, "Conditional").forms;
    assert_eq!(by_label(cond, "we"), "would walk");

    let nf = &find_section(&conj.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "infinitive"), "to walk");
    assert_eq!(by_label(nf, "present participle"), "walking");
    assert_eq!(by_label(nf, "past participle"), "walked");

    // Grid has all 17 sections.
    assert_eq!(conj.sections.len(), 17);
}

#[test]
fn irregular_go() {
    let c = EnglishConjugator::new();
    // `go` is in the irregular table; verb-POS should not be required.
    let conj = c.conjugate("go", None, true).unwrap();

    let pres = &find_section(&conj.sections, "Indicative present").forms;
    assert_eq!(by_label(pres, "he/she/it"), "goes");

    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "went");

    let pf = &find_section(&conj.sections, "Indicative present perfect").forms;
    assert_eq!(by_label(pf, "he/she/it"), "has gone");

    let nf = &find_section(&conj.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "past participle"), "gone");
    assert_eq!(by_label(nf, "present participle"), "going");

    // Present continuous: "I am going"
    let cont = &find_section(&conj.sections, "Indicative present continuous").forms;
    assert_eq!(by_label(cont, "I"), "am going");
}

#[test]
fn irregular_empty_past_participle_reuses_past() {
    // `buy` is irregular with no distinct past participle (bought/bought); the
    // participle must reuse the irregular past, not the regularized `buyed`.
    let c = EnglishConjugator::new();
    let conj = c.conjugate("buy", None, true).unwrap();

    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "bought");

    let pf = &find_section(&conj.sections, "Indicative present perfect").forms;
    assert_eq!(by_label(pf, "he/she/it"), "has bought");

    let nf = &find_section(&conj.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "past participle"), "bought");
}

#[test]
fn wiktionary_derived_verbs_conjugate() {
    // Verbs folded in from the Wiktionary-derived table: a prefixed compound and
    // a standalone irregular with a distinct participle.
    let c = EnglishConjugator::new();

    let mislead = c.conjugate("mislead", None, true).unwrap();
    let past = &find_section(&mislead.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "misled");
    let nf = &find_section(&mislead.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "past participle"), "misled");

    let beget = c.conjugate("beget", None, true).unwrap();
    let past = &find_section(&beget.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "begot");
    let nf = &find_section(&beget.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "past participle"), "begotten");
}

#[test]
fn suppletive_be() {
    let c = EnglishConjugator::new();
    let conj = c.conjugate("be", None, true).unwrap();

    let pres = &find_section(&conj.sections, "Indicative present").forms;
    assert_eq!(by_label(pres, "I"), "am");
    assert_eq!(by_label(pres, "you"), "are");
    assert_eq!(by_label(pres, "he/she/it"), "is");
    assert_eq!(by_label(pres, "we"), "are");
    assert_eq!(by_label(pres, "they"), "are");

    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "was");
    assert_eq!(by_label(past, "he/she/it"), "was");
    assert_eq!(by_label(past, "you"), "were");
    assert_eq!(by_label(past, "we"), "were");
    assert_eq!(by_label(past, "they"), "were");

    let cont = &find_section(&conj.sections, "Indicative present continuous").forms;
    assert_eq!(by_label(cont, "I"), "am being");
    assert_eq!(by_label(cont, "he/she/it"), "is being");

    let past_cont = &find_section(&conj.sections, "Indicative past continuous").forms;
    assert_eq!(by_label(past_cont, "I"), "was being");
    assert_eq!(by_label(past_cont, "they"), "were being");

    let pf = &find_section(&conj.sections, "Indicative present perfect").forms;
    assert_eq!(by_label(pf, "I"), "have been");
}

#[test]
fn irregular_run_with_alternatives() {
    let c = EnglishConjugator::new();
    let conj = c.conjugate("run", None, true).unwrap();

    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "ran");

    let pf = &find_section(&conj.sections, "Indicative present perfect").forms;
    assert_eq!(by_label(pf, "he/she/it"), "has run");

    let cont = &find_section(&conj.sections, "Indicative present continuous").forms;
    assert_eq!(by_label(cont, "I"), "am running");
}

#[test]
fn regular_spelling_rules() {
    let c = EnglishConjugator::new();
    // y -> ies / ied
    let conj = c.conjugate("carry", None, true).unwrap();
    let pres = &find_section(&conj.sections, "Indicative present").forms;
    assert_eq!(by_label(pres, "he/she/it"), "carries");
    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "carried");
    let nf = &find_section(&conj.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "present participle"), "carrying");

    // e -> d, drop-e+ing
    let conj = c.conjugate("like", None, true).unwrap();
    let pres = &find_section(&conj.sections, "Indicative present").forms;
    assert_eq!(by_label(pres, "he/she/it"), "likes");
    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "liked");
    let nf = &find_section(&conj.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "present participle"), "liking");

    // Sibilant -> es
    let conj = c.conjugate("watch", None, true).unwrap();
    let pres = &find_section(&conj.sections, "Indicative present").forms;
    assert_eq!(by_label(pres, "he/she/it"), "watches");

    // Consonant doubling (monosyllabic CVC)
    let conj = c.conjugate("stop", None, true).unwrap();
    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "stopped");
    let nf = &find_section(&conj.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "present participle"), "stopping");
}

#[test]
fn c_final_takes_k_before_ed_ing() {
    // Verbs ending in `-ic` add `k` to keep the /k/ sound: panic -> panicked.
    let c = EnglishConjugator::new();
    let conj = c.conjugate("panic", None, true).unwrap();
    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "panicked");
    let nf = &find_section(&conj.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "present participle"), "panicking");
    assert_eq!(by_label(nf, "past participle"), "panicked");

    let traffic = c.conjugate("traffic", None, true).unwrap();
    let past = &find_section(&traffic.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "trafficked");
}

#[test]
fn polysyllabic_stress_doubling() {
    // Final-stress polysyllables double; near-twins with non-final stress don't.
    let c = EnglishConjugator::new();

    let prefer = c.conjugate("prefer", None, true).unwrap();
    let past = &find_section(&prefer.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "preferred");
    let nf = &find_section(&prefer.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "present participle"), "preferring");

    let equip = c.conjugate("equip", None, true).unwrap();
    let nf = &find_section(&equip.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "past participle"), "equipped");
    assert_eq!(by_label(nf, "present participle"), "equipping");

    // Irregular-table verbs take their present participle from the regular
    // rules, so polysyllabic final-stress irregulars must double there too.
    let begin = c.conjugate("begin", None, true).unwrap();
    let nf = &find_section(&begin.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "present participle"), "beginning");
    let forget = c.conjugate("forget", None, true).unwrap();
    let nf = &find_section(&forget.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "present participle"), "forgetting");

    // Not in the doubler list, first-syllable stress -> no doubling.
    let visit = c.conjugate("visit", None, true).unwrap();
    let past = &find_section(&visit.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "visited");
    let offer = c.conjugate("offer", None, true).unwrap();
    let nf = &find_section(&offer.sections, "Non-finite").forms;
    assert_eq!(by_label(nf, "present participle"), "offering");
}

#[test]
fn compound_irregulars_inherit_base_forms() {
    let c = EnglishConjugator::new();
    let cases = [
        ("partake", "partook", "partaken"),
        ("outdo", "outdid", "outdone"),
        ("gainsay", "gainsaid", "gainsaid"),
        ("browbeat", "browbeat", "browbeaten"),
        ("spellbind", "spellbound", "spellbound"),
        ("proofread", "proofread", "proofread"),
        ("typeset", "typeset", "typeset"),
    ];
    for (verb, want_past, want_pp) in cases {
        let conj = c.conjugate(verb, None, true).unwrap();
        let past = &find_section(&conj.sections, "Indicative past").forms;
        assert_eq!(by_label(past, "I"), want_past, "{verb} past");
        let nf = &find_section(&conj.sections, "Non-finite").forms;
        assert_eq!(by_label(nf, "past participle"), want_pp, "{verb} pp");
    }
}

#[test]
fn defective_modals_decline() {
    let c = EnglishConjugator::new();
    // No periphrastic grid for pure modals, even when forced.
    for m in ["may", "must", "shall", "ought", "would"] {
        assert!(c.conjugate(m, None, true).is_none(), "{m} should decline");
    }
    // `can`/`will` have ordinary lexical-verb senses and still conjugate.
    assert!(c.conjugate("can", None, true).is_some());
    assert!(c.conjugate("will", None, true).is_some());
}

#[test]
fn auto_routing_requires_verb_evidence() {
    let c = EnglishConjugator::new();
    // No definition, not forced (Auto mode) — can't tell it's a verb.
    assert!(c.conjugate("walk", None, false).is_none());
    // Non-verb definition — decline under Auto.
    assert!(c
        .conjugate("arm", Some("Arm \\Arm\\, n. The limb."), false)
        .is_none());
}

#[test]
fn auto_routing_accepts_irregular_even_without_pos() {
    let c = EnglishConjugator::new();
    // "go" is in the irregular table — recognised even without a POS marker.
    assert!(c.conjugate("go", None, false).is_some());
}

#[test]
fn auto_routing_accepts_verb_pos() {
    let c = EnglishConjugator::new();
    // Regular verb "walk" with a verb POS marker in the definition.
    assert!(c
        .conjugate("walk", Some("Walk \\Walk\\, v. i. To move."), false)
        .is_some());
}

#[test]
fn registry_routes_english() {
    let reg = ConjugatorRegistry::new();
    // Pinned English forces a table even with no definition.
    let conj = reg.conjugate("jump", None, Language::English).unwrap();
    assert_eq!(conj.language, Language::English);
    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "jumped");

    // Auto with a GCIDE verb definition resolves to English.
    let conj = reg
        .conjugate("go", Some("Go \\Go\\, v. i."), Language::Auto)
        .unwrap();
    assert_eq!(conj.language, Language::English);
}

#[test]
fn have_third_singular_has() {
    let c = EnglishConjugator::new();
    let conj = c.conjugate("have", None, true).unwrap();
    let pres = &find_section(&conj.sections, "Indicative present").forms;
    assert_eq!(by_label(pres, "he/she/it"), "has");
    let past = &find_section(&conj.sections, "Indicative past").forms;
    assert_eq!(by_label(past, "I"), "had");
}

// --- French (unchanged) -----------------------------------------------------

#[test]
fn french_parses_a_real_conjugation_table() {
    let text = "parler\n\
Indicatif présent\n\
je parle\n\
tu parles\n\
il parle\n\
nous parlons\n\
vous parlez\n\
ils parlent\n\
Imparfait\n\
je parlais\n\
tu parlais\n\
il parlait\n\
nous parlions\n\
vous parliez\n\
ils parlaient\n";
    let reg = ConjugatorRegistry::new();
    let conj = reg
        .conjugate("parler", Some(text), Language::French)
        .unwrap();
    assert_eq!(conj.language, Language::French);
    assert_eq!(conj.sections.len(), 2);
    assert_eq!(conj.sections[0].label, "Indicatif présent");
    assert_eq!(conj.sections[0].forms.len(), 6);
    assert_eq!(conj.sections[0].forms[0].text, "parle");
}

#[test]
fn french_declines_ordinary_prose() {
    let prose = "parler : v. Le présent de ce verbe est courant. \
        Conjugaison : voir modèle 6.";
    let c = FrenchConjugator::new();
    assert!(c.conjugate("parler", Some(prose), true).is_none());
}

#[test]
fn french_conj_companion_html_strips_and_parses() {
    let text = concat!(
        "<b>Indicatif présent</b><br>je mange<br>tu manges<br>il/elle/on mange<br>",
        "nous mangeons<br>vous mangez<br>ils/elles mangent<br>",
        "<b>Indicatif imparfait</b><br>je mangeais<br>tu mangeais<br>il/elle/on mangeait<br>",
        "nous mangions<br>vous mangiez<br>ils/elles mangeaient<br>",
    );
    let c = FrenchConjugator::new();
    let conj = c
        .conjugate("manger", Some(text), true)
        .expect("should parse companion HTML");
    assert_eq!(conj.language, Language::French);
    assert!(
        conj.sections.len() >= 2,
        "expected at least 2 sections, got {}: {:?}",
        conj.sections.len(),
        conj.sections.iter().map(|s| &s.label).collect::<Vec<_>>()
    );
    assert_eq!(conj.sections[0].label, "Indicatif présent");
    assert_eq!(conj.sections[0].forms.len(), 6);
    assert_eq!(conj.sections[0].forms[0].text, "mange");
    assert_eq!(conj.sections[1].label, "Indicatif imparfait");
}

// --- Italian (unchanged) ----------------------------------------------------

#[test]
fn italian_conj_companion_html_strips_and_parses() {
    let text = concat!(
        "<b>Indicativo presente</b><br>io parlo<br>tu parli<br>egli parla<br>",
        "noi parliamo<br>voi parlate<br>essi parlano<br>",
        "<b>Indicativo imperfetto</b><br>io parlavo<br>tu parlavi<br>egli parlava<br>",
        "noi parlavamo<br>voi parlavate<br>essi parlavano<br>",
        "<b>Participio passato</b><br>parlato<br>",
    );
    let c = ItalianConjugator::new();
    let conj = c
        .conjugate("parlare", Some(text), true)
        .expect("should parse companion HTML");
    assert_eq!(conj.language, Language::Italian);
    assert_eq!(conj.sections[0].label, "Indicativo presente");
    assert_eq!(conj.sections[0].forms.len(), 6);
    assert_eq!(conj.sections[0].forms[0].text, "parlo");
    assert_eq!(conj.sections[0].forms[0].label, "io");
    assert_eq!(conj.sections[1].label, "Indicativo imperfetto");
}

#[test]
fn italian_declines_ordinary_prose() {
    let prose = "parlare: v. Il presente indicativo è regolare. Coniugazione: modello 1.";
    let c = ItalianConjugator::new();
    assert!(c.conjugate("parlare", Some(prose), true).is_none());
}

#[test]
fn italian_routes_through_registry() {
    let text = concat!(
        "<b>Indicativo presente</b><br>io temo<br>tu temi<br>egli teme<br>",
        "noi temiamo<br>voi temete<br>essi temono<br>",
        "<b>Congiuntivo presente</b><br>io tema<br>tu tema<br>egli tema<br>",
        "noi temiamo<br>voi temiate<br>essi temano<br>",
    );
    let reg = ConjugatorRegistry::new();
    let conj = reg
        .conjugate("temere", Some(text), Language::Italian)
        .unwrap();
    assert_eq!(conj.language, Language::Italian);
    assert_eq!(conj.sections.len(), 2);
    assert_eq!(conj.sections[1].label, "Congiuntivo presente");
}

// --- Helpers ----------------------------------------------------------------

fn find_section<'a>(sections: &'a [ConjSection], label: &str) -> &'a ConjSection {
    sections
        .iter()
        .find(|s| s.label == label)
        .unwrap_or_else(|| panic!("section '{label}' not found"))
}

fn by_label<'a>(forms: &'a [ConjForm], label: &str) -> &'a str {
    forms
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.text.as_str())
        .unwrap_or("<missing>")
}
