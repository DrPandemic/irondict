// Phase 6 step 2: the "toolbar layout" wired to the real backend
// (DictionaryManager + SearchEngine over the bundled GCIDE).
//
// The search index takes a few seconds to build on first run, so it is built on
// a worker thread; the window appears immediately (showing the word of the
// moment) and search becomes live once the index is ready.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use slint::{Color, ModelRc, SharedString, Timer, TimerMode, VecModel};

use irondict_core::{
    bundled_gcide_path, search, Config, DictionaryConfig, DictionaryManager, Language, Preferences,
    SearchEngine, SearchMode, ThemeMode,
};

/// Preset accent swatches offered in the settings page, in display order. Index
/// `n` here corresponds to `accent-choice == n + 1` in the UI (choice 0 = Auto).
const ACCENT_SWATCHES: [(u8, u8, u8); 6] = [
    (0x4f, 0x46, 0xe5), // indigo
    (0x35, 0x84, 0xe4), // blue
    (0x21, 0x90, 0xa4), // teal
    (0x3a, 0x94, 0x4a), // green
    (0xed, 0x5b, 0x00), // orange
    (0xd5, 0x61, 0x99), // pink
];

mod theme;

slint::include_modules!();

/// Backing data for a results row, so a click can resolve the full definition.
struct RowData {
    headword: String,
}

/// Load the manager from the persisted config, seeding bundled GCIDE on first
/// run (mirrors the CLI). Per-dictionary load failures are warnings, not fatal.
fn load_manager() -> DictionaryManager {
    let config = match Config::default_path() {
        Ok(path) => {
            if !path.exists() {
                let mut c = Config::default();
                c.dictionaries
                    .push(DictionaryConfig::new(bundled_gcide_path()));
                let _ = c.save_to(&path);
                c
            } else {
                Config::load_from(&path).unwrap_or_default()
            }
        }
        Err(_) => {
            let mut c = Config::default();
            c.dictionaries
                .push(DictionaryConfig::new(bundled_gcide_path()));
            c
        }
    };
    let (manager, errors) = DictionaryManager::from_config(&config);
    for e in errors {
        eprintln!("warning: failed to load {}: {}", e.path.display(), e.error);
    }
    manager
}

/// Signature of the enabled dictionary set, stored next to the index so we know
/// whether the cached index is current (mirrors the CLI). Enabling/disabling or
/// adding/removing a dictionary changes this and forces a rebuild.
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

/// Open the cached index when it matches the current (on-disk) dictionary set,
/// otherwise build it. Warmed before returning so the first keystroke is snappy.
fn open_or_build_index() -> Result<SearchEngine, String> {
    let dir = search::default_index_dir().map_err(|e| e.to_string())?;
    let mut manager = load_manager();
    let signature = index_signature(&manager);
    let cached = std::fs::read_to_string(dir.join("manifest")).ok();
    if cached.as_deref() == Some(signature.as_str()) {
        if let Ok(engine) = SearchEngine::open(&dir) {
            warm_up(&engine);
            return Ok(engine);
        }
    }
    build_engine(&dir, &mut manager, &signature)
}

/// Build the index from `manager`, write the manifest, and warm it.
fn build_engine(
    dir: &std::path::Path,
    manager: &mut DictionaryManager,
    signature: &str,
) -> Result<SearchEngine, String> {
    let engine = SearchEngine::build(dir, manager).map_err(|e| e.to_string())?;
    let _ = std::fs::write(dir.join("manifest"), signature);
    warm_up(&engine);
    Ok(engine)
}

/// Rebuild the index from the freshly-saved config (used after the user changes
/// the dictionary set). Loads its own manager so it can run on a worker thread.
fn rebuild_index() -> Result<SearchEngine, String> {
    let dir = search::default_index_dir().map_err(|e| e.to_string())?;
    let mut manager = load_manager();
    let signature = index_signature(&manager);
    build_engine(&dir, &mut manager, &signature)
}

/// Run throwaway queries so the first real search doesn't pay the one-time cost
/// of mmapping the term dictionary, opening segment/store readers, and building
/// the fuzzy automaton. Runs on the worker thread, off the UI thread.
fn warm_up(engine: &SearchEngine) {
    let _ = engine.search("a", SearchMode::Prefix, 80);
    let _ = engine.search("warmup", SearchMode::Fuzzy, 40);
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    // The accent (indigo) and light default live in the .slint; the OS values are
    // detected and applied below, off the startup path.

    let manager = Rc::new(RefCell::new(load_manager()));
    let engine: Rc<RefCell<Option<SearchEngine>>> = Rc::new(RefCell::new(None));
    let results_model: Rc<VecModel<ResultItem>> = Rc::new(VecModel::default());
    ui.set_results(ModelRc::from(results_model.clone()));
    let blocks_model: Rc<VecModel<DefBlock>> = Rc::new(VecModel::default());
    ui.set_def_blocks(ModelRc::from(blocks_model.clone()));
    let rows: Rc<RefCell<Vec<RowData>>> = Rc::new(RefCell::new(Vec::new()));
    let dict_items: Rc<VecModel<DictRow>> = Rc::new(VecModel::default());
    ui.set_dict_items(ModelRc::from(dict_items.clone()));
    let scopes: Rc<VecModel<SharedString>> = Rc::new(VecModel::default());
    ui.set_scopes(ModelRc::from(scopes.clone()));

    // Scope control ("All" + enabled dictionaries) and the settings dictionary
    // list are both derived from the manager.
    refresh_lists(&ui, &manager, &dict_items, &scopes);

    // Appearance settings: accent swatches + the persisted theme/accent choices.
    let swatches: Vec<Color> = ACCENT_SWATCHES
        .iter()
        .map(|&(r, g, b)| Color::from_rgb_u8(r, g, b))
        .collect();
    ui.set_accent_swatches(ModelRc::from(Rc::new(VecModel::from(swatches))));
    {
        let prefs = manager.borrow().preferences().clone();
        ui.set_theme_mode(theme_mode_index(prefs.theme_mode));
        ui.set_accent_choice(accent_choice_index(&prefs));
    }

    // Restore the last-used dictionary scope (refresh_lists reset it to "All").
    {
        let last = manager.borrow().preferences().last_scope.clone();
        ui.set_scope(scope_index_for(&manager, last.as_deref()));
    }

    // Apply the theme (persisted override, else OS detection) without blocking
    // startup.
    apply_appearance(&ui, &manager);

    // Word of the moment: one fixed entry per launch (shown immediately, before
    // the index is ready).
    let wotm = {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0);
        manager
            .borrow()
            .dictionaries()
            .first()
            .and_then(|d| d.dictionary.nth_headword(seed))
            .unwrap_or_else(|| "irondict".to_string())
    };
    show_word(
        &ui,
        &manager,
        &blocks_model,
        &wotm,
        "WORD OF THE MOMENT",
        None,
    );

    // Build/open the index off the UI thread; deliver it via a channel. The
    // sender is kept (wrapped in an `Rc`) so settings changes can request a
    // rebuild on the same channel.
    let (tx, rx) = mpsc::channel::<Result<SearchEngine, String>>();
    let tx = Rc::new(tx);
    {
        let tx = mpsc::Sender::clone(&tx);
        std::thread::spawn(move || {
            let _ = tx.send(open_or_build_index());
        });
    }

    // Poll for the finished index and switch search on when it arrives.
    let index_timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let engine = engine.clone();
        let results_model = results_model.clone();
        let blocks_model = blocks_model.clone();
        let rows = rows.clone();
        index_timer.start(
            TimerMode::Repeated,
            Duration::from_millis(120),
            move || match rx.try_recv() {
                Ok(Ok(e)) => {
                    let ui = ui_weak.unwrap();
                    *engine.borrow_mut() = Some(e);
                    ui.set_index_ready(true);
                    let q = ui.get_query();
                    if !q.trim().is_empty() {
                        run_search(
                            &ui,
                            &manager,
                            &engine,
                            &results_model,
                            &blocks_model,
                            &rows,
                            &q,
                        );
                    }
                }
                Ok(Err(msg)) => {
                    let ui = ui_weak.unwrap();
                    show_message(&ui, &blocks_model, "Couldn't build index", &msg);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {}
            },
        );
    }

    // Live search as the user types.
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let engine = engine.clone();
        let results_model = results_model.clone();
        let blocks_model = blocks_model.clone();
        let rows = rows.clone();
        let wotm = wotm.clone();
        // Debounce: keep typing responsive by deferring the (render-heavy) search
        // until the user pauses, instead of running it on every keystroke.
        let debounce = Rc::new(Timer::default());
        ui.on_query_changed(move |q| {
            if q.trim().is_empty() {
                let ui = ui_weak.unwrap();
                debounce.stop();
                ui.set_searching(false);
                results_model.set_vec(Vec::new());
                rows.borrow_mut().clear();
                ui.set_selected_index(-1);
                show_word(
                    &ui,
                    &manager,
                    &blocks_model,
                    &wotm,
                    "WORD OF THE MOMENT",
                    None,
                );
                return;
            }
            let ui_weak = ui_weak.clone();
            let manager = manager.clone();
            let engine = engine.clone();
            let results_model = results_model.clone();
            let blocks_model = blocks_model.clone();
            let rows = rows.clone();
            debounce.start(
                TimerMode::SingleShot,
                Duration::from_millis(110),
                move || {
                    let ui = ui_weak.unwrap();
                    run_search(
                        &ui,
                        &manager,
                        &engine,
                        &results_model,
                        &blocks_model,
                        &rows,
                        &ui.get_query(),
                    );
                },
            );
        });
    }

    // Click a result to show its definition.
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let blocks_model = blocks_model.clone();
        let rows = rows.clone();
        ui.on_select(move |row| {
            let ui = ui_weak.unwrap();
            let headword = rows
                .borrow()
                .get(row.max(0) as usize)
                .map(|r| r.headword.clone());
            if let Some(headword) = headword {
                let filter = scope_filter(ui.get_scope(), &manager);
                ui.set_selected_index(row);
                show_word(
                    &ui,
                    &manager,
                    &blocks_model,
                    &headword,
                    "",
                    filter.as_deref(),
                );
            }
        });
    }

    // Scope change: switch the active dictionary and re-run the current query
    // (or fall back to the word of the moment when the search box is empty).
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let engine = engine.clone();
        let results_model = results_model.clone();
        let blocks_model = blocks_model.clone();
        let rows = rows.clone();
        let wotm = wotm.clone();
        ui.on_scope_changed(move |idx| {
            let ui = ui_weak.unwrap();
            // Ignore out-of-range scopes (e.g. Ctrl+5 with only two dictionaries).
            if idx < 0 || idx as usize > enabled_count(&manager) {
                return;
            }
            ui.set_scope(idx);
            // Remember the choice so the next launch reopens on this dictionary.
            let name = scope_filter(idx, &manager);
            manager.borrow_mut().preferences_mut().last_scope = name;
            save_config(&manager);
            let q = ui.get_query();
            if q.trim().is_empty() {
                show_word(
                    &ui,
                    &manager,
                    &blocks_model,
                    &wotm,
                    "WORD OF THE MOMENT",
                    None,
                );
            } else {
                run_search(
                    &ui,
                    &manager,
                    &engine,
                    &results_model,
                    &blocks_model,
                    &rows,
                    &q,
                );
            }
        });
    }

    // ---- settings: open / close ----
    {
        let ui_weak = ui.as_weak();
        ui.on_open_settings(move || ui_weak.unwrap().set_show_settings(true));
    }
    {
        let ui_weak = ui.as_weak();
        ui.on_close_settings(move || ui_weak.unwrap().set_show_settings(false));
    }

    // ---- settings: enable / disable a dictionary (rebuilds the index) ----
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        let tx = tx.clone();
        ui.on_toggle_dict(move |row| {
            let ui = ui_weak.unwrap();
            let name = nth_dict_name(&manager, row);
            if let Some(name) = name {
                let now = !is_enabled(&manager, &name);
                manager.borrow_mut().set_enabled(&name, now);
                save_config(&manager);
                refresh_lists(&ui, &manager, &dict_items, &scopes);
                request_rebuild(&ui, &tx);
            }
        });
    }

    // ---- settings: pin a dictionary's language (no reindex needed) ----
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        ui.on_set_dict_language(move |row, lang| {
            let ui = ui_weak.unwrap();
            if let Some(name) = nth_dict_name(&manager, row) {
                manager
                    .borrow_mut()
                    .set_language(&name, index_to_lang(lang));
                save_config(&manager);
                refresh_lists(&ui, &manager, &dict_items, &scopes);
            }
        });
    }

    // ---- settings: remove a dictionary (rebuilds the index) ----
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        let tx = tx.clone();
        ui.on_remove_dict(move |row| {
            let ui = ui_weak.unwrap();
            if let Some(name) = nth_dict_name(&manager, row) {
                manager.borrow_mut().remove(&name);
                save_config(&manager);
                refresh_lists(&ui, &manager, &dict_items, &scopes);
                request_rebuild(&ui, &tx);
            }
        });
    }

    // ---- settings: add a dictionary via the native (portal) file picker ----
    // The dialog runs on a worker thread; the chosen path comes back over a
    // channel and is processed on the UI thread by `add_timer`.
    let (path_tx, path_rx) = mpsc::channel::<PathBuf>();
    {
        ui.on_add_dict(move || {
            let path_tx = path_tx.clone();
            std::thread::spawn(move || {
                if let Some(file) = rfd::FileDialog::new()
                    .add_filter("StarDict", &["ifo"])
                    .set_title("Add a StarDict dictionary")
                    .pick_file()
                {
                    let _ = path_tx.send(file);
                }
            });
        });
    }
    let add_timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        let tx = tx.clone();
        add_timer.start(TimerMode::Repeated, Duration::from_millis(150), move || {
            let Ok(path) = path_rx.try_recv() else {
                return;
            };
            let ui = ui_weak.unwrap();
            let added = manager.borrow_mut().add(&path).map(|_| ());
            match added {
                Ok(()) => {
                    save_config(&manager);
                    refresh_lists(&ui, &manager, &dict_items, &scopes);
                    request_rebuild(&ui, &tx);
                }
                Err(e) => eprintln!("failed to add {}: {}", path.display(), e),
            }
        });
    }

    // ---- settings: theme mode + accent ----
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        ui.on_set_theme_mode(move |mode| {
            let ui = ui_weak.unwrap();
            manager.borrow_mut().preferences_mut().theme_mode = index_to_theme_mode(mode);
            ui.set_theme_mode(mode);
            save_config(&manager);
            apply_appearance(&ui, &manager);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        ui.on_set_accent(move |choice| {
            let ui = ui_weak.unwrap();
            let accent = if choice <= 0 {
                None
            } else {
                ACCENT_SWATCHES
                    .get(choice as usize - 1)
                    .map(|&(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"))
            };
            manager.borrow_mut().preferences_mut().accent = accent;
            ui.set_accent_choice(choice);
            save_config(&manager);
            apply_appearance(&ui, &manager);
        });
    }

    let r = ui.run();
    drop(index_timer);
    drop(add_timer);
    r
}

/// Trigger a background index rebuild, delivered over the engine channel and
/// hot-swapped in by `index_timer`. The old index keeps serving until it lands.
fn request_rebuild(ui: &AppWindow, tx: &Rc<mpsc::Sender<Result<SearchEngine, String>>>) {
    ui.set_index_ready(false);
    let tx = mpsc::Sender::clone(tx);
    std::thread::spawn(move || {
        let _ = tx.send(rebuild_index());
    });
}

/// Persist the manager's dictionaries + preferences to the config file.
fn save_config(manager: &Rc<RefCell<DictionaryManager>>) {
    if let Err(e) = manager.borrow().config().save() {
        eprintln!("failed to save config: {e}");
    }
}

/// Rebuild the scope control (All + enabled dictionaries) and the settings
/// dictionary list from the manager, resetting the active scope to All since the
/// indices may have shifted.
fn refresh_lists(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    dict_items: &Rc<VecModel<DictRow>>,
    scopes: &Rc<VecModel<SharedString>>,
) {
    let m = manager.borrow();
    let mut labels: Vec<SharedString> = vec!["All".into()];
    labels.extend(
        m.dictionaries()
            .iter()
            .filter(|d| d.enabled)
            .map(|d| pretty_dict_name(d.name()).into()),
    );
    scopes.set_vec(labels);

    let rows: Vec<DictRow> = m
        .dictionaries()
        .iter()
        .map(|d| DictRow {
            name: pretty_dict_name(d.name()).into(),
            words: group_thousands(d.dictionary.info.word_count).into(),
            enabled: d.enabled,
            language: lang_to_index(d.language),
        })
        .collect();
    dict_items.set_vec(rows);
    ui.set_scope(0);
}

/// Apply the persisted appearance (theme + accent), falling back to OS detection
/// for whatever the user left on "System"/"Auto".
fn apply_appearance(ui: &AppWindow, manager: &Rc<RefCell<DictionaryManager>>) {
    let prefs = manager.borrow().preferences().clone();
    let forced_dark = match prefs.theme_mode {
        ThemeMode::System => None,
        ThemeMode::Light => Some(false),
        ThemeMode::Dark => Some(true),
    };
    let forced_accent = prefs.accent.as_deref().and_then(theme::parse_hex);
    theme::apply_os_theme(ui.as_weak(), forced_dark, forced_accent);
}

/// Number of enabled dictionaries (the scope control's segment count, minus All).
fn enabled_count(manager: &Rc<RefCell<DictionaryManager>>) -> usize {
    manager
        .borrow()
        .dictionaries()
        .iter()
        .filter(|d| d.enabled)
        .count()
}

/// Name of the `row`-th managed dictionary (settings list order = manager order).
fn nth_dict_name(manager: &Rc<RefCell<DictionaryManager>>, row: i32) -> Option<String> {
    manager
        .borrow()
        .dictionaries()
        .get(row.max(0) as usize)
        .map(|d| d.name().to_string())
}

fn is_enabled(manager: &Rc<RefCell<DictionaryManager>>, name: &str) -> bool {
    manager
        .borrow()
        .dictionaries()
        .iter()
        .any(|d| d.name() == name && d.enabled)
}

fn lang_to_index(lang: Language) -> i32 {
    match lang {
        Language::Auto => 0,
        Language::English => 1,
        Language::French => 2,
    }
}

fn index_to_lang(index: i32) -> Language {
    match index {
        1 => Language::English,
        2 => Language::French,
        _ => Language::Auto,
    }
}

fn theme_mode_index(mode: ThemeMode) -> i32 {
    match mode {
        ThemeMode::System => 0,
        ThemeMode::Light => 1,
        ThemeMode::Dark => 2,
    }
}

fn index_to_theme_mode(index: i32) -> ThemeMode {
    match index {
        1 => ThemeMode::Light,
        2 => ThemeMode::Dark,
        _ => ThemeMode::System,
    }
}

/// Which accent chip is active: 0 = Auto (follow OS), else the matching swatch.
fn accent_choice_index(prefs: &Preferences) -> i32 {
    let Some(rgb) = prefs.accent.as_deref().and_then(theme::parse_hex) else {
        return 0;
    };
    ACCENT_SWATCHES
        .iter()
        .position(|&s| s == rgb)
        .map(|i| i as i32 + 1)
        .unwrap_or(0)
}

/// Format a word count with thousands separators (e.g. `174222` → `174,222`).
fn group_thousands(n: usize) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Map a scope index to the dictionary it restricts results to: index 0 ("All")
/// and any out-of-range index mean no restriction; index `n` selects the
/// `n-1`th *enabled* dictionary (the scope control lists only enabled ones).
fn scope_filter(scope: i32, manager: &Rc<RefCell<DictionaryManager>>) -> Option<String> {
    if scope <= 0 {
        return None;
    }
    manager
        .borrow()
        .dictionaries()
        .iter()
        .filter(|d| d.enabled)
        .nth(scope as usize - 1)
        .map(|d| d.name().to_string())
}

/// The scope index for a dictionary `name` (`None` = "All"). Falls back to "All"
/// (0) when the named dictionary is gone or disabled.
fn scope_index_for(manager: &Rc<RefCell<DictionaryManager>>, name: Option<&str>) -> i32 {
    let Some(name) = name else {
        return 0;
    };
    manager
        .borrow()
        .dictionaries()
        .iter()
        .filter(|d| d.enabled)
        .position(|d| d.name() == name)
        .map(|p| p as i32 + 1)
        .unwrap_or(0)
}

/// A friendlier label for a StarDict bookname (e.g. the bundled
/// `dictd_www.dict.org_gcide` shows up as `GCIDE`).
fn pretty_dict_name(name: &str) -> String {
    if name.to_lowercase().contains("gcide") {
        "GCIDE".to_string()
    } else {
        name.to_string()
    }
}

/// Show a plain text message (no entry) in the definition pane.
fn show_message(ui: &AppWindow, blocks_model: &Rc<VecModel<DefBlock>>, title: &str, body: &str) {
    ui.set_section_label("".into());
    ui.set_def_headword(title.into());
    ui.set_def_pron("".into());
    ui.set_def_pos("".into());
    blocks_model.set_vec(vec![DefBlock {
        number: 0,
        text: body.into(),
    }]);
    ui.set_def_source("".into());
}

/// Run a query through the engine and update the list + definition pane.
fn run_search(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    engine: &Rc<RefCell<Option<SearchEngine>>>,
    results_model: &Rc<VecModel<ResultItem>>,
    blocks_model: &Rc<VecModel<DefBlock>>,
    rows: &Rc<RefCell<Vec<RowData>>>,
    query: &str,
) {
    let needle = query.trim();
    let filter = scope_filter(ui.get_scope(), manager);
    let eng_ref = engine.borrow();
    let Some(eng) = eng_ref.as_ref() else {
        show_message(
            ui,
            blocks_model,
            "Preparing dictionary…",
            "Building the search index — one moment.",
        );
        return;
    };

    // Prefix (autocomplete) first; fall back to fuzzy for typos. Both are scoped
    // to the selected dictionary (or all, when no scope is active).
    let mut hits = eng
        .search_scoped(needle, SearchMode::Prefix, 80, filter.as_deref())
        .unwrap_or_default();
    if hits.is_empty() {
        // Fuzzy hits are already ranked by edit distance — keep that order.
        hits = eng
            .search_scoped(needle, SearchMode::Fuzzy, 40, filter.as_deref())
            .unwrap_or_default();
    } else {
        // Prefix hits are unranked; show the shortest (closest) completion first.
        hits.sort_by(|a, b| {
            a.headword
                .chars()
                .count()
                .cmp(&b.headword.chars().count())
                .then_with(|| a.headword.to_lowercase().cmp(&b.headword.to_lowercase()))
        });
    }
    hits.truncate(40);

    let mut items = Vec::with_capacity(hits.len());
    let mut rd = Vec::with_capacity(hits.len());
    for h in &hits {
        items.push(ResultItem {
            headword: h.headword.clone().into(),
            snippet: make_snippet(&h.snippet).into(),
            source: h.dictionary.clone().into(),
        });
        rd.push(RowData {
            headword: h.headword.clone(),
        });
    }
    results_model.set_vec(items);
    *rows.borrow_mut() = rd;
    ui.set_searching(true);

    if let Some(first) = hits.first() {
        ui.set_selected_index(0);
        show_word(
            ui,
            manager,
            blocks_model,
            &first.headword,
            "",
            filter.as_deref(),
        );
    } else {
        ui.set_selected_index(-1);
        show_message(
            ui,
            blocks_model,
            "No results",
            &format!("Nothing matches \u{201c}{needle}\u{201d}."),
        );
    }
}

/// Resolve `headword` to its full definition, parse it, and show it.
fn show_word(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    blocks_model: &Rc<VecModel<DefBlock>>,
    headword: &str,
    label: &str,
    filter: Option<&str>,
) {
    let (raw, source, html) = lookup_raw(manager, headword, filter);
    if raw.is_empty() {
        show_message(ui, blocks_model, headword, "No definition found.");
        ui.set_section_label(label.into());
        return;
    }

    ui.set_section_label(label.into());
    ui.set_def_headword(headword.into());
    ui.set_def_source(source.into());

    let blocks: Vec<DefBlock> = if html {
        // HTML entry (e.g. Petit Robert): no GCIDE pos/pron. Convert to plain-text
        // paragraphs so tags don't show, and so the body can be virtualized.
        ui.set_def_pron("".into());
        ui.set_def_pos("".into());
        html_to_blocks(&raw)
            .into_iter()
            .map(|t| DefBlock {
                number: 0,
                text: t.into(),
            })
            .collect()
    } else {
        let parsed = parse_entry(&raw);
        ui.set_def_pron(parsed.pronunciation.into());
        ui.set_def_pos(parsed.pos.into());
        if parsed.senses.is_empty() {
            // Fall back to lightly-cleaned text when we couldn't split senses.
            vec![DefBlock {
                number: 0,
                text: cleaned_plain(&raw).into(),
            }]
        } else {
            parsed
                .senses
                .into_iter()
                .enumerate()
                .map(|(i, s)| DefBlock {
                    number: i as i32 + 1,
                    text: s.into(),
                })
                .collect()
        }
    };
    blocks_model.set_vec(blocks);
}

/// Look up `headword` and return (joined raw definition text, source name,
/// whether the entry is HTML). When `filter` is set, only that dictionary's
/// results are considered.
fn lookup_raw(
    manager: &Rc<RefCell<DictionaryManager>>,
    headword: &str,
    filter: Option<&str>,
) -> (String, String, bool) {
    let mut m = manager.borrow_mut();
    match m.lookup(headword) {
        Ok(results) if !results.is_empty() => {
            let results: Vec<_> = results
                .into_iter()
                .filter(|r| filter.is_none_or(|name| r.dictionary == name))
                .collect();
            let Some(first) = results.first() else {
                return (String::new(), String::new(), false);
            };
            let source = first.dictionary.clone();
            // StarDict type 'h' = HTML (`sametypesequence=h`, e.g. Petit Robert).
            let html = first
                .entries
                .first()
                .and_then(|e| e.segments.first())
                .map(|s| s.type_.contains('h'))
                .unwrap_or(false);
            let mut parts = Vec::new();
            for r in &results {
                for e in &r.entries {
                    parts.push(
                        e.segments
                            .iter()
                            .map(|s| s.text.as_str())
                            .collect::<String>(),
                    );
                }
            }
            (parts.join("\n\n"), source, html)
        }
        _ => (String::new(), String::new(), false),
    }
}

// ---- HTML entry rendering (StarDict type 'h') ----

/// Convert an HTML dictionary entry into plain-text paragraphs: split on
/// block-level tags, strip the rest, and decode entities. Long paragraphs are
/// chopped so each ListView delegate stays small (and the body renders at 60fps).
fn html_to_blocks(html: &str) -> Vec<String> {
    let chars: Vec<char> = html.chars().collect();
    let mut paras: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '<' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '>' {
                    j += 1;
                }
                let tag: String = chars[start..j].iter().collect();
                if is_block_tag(&tag) {
                    flush_paragraph(&mut cur, &mut paras);
                }
                i = if j < chars.len() { j + 1 } else { j };
            }
            '&' => {
                let limit = (i + 12).min(chars.len());
                let mut j = i + 1;
                while j < limit && chars[j] != ';' {
                    j += 1;
                }
                if j < limit && chars[j] == ';' {
                    let ent: String = chars[i + 1..j].iter().collect();
                    if let Some(s) = decode_entity(&ent) {
                        cur.push_str(&s);
                    }
                    i = j + 1;
                } else {
                    cur.push('&');
                    i += 1;
                }
            }
            c => {
                cur.push(c);
                i += 1;
            }
        }
    }
    flush_paragraph(&mut cur, &mut paras);

    let mut out = Vec::new();
    for p in paras {
        if p.chars().count() > 900 {
            out.extend(split_long(&p, 800));
        } else {
            out.push(p);
        }
    }
    out
}

/// Collapse whitespace in `cur`, push it as a paragraph if non-empty, and reset.
fn flush_paragraph(cur: &mut String, paras: &mut Vec<String>) {
    let p = collapse_ws(cur);
    if !p.is_empty() {
        paras.push(p);
    }
    cur.clear();
}

/// Whether an HTML tag is block-level (so it ends the current paragraph).
fn is_block_tag(tag: &str) -> bool {
    let name: String = tag
        .trim()
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "div"
            | "p"
            | "br"
            | "li"
            | "ul"
            | "ol"
            | "tr"
            | "table"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "hr"
            | "dd"
            | "dt"
            | "dl"
    )
}

/// Decode an HTML entity body (the text between `&` and `;`).
fn decode_entity(ent: &str) -> Option<String> {
    if let Some(num) = ent.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    Some(
        match ent {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            "nbsp" => " ",
            "laquo" => "«",
            "raquo" => "»",
            "hellip" => "…",
            "mdash" => "—",
            "ndash" => "–",
            "rsquo" => "\u{2019}",
            "lsquo" => "\u{2018}",
            "deg" => "°",
            "agrave" => "à",
            "acirc" => "â",
            "aelig" => "æ",
            "ccedil" => "ç",
            "eacute" => "é",
            "egrave" => "è",
            "ecirc" => "ê",
            "euml" => "ë",
            "icirc" => "î",
            "iuml" => "ï",
            "ocirc" => "ô",
            "oelig" => "œ",
            "ugrave" => "ù",
            "ucirc" => "û",
            "uuml" => "ü",
            _ => return None,
        }
        .to_string(),
    )
}

/// Split a long plain-text paragraph into ≤`max`-char pieces at word boundaries.
fn split_long(s: &str, max: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + max).min(chars.len());
        let mut cut = end;
        if end < chars.len() {
            let mut k = end;
            while k > start && chars[k] != ' ' {
                k -= 1;
            }
            if k > start {
                cut = k;
            }
        }
        let piece: String = chars[start..cut].iter().collect();
        let piece = piece.trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        start = cut;
    }
    out
}

/// Strip HTML to a single plain-text string (used for result-list snippets).
fn strip_html(html: &str) -> String {
    html_to_blocks(html).join(" ")
}

// ---- GCIDE markup parsing (display only; proper rendering is Phase 7) ----

/// A parsed GCIDE entry: pronunciation respelling, part of speech, and senses.
struct Parsed {
    pronunciation: String,
    pos: String,
    senses: Vec<String>,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

fn parse_entry(raw: &str) -> Parsed {
    let text = drop_markers(&strip_braces(raw));

    // Pronunciation: prefer the phonetic respelling in parentheses right after the
    // headword (`\Word\ (ph[=o]netic)`); fall back to the backslash respelling.
    static PHON: OnceLock<Regex> = OnceLock::new();
    static PRON: OnceLock<Regex> = OnceLock::new();
    let phonetic = re(&PHON, r"\\[^\\]+\\\s*\(([^()]+)\)")
        .captures(&text)
        .map(|c| first_variant(&c[1]).to_string())
        .or_else(|| {
            re(&PRON, r"\\([^\\]+)\\")
                .captures(&text)
                .map(|c| c[1].to_string())
        })
        .map(|p| clean_pron(&decode_gcide(&p)))
        .unwrap_or_default();

    static POS: OnceLock<Regex> = OnceLock::new();
    let pos = re(&POS, r"\\[^\\]+\\[^,]*,\s*([A-Za-z]\.(?:\s*[A-Za-z]\.)*)")
        .captures(&text)
        .map(|c| expand_pos(c[1].trim()))
        .unwrap_or_default();

    Parsed {
        pronunciation: phonetic,
        pos,
        senses: parse_senses(&text)
            .into_iter()
            .map(|s| decode_gcide(&s))
            .collect(),
    }
}

/// Drop editorial marker lines while preserving line structure.
fn drop_markers(text: &str) -> String {
    text.lines()
        .filter(|l| {
            let t = l.trim();
            !(t == "[1913 Webster]"
                || t == "[PJC]"
                || t.starts_with("[Webster 1913")
                || t.starts_with("[Century"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_braces(s: &str) -> String {
    s.chars().filter(|&c| c != '{' && c != '}').collect()
}

/// Keep only the first pronunciation variant, dropping alternates (`… or …`),
/// language tags (`Sp.`, `F.`, …), and reference numbers (`; 277`).
fn first_variant(phon: &str) -> &str {
    let mut end = phon.len();
    let cuts = [
        " or ", ";", ",", " Sp.", " F.", " L.", " G.", " Gr.", " It.", " NL.", " D.", " AS.",
        " OF.", " Pg.",
    ];
    for c in cuts {
        if let Some(i) = phon.find(c) {
            end = end.min(i);
        }
    }
    phon[..end].trim()
}

/// Turn GCIDE's respelling (`Dic"tion*a*ry`) into syllables (`Dic·tion·a·ry`).
fn clean_pron(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' | '`' | '*' => out.push('·'),
            _ => out.push(c),
        }
    }
    while out.contains("··") {
        out = out.replace("··", "·");
    }
    out.trim_matches('·').trim().to_string()
}

fn expand_pos(p: &str) -> String {
    let norm: String = p.chars().filter(|c| !c.is_whitespace()).collect();
    match norm.as_str() {
        "n." => "noun",
        "n.pl." => "noun plural",
        "v." => "verb",
        "v.t." => "verb (transitive)",
        "v.i." => "verb (intransitive)",
        "a." | "adj." => "adjective",
        "adv." => "adverb",
        "prep." => "preposition",
        "conj." => "conjunction",
        "interj." => "interjection",
        "pron." => "pronoun",
        _ => p,
    }
    .to_string()
}

/// True for indented quotation/attribution lines we want to drop from senses.
fn is_quote(line: &str) -> bool {
    let lead = line.len() - line.trim_start().len();
    lead >= 10 || line.trim_start().starts_with("--")
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split a GCIDE entry into its numbered senses (quotations dropped).
fn parse_senses(text: &str) -> Vec<String> {
    static MARK: OnceLock<Regex> = OnceLock::new();
    let mark = re(&MARK, r"^\s*\d+\.\s+(.*)$");
    let lines: Vec<&str> = text.lines().collect();

    let Some(start) = lines.iter().position(|l| mark.is_match(l)) else {
        return unnumbered_sense(text).into_iter().collect();
    };

    let mut senses = Vec::new();
    let mut i = start;
    while i < lines.len() {
        if let Some(c) = mark.captures(lines[i]) {
            let mut buf = c[1].trim().to_string();
            i += 1;
            while i < lines.len() && !mark.is_match(lines[i]) {
                if !is_quote(lines[i]) {
                    let t = lines[i].trim();
                    if !t.is_empty() {
                        buf.push(' ');
                        buf.push_str(t);
                    }
                }
                i += 1;
            }
            let s = collapse_ws(&buf);
            if !s.is_empty() {
                senses.push(s);
            }
        } else {
            i += 1;
        }
    }
    senses
}

/// For entries without numbered senses: strip the header (pronunciation,
/// etymology) and quotations, returning the remaining prose as a single sense.
fn unnumbered_sense(text: &str) -> Option<String> {
    static PRON: OnceLock<Regex> = OnceLock::new();
    static BR: OnceLock<Regex> = OnceLock::new();
    let no_pron = re(&PRON, r"\\[^\\]*\\").replace_all(text, "");
    let no_etym = re(&BR, r"(?s)\[[^\[\]]*\]").replace_all(&no_pron, "");
    let prose: String = no_etym
        .lines()
        .filter(|l| !is_quote(l))
        .collect::<Vec<_>>()
        .join(" ");
    let flat = collapse_ws(&prose);
    if flat.is_empty() {
        None
    } else {
        Some(flat)
    }
}

/// Lightly cleaned plain text, used when sense parsing yields nothing.
fn cleaned_plain(raw: &str) -> String {
    let mut joined = decode_gcide(&drop_markers(&strip_braces(raw)));
    while joined.contains("\n\n\n") {
        joined = joined.replace("\n\n\n", "\n\n");
    }
    joined.trim().to_string()
}

/// A one-line preview for the results list. HTML entries are stripped to text;
/// GCIDE entries prefer the first numbered sense.
fn make_snippet(raw: &str) -> String {
    let tail = if raw.contains('<') {
        strip_html(raw)
    } else {
        let flat = collapse_ws(&strip_braces(raw));
        let start = flat.find(" 1. ").map(|i| i + 4).unwrap_or(0);
        decode_gcide(&flat[start..])
    };
    let mut chars = tail.chars();
    let head: String = chars.by_ref().take(90).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Decode GCIDE's ASCII diacritic codes (`[=a]`→ā, `["o]`→ö, `[ae]`→æ, …) into
/// Unicode. Unknown codes (usage labels like `[Obs.]`, long brackets) are left
/// as-is. Used on every displayed string so phonetics and accented words render.
fn decode_gcide(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = re(&RE, r"\[([^\[\]]{1,5})\]");
    re.replace_all(s, |c: &regex::Captures| {
        decode_code(&c[1]).unwrap_or(&c[0]).to_string()
    })
    .into_owned()
}

fn decode_code(code: &str) -> Option<&'static str> {
    Some(match code {
        // ligatures and letters
        "ae" => "æ",
        "AE" => "Æ",
        "oe" => "œ",
        "OE" => "Œ",
        "eth" => "ð",
        "deg" => "°",
        "root" => "√",
        ",c" => "ç",
        ",C" => "Ç",
        // macron (long vowels), incl. "-" and "Xmac" variants
        "=a" | "-a" | "amac" => "ā",
        "=e" | "-e" | "emac" => "ē",
        "=i" | "-i" | "imac" => "ī",
        "=o" | "-o" | "omac" => "ō",
        "=u" | "-u" | "umac" => "ū",
        "=y" => "ȳ",
        "=ae" => "ǣ",
        "=oo" => "ōō",
        // breve (short vowels): letter then caret
        "a^" => "ă",
        "e^" => "ĕ",
        "i^" => "ĭ",
        "o^" => "ŏ",
        "u^" => "ŭ",
        "y^" => "y̆",
        // diaeresis / umlaut
        "\"a" | "add" | "aum" => "ä",
        "\"e" => "ë",
        "\"i" => "ï",
        "\"o" => "ö",
        "\"u" => "ü",
        "\"y" => "ÿ",
        // grave
        "`a" => "à",
        "`e" => "è",
        "`i" => "ì",
        "`o" => "ò",
        "`u" => "ù",
        // circumflex: caret then letter
        "^a" => "â",
        "^e" => "ê",
        "^i" => "î",
        "^o" => "ô",
        "^u" => "û",
        // tilde
        "~a" => "ã",
        "~e" => "ẽ",
        "~i" => "ĩ",
        "~n" => "ñ",
        "~o" => "õ",
        "~u" => "ũ",
        // dot above
        ".a" => "ȧ",
        ".e" => "ė",
        // digraphs / consonants
        "th" => "th",
        "dh" => "ð",
        "ng" => "ŋ",
        "sh" => "sh",
        "zh" => "zh",
        "ch" => "ch",
        "hw" => "hw",
        "oo" => "oo",
        "OO" => "OO",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_entry_becomes_clean_paragraphs() {
        let html = "<DIV style=\"font-weight:bold\">manger</DIV> \
                    <DIV>Ce verbe vient du <SPAN style=\"color: maroon\">latin</span> \
                    <SPAN style=\"font-style:italic\">manducare</span> &laquo; m&acirc;cher &raquo;.</DIV>";
        let blocks = html_to_blocks(html);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], "manger");
        assert_eq!(blocks[1], "Ce verbe vient du latin manducare « mâcher ».");
        // no raw tags leak through
        assert!(blocks.iter().all(|b| !b.contains('<') && !b.contains('>')));
    }

    #[test]
    fn decodes_numeric_and_named_entities() {
        assert_eq!(decode_entity("#233").as_deref(), Some("é"));
        assert_eq!(decode_entity("#xE9").as_deref(), Some("é"));
        assert_eq!(decode_entity("nbsp").as_deref(), Some(" "));
        assert_eq!(decode_entity("notathing"), None);
    }

    #[test]
    fn long_paragraph_is_split_into_small_blocks() {
        let long = "mot ".repeat(400); // ~1600 chars, one paragraph
        let blocks = html_to_blocks(&format!("<p>{long}</p>"));
        assert!(blocks.len() > 1);
        assert!(blocks.iter().all(|b| b.chars().count() <= 800));
    }

    #[test]
    fn decodes_phonetic_diacritics() {
        assert_eq!(decode_gcide("[=a]"), "ā");
        assert_eq!(decode_gcide("[a^]"), "ă");
        assert_eq!(decode_gcide("[\"o]"), "ö");
        assert_eq!(decode_gcide("[ae]sthetic"), "æsthetic");
        // unknown codes are left untouched
        assert_eq!(decode_gcide("[Obs.]"), "[Obs.]");
    }

    #[test]
    fn parses_archaeology_phonetic() {
        let raw = "Archaeology \\Ar`ch[ae]*ol\"o*gy\\ ([aum]r`k[-e]*[o^]l\"[-o]*j[y^]), n.\n   \
                   1. The science of antiquities.\n";
        let parsed = parse_entry(raw);
        assert_eq!(parsed.pronunciation, "är·kē·ŏl·ō·jy̆");
        assert_eq!(parsed.pos, "noun");
        assert_eq!(parsed.senses, vec!["The science of antiquities."]);
    }

    #[test]
    fn keeps_only_first_phonetic_variant() {
        // "either": two variants plus a reference number.
        let raw = "Either \\Ei\"ther\\ ([=e]\"[th][~e]r or [imac]\"[th][~e]r; 277), a.\n   \
                   1. One of two.\n";
        let parsed = parse_entry(raw);
        assert_eq!(parsed.pronunciation, "ē·thẽr");
    }
}
