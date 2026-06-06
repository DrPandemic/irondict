use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use irondict_core::{
    bundled_gcide_path, search, Config, DictionaryConfig, DictionaryManager, SearchEngine,
    SearchMode,
};

/// Multi-dictionary lookup over StarDict dictionaries.
#[derive(Parser)]
#[command(name = "irondict", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Look up a word across all enabled dictionaries.
    Lookup {
        /// The word to look up.
        word: String,
    },
    /// Search the index across all enabled dictionaries.
    Search {
        /// The query to search for.
        query: String,
        /// How to match the query.
        #[arg(long, value_enum, default_value_t = Mode::FullText)]
        mode: Mode,
        /// Maximum number of results to return.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Add a StarDict dictionary (path to its `.ifo` file).
    Add {
        /// Path to the dictionary's `.ifo` file.
        path: PathBuf,
    },
    /// List the managed dictionaries.
    List,
    /// Remove a dictionary by name.
    Remove {
        /// The dictionary name (as shown by `list`).
        name: String,
    },
    /// Enable a dictionary by name.
    Enable {
        /// The dictionary name (as shown by `list`).
        name: String,
    },
    /// Disable a dictionary by name.
    Disable {
        /// The dictionary name (as shown by `list`).
        name: String,
    },
}

/// CLI mirror of [`SearchMode`].
#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    /// Exact (case-insensitive) headword match.
    Exact,
    /// Headword starts with the query.
    Prefix,
    /// Typo-tolerant headword match.
    Fuzzy,
    /// Free-text match across headwords and definitions.
    FullText,
}

impl From<Mode> for SearchMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Exact => SearchMode::Exact,
            Mode::Prefix => SearchMode::Prefix,
            Mode::Fuzzy => SearchMode::Fuzzy,
            Mode::FullText => SearchMode::FullText,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Lookup { word } => lookup(&word),
        Command::Search { query, mode, limit } => run_search(&query, mode.into(), limit),
        Command::Add { path } => add(path),
        Command::List => list(),
        Command::Remove { name } => remove(&name),
        Command::Enable { name } => set_enabled(&name, true),
        Command::Disable { name } => set_enabled(&name, false),
    }
}

/// Load the dictionary manager from the persisted config, seeding the bundled
/// GCIDE dictionary on first run. Per-dictionary load failures are reported as
/// warnings rather than aborting.
fn load_manager() -> Result<DictionaryManager> {
    let config_path = Config::default_path().context("locating config file")?;
    let first_run = !config_path.exists();

    let mut config = Config::load_from(&config_path).context("reading config")?;
    if first_run {
        config.dictionaries.push(DictionaryConfig {
            path: bundled_gcide_path(),
            enabled: true,
        });
        config
            .save_to(&config_path)
            .context("writing initial config")?;
    }

    let (manager, errors) = DictionaryManager::from_config(&config);
    for e in errors {
        eprintln!("warning: failed to load {}: {}", e.path.display(), e.error);
    }
    Ok(manager)
}

fn save_manager(manager: &DictionaryManager) -> Result<()> {
    manager.config().save().context("saving config")
}

fn lookup(word: &str) -> Result<()> {
    let mut manager = load_manager()?;
    let results = manager.lookup(word).context("looking up word")?;

    if results.is_empty() {
        println!("No results for \"{word}\".");
        return Ok(());
    }

    for result in results {
        println!("== {} ==", result.dictionary);
        for entry in result.entries {
            println!("{}", entry.headword);
            let definition: String = entry.segments.iter().map(|s| s.text.as_str()).collect();
            println!("{}", definition.trim_end());
            println!();
        }
    }
    Ok(())
}

/// The signature of the enabled dictionary set, written next to the index so we
/// know whether the cached index is still current. Changing which dictionaries
/// are enabled (or their word counts) invalidates the cache and forces a rebuild.
fn index_signature(manager: &DictionaryManager) -> String {
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
    lines.join("\n")
}

/// Open the cached search index if it matches the current dictionary set,
/// otherwise (re)build it. The index lives under the OS cache dir.
fn build_or_open_index(manager: &mut DictionaryManager) -> Result<SearchEngine> {
    let dir = search::default_index_dir().context("locating index directory")?;
    let manifest = dir.join("manifest");
    let signature = index_signature(manager);

    let cached = std::fs::read_to_string(&manifest).ok();
    if cached.as_deref() == Some(signature.as_str()) {
        if let Ok(engine) = SearchEngine::open(&dir) {
            return Ok(engine);
        }
    }

    eprintln!("Building search index...");
    let engine = SearchEngine::build(&dir, manager).context("building search index")?;
    std::fs::write(&manifest, &signature).context("writing index manifest")?;
    Ok(engine)
}

fn run_search(query: &str, mode: SearchMode, limit: usize) -> Result<()> {
    let mut manager = load_manager()?;
    let engine = build_or_open_index(&mut manager)?;
    let hits = engine.search(query, mode, limit).context("searching")?;

    if hits.is_empty() {
        println!("No results for \"{query}\".");
        return Ok(());
    }

    for hit in hits {
        println!(
            "{}  [{}]  (score {:.2})",
            hit.headword, hit.dictionary, hit.score
        );
    }
    Ok(())
}

fn add(path: PathBuf) -> Result<()> {
    let mut manager = load_manager()?;
    let dict = manager
        .add(&path)
        .with_context(|| format!("loading dictionary at {}", path.display()))?;
    let name = dict.name().to_string();
    let word_count = dict.dictionary.info.word_count;

    save_manager(&manager)?;
    println!(
        "Added \"{name}\" ({word_count} words) from {}",
        path.display()
    );
    Ok(())
}

fn list() -> Result<()> {
    let manager = load_manager()?;
    let dicts = manager.dictionaries();
    if dicts.is_empty() {
        println!("No dictionaries configured.");
        return Ok(());
    }

    for d in dicts {
        let state = if d.enabled { "enabled" } else { "disabled" };
        println!(
            "{} [{state}] — {} words ({})",
            d.name(),
            d.dictionary.info.word_count,
            d.path.display()
        );
    }
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    let mut manager = load_manager()?;
    if manager.remove(name) {
        save_manager(&manager)?;
        println!("Removed \"{name}\".");
        Ok(())
    } else {
        anyhow::bail!("no dictionary named \"{name}\"");
    }
}

fn set_enabled(name: &str, enabled: bool) -> Result<()> {
    let mut manager = load_manager()?;
    if manager.set_enabled(name, enabled) {
        save_manager(&manager)?;
        let state = if enabled { "Enabled" } else { "Disabled" };
        println!("{state} \"{name}\".");
        Ok(())
    } else {
        anyhow::bail!("no dictionary named \"{name}\"");
    }
}
