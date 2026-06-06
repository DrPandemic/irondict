use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use tantivy::collector::TopDocs;
use tantivy::query::{FuzzyTermQuery, Query, QueryParser, RegexQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, Value, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexReader, Term};

use crate::manager::DictionaryManager;
use crate::Error;

/// How a query string is matched against the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Exact (case-insensitive) headword match.
    Exact,
    /// Headword starts with the query (autocomplete).
    Prefix,
    /// Typo-tolerant headword match (Levenshtein distance up to 2).
    Fuzzy,
    /// Free-text match across headwords and definitions (BM25 ranked).
    FullText,
}

/// One search result: which dictionary it came from, the matched headword, and
/// the relevance score (higher is better).
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
    /// Lowercased headword as a single token, for exact/prefix/fuzzy matching.
    key: Field,
    /// Original-case headword, tokenized for full-text and stored for display.
    headword: Field,
    /// Definition text, tokenized for full-text and stored for snippets.
    definition: Field,
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let fields = Fields {
        dictionary: builder.add_text_field("dictionary", STRING | STORED),
        key: builder.add_text_field("key", STRING),
        headword: builder.add_text_field("headword", TEXT | STORED),
        definition: builder.add_text_field("definition", TEXT | STORED),
    };
    (builder.build(), fields)
}

/// A search index over the headwords and definitions of the enabled
/// dictionaries. Backed by a tantivy index stored on disk so it can be cached
/// across runs and only rebuilt when the set of dictionaries changes.
pub struct SearchEngine {
    index: Index,
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

        let mut writer = index.writer(50_000_000).map_err(map_err)?;
        let mut indexing_error = None;
        manager.for_each_enabled_entry(|name, entry| {
            if indexing_error.is_some() {
                return;
            }
            let definition: String = entry
                .segments
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let result = writer.add_document(doc!(
                fields.dictionary => name,
                fields.key => entry.headword.to_lowercase(),
                fields.headword => entry.headword,
                fields.definition => definition,
            ));
            if let Err(e) = result {
                indexing_error = Some(map_err(e));
            }
        })?;
        if let Some(e) = indexing_error {
            return Err(e);
        }
        writer.commit().map_err(map_err)?;

        let reader = index.reader().map_err(map_err)?;
        Ok(Self {
            index,
            reader,
            fields,
        })
    }

    /// Open an index previously built in `dir`.
    pub fn open(dir: &Path) -> Result<Self, Error> {
        let index = Index::open_in_dir(dir).map_err(map_err)?;
        let (_, fields) = schema();
        let reader = index.reader().map_err(map_err)?;
        Ok(Self {
            index,
            reader,
            fields,
        })
    }

    /// Run `query` in the given `mode`, returning up to `limit` ranked hits.
    pub fn search(
        &self,
        query: &str,
        mode: SearchMode,
        limit: usize,
    ) -> Result<Vec<SearchHit>, Error> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let parsed: Box<dyn Query> = match mode {
            SearchMode::Exact => {
                let term = Term::from_field_text(self.fields.key, &query.to_lowercase());
                Box::new(TermQuery::new(term, IndexRecordOption::Basic))
            }
            SearchMode::Prefix => {
                let pattern = format!("{}.*", regex_escape(&query.to_lowercase()));
                Box::new(RegexQuery::from_pattern(&pattern, self.fields.key).map_err(map_err)?)
            }
            SearchMode::Fuzzy => {
                let term = Term::from_field_text(self.fields.key, &query.to_lowercase());
                Box::new(FuzzyTermQuery::new(term, 2, true))
            }
            SearchMode::FullText => {
                let parser = QueryParser::for_index(
                    &self.index,
                    vec![self.fields.headword, self.fields.definition],
                );
                let (parsed, _errors) = parser.parse_query_lenient(query);
                parsed
            }
        };

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
        Ok(hits)
    }
}

fn stored_text(doc: &tantivy::TantivyDocument, field: Field) -> String {
    doc.get_first(field)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
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
