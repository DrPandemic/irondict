//! Verb conjugation (Phase 8).
//!
//! The model is deliberately language-agnostic: a [`Conjugation`] is a list of
//! [`ConjSection`]s (one mood/tense), each holding person-tagged [`ConjForm`]s.
//! English collapses to a single section of principal parts; French expands to
//! the full person × tense × mood grid.
//!
//! Backends implement [`Conjugator`]; a [`ConjugatorRegistry`] routes a lookup
//! to the right one. Routing prefers the per-dictionary [`Language`] pinned in
//! the settings page (Phase 7); when that is `Auto` it tries each backend and
//! accepts the first that recognizes the headword as a verb.
//!
//! Conjugation is sourced **from the loaded dictionaries**, not a bundled verb
//! dataset: English reads GCIDE's inflection block (regular verbs fall back to
//! in-code spelling rules); French parses whatever conjugation content the loaded
//! French dictionary provides (nothing for dictionaries that only reference a
//! numbered conjugation model, like Le Petit Robert).

use crate::config::Language;

mod english;
mod french;
#[cfg(test)]
mod tests;

pub use english::EnglishConjugator;
pub use french::FrenchConjugator;

/// One inflected form, e.g. `("je", "parle")` or `("past participle", "gone")`.
/// The `label` is the person/role tag and may be empty for unlabeled forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConjForm {
    pub label: String,
    pub text: String,
}

impl ConjForm {
    pub fn new(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            text: text.into(),
        }
    }
}

/// One mood/tense block, e.g. "Indicatif présent" with its six person forms, or
/// English's single "Principal parts" block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConjSection {
    pub label: String,
    pub forms: Vec<ConjForm>,
}

/// A verb's full conjugation, grouped into sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conjugation {
    pub language: Language,
    pub infinitive: String,
    pub sections: Vec<ConjSection>,
}

/// A per-language conjugation backend.
pub trait Conjugator: Send + Sync {
    /// The language this backend conjugates.
    fn language(&self) -> Language;

    /// Conjugate `headword`, returning `None` if it isn't recognized as a verb.
    ///
    /// `definition` is the dictionary entry text when available (English uses it
    /// to read GCIDE's authoritative inflection block). `force` asks the backend
    /// for a best-effort table even without verb evidence — set when the user has
    /// explicitly pinned this language, cleared during `Auto` routing so a
    /// permissive backend (e.g. English's rule generator) can't shadow another.
    fn conjugate(
        &self,
        headword: &str,
        definition: Option<&str>,
        force: bool,
    ) -> Option<Conjugation>;
}

/// Owns the registered conjugation backends and routes a lookup to the right one.
pub struct ConjugatorRegistry {
    backends: Vec<Box<dyn Conjugator>>,
}

impl Default for ConjugatorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ConjugatorRegistry {
    /// A registry with all built-in backends (English + French), each of which
    /// parses conjugation out of the dictionary entry it is handed.
    pub fn new() -> Self {
        Self {
            backends: vec![
                Box::new(EnglishConjugator::new()),
                Box::new(FrenchConjugator::new()),
            ],
        }
    }

    /// Register an additional backend.
    pub fn register(&mut self, backend: Box<dyn Conjugator>) {
        self.backends.push(backend);
    }

    /// Conjugate `headword`, routing by the caller's preferred `language`.
    ///
    /// When `language` is a specific one, the matching backend is asked for a
    /// forced best-effort table. When it is `Auto`, every backend is tried in
    /// registration order and the first confident match wins.
    pub fn conjugate(
        &self,
        headword: &str,
        definition: Option<&str>,
        language: Language,
    ) -> Option<Conjugation> {
        match language {
            Language::Auto => self
                .backends
                .iter()
                .find_map(|b| b.conjugate(headword, definition, false)),
            lang => self
                .backends
                .iter()
                .find(|b| b.language() == lang)
                .and_then(|b| b.conjugate(headword, definition, true)),
        }
    }
}
