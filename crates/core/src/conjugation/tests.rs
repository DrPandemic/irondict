use super::*;
use crate::config::Language;

/// A trimmed GCIDE "go" entry: irregular principal parts in the inflection block,
/// followed by the etymology block (which must be skipped).
const GCIDE_GO: &str = "Go \\Go\\, v. i. [imp. {Went} (w[e^]nt); p. p. {Gone} (g[o^]n; \
115); p. pr. & vb. n. {Going}. Went comes from the AS, wendan. See {Wend}, v. i.] \
[OE. gan, gon, AS. g[=a]n.] 1. To pass from one place to another.";

/// A trimmed GCIDE "be" entry — suppletive present tense (is) comes from a
/// grammar rule, not the (absent) inline form.
const GCIDE_BE: &str =
    "Be \\Be\\, v. i. [imp. {Was}; p. p. {Been}; p. pr. & vb. n. {Being}.] To exist.";

/// A "run" entry with alternative past forms ({Ran} or {Run}).
const GCIDE_RUN: &str =
    "Run \\Run\\, v. i. [imp. {Ran}or {Run}; p. p. {Run}; p. pr. & vb. n. {Running}.] To move.";

/// A verb GCIDE leaves unannotated (no inflection block) — exercises the rules.
const GCIDE_STOP: &str = "Stop \\Stop\\, v. i. 1. To cease to go on; to halt.";

#[test]
fn parses_irregular_from_gcide_block() {
    let c = EnglishConjugator::new();
    let conj = c.conjugate("go", Some(GCIDE_GO), false).unwrap();
    let forms = &conj.sections[0].forms;
    assert_eq!(by_label(forms, "past"), "went");
    assert_eq!(by_label(forms, "past participle"), "gone");
    // The "See {Wend}" cross-reference must not be mistaken for a form.
    assert_eq!(by_label(forms, "present participle"), "going");
    assert_eq!(by_label(forms, "present (he/she/it)"), "goes");
}

#[test]
fn suppletive_present_tense() {
    let c = EnglishConjugator::new();
    let conj = c.conjugate("be", Some(GCIDE_BE), false).unwrap();
    let f = &conj.sections[0].forms;
    assert_eq!(by_label(f, "present (he/she/it)"), "is");
    assert_eq!(by_label(f, "past"), "was");
    assert_eq!(by_label(f, "past participle"), "been");
}

#[test]
fn parses_alternative_past_forms() {
    let c = EnglishConjugator::new();
    let conj = c.conjugate("run", Some(GCIDE_RUN), false).unwrap();
    assert_eq!(by_label(&conj.sections[0].forms, "past"), "ran or run");
    assert_eq!(
        by_label(&conj.sections[0].forms, "present participle"),
        "running"
    );
}

#[test]
fn regular_rules_fill_unannotated_verb() {
    let c = EnglishConjugator::new();
    let conj = c.conjugate("stop", Some(GCIDE_STOP), false).unwrap();
    let f = &conj.sections[0].forms;
    assert_eq!(by_label(f, "past"), "stopped");
    assert_eq!(by_label(f, "present participle"), "stopping");
    assert_eq!(by_label(f, "present (he/she/it)"), "stops");
}

#[test]
fn regular_spelling_rules() {
    let c = EnglishConjugator::new();
    // y -> ies / ied, e -> d / drop, sibilant -> es
    let cases = [
        ("carry", "carries", "carried", "carrying"),
        ("study", "studies", "studied", "studying"),
        ("like", "likes", "liked", "liking"),
        ("watch", "watches", "watched", "watching"),
        ("walk", "walks", "walked", "walking"),
    ];
    for (verb, third, past, pres_part) in cases {
        let conj = c.conjugate(verb, None, true).unwrap();
        let f = &conj.sections[0].forms;
        assert_eq!(by_label(f, "present (he/she/it)"), third, "{verb} 3sg");
        assert_eq!(by_label(f, "past"), past, "{verb} past");
        assert_eq!(by_label(f, "present participle"), pres_part, "{verb} prp");
    }
}

#[test]
fn auto_routing_requires_verb_evidence() {
    let c = EnglishConjugator::new();
    // No definition + not forced => can't tell it's a verb => decline.
    assert!(c.conjugate("walk", None, false).is_none());
    // A non-verb definition => decline under Auto.
    assert!(c
        .conjugate("arm", Some("Arm \\Arm\\, n. The limb."), false)
        .is_none());
}

#[test]
fn registry_routes_by_language() {
    let reg = ConjugatorRegistry::new();
    // Pinned English forces a table even with no definition.
    let conj = reg.conjugate("jump", None, Language::English).unwrap();
    assert_eq!(conj.language, Language::English);
    assert_eq!(by_label(&conj.sections[0].forms, "past"), "jumped");

    // Auto with a GCIDE verb definition resolves to English.
    let conj = reg.conjugate("go", Some(GCIDE_GO), Language::Auto).unwrap();
    assert_eq!(conj.language, Language::English);
}

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
    // Petit-Robert-style prose mentioning "présent" must not be mistaken for a
    // conjugation table.
    let prose = "parler : v. Le présent de ce verbe est courant. \
        Conjugaison : voir modèle 6.";
    let c = FrenchConjugator::new();
    assert!(c.conjugate("parler", Some(prose), true).is_none());
}

fn by_label<'a>(forms: &'a [ConjForm], label: &str) -> &'a str {
    forms
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.text.as_str())
        .unwrap_or("<missing>")
}
