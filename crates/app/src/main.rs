use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};

use irondict_core::{
    bundled_gcide_config, download, search, Config, Conjugation, ConjugatorRegistry,
    DictionaryManager, Language, Progress, SearchEngine, SearchMode,
};

mod gui;

/// Multi-dictionary lookup over StarDict dictionaries.
///
/// Run a subcommand for the command-line front-end, or pass `--gui` to launch
/// the graphical interface.
#[derive(Parser)]
#[command(name = "irondict", version, about)]
struct Cli {
    /// Launch the graphical interface instead of running a command.
    #[arg(long)]
    gui: bool,
    /// With `--gui`, open straight to this word's definition.
    #[arg(long, value_name = "WORD")]
    word: Option<String>,
    /// With `--gui`, restrict the view to this dictionary (its name as shown by
    /// `list`). Pairs with `--word` to open it scoped to that dictionary.
    #[arg(long, value_name = "NAME")]
    dict: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
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
        #[arg(long, value_enum, default_value_t = Mode::Prefix)]
        mode: Mode,
        /// Maximum number of results to return.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Restrict results to this dictionary (its name as shown by `list`).
        #[arg(long, value_name = "NAME")]
        dict: Option<String>,
    },
    /// Conjugate a verb, sourcing forms from the loaded dictionaries.
    Conjugate {
        /// The verb to conjugate.
        verb: String,
        /// Force a language instead of auto-detecting from the dictionaries.
        #[arg(long, value_enum)]
        lang: Option<Lang>,
    },
    /// Add a StarDict dictionary (path to its `.ifo` file).
    Add {
        /// Path to the dictionary's `.ifo` file.
        path: PathBuf,
    },
    /// List the dictionaries available to download.
    Catalog,
    /// Download and install a dictionary from the catalog by id (e.g. `en-en`).
    Install {
        /// Catalog id, as shown by `catalog`.
        id: String,
    },
    /// Delete an installed catalog dictionary (files + registration).
    Uninstall {
        /// Catalog id, as shown by `catalog`.
        id: String,
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
    /// Headword starts with the query; the exact match ranks first.
    Prefix,
    /// Typo-tolerant headword match.
    Fuzzy,
}

impl From<Mode> for SearchMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Prefix => SearchMode::Prefix,
            Mode::Fuzzy => SearchMode::Fuzzy,
        }
    }
}

/// CLI mirror of the conjugation [`Language`] choices.
#[derive(Clone, Copy, ValueEnum)]
enum Lang {
    En,
    Fr,
}

impl From<Lang> for Language {
    fn from(lang: Lang) -> Self {
        match lang {
            Lang::En => Language::English,
            Lang::Fr => Language::French,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.gui {
        return gui::run(cli.word, cli.dict).map_err(|e| anyhow::anyhow!("running GUI: {e}"));
    }

    match cli.command {
        Some(Command::Lookup { word }) => lookup(&word),
        Some(Command::Conjugate { verb, lang }) => conjugate(&verb, lang.map(Into::into)),
        Some(Command::Search { query, mode, limit, dict }) => {
            run_search(&query, mode.into(), limit, dict.as_deref())
        }
        Some(Command::Add { path }) => add(path),
        Some(Command::Catalog) => catalog(),
        Some(Command::Install { id }) => install(&id),
        Some(Command::Uninstall { id }) => uninstall(&id),
        Some(Command::List) => list(),
        Some(Command::Remove { name }) => remove(&name),
        Some(Command::Enable { name }) => set_enabled(&name, true),
        Some(Command::Disable { name }) => set_enabled(&name, false),
        // No subcommand and no `--gui`: show usage rather than doing nothing.
        None => {
            Cli::command().print_help().context("printing help")?;
            println!();
            Ok(())
        }
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
        config.dictionaries.push(bundled_gcide_config());
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

/// Conjugate `verb`, drawing forms from the loaded dictionaries. When `lang` is
/// given it forces that backend; otherwise routing follows each dictionary's
/// pinned language (default Auto, which detects from the entry).
fn conjugate(verb: &str, lang: Option<Language>) -> Result<()> {
    let mut manager = load_manager()?;
    let results = manager.lookup(verb).context("looking up verb")?;
    // Map each source dictionary to its pinned language (borrow after lookup).
    let langs: HashMap<String, Language> = manager
        .dictionaries()
        .iter()
        .map(|d| (d.name().to_string(), d.language))
        .collect();

    let definitions: Vec<(Language, String)> = results
        .iter()
        .map(|r| {
            let lang = langs.get(&r.dictionary).copied().unwrap_or(Language::Auto);
            let text: String = r
                .entries
                .iter()
                .flat_map(|e| e.segments.iter())
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            (lang, text)
        })
        .collect();

    let reg = ConjugatorRegistry::new();
    let conjugation = match lang {
        // Forced language: parse from any available definition (or none).
        Some(forced) => {
            let def = definitions.first().map(|(_, t)| t.as_str());
            reg.conjugate(verb, def, forced)
        }
        // Auto: try each dictionary's definition under its pinned language and
        // accept the first recognized verb.
        None => definitions
            .iter()
            .find_map(|(dl, text)| reg.conjugate(verb, Some(text), *dl)),
    };

    match conjugation {
        Some(c) => {
            print_conjugation(&c);
            Ok(())
        }
        None => {
            println!("No conjugation found for \"{verb}\".");
            Ok(())
        }
    }
}

fn print_conjugation(c: &Conjugation) {
    let lang = match c.language {
        Language::English => "English",
        Language::French => "French",
        Language::Italian => "Italian",
        Language::Auto => "",
    };
    println!("{} ({lang})", c.infinitive);
    let multi = c.sections.len() > 1;
    for sec in &c.sections {
        if multi || !sec.label.is_empty() {
            println!("\n{}", sec.label);
        }
        for f in &sec.forms {
            if f.label.is_empty() {
                println!("  {}", f.text);
            } else {
                println!("  {:<22} {}", f.label, f.text);
            }
        }
    }
}

/// Open the cached search index if it matches the current dictionary set,
/// otherwise (re)build it. The index lives under the OS cache dir.
fn build_or_open_index(manager: &mut DictionaryManager) -> Result<SearchEngine> {
    let dir = search::default_index_dir().context("locating index directory")?;
    let manifest = dir.join("manifest");
    let signature = search::index_signature(manager);

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

fn run_search(query: &str, mode: SearchMode, limit: usize, dict: Option<&str>) -> Result<()> {
    let mut manager = load_manager()?;
    let engine = build_or_open_index(&mut manager)?;
    let hits = engine
        .search_scoped(query, mode, limit, dict)
        .context("searching")?;

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

/// Format a byte count as a short human-readable size (e.g. `98.0 MB`).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn catalog() -> Result<()> {
    for entry in download::catalog() {
        let state = if download::is_installed(entry.id) {
            "installed"
        } else {
            "available"
        };
        println!(
            "{:<7} {:<26} ~{:>8}  [{state}]  {}",
            entry.id,
            entry.label,
            human_size(entry.approx_size),
            entry.license,
        );
    }
    Ok(())
}

/// Download a catalog dictionary, install it under the data dir, and register it
/// in the config (pinning its language) so it participates in lookups.
fn install(id: &str) -> Result<()> {
    let entry = download::find(id)
        .with_context(|| format!("no dictionary with id \"{id}\" in the catalog"))?;

    println!("Downloading {} ({})...", entry.label, entry.source);
    let mut last_pct = u8::MAX;
    let ifo = download::install(entry, |Progress::Downloading { received, total }| {
        if let Some(total) = total.filter(|t| *t > 0) {
            let pct = (received * 100 / total) as u8;
            if pct != last_pct {
                last_pct = pct;
                eprint!("\r  {pct:>3}%  ({} / {})", human_size(received), human_size(total));
            }
        }
    })
    .with_context(|| format!("installing {id}"))?;
    eprintln!();

    let mut manager = load_manager()?;
    let dict = manager
        .add(&ifo)
        .with_context(|| format!("loading installed dictionary at {}", ifo.display()))?;
    let name = dict.name().to_string();
    let word_count = dict.dictionary.info.word_count;
    manager.set_language(&name, entry.language);
    save_manager(&manager)?;

    println!("Installed \"{name}\" ({word_count} words) at {}", ifo.display());
    Ok(())
}

/// Unregister an installed catalog dictionary and delete its files from disk.
fn uninstall(id: &str) -> Result<()> {
    download::find(id).with_context(|| format!("no dictionary with id \"{id}\" in the catalog"))?;
    let ifo = download::installed_ifo(id)
        .with_context(|| format!("dictionary \"{id}\" is not installed"))?;

    let mut manager = load_manager()?;
    manager.remove_path(&ifo);
    save_manager(&manager)?;
    download::uninstall(id).with_context(|| format!("deleting files for {id}"))?;

    println!("Uninstalled \"{id}\".");
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
            "{} [{state}] [{}] — {} words ({})",
            d.name(),
            d.language.code(),
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
