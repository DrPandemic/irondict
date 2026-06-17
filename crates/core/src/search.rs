use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use rayon::prelude::*;
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

/// Tantivy schema field handles for a single-dictionary index. The source
/// dictionary is no longer a field: each index holds exactly one dictionary
/// (PLAN.md §1a), so the name is tracked alongside the index ([`DictIndex`])
/// rather than stored per document.
#[derive(Clone, Copy)]
struct Fields {
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
/// are neither indexed nor stored). Built one tantivy index per dictionary
/// (PLAN.md §1a) under `index/<dict-id>/`, so adding, removing, or updating a
/// single dictionary only (re)builds that one — the others are opened from
/// cache. At query time every relevant per-dict index is searched and the
/// results are merged into one ranked list.
pub struct SearchEngine {
    indexes: Vec<DictIndex>,
}

/// One dictionary's on-disk tantivy index, tagged with the dictionary's name so
/// hits can report their source without storing it per document.
struct DictIndex {
    name: String,
    reader: IndexReader,
    fields: Fields,
}

/// Filename of the per-dict signature sidecar written next to each index, used
/// to decide whether that dictionary's cached index is still current.
const MANIFEST: &str = "manifest";

fn map_err<E: std::error::Error + Send + Sync + 'static>(e: E) -> Error {
    Error::Search(Box::new(e))
}

/// Directory under the OS cache dir that holds the per-dictionary indexes
/// (e.g. `~/.cache/irondict/index` on Linux); each dictionary gets its own
/// `<dict-id>/` subdirectory inside it.
pub fn default_index_dir() -> Result<PathBuf, Error> {
    let dirs = ProjectDirs::from("", "", "irondict").ok_or(Error::NoConfigDir)?;
    Ok(dirs.cache_dir().join("index"))
}

/// Stable, filesystem-safe subdirectory name for a dictionary's index, derived
/// from its name and path (the unique key). The hash only has to be stable
/// within a build of the binary — if it ever changes, the old directory is
/// pruned and the dictionary is simply re-indexed.
fn dict_id(name: &str, path: &Path) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    path.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Per-dictionary cache signature (name, path, word count), prefixed with
/// [`INDEX_VERSION`]. Written to the dict's `MANIFEST`; a mismatch (the dict
/// changed, or the schema version was bumped) forces just that dictionary's
/// index to be rebuilt.
fn dict_signature(name: &str, path: &Path, word_count: usize) -> String {
    format!("v{INDEX_VERSION}\n{name}|{}|{word_count}", path.display())
}

fn manifest_matches(dir: &Path, signature: &str) -> bool {
    std::fs::read_to_string(dir.join(MANIFEST)).ok().as_deref() == Some(signature)
}

impl SearchEngine {
    /// Build (or incrementally refresh) the per-dictionary indexes under `root`
    /// from the manager's enabled dictionaries.
    pub fn build(root: &Path, manager: &mut DictionaryManager) -> Result<Self, Error> {
        Self::build_cancellable(root, manager, || false, |_| {})
            .map(|engine| engine.expect("a build that never cancels always yields an engine"))
    }

    /// Open the cached per-dictionary indexes under `root` for the manager's
    /// enabled dictionaries, without building anything. Errors if any enabled
    /// dictionary's index is missing or stale, so the caller falls back to
    /// [`build`](Self::build) (which then rebuilds only the stale ones).
    pub fn open(root: &Path, manager: &DictionaryManager) -> Result<Self, Error> {
        let mut indexes = Vec::new();
        for d in manager.dictionaries().iter().filter(|d| d.enabled) {
            let dir = root.join(dict_id(d.name(), &d.path));
            let signature = dict_signature(d.name(), &d.path, d.dictionary.info.word_count);
            if !manifest_matches(&dir, &signature) {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "cached index is missing or stale",
                )));
            }
            indexes.push(DictIndex::open(&dir, d.name().to_string())?);
        }
        Ok(Self { indexes })
    }

    /// Like [`build`](Self::build), but `cancel` is polled periodically while
    /// indexing; when it returns `true` the build is abandoned and `Ok(None)` is
    /// returned. Already-committed per-dict indexes are left in place (a later
    /// build reuses them), so a cancel costs at most the dictionary in flight.
    /// Used by the GUI so deleting or changing a dictionary stops an in-flight
    /// build instead of finishing a now-stale one. `progress` is called at the
    /// same cadence with the running count across the dictionaries being built.
    pub fn build_cancellable(
        root: &Path,
        manager: &mut DictionaryManager,
        mut cancel: impl FnMut() -> bool,
        mut progress: impl FnMut(IndexProgress),
    ) -> Result<Option<Self>, Error> {
        std::fs::create_dir_all(root)?;

        // Snapshot the enabled dictionaries' identities up front so we can decide
        // what to (re)build without holding a borrow on the manager (the build
        // loop below needs `&mut` to iterate one dictionary's entries).
        let enabled: Vec<(String, PathBuf, usize)> = manager
            .dictionaries()
            .iter()
            .filter(|d| d.enabled)
            .map(|d| {
                (
                    d.name().to_string(),
                    d.path.clone(),
                    d.dictionary.info.word_count,
                )
            })
            .collect();

        // Open every dictionary whose cached index is current; collect the rest
        // (stale or missing) to rebuild. `live_ids` is every enabled dict's id,
        // used afterwards to prune indexes of dictionaries no longer enabled.
        let mut indexes: Vec<DictIndex> = Vec::new();
        let mut to_build: Vec<(String, PathBuf, usize)> = Vec::new();
        let mut live_ids: HashSet<String> = HashSet::new();
        let mut total: u64 = 0;
        for (name, path, word_count) in &enabled {
            let id = dict_id(name, path);
            live_ids.insert(id.clone());
            let dir = root.join(&id);
            let signature = dict_signature(name, path, *word_count);
            if manifest_matches(&dir, &signature) {
                if let Ok(idx) = DictIndex::open(&dir, name.clone()) {
                    indexes.push(idx);
                    continue;
                }
            }
            total += *word_count as u64;
            to_build.push((name.clone(), path.clone(), *word_count));
        }

        // (Re)build the stale/missing dictionaries, one tantivy index each.
        let mut seen: u64 = 0;
        for (name, path, word_count) in &to_build {
            let dir = root.join(dict_id(name, path));
            // A previous partial/stale build may have left files; clear so
            // `create_in_dir` (which refuses a non-empty dir) succeeds.
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
            }
            std::fs::create_dir_all(&dir)?;
            let (schema, fields) = schema();
            let index = Index::create_in_dir(&dir, schema).map_err(map_err)?;
            let mut writer = index.writer(50_000_000).map_err(map_err)?;
            let mut indexing_error = None;
            let mut cancelled = false;
            // Poll `cancel` and report `progress` on the first entry and then once
            // per batch, so the checks stay negligible on large dictionaries while
            // still catching a cancellation early on small ones.
            manager.for_each_entry_in(path, |entry| {
                seen += 1;
                if seen % 4096 == 1 {
                    if cancel() {
                        cancelled = true;
                        return ControlFlow::Break(());
                    }
                    progress(IndexProgress {
                        indexed: seen,
                        total,
                    });
                }
                // Index only the headword (folded for matching, original for
                // display). Definitions are never searched and no longer stored.
                let result = writer.add_document(doc!(
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
                // Drop the writer without committing; this dictionary's dir is
                // left without a manifest, so the next build treats it as stale
                // and rebuilds it. Already-finished dicts stay cached.
                drop(writer);
                return Ok(None);
            }
            writer.commit().map_err(map_err)?;
            std::fs::write(dir.join(MANIFEST), dict_signature(name, path, *word_count))?;
            let reader = index.reader().map_err(map_err)?;
            indexes.push(DictIndex {
                name: name.clone(),
                reader,
                fields,
            });
        }

        prune_orphans(root, &live_ids)?;
        Ok(Some(Self { indexes }))
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
    /// results are restricted to that single source dictionary — which now means
    /// searching only that dictionary's index instead of filtering a shared one.
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
        let targets: Vec<&DictIndex> = self
            .indexes
            .iter()
            .filter(|idx| dictionary.is_none_or(|name| idx.name == name))
            .collect();
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        if mode == SearchMode::Fuzzy {
            // Each index contributes its closest candidates (with their true edit
            // distance); merge and re-rank globally so the order matches what a
            // single shared index would have produced. Searched in parallel across
            // the per-dict indexes.
            let lower = fold(query);
            let len = lower.chars().count();
            let max_distance: u8 = if len <= 1 { 0 } else { 2 };
            let first = lower.chars().next();
            let mut scored: Vec<(usize, usize, SearchHit)> = targets
                .par_iter()
                .try_fold(Vec::new, |mut acc, idx| {
                    idx.fuzzy_candidates(&lower, max_distance, first, limit, &mut acc)?;
                    Ok::<_, Error>(acc)
                })
                .try_reduce(Vec::new, |mut a, b| {
                    a.extend(b);
                    Ok::<_, Error>(a)
                })?;
            // Closest first, then shorter headwords, then alphabetical for stability.
            scored.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then(a.1.cmp(&b.1))
                    .then_with(|| a.2.headword.to_lowercase().cmp(&b.2.headword.to_lowercase()))
            });
            return Ok(scored.into_iter().take(limit).map(|(_, _, h)| h).collect());
        }

        debug_assert_eq!(mode, SearchMode::Prefix);
        let folded = fold(query);
        let mut hits: Vec<SearchHit> = targets
            .par_iter()
            .try_fold(Vec::new, |mut acc, idx| {
                idx.prefix_hits(&folded, limit, &mut acc)?;
                Ok::<_, Error>(acc)
            })
            .try_reduce(Vec::new, |mut a, b| {
                a.extend(b);
                Ok::<_, Error>(a)
            })?;
        // Prefix/exact are constant-scored, so impose a deterministic order in
        // Rust (shared by every front-end): exact accent-and-case match first,
        // then accent-insensitive exact, then shortest completion, then alpha.
        // Merging per-dict lists and re-ranking yields the same total order a
        // single shared index would have produced.
        rank_prefix_exact_first(query, &folded, &mut hits);
        hits.truncate(limit);
        Ok(hits)
    }
}

impl DictIndex {
    /// Open a dictionary's index from its directory, tagged with `name`.
    fn open(dir: &Path, name: String) -> Result<Self, Error> {
        let index = Index::open_in_dir(dir).map_err(map_err)?;
        let (_, fields) = schema();
        let reader = index.reader().map_err(map_err)?;
        Ok(Self {
            name,
            reader,
            fields,
        })
    }

    /// Append up to `limit` prefix matches for the already-folded `folded` query
    /// to `out`. OR an exact term with the prefix regex: tantivy's RegexQuery DFA
    /// can miss a term equal to the literal prefix (e.g. `go.*` not matching
    /// "go"), and matching both clauses also lets the exact headword out-score
    /// partial completions. Scores are constant here; the caller imposes order.
    fn prefix_hits(
        &self,
        folded: &str,
        limit: usize,
        out: &mut Vec<SearchHit>,
    ) -> Result<(), Error> {
        let pattern = format!("{}.*", regex_escape(folded));
        let regex = RegexQuery::from_pattern(&pattern, self.fields.key_folded).map_err(map_err)?;
        let exact = TermQuery::new(
            Term::from_field_text(self.fields.key_folded, folded),
            IndexRecordOption::Basic,
        );
        let query = BooleanQuery::new(vec![
            (Occur::Should, Box::new(regex) as Box<dyn Query>),
            (Occur::Should, Box::new(exact)),
        ]);
        let searcher = self.reader.searcher();
        let docs = searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())
            .map_err(map_err)?;
        for (score, addr) in docs {
            let doc: tantivy::TantivyDocument = searcher.doc(addr).map_err(map_err)?;
            out.push(SearchHit {
                dictionary: self.name.clone(),
                headword: stored_text(&doc, self.fields.headword),
                score,
            });
        }
        Ok(())
    }

    /// Append this index's closest fuzzy candidates to `scored` as
    /// `(edit_distance, headword_len, hit)` tuples for the caller to merge and
    /// rank. Mirrors the single-index fuzzy retrieval:
    ///
    /// tantivy's [`FuzzyTermQuery`] is constant-scored (every match scores the
    /// same), so on its own the top-N would be an arbitrary slice of all matches
    /// — a perfect match can fall outside the limit. To avoid that we:
    ///
    /// 1. cap the allowed edit distance by query length, so short, ambiguous
    ///    queries don't match a large fraction of the dictionary (length guard);
    /// 2. stack an exact + distance-1 + distance-2 query with widely separated
    ///    boosts, so closer matches both *retrieve* first and rank first;
    /// 3. over-fetch and compute the true edit distance (transposition-aware),
    ///    with a first-character anchor as a prefix guard.
    fn fuzzy_candidates(
        &self,
        lower: &str,
        max_distance: u8,
        first: Option<char>,
        limit: usize,
        scored: &mut Vec<(usize, usize, SearchHit)>,
    ) -> Result<(), Error> {
        let exact = Term::from_field_text(self.fields.key_folded, lower);
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
        let query = BooleanQuery::new(clauses);

        // Over-fetch so the re-rank has room to find the closest matches even
        // when there are many equally-scored fuzzy candidates.
        let fetch = limit.saturating_mul(4).max(64);
        let searcher = self.reader.searcher();
        let docs = searcher
            .search(&query, &TopDocs::with_limit(fetch).order_by_score())
            .map_err(map_err)?;

        for (_score, addr) in docs {
            let doc: tantivy::TantivyDocument = searcher.doc(addr).map_err(map_err)?;
            let headword = stored_text(&doc, self.fields.headword);
            let hw_folded = fold(&headword);
            // Prefix guard: keep only candidates sharing the query's first
            // (folded) character — legitimate typo corrections rarely change it.
            if hw_folded.chars().next() != first {
                continue;
            }
            let distance = edit_distance(lower, &hw_folded);
            let len = hw_folded.chars().count();
            scored.push((
                distance,
                len,
                SearchHit {
                    dictionary: self.name.clone(),
                    headword,
                    // A distance-based score so closer matches read as more relevant.
                    score: 1.0 / (1.0 + distance as f32),
                },
            ));
        }
        Ok(())
    }
}

/// Remove index subdirectories under `root` that don't belong to a currently
/// enabled dictionary, plus any stray files left by a previous (e.g. single-
/// index) layout. Keeps the cache from accumulating orphans when a dictionary
/// is disabled or removed.
fn prune_orphans(root: &Path, live_ids: &HashSet<String>) -> Result<(), Error> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let keep = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| live_ids.contains(n));
        if keep {
            continue;
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
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
const INDEX_VERSION: u32 = 4;

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
