use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use irondict_core::{bundled_gcide_path, Config, DictionaryConfig, DictionaryManager};

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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Lookup { word } => lookup(&word),
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
