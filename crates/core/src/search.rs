use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use tantivy::collector::TopDocs;
use tantivy::query::{
    BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, RegexQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value, STORED, STRING};
use tantivy::{doc, Index, IndexReader, Term};
use unicode_normalization::UnicodeNormalization;

use crate::manager::DictionaryManager;
use crate::Error;

/// How a query string is matched against the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Headword starts with the query (autocomplete). The exact match, when one
    /// exists, is ranked first (see [`rank_prefix_exact_first`]), so this also
    /// serves the "look up this exact word" case.
    Prefix,
    /// Typo-tolerant headword match (Levenshtein distance up to 2).
    Fuzzy,
}

/// One search result: which dictionary it came from, the matched headword, and
/// the relevance score (higher is better). The index no longer stores a
/// definition preview; front-ends fetch a snippet on demand via the normal
/// lookup path when they want one (see the GUI result list).
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub dictionary: String,
    pub headword: String,
    pub score: f32,
}

/// Tantivy schema field handles for the dictionary index.
#[derive(Clone, Copy)]
struct Fields {
    /// Source dictionary name (untokenized; stored for display).
    dictionary: Field,
    /// Accent- and case-folded headword as a single token (see [`fold`]), for
    /// prefix/fuzzy matching. Folding lets accent-free queries ("etre") match
    /// accented headwords ("être").
    key_folded: Field,
    /// Original-case headword as a single token; stored for display.
    headword: Field,
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let fields = Fields {
        dictionary: builder.add_text_field("dictionary", STRING | STORED),
        key_folded: builder.add_text_field("key_folded", STRING),
        headword: builder.add_text_field("headword", STRING | STORED),
    };
    (builder.build(), fields)
}

/// Progress reported while [`build_cancellable`](SearchEngine::build_cancellable)
/// indexes the enabled dictionaries. `indexed` is the number of entries written
/// so far; `total` is the summed word count of the enabled dictionaries — an
/// estimate (a dictionary's stated word count can differ slightly from the
/// entries actually visited), and `0` when unknown, so consumers must guard the
/// division before computing a fraction.
#[derive(Debug, Clone, Copy)]
pub struct IndexProgress {
    pub indexed: u64,
    pub total: u64,
}

/// A search index over the headwords of the enabled dictionaries (definitions
/// are neither indexed nor stored). Backed by a tantivy index stored on disk so
/// it can be cached across runs and only rebuilt when the set of dictionaries
/// changes.
pub struct SearchEngine {
    reader: IndexReader,
    fields: Fields,
}

fn map_err<E: std::error::Error + Send + Sync + 'static>(e: E) -> Error {
    Error::Search(Box::new(e))
}

/// Directory under the OS cache dir where the search index is stored
/// (e.g. `~/.cache/irondict/index` on Linux).
pub fn default_index_dir() -> Result<PathBuf, Error> {
    let dirs = ProjectDirs::from("", "", "irondict").ok_or(Error::NoConfigDir)?;
    Ok(dirs.cache_dir().join("index"))
}

impl SearchEngine {
    /// Build (or rebuild) the index in `dir` from the manager's enabled
    /// dictionaries, replacing any existing index there.
    pub fn build(dir: &Path, manager: &mut DictionaryManager) -> Result<Self, Error> {
        Self::build_cancellable(dir, manager, || false, |_| {})
            .map(|engine| engine.expect("a build that never cancels always yields an engine"))
    }

    /// Like [`build`](Self::build), but `cancel` is polled periodically while
    /// indexing; when it returns `true` the build is abandoned and `Ok(None)` is
    /// returned (no partial index is committed). Used by the GUI so deleting or
    /// changing a dictionary stops an in-flight build instead of finishing a
    /// now-stale one. `progress` is called at the same cadence with the running
    /// count, so a front-end can show a progress bar / time-remaining estimate.
    pub fn build_cancellable(
        dir: &Path,
        manager: &mut DictionaryManager,
        mut cancel: impl FnMut() -> bool,
        mut progress: impl FnMut(IndexProgress),
    ) -> Result<Option<Self>, Error> {
        std::fs::create_dir_all(dir)?;
        let (schema, fields) = schema();

        // `create_in_dir` refuses a non-empty directory, so clear any stale
        // index first to make rebuilds idempotent.
        let index = match Index::create_in_dir(dir, schema.clone()) {
            Ok(index) => index,
            Err(_) => {
                for entry in std::fs::read_dir(dir)? {
                    let entry = entry?;
                    if entry.path().is_dir() {
                        std::fs::remove_dir_all(entry.path())?;
                    } else {
                        std::fs::remove_file(entry.path())?;
                    }
                }
                Index::create_in_dir(dir, schema).map_err(map_err)?
            }
        };

        // Expected entry count for the progress estimate (the stated word counts
        // of the enabled dictionaries). Only an estimate, so the UI guards the
        // division and the actual `seen` total may overshoot slightly.
        let total: u64 = manager
            .dictionaries()
            .iter()
            .filter(|d| d.enabled)
            .map(|d| d.dictionary.info.word_count as u64)
            .sum();

        let mut writer = index.writer(50_000_000).map_err(map_err)?;
        let mut indexing_error = None;
        let mut cancelled = false;
        // Poll `cancel` and report `progress` on the first entry and then once per
        // batch, so the checks stay negligible on large dictionaries while still
        // catching a cancellation (and showing first progress) early on small ones.
        let mut seen: u64 = 0;
        manager.for_each_enabled_entry(|name, entry| {
            seen += 1;
            if seen % 4096 == 1 {
                // Stop the moment a newer request supersedes this build, so we
                // don't keep decoding the remaining (possibly millions of) entries.
                if cancel() {
                    cancelled = true;
                    return ControlFlow::Break(());
                }
                progress(IndexProgress {
                    indexed: seen,
                    total,
                });
            }
            // Index only the headword (folded for matching, original for display).
            // Definitions are never searched and no longer stored — front-ends
            // fetch a preview snippet on demand via the lookup path — which keeps
            // the on-disk index small.
            let result = writer.add_document(doc!(
                fields.dictionary => name,
                fields.key_folded => fold(&entry.headword),
                fields.headword => entry.headword,
            ));
            if let Err(e) = result {
                indexing_error = Some(map_err(e));
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        })?;
        if let Some(e) = indexing_error {
            return Err(e);
        }
        if cancelled {
            // Drop the writer without committing; the partial segments are left
            // for the next build's create-in-dir cleanup to clear.
            drop(writer);
            return Ok(None);
        }
        writer.commit().map_err(map_err)?;

        let reader = index.reader().map_err(map_err)?;
        Ok(Some(Self { reader, fields }))
    }

    /// Open an index previously built in `dir`.
    pub fn open(dir: &Path) -> Result<Self, Error> {
        let index = Index::open_in_dir(dir).map_err(map_err)?;
        let (_, fields) = schema();
        let reader = index.reader().map_err(map_err)?;
        Ok(Self { reader, fields })
    }

    /// Run `query` in the given `mode`, returning up to `limit` ranked hits
    /// across all dictionaries.
    pub fn search(
        &self,
        query: &str,
        mode: SearchMode,
        limit: usize,
    ) -> Result<Vec<SearchHit>, Error> {
        self.search_scoped(query, mode, limit, None)
    }

    /// Like [`search`](Self::search), but when `dictionary` is `Some(name)` the
    /// results are restricted to that single source dictionary.
    pub fn search_scoped(
        &self,
        query: &str,
        mode: SearchMode,
        limit: usize,
        dictionary: Option<&str>,
    ) -> Result<Vec<SearchHit>, Error> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        // Fuzzy retrieval needs its own over-fetch + re-rank, so it bypasses the
        // generic single-query path below.
        if mode == SearchMode::Fuzzy {
            return self.fuzzy_search(query, limit, dictionary);
        }
        // Prefix is the only non-fuzzy mode. OR an exact term with the prefix
        // regex: tantivy's RegexQuery DFA can miss a term equal to the literal
        // prefix (e.g. `go.*` not matching "go"), and matching both clauses also
        // lets the exact headword out-score partial completions.
        debug_assert_eq!(mode, SearchMode::Prefix);
        let folded = fold(query);
        let pattern = format!("{}.*", regex_escape(&folded));
        let regex = RegexQuery::from_pattern(&pattern, self.fields.key_folded).map_err(map_err)?;
        let exact = TermQuery::new(
            Term::from_field_text(self.fields.key_folded, &folded),
            IndexRecordOption::Basic,
        );
        let parsed: Box<dyn Query> = Box::new(BooleanQuery::new(vec![
            (Occur::Should, Box::new(regex) as Box<dyn Query>),
            (Occur::Should, Box::new(exact)),
        ]));
        let parsed = self.with_scope(parsed, dictionary);

        let searcher = self.reader.searcher();
        let docs = searcher
            .search(&*parsed, &TopDocs::with_limit(limit).order_by_score())
            .map_err(map_err)?;

        let mut hits = Vec::with_capacity(docs.len());
        for (score, addr) in docs {
            let doc: tantivy::TantivyDocument = searcher.doc(addr).map_err(map_err)?;
            let dictionary = stored_text(&doc, self.fields.dictionary);
            let headword = stored_text(&doc, self.fields.headword);
            hits.push(SearchHit {
                dictionary,
                headword,
                score,
            });
        }
        if mode == SearchMode::Prefix {
            // Prefix/exact are constant-scored, so impose a deterministic order in
            // Rust (shared by every front-end): exact accent-and-case match first,
            // then accent-insensitive exact, then shortest completion, then alpha.
            rank_prefix_exact_first(query, &folded, &mut hits);
        }
        Ok(hits)
    }

    /// Wrap `query` so it only matches documents from `dictionary` (when set),
    /// by ANDing it with an exact term query on the stored dictionary name.
    fn with_scope(&self, query: Box<dyn Query>, dictionary: Option<&str>) -> Box<dyn Query> {
        match dictionary {
            None => query,
            Some(name) => {
                let term = Term::from_field_text(self.fields.dictionary, name);
                Box::new(BooleanQuery::new(vec![
                    (
                        Occur::Must,
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                    ),
                    (Occur::Must, query),
                ]))
            }
        }
    }

    /// Typo-tolerant headword search, ranked by actual edit distance.
    ///
    /// tantivy's [`FuzzyTermQuery`] is constant-scored (every match scores the
    /// same), so on its own the top-N would be an arbitrary slice of all matches
    /// — a perfect match can fall outside the limit. To avoid that we:
    ///
    /// 1. cap the allowed edit distance by query length, so short, ambiguous
    ///    queries don't match a large fraction of the dictionary (length guard);
    /// 2. stack an exact + distance-1 + distance-2 query with widely separated
    ///    boosts, so closer matches both *retrieve* first and rank first;
    /// 3. over-fetch and re-rank in Rust by true edit distance (transposition-
    ///    aware), with a first-character anchor as a prefix guard and stable
    ///    tie-breaks.
    fn fuzzy_search(
        &self,
        query: &str,
        limit: usize,
        dictionary: Option<&str>,
    ) -> Result<Vec<SearchHit>, Error> {
        // Fuzz over the folded key so accents don't count as edits (être is a
        // distance-0 match for "etre").
        let lower = fold(query);
        let len = lower.chars().count();
        // Mild length guard: a single character is too ambiguous to fuzz, but
        // anything longer is allowed the full distance-2 budget. Ranking by true
        // edit distance (below) keeps the closest matches on top regardless.
        let max_distance: u8 = if len <= 1 { 0 } else { 2 };

        let exact = Term::from_field_text(self.fields.key_folded, &lower);
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Should,
            Box::new(BoostQuery::new(
                Box::new(TermQuery::new(exact.clone(), IndexRecordOption::Basic)),
                1_000_000.0,
            )),
        )];
        if max_distance >= 1 {
            clauses.push((
                Occur::Should,
                Box::new(BoostQuery::new(
                    Box::new(FuzzyTermQuery::new(exact.clone(), 1, true)),
                    1_000.0,
                )),
            ));
        }
        if max_distance >= 2 {
            clauses.push((
                Occur::Should,
                Box::new(BoostQuery::new(
                    Box::new(FuzzyTermQuery::new(exact, 2, true)),
                    1.0,
                )),
            ));
        }
        let query = self.with_scope(Box::new(BooleanQuery::new(clauses)), dictionary);

        // Over-fetch so the Rust re-rank has room to find the closest matches
        // even when there are many equally-scored fuzzy candidates.
        let fetch = limit.saturating_mul(4).max(64);
        let searcher = self.reader.searcher();
        let docs = searcher
            .search(&*query, &TopDocs::with_limit(fetch).order_by_score())
            .map_err(map_err)?;

        let first = lower.chars().next();
        let mut scored: Vec<(usize, usize, SearchHit)> = Vec::with_capacity(docs.len());
        for (_score, addr) in docs {
            let doc: tantivy::TantivyDocument = searcher.doc(addr).map_err(map_err)?;
            let headword = stored_text(&doc, self.fields.headword);
            let hw_folded = fold(&headword);
            // Prefix guard: keep only candidates sharing the query's first
            // (folded) character — legitimate typo corrections rarely change it.
            if hw_folded.chars().next() != first {
                continue;
            }
            let distance = edit_distance(&lower, &hw_folded);
            let hit = SearchHit {
                dictionary: stored_text(&doc, self.fields.dictionary),
                headword,
                // A distance-based score so closer matches read as more relevant.
                score: 1.0 / (1.0 + distance as f32),
            };
            scored.push((distance, hw_folded.chars().count(), hit));
        }
        // Closest first, then shorter headwords, then alphabetical for stability.
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then_with(|| {
                a.2.headword
                    .to_lowercase()
                    .cmp(&b.2.headword.to_lowercase())
            })
        });
        Ok(scored.into_iter().take(limit).map(|(_, _, h)| h).collect())
    }
}

fn stored_text(doc: &tantivy::TantivyDocument, field: Field) -> String {
    doc.get_first(field)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Optimal string alignment distance (restricted Damerau–Levenshtein): like
/// Levenshtein (insert/delete/substitute) but a swap of two *adjacent*
/// characters counts as a single edit. That keeps common typos close — e.g.
/// "recieve" → "receive" is distance 1, not 2. Operates on `char`s, so it is
/// Unicode-correct.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    // Three rolling rows: row i-2 (`two`), row i-1 (`one`), row i (`cur`).
    let mut two = vec![0usize; m + 1];
    let mut one: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (one[j] + 1).min(cur[j - 1] + 1).min(one[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(two[j - 2] + 1); // adjacent transposition
            }
            cur[j] = v;
        }
        // Rotate rows: two <- one, one <- cur, cur <- (scratch).
        std::mem::swap(&mut one, &mut two);
        std::mem::swap(&mut one, &mut cur);
    }
    one[m]
}

/// Bumped whenever the index schema or tokenization changes, so a cached index
/// built by an older binary is rebuilt rather than read with a mismatched layout.
/// Folded into [`index_signature`].
const INDEX_VERSION: u32 = 3;

/// Fold a headword or query into an accent- and case-insensitive key: lowercase,
/// Unicode-decompose (NFD), then drop the combining marks, so "Être", "être" and
/// "etre" all collapse to "etre". This is what gets indexed and queried, letting
/// an accent-free query match an accented headword.
fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Order prefix hits deterministically (they are otherwise constant-scored):
/// an exact accent-and-case match first, then an accent-insensitive exact match,
/// then the shortest completion, then alphabetical for stability. `folded` must
/// be `fold(query)`. Shared by every front-end so the Albert/CLI path gets the
/// same exact-first behaviour the GUI used to apply on its own.
fn rank_prefix_exact_first(query: &str, folded: &str, hits: &mut [SearchHit]) {
    let q_lower = query.to_lowercase();
    hits.sort_by_cached_key(|h| {
        let hw_lower = h.headword.to_lowercase();
        (
            hw_lower != q_lower,    // false (accent-and-case exact) sorts first
            fold(&h.headword) != folded, // then accent-insensitive exact
            h.headword.chars().count(), // then shortest completion
            hw_lower,               // then alphabetical
        )
    });
}

/// Signature of the enabled dictionary set (name, path, word count), prefixed
/// with [`INDEX_VERSION`], stored next to the index so we know whether the cached
/// index is still current. Changing which dictionaries are enabled (or their word
/// counts), or bumping the schema version, invalidates the cache and forces a
/// rebuild. Shared by the CLI and GUI so both invalidate identically.
pub fn index_signature(manager: &DictionaryManager) -> String {
    let mut lines: Vec<String> = manager
        .dictionaries()
        .iter()
        .filter(|d| d.enabled)
        .map(|d| {
            format!(
                "{}|{}|{}",
                d.name(),
                d.path.display(),
                d.dictionary.info.word_count
            )
        })
        .collect();
    lines.sort();
    format!("v{INDEX_VERSION}\n{}", lines.join("\n"))
}

/// Escape regex metacharacters so a literal prefix can be used in a
/// [`RegexQuery`] pattern.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{edit_distance, fold};

    #[test]
    fn fold_strips_case_and_accents() {
        assert_eq!(fold("Être"), "etre");
        assert_eq!(fold("CAFÉ"), "cafe");
        assert_eq!(fold("Niño"), "nino");
        assert_eq!(fold("Voilà"), "voila");
        // ASCII text is only lowercased.
        assert_eq!(fold("Hello"), "hello");
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("hello", "hello"), 0);
        assert_eq!(edit_distance("helo", "hello"), 1); // insertion
        assert_eq!(edit_distance("hallo", "hello"), 1); // substitution
        assert_eq!(edit_distance("hell", "hello"), 1); // deletion
        assert_eq!(edit_distance("ba", "baba"), 2);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("abc", ""), 3);
    }

    #[test]
    fn edit_distance_counts_adjacent_transposition_as_one() {
        assert_eq!(edit_distance("ba", "ab"), 1);
        // The classic "i before e" typo is a single transposition, not two subs.
        assert_eq!(edit_distance("recieve", "receive"), 1);
    }

    #[test]
    fn edit_distance_is_unicode_aware() {
        // Counts characters, not bytes: "é" is multi-byte but one char.
        assert_eq!(edit_distance("café", "cafe"), 1);
    }
}
