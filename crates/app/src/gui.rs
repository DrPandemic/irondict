// Phase 6 step 2: the "toolbar layout" wired to the real backend
// (DictionaryManager + SearchEngine over the bundled GCIDE).
//
// The search index takes a few seconds to build on first run, so it is built on
// a worker thread; the window appears immediately (showing the word of the
// moment) and search becomes live once the index is ready.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use slint::private_unstable_api::re_exports::{parse_markdown, StyledText};
use slint::{Color, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use irondict_core::{
    bundled_gcide_path, download, search, Config, ConjugatorRegistry, DictionaryConfig,
    DictionaryManager, Language, Preferences, Progress, SearchEngine, SearchMode, ThemeMode,
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

// ---- threaded search/render pipeline ----
//
// The expensive work — the tantivy query, the disk read of the full entry, HTML
// stripping, snippet building and conjugation — runs on a dedicated worker
// thread so it never blocks the UI thread while the user is typing. The worker
// owns its own `DictionaryManager` + `SearchEngine` and produces plain,
// `Send`-able render data; the UI thread only assembles the (non-`Send`) Slint
// models from it. Synchronous user actions (clicks, links, back/forward) render
// on the UI thread through the same `compute_page`/`apply_page` split, using the
// UI's own manager.

/// One result-list row, computed off the UI thread.
struct RenderedItem {
    headword: String,
    snippet: String,
    source: String,
}

/// One definition body block, with the markdown source (cross-reference links)
/// kept as a plain string; the (non-`Send`) styled text is parsed on the UI
/// thread in `apply_page`.
struct RenderedBlock {
    marker: String,
    text: String,
    md: String,
    /// True for an indented example/quotation block (rendered italic + greyed).
    quote: bool,
}

struct RenderedConjForm {
    label: String,
    text: String,
}

struct RenderedConjSection {
    label: String,
    forms: Vec<RenderedConjForm>,
}

/// The body of a rendered page: either a real entry or a plain message.
enum PageBody {
    Entry {
        pron: String,
        pos: String,
        etym: String,
        blocks: Vec<RenderedBlock>,
        conjugation: Vec<RenderedConjSection>,
    },
    Message {
        body: String,
    },
}

/// A fully-computed definition page, ready for `apply_page` to push into the UI.
struct RenderedPage {
    section_label: String,
    /// The `def-headword` line — the entry's headword, or a message title.
    headword: String,
    source: String,
    body: PageBody,
}

/// The outcome of a search: the result list, the rendered first page (the first
/// hit, or a "no results" message), and the first hit's headword when there is
/// one (so the UI can select it and record it in history).
struct RenderedResults {
    items: Vec<RenderedItem>,
    first_headword: Option<String>,
    page: RenderedPage,
}

/// A request from the UI thread to the search worker.
enum WorkerReq {
    /// Run `query` (scoped to `scope` when set); `gen` lets the UI drop stale
    /// responses once the user has typed more.
    Search {
        gen: u64,
        query: String,
        scope: Option<String>,
    },
    /// Re-read the manager from the saved config (e.g. a language pin changed);
    /// when `rebuild` is set, also rebuild the search index (dictionary set
    /// changed).
    Reload { rebuild: bool },
    /// Stop the worker (on shutdown).
    Shutdown,
}

/// A message from the search worker back to the UI thread.
enum WorkerMsg {
    Results {
        gen: u64,
        results: Box<RenderedResults>,
    },
    /// A search arrived before the index was ready.
    NoEngine {
        gen: u64,
    },
    IndexReady,
    BuildError(String),
}

/// One visited definition page, captured so back/forward can replay it. `scope`
/// is the dictionary scope index that was active, so navigation is faithful even
/// if the user later switches dictionaries.
#[derive(Clone)]
struct NavEntry {
    headword: String,
    label: String,
    scope: i32,
}

/// Browser-style back/forward stack over the definition pages. `cursor` indexes
/// the page currently shown; pushing a new page truncates any forward history.
#[derive(Default)]
struct NavHistory {
    entries: Vec<NavEntry>,
    cursor: usize,
}

impl NavHistory {
    fn current(&self) -> Option<&NavEntry> {
        self.entries.get(self.cursor)
    }

    fn can_back(&self) -> bool {
        !self.entries.is_empty() && self.cursor > 0
    }

    fn can_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// Record a freshly-shown page, unless it repeats the current one. Drops any
    /// forward history (the user navigated somewhere new).
    fn push(&mut self, entry: NavEntry) {
        if let Some(cur) = self.current() {
            if cur.headword == entry.headword
                && cur.label == entry.label
                && cur.scope == entry.scope
            {
                return;
            }
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.cursor + 1);
        }
        self.entries.push(entry);
        self.cursor = self.entries.len() - 1;
    }
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

/// Open the cached index for `manager` when it matches the current dictionary
/// set, otherwise (re)build it and refresh the manifest. Warmed before returning
/// so the first keystroke is snappy. Runs on the worker thread with the worker's
/// own manager, so the same call covers first launch and post-settings rebuilds
/// (a changed dictionary set yields a new signature, which forces a rebuild).
/// Open the cached index when it matches, else (re)build it. `cancel` is polled
/// during a build so a superseding request (e.g. a deleted dictionary) abandons
/// it; in that case the result is `Ok(None)`.
fn prepare_engine(
    manager: &mut DictionaryManager,
    cancel: impl FnMut() -> bool,
) -> Result<Option<SearchEngine>, String> {
    let dir = search::default_index_dir().map_err(|e| e.to_string())?;
    let signature = index_signature(manager);
    let cached = std::fs::read_to_string(dir.join("manifest")).ok();
    if cached.as_deref() == Some(signature.as_str()) {
        if let Ok(engine) = SearchEngine::open(&dir) {
            warm_up(&engine);
            return Ok(Some(engine));
        }
    }
    match SearchEngine::build_cancellable(&dir, manager, cancel).map_err(|e| e.to_string())? {
        Some(engine) => {
            let _ = std::fs::write(dir.join("manifest"), &signature);
            warm_up(&engine);
            Ok(Some(engine))
        }
        None => Ok(None),
    }
}

/// The search worker: owns its own manager + index and serves search/lookup work
/// off the UI thread. It builds the index first (reporting `IndexReady` or
/// `BuildError`), then processes requests until the UI drops the request channel
/// or sends `Shutdown`. The manager is reloaded from the saved config on
/// `Reload`, and the index rebuilt when the dictionary set changed.
fn run_worker(req_rx: mpsc::Receiver<WorkerReq>, resp_tx: mpsc::Sender<WorkerMsg>) {
    let mut manager = load_manager();
    let mut engine: Option<SearchEngine> = None;
    // A control request pulled off the channel while a build was running, to be
    // handled before blocking for the next one.
    let mut pending: Option<WorkerReq> = None;
    rebuild_index(&mut engine, &mut manager, &req_rx, &resp_tx, &mut pending);

    loop {
        // A superseding request captured during a cancelled build takes priority;
        // otherwise block for the next one.
        let req = match pending.take() {
            Some(req) => req,
            None => match req_rx.recv() {
                Ok(req) => req,
                Err(_) => break,
            },
        };
        match req {
            WorkerReq::Search { gen, query, scope } => match &engine {
                Some(eng) => {
                    let results = compute_search(eng, &mut manager, &query, scope.as_deref());
                    let _ = resp_tx.send(WorkerMsg::Results {
                        gen,
                        results: Box::new(results),
                    });
                }
                None => {
                    let _ = resp_tx.send(WorkerMsg::NoEngine { gen });
                }
            },
            WorkerReq::Reload { rebuild } => {
                // Pick up any language pins / dictionary-set changes the UI saved.
                manager = load_manager();
                if rebuild {
                    rebuild_index(&mut engine, &mut manager, &req_rx, &resp_tx, &mut pending);
                }
            }
            WorkerReq::Shutdown => break,
        }
    }
}

/// (Re)build the index, watching `req_rx` for a newer request so the build can be
/// abandoned the moment the dictionary set changes again (e.g. a delete). Search
/// requests that arrive mid-build can't be served, so they're answered with
/// `NoEngine` (the UI retries on the next `IndexReady`); a `Reload`/`Shutdown`
/// supersedes the build and is stashed in `pending` for the worker loop.
fn rebuild_index(
    engine: &mut Option<SearchEngine>,
    manager: &mut DictionaryManager,
    req_rx: &mpsc::Receiver<WorkerReq>,
    resp_tx: &mpsc::Sender<WorkerMsg>,
    pending: &mut Option<WorkerReq>,
) {
    let cancel = || loop {
        match req_rx.try_recv() {
            Ok(WorkerReq::Search { gen, .. }) => {
                let _ = resp_tx.send(WorkerMsg::NoEngine { gen });
            }
            Ok(other) => {
                *pending = Some(other);
                return true;
            }
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                *pending = Some(WorkerReq::Shutdown);
                return true;
            }
        }
    };
    match prepare_engine(manager, cancel) {
        Ok(Some(e)) => {
            *engine = Some(e);
            let _ = resp_tx.send(WorkerMsg::IndexReady);
        }
        // Cancelled: a superseding request is now pending, which will drive the
        // next (correct) rebuild. Keep the previous engine state untouched.
        Ok(None) => {}
        Err(msg) => {
            let _ = resp_tx.send(WorkerMsg::BuildError(msg));
        }
    }
}

/// Run throwaway queries so the first real search doesn't pay the one-time cost
/// of mmapping the term dictionary, opening segment/store readers, and building
/// the fuzzy automaton. Runs on the worker thread, off the UI thread.
fn warm_up(engine: &SearchEngine) {
    let _ = engine.search("a", SearchMode::Prefix, 80);
    let _ = engine.search("warmup", SearchMode::Fuzzy, 40);
}

/// Launch the graphical front-end and run the Slint event loop until the window
/// is closed.
pub fn run() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    // The accent (indigo) and light default live in the .slint; the OS values are
    // detected and applied below, off the startup path.

    let manager = Rc::new(RefCell::new(load_manager()));
    let results_model: Rc<VecModel<ResultItem>> = Rc::new(VecModel::default());
    ui.set_results(ModelRc::from(results_model.clone()));
    let blocks_model: Rc<VecModel<DefBlock>> = Rc::new(VecModel::default());
    ui.set_def_blocks(ModelRc::from(blocks_model.clone()));
    let rows: Rc<RefCell<Vec<RowData>>> = Rc::new(RefCell::new(Vec::new()));
    let history: Rc<RefCell<NavHistory>> = Rc::new(RefCell::new(NavHistory::default()));
    let dict_items: Rc<VecModel<DictRow>> = Rc::new(VecModel::default());
    ui.set_dict_items(ModelRc::from(dict_items.clone()));
    let scopes: Rc<VecModel<SharedString>> = Rc::new(VecModel::default());
    ui.set_scopes(ModelRc::from(scopes.clone()));
    let catalog_items: Rc<VecModel<CatalogRow>> = Rc::new(VecModel::default());
    ui.set_catalog_items(ModelRc::from(catalog_items.clone()));
    catalog_items.set_vec(build_catalog_rows());

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

    // Word of the moment: one fixed seed per launch, but the actual word is drawn
    // from whichever dictionary the active scope selects (so it follows the chosen
    // dictionary). Shown immediately, before the index is ready.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    let (wotm, wotm_src) = word_of_the_moment(&manager, ui.get_scope(), seed);
    navigate(
        &ui,
        &manager,
        &blocks_model,
        &history,
        &wotm,
        "WORD OF THE MOMENT",
        wotm_src.as_deref(),
    );

    // Spin up the search worker. It builds/opens the index and runs every search
    // and first-result render off the UI thread, so typing never blocks. Requests
    // go out on `req_tx`; results come back on `resp_rx`, drained by a fast timer.
    // `gen` tags each search so stale responses (the user kept typing) are dropped.
    let (req_tx, req_rx) = mpsc::channel::<WorkerReq>();
    let (resp_tx, resp_rx) = mpsc::channel::<WorkerMsg>();
    let req_tx = Rc::new(req_tx);
    let gen = Rc::new(Cell::new(0u64));
    std::thread::spawn(move || run_worker(req_rx, resp_tx));

    // Drain the worker's responses and apply them to the UI. Polls often enough
    // that results land within a frame or two of the worker finishing.
    let resp_timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let results_model = results_model.clone();
        let blocks_model = blocks_model.clone();
        let history = history.clone();
        let rows = rows.clone();
        let req_tx = req_tx.clone();
        let gen = gen.clone();
        resp_timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
            while let Ok(msg) = resp_rx.try_recv() {
                let ui = ui_weak.unwrap();
                match msg {
                    WorkerMsg::Results { gen: g, results } => {
                        if g == gen.get() {
                            apply_results(
                                &ui,
                                &results_model,
                                &rows,
                                &blocks_model,
                                &history,
                                *results,
                            );
                        }
                    }
                    WorkerMsg::NoEngine { gen: g } => {
                        if g == gen.get() {
                            apply_page(
                                &ui,
                                &blocks_model,
                                &message_page(
                                    "",
                                    "Preparing dictionary…",
                                    "Building the search index — one moment.",
                                ),
                            );
                        }
                    }
                    WorkerMsg::IndexReady => {
                        ui.set_index_ready(true);
                        // Run whatever the user has already typed against the new index.
                        let q = ui.get_query();
                        if !q.trim().is_empty() {
                            gen.set(gen.get() + 1);
                            let scope = scope_filter(ui.get_scope(), &manager);
                            let _ = req_tx.send(WorkerReq::Search {
                                gen: gen.get(),
                                query: q.to_string(),
                                scope,
                            });
                        }
                    }
                    WorkerMsg::BuildError(msg) => {
                        apply_page(
                            &ui,
                            &blocks_model,
                            &message_page("", "Couldn't build index", &msg),
                        );
                    }
                }
            }
        });
    }

    // Live search as the user types.
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let results_model = results_model.clone();
        let blocks_model = blocks_model.clone();
        let history = history.clone();
        let rows = rows.clone();
        let req_tx = req_tx.clone();
        let gen = gen.clone();
        // Debounce: defer the search until the user pauses, instead of dispatching
        // one on every keystroke. The search itself runs on the worker thread.
        let debounce = Rc::new(Timer::default());
        ui.on_query_changed(move |q| {
            if q.trim().is_empty() {
                let ui = ui_weak.unwrap();
                debounce.stop();
                // Bump the generation so any in-flight search response is dropped.
                gen.set(gen.get() + 1);
                ui.set_searching(false);
                results_model.set_vec(Vec::new());
                rows.borrow_mut().clear();
                ui.set_selected_index(-1);
                let (wotm, wotm_src) = word_of_the_moment(&manager, ui.get_scope(), seed);
                navigate(
                    &ui,
                    &manager,
                    &blocks_model,
                    &history,
                    &wotm,
                    "WORD OF THE MOMENT",
                    wotm_src.as_deref(),
                );
                return;
            }
            let ui_weak = ui_weak.clone();
            let manager = manager.clone();
            let req_tx = req_tx.clone();
            let gen = gen.clone();
            debounce.start(
                TimerMode::SingleShot,
                Duration::from_millis(110),
                move || {
                    let ui = ui_weak.unwrap();
                    gen.set(gen.get() + 1);
                    let scope = scope_filter(ui.get_scope(), &manager);
                    let _ = req_tx.send(WorkerReq::Search {
                        gen: gen.get(),
                        query: ui.get_query().to_string(),
                        scope,
                    });
                },
            );
        });
    }

    // Click a result to show its definition.
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let blocks_model = blocks_model.clone();
        let history = history.clone();
        let rows = rows.clone();
        let gen = gen.clone();
        ui.on_select(move |row| {
            let ui = ui_weak.unwrap();
            let headword = rows
                .borrow()
                .get(row.max(0) as usize)
                .map(|r| r.headword.clone());
            if let Some(headword) = headword {
                // Supersede any in-flight search so its result can't clobber this.
                gen.set(gen.get() + 1);
                let filter = scope_filter(ui.get_scope(), &manager);
                ui.set_selected_index(row);
                navigate(
                    &ui,
                    &manager,
                    &blocks_model,
                    &history,
                    &headword,
                    "",
                    filter.as_deref(),
                );
            }
        });
    }

    // Double-click / word selection in the definition body: look up the token.
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let blocks_model = blocks_model.clone();
        let history = history.clone();
        let gen = gen.clone();
        ui.on_lookup_token(move |link| {
            let ui = ui_weak.unwrap();
            // Supersede any in-flight search so its result can't clobber this.
            gen.set(gen.get() + 1);
            let filter = scope_filter(ui.get_scope(), &manager);
            // The clicked link is a `lookup://<word>` URL; strip the scheme.
            let word = link
                .as_str()
                .strip_prefix("lookup://")
                .unwrap_or(link.as_str());
            navigate(
                &ui,
                &manager,
                &blocks_model,
                &history,
                word,
                "",
                filter.as_deref(),
            );
        });
    }

    // Scope change: switch the active dictionary and re-run the current query
    // (or fall back to the word of the moment when the search box is empty).
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let blocks_model = blocks_model.clone();
        let history = history.clone();
        let req_tx = req_tx.clone();
        let gen = gen.clone();
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
            // A scope change supersedes any in-flight search either way.
            gen.set(gen.get() + 1);
            let q = ui.get_query();
            if q.trim().is_empty() {
                // The word of the moment follows the newly-selected dictionary.
                let (wotm, wotm_src) = word_of_the_moment(&manager, idx, seed);
                navigate(
                    &ui,
                    &manager,
                    &blocks_model,
                    &history,
                    &wotm,
                    "WORD OF THE MOMENT",
                    wotm_src.as_deref(),
                );
            } else {
                // Re-run the current query under the new scope on the worker.
                let scope = scope_filter(idx, &manager);
                let _ = req_tx.send(WorkerReq::Search {
                    gen: gen.get(),
                    query: q.to_string(),
                    scope,
                });
            }
        });
    }

    // Back / forward through the visited-page history (Alt+Left/Right or the
    // mouse back/forward buttons). These replay a stored page without pushing a
    // new history entry.
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let blocks_model = blocks_model.clone();
        let history = history.clone();
        let gen = gen.clone();
        ui.on_navigate_back(move || {
            let ui = ui_weak.unwrap();
            let entry = {
                let mut h = history.borrow_mut();
                if !h.can_back() {
                    return;
                }
                h.cursor -= 1;
                h.current().cloned()
            };
            if let Some(e) = entry {
                // Supersede any in-flight search so its result can't clobber this.
                gen.set(gen.get() + 1);
                replay(&ui, &manager, &blocks_model, &e);
            }
            sync_nav_state(&ui, &history);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let blocks_model = blocks_model.clone();
        let history = history.clone();
        let gen = gen.clone();
        ui.on_navigate_forward(move || {
            let ui = ui_weak.unwrap();
            let entry = {
                let mut h = history.borrow_mut();
                if !h.can_forward() {
                    return;
                }
                h.cursor += 1;
                h.current().cloned()
            };
            if let Some(e) = entry {
                // Supersede any in-flight search so its result can't clobber this.
                gen.set(gen.get() + 1);
                replay(&ui, &manager, &blocks_model, &e);
            }
            sync_nav_state(&ui, &history);
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
        let req_tx = req_tx.clone();
        ui.on_toggle_dict(move |row| {
            let ui = ui_weak.unwrap();
            let name = nth_dict_name(&manager, row);
            if let Some(name) = name {
                let now = !is_enabled(&manager, &name);
                manager.borrow_mut().set_enabled(&name, now);
                save_config(&manager);
                refresh_lists(&ui, &manager, &dict_items, &scopes);
                request_rebuild(&ui, &req_tx);
            }
        });
    }

    // ---- settings: pin a dictionary's language (no reindex needed) ----
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        let req_tx = req_tx.clone();
        ui.on_set_dict_language(move |row, lang| {
            let ui = ui_weak.unwrap();
            if let Some(name) = nth_dict_name(&manager, row) {
                manager
                    .borrow_mut()
                    .set_language(&name, index_to_lang(lang));
                save_config(&manager);
                refresh_lists(&ui, &manager, &dict_items, &scopes);
                // No reindex, but the worker's manager must pick up the new
                // language pin (it drives conjugation).
                let _ = req_tx.send(WorkerReq::Reload { rebuild: false });
            }
        });
    }

    // ---- settings: remove a dictionary (rebuilds the index) ----
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        let req_tx = req_tx.clone();
        ui.on_remove_dict(move |row| {
            let ui = ui_weak.unwrap();
            if let Some(name) = nth_dict_name(&manager, row) {
                manager.borrow_mut().remove(&name);
                save_config(&manager);
                refresh_lists(&ui, &manager, &dict_items, &scopes);
                request_rebuild(&ui, &req_tx);
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
        let req_tx = req_tx.clone();
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
                    request_rebuild(&ui, &req_tx);
                }
                Err(e) => eprintln!("failed to add {}: {}", path.display(), e),
            }
        });
    }

    // ---- settings: download a dictionary from the catalog ----
    // Each download runs on its own worker thread; progress and completion come
    // back over a channel and are applied on the UI thread by `dl_timer`. The
    // row index is stable (the catalog is fixed-order) so it doubles as the
    // dictionary's id between threads.
    let (dl_tx, dl_rx) = mpsc::channel::<DownloadMsg>();
    {
        let catalog_items = catalog_items.clone();
        let dl_tx = dl_tx.clone();
        ui.on_download_dict(move |index| {
            let index = index as usize;
            let Some(entry) = download::catalog().get(index) else {
                return;
            };
            // Ignore clicks on rows already installed or in flight.
            match catalog_items.row_data(index) {
                Some(mut row) if row.status == 0 => {
                    row.status = 1;
                    row.progress = 0.0;
                    catalog_items.set_row_data(index, row);
                }
                _ => return,
            }
            spawn_install(Some(index), entry.id.to_string(), dl_tx.clone());
        });
    }
    let dl_timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        let catalog_items = catalog_items.clone();
        let req_tx = req_tx.clone();
        let dl_tx = dl_tx.clone();
        dl_timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            while let Ok(msg) = dl_rx.try_recv() {
                match msg {
                    DownloadMsg::Progress { index, fraction } => {
                        if let Some(mut row) = index.and_then(|i| catalog_items.row_data(i)) {
                            row.progress = fraction;
                            catalog_items.set_row_data(index.unwrap(), row);
                        }
                    }
                    DownloadMsg::Done {
                        index,
                        ifo,
                        language,
                    } => {
                        let ui = ui_weak.unwrap();
                        let added = manager.borrow_mut().add(&ifo).map(|d| d.name().to_string());
                        match added {
                            Ok(name) => {
                                manager.borrow_mut().set_language(&name, language);
                                save_config(&manager);
                                refresh_lists(&ui, &manager, &dict_items, &scopes);
                                request_rebuild(&ui, &req_tx);
                                if let Some(index) = index {
                                    if let Some(mut row) = catalog_items.row_data(index) {
                                        row.status = 2;
                                        row.progress = 1.0;
                                        catalog_items.set_row_data(index, row);
                                    }
                                    // A primary dictionary may pull in a hidden
                                    // background companion (e.g. fr-fr → fr-conj).
                                    if let Some(companion) = download::catalog()
                                        .get(index)
                                        .and_then(|e| download::companion_for(e.id))
                                        .filter(|c| !download::is_installed(c))
                                    {
                                        spawn_install(None, companion.to_string(), dl_tx.clone());
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "failed to load downloaded dictionary at {}: {e}",
                                    ifo.display()
                                );
                                if let Some(index) = index {
                                    reset_catalog_row(&catalog_items, index);
                                }
                            }
                        }
                    }
                    DownloadMsg::Failed { index, error } => {
                        eprintln!("dictionary download failed: {error}");
                        if let Some(index) = index {
                            reset_catalog_row(&catalog_items, index);
                        }
                    }
                }
            }
        });
    }

    // ---- settings: delete a downloaded dictionary (files + registration) ----
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        let catalog_items = catalog_items.clone();
        let req_tx = req_tx.clone();
        ui.on_delete_dict(move |index| {
            let index = index as usize;
            let Some(entry) = download::catalog().get(index) else {
                return;
            };
            let ui = ui_weak.unwrap();
            // Unregister the installed file, then delete it from disk.
            if let Some(ifo) = download::installed_ifo(entry.id) {
                manager.borrow_mut().remove_path(&ifo);
            }
            if let Err(e) = download::uninstall(entry.id) {
                eprintln!("failed to delete {}: {e}", entry.id);
                return;
            }
            // A primary dictionary's hidden companion is paired with it: remove
            // it too (e.g. deleting fr-fr also removes fr-conj).
            if let Some(companion) = download::companion_for(entry.id) {
                if let Some(ifo) = download::installed_ifo(companion) {
                    manager.borrow_mut().remove_path(&ifo);
                }
                if let Err(e) = download::uninstall(companion) {
                    eprintln!("failed to delete {companion}: {e}");
                }
            }
            save_config(&manager);
            refresh_lists(&ui, &manager, &dict_items, &scopes);
            request_rebuild(&ui, &req_tx);
            reset_catalog_row(&catalog_items, index);
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
    // Stop the timers and ask the worker to exit so the process winds down cleanly.
    drop(resp_timer);
    drop(add_timer);
    drop(dl_timer);
    let _ = req_tx.send(WorkerReq::Shutdown);
    r
}

/// A message from a dictionary-download worker thread back to the UI thread.
/// `index` is the catalog row being downloaded.
enum DownloadMsg {
    Progress { index: Option<usize>, fraction: f32 },
    Done {
        index: Option<usize>,
        ifo: PathBuf,
        language: Language,
    },
    Failed { index: Option<usize>, error: String },
}

/// Spawn a worker thread that downloads and installs the catalog dictionary
/// `id`, reporting progress/completion over `dl_tx`. `index` is the settings
/// catalog row to update, or `None` for a background companion that has no row.
fn spawn_install(index: Option<usize>, id: String, dl_tx: mpsc::Sender<DownloadMsg>) {
    std::thread::spawn(move || {
        let Some(entry) = download::find(&id) else {
            let _ = dl_tx.send(DownloadMsg::Failed {
                index,
                error: format!("unknown dictionary id: {id}"),
            });
            return;
        };
        let mut last_pct = u8::MAX;
        let result = download::install(entry, |Progress::Downloading { received, total }| {
            if let Some(total) = total.filter(|t| *t > 0) {
                let pct = (received * 100 / total) as u8;
                if pct != last_pct {
                    last_pct = pct;
                    let _ = dl_tx.send(DownloadMsg::Progress {
                        index,
                        fraction: received as f32 / total as f32,
                    });
                }
            }
        });
        let _ = dl_tx.send(match result {
            Ok(ifo) => DownloadMsg::Done {
                index,
                ifo,
                language: entry.language,
            },
            Err(e) => DownloadMsg::Failed {
                index,
                error: e.to_string(),
            },
        });
    });
}

/// Build the settings "Download" rows from the built-in catalog, marking the
/// ones already present on disk as installed.
fn build_catalog_rows() -> Vec<CatalogRow> {
    // Companions (e.g. fr-conj) are auto-installed alongside their primary, so
    // they get no row of their own. They are appended last in `catalog()`, so
    // dropping them keeps the remaining rows index-aligned with `catalog()` —
    // which the download/delete handlers rely on.
    download::catalog()
        .iter()
        .filter(|e| !download::is_companion(e.id))
        .map(|e| CatalogRow {
            label: e.label.into(),
            detail: format!("~{} · {}", human_size(e.approx_size), e.license).into(),
            status: if download::is_installed(e.id) { 2 } else { 0 },
            progress: 0.0,
        })
        .collect()
}

/// Return a catalog row to the "available" state (e.g. after a failed download).
fn reset_catalog_row(catalog_items: &Rc<VecModel<CatalogRow>>, index: usize) {
    if let Some(mut row) = catalog_items.row_data(index) {
        row.status = 0;
        row.progress = 0.0;
        catalog_items.set_row_data(index, row);
    }
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
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Ask the worker to rebuild the index (the dictionary set changed). The worker
/// keeps serving the old index until the new one is ready; `index_ready` flips
/// back on via the `IndexReady` message.
fn request_rebuild(ui: &AppWindow, req_tx: &Rc<mpsc::Sender<WorkerReq>>) {
    ui.set_index_ready(false);
    let _ = req_tx.send(WorkerReq::Reload { rebuild: true });
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
            .filter(|d| d.enabled && !is_companion_dict(&d.path))
            .map(|d| pretty_dict_name(d.name()).into()),
    );
    scopes.set_vec(labels);

    let rows: Vec<DictRow> = m
        .dictionaries()
        .iter()
        .filter(|d| !is_companion_dict(&d.path))
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
        .filter(|d| d.enabled && !is_companion_dict(&d.path))
        .count()
}

/// Name of the `row`-th managed dictionary (settings list order = manager order).
fn nth_dict_name(manager: &Rc<RefCell<DictionaryManager>>, row: i32) -> Option<String> {
    manager
        .borrow()
        .dictionaries()
        .iter()
        .filter(|d| !is_companion_dict(&d.path))
        .nth(row.max(0) as usize)
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

/// Pick the "word of the moment" for the active `scope`: a stable-per-launch
/// (`seed`-chosen) headword drawn from the dictionary that scope selects, together
/// with that dictionary's name so the lookup can be scoped to it. Index 0 ("All")
/// and any out-of-range index fall back to the first enabled dictionary.
fn word_of_the_moment(
    manager: &Rc<RefCell<DictionaryManager>>,
    scope: i32,
    seed: usize,
) -> (String, Option<String>) {
    let m = manager.borrow();
    let mut enabled = m
        .dictionaries()
        .iter()
        .filter(|d| d.enabled && !is_companion_dict(&d.path));
    let dict = if scope <= 0 {
        enabled.next()
    } else {
        enabled.nth(scope as usize - 1)
    };
    dict.and_then(|d| {
        d.dictionary
            .nth_headword(seed)
            .map(|w| (w, Some(d.name().to_string())))
    })
    .unwrap_or_else(|| ("IronDict".to_string(), None))
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
        .filter(|d| d.enabled && !is_companion_dict(&d.path))
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
        .filter(|d| d.enabled && !is_companion_dict(&d.path))
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

fn is_companion_dict(path: &std::path::Path) -> bool {
    path.to_string_lossy().contains("/fr-conj/")
}

/// A plain-message page (no entry): a "Preparing…" / "No results" / error notice.
fn message_page(section_label: &str, title: &str, body: &str) -> RenderedPage {
    RenderedPage {
        section_label: section_label.to_string(),
        headword: title.to_string(),
        source: String::new(),
        body: PageBody::Message {
            body: body.to_string(),
        },
    }
}

/// Resolve `headword` to its full definition and parse it into render-ready data.
/// This is the heavy half (disk read, HTML stripping, markdown conversion,
/// conjugation) and is `Send`-free of Slint types, so it can run on the worker
/// thread; `apply_page` turns the result into UI models.
fn compute_page(
    manager: &mut DictionaryManager,
    headword: &str,
    label: &str,
    filter: Option<&str>,
) -> RenderedPage {
    let (raw, source, html) = lookup_raw(manager, headword, filter);
    if raw.is_empty() {
        return message_page(label, headword, "No definition found.");
    }

    // Build the plain-text body blocks (marker + text + quote flag), plus the
    // header fields. A quote block is an indented GCIDE example/quotation, shown
    // in lighter italic under the sense it belongs to.
    let (pron, pos, etym, plain): (String, String, String, Vec<(String, String, bool)>) = if html {
        // HTML entry: the first paragraph repeats the headword with its phonetic
        // and part of speech. Lift those onto the grey pron/pos line (as GCIDE
        // does) and drop the duplicate, then convert the rest to plain-text
        // paragraphs so tags don't show and the body can be virtualized.
        let mut paras = html_to_blocks(&raw);
        let (pron, pos, etym) = extract_html_header(&mut paras, headword);
        let blocks = paras
            .into_iter()
            .map(|t| {
                let (marker, body) = split_sense_marker(&t);
                (marker, body, false)
            })
            .collect();
        // The header fields are shown as plain text, so drop any link sentinels.
        (
            strip_link_markers(&pron),
            strip_link_markers(&pos),
            strip_link_markers(&etym),
            blocks,
        )
    } else {
        let parsed = parse_entry(&raw);
        let blocks = if parsed.senses.is_empty() {
            // Fall back to lightly-cleaned text when we couldn't split senses.
            vec![(String::new(), cleaned_plain(&raw), false)]
        } else {
            parsed
                .senses
                .into_iter()
                .enumerate()
                .flat_map(|(i, s)| {
                    // The sense body carries the number; its quotations follow as
                    // markerless italic blocks.
                    std::iter::once((format!("{}.", i + 1), s.body, false))
                        .chain(s.quotes.into_iter().map(|q| (String::new(), q, true)))
                })
                .collect()
        };
        (parsed.pronunciation, parsed.pos, String::new(), blocks)
    };

    // Convert each block's text to markdown with clickable cross-reference links.
    // GCIDE `{word}` braces and HTML `bword://` anchors (carried as `LINK_*`
    // sentinels) both become `[word](lookup://word)` links; the block's plain
    // `text` keeps the link label but not the markup.
    let blocks: Vec<RenderedBlock> = plain
        .into_iter()
        .map(|(marker, text, quote)| {
            let (md, text) = if html {
                (convert_html_refs_to_links(&text), strip_link_markers(&text))
            } else {
                (convert_gcide_refs_to_links(&text), text)
            };
            RenderedBlock {
                marker,
                text,
                md,
                quote,
            }
        })
        .collect();

    let conjugation = compute_conjugation(manager, headword, &raw, &source);

    RenderedPage {
        section_label: label.to_string(),
        headword: headword.to_string(),
        source,
        body: PageBody::Entry {
            pron,
            pos,
            etym,
            blocks,
            conjugation,
        },
    }
}

/// Compute `headword`'s conjugation, sourcing from the companion French
/// conjugation dictionary when it is installed, falling back to the current
/// entry's text otherwise.
fn compute_conjugation(
    manager: &mut DictionaryManager,
    headword: &str,
    raw: &str,
    source: &str,
) -> Vec<RenderedConjSection> {
    let companion_text = if let Ok(results) = manager.lookup(headword) {
        results
            .into_iter()
            .find(|r| r.dictionary == "Conjugaison — Français")
            .map(|r| {
                r.entries
                    .iter()
                    .flat_map(|e| e.segments.iter())
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    } else {
        None
    };

    let language = manager
        .dictionaries()
        .iter()
        .find(|d| d.name() == source)
        .map(|d| d.language)
        .unwrap_or(Language::Auto);

    let def = companion_text
        .as_deref()
        .or(if !raw.is_empty() { Some(raw) } else { None });

    let Some(conj) = ConjugatorRegistry::new().conjugate(headword, def, language) else {
        return Vec::new();
    };
    conj.sections
        .iter()
        .map(|s| RenderedConjSection {
            label: s.label.clone(),
            forms: s
                .forms
                .iter()
                .map(|f| RenderedConjForm {
                    label: f.label.clone(),
                    text: f.text.clone(),
                })
                .collect(),
        })
        .collect()
}

/// Push a computed page into the UI. This is the light half (it builds the Slint
/// models and parses the markdown into styled text) and must run on the UI thread.
fn apply_page(ui: &AppWindow, blocks_model: &Rc<VecModel<DefBlock>>, page: &RenderedPage) {
    ui.set_section_label(page.section_label.as_str().into());
    ui.set_def_headword(page.headword.as_str().into());
    ui.set_def_source(page.source.as_str().into());
    match &page.body {
        PageBody::Message { body } => {
            ui.set_def_pron("".into());
            ui.set_def_pos("".into());
            ui.set_def_etym("".into());
            blocks_model.set_vec(vec![DefBlock {
                marker: "".into(),
                text: body.as_str().into(),
                styled: Default::default(),
                quote: false,
            }]);
            clear_conjugation(ui);
        }
        PageBody::Entry {
            pron,
            pos,
            etym,
            blocks,
            conjugation,
        } => {
            ui.set_def_pron(pron.as_str().into());
            ui.set_def_pos(pos.as_str().into());
            ui.set_def_etym(etym.as_str().into());
            let blocks: Vec<DefBlock> = blocks
                .iter()
                .map(|b| DefBlock {
                    marker: b.marker.as_str().into(),
                    text: b.text.as_str().into(),
                    styled: parse_markdown::<StyledText>(&b.md, &[]),
                    quote: b.quote,
                })
                .collect();
            blocks_model.set_vec(blocks);
            apply_conjugation(ui, conjugation);
        }
    }
}

/// Build the conjugation models from computed sections (UI thread).
fn apply_conjugation(ui: &AppWindow, sections: &[RenderedConjSection]) {
    if sections.is_empty() {
        clear_conjugation(ui);
        return;
    }
    // Group the tense sections into mood tabs, preserving first-appearance order.
    let mut grouped: Vec<(SharedString, Vec<ConjSection>)> = Vec::new();
    for s in sections {
        let group: SharedString = conj_group(&s.label).into();
        let forms: Vec<ConjForm> = s
            .forms
            .iter()
            .map(|f| ConjForm {
                label: f.label.as_str().into(),
                text: f.text.as_str().into(),
            })
            .collect();
        let section = ConjSection {
            label: s.label.as_str().into(),
            forms: ModelRc::from(Rc::new(VecModel::from(forms))),
        };
        match grouped.iter_mut().find(|(g, _)| *g == group) {
            Some((_, secs)) => secs.push(section),
            None => grouped.push((group, vec![section])),
        }
    }
    let moods: Vec<ConjMood> = grouped
        .into_iter()
        .map(|(label, secs)| ConjMood {
            label,
            sections: ModelRc::from(Rc::new(VecModel::from(secs))),
        })
        .collect();
    ui.set_conj_tab(0);
    // The table starts hidden; the page only shows a button to open it.
    ui.set_conjugation(ModelRc::from(Rc::new(VecModel::from(moods))));
    ui.set_show_conjugation(false);
}

/// The mood tab a conjugation section belongs to. French sections group by mood
/// (`Indicatif présent` → `Indicatif`); anything else (e.g. English "Principal
/// parts") forms its own tab.
fn conj_group(label: &str) -> &str {
    const MOODS: &[&str] = &[
        "Indicatif",
        "Subjonctif",
        "Conditionnel",
        "Impératif",
        "Infinitif",
        "Gérondif",
        "Participe",
    ];
    MOODS
        .iter()
        .find(|m| label.starts_with(**m))
        .copied()
        .unwrap_or(label)
}

/// Run a query through the engine and render its first result. Runs on the worker
/// thread; the UI applies the outcome via `apply_results`.
fn compute_search(
    engine: &SearchEngine,
    manager: &mut DictionaryManager,
    query: &str,
    filter: Option<&str>,
) -> RenderedResults {
    let needle = query.trim();

    // Prefix (autocomplete) first; fall back to fuzzy for typos. Both are scoped
    // to the selected dictionary (or all, when no scope is active). Exact-match
    // headwords are always placed first, even if the prefix query doesn't return
    // them (tantivy's RegexQuery DFA misses terms exactly matching the literal).
    let mut hits = engine
        .search_scoped(needle, SearchMode::Prefix, 80, filter)
        .unwrap_or_default();
    if hits.is_empty() {
        // Fuzzy hits are already ranked by edit distance — keep that order.
        hits = engine
            .search_scoped(needle, SearchMode::Fuzzy, 40, filter)
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
    // Always look up the exact match and place it first, even if the prefix
    // query missed it (tantivy bug: `go.*` regex doesn't match term "go").
    if let Ok(exact_hits) = engine.search_scoped(needle, SearchMode::Exact, 1, filter) {
        if let Some(exact) = exact_hits.into_iter().next() {
            // Remove any duplicate from prefix results.
            hits.retain(|h| h.headword.to_lowercase() != needle.to_lowercase());
            hits.insert(0, exact);
        }
    }
    hits.truncate(40);

    let items: Vec<RenderedItem> = hits
        .iter()
        .map(|h| RenderedItem {
            headword: h.headword.clone(),
            snippet: make_snippet(&h.snippet),
            source: h.dictionary.clone(),
        })
        .collect();

    if let Some(first) = hits.first() {
        let page = compute_page(manager, &first.headword, "", filter);
        RenderedResults {
            items,
            first_headword: Some(first.headword.clone()),
            page,
        }
    } else {
        let page = message_page(
            "",
            "No results",
            &format!("Nothing matches \u{201c}{needle}\u{201d}."),
        );
        RenderedResults {
            items,
            first_headword: None,
            page,
        }
    }
}

/// Apply a worker search outcome to the UI: fill the result list, show the first
/// page, and record the first hit in history (so back/forward work).
fn apply_results(
    ui: &AppWindow,
    results_model: &Rc<VecModel<ResultItem>>,
    rows: &Rc<RefCell<Vec<RowData>>>,
    blocks_model: &Rc<VecModel<DefBlock>>,
    history: &Rc<RefCell<NavHistory>>,
    results: RenderedResults,
) {
    let items: Vec<ResultItem> = results
        .items
        .iter()
        .map(|i| ResultItem {
            headword: i.headword.as_str().into(),
            snippet: i.snippet.as_str().into(),
            source: i.source.as_str().into(),
        })
        .collect();
    *rows.borrow_mut() = results
        .items
        .iter()
        .map(|i| RowData {
            headword: i.headword.clone(),
        })
        .collect();
    results_model.set_vec(items);
    ui.set_searching(true);

    apply_page(ui, blocks_model, &results.page);
    if let Some(headword) = results.first_headword {
        ui.set_selected_index(0);
        history.borrow_mut().push(NavEntry {
            headword,
            label: String::new(),
            scope: ui.get_scope(),
        });
        sync_nav_state(ui, history);
    } else {
        ui.set_selected_index(-1);
    }
}

/// Render `headword` (on the UI thread, via the UI's own manager) and record it
/// in the navigation history, so it can be revisited with back/forward. Used for
/// synchronous user actions (clicks, links, word of the moment).
fn navigate(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    blocks_model: &Rc<VecModel<DefBlock>>,
    history: &Rc<RefCell<NavHistory>>,
    headword: &str,
    label: &str,
    filter: Option<&str>,
) {
    let page = compute_page(&mut manager.borrow_mut(), headword, label, filter);
    apply_page(ui, blocks_model, &page);
    history.borrow_mut().push(NavEntry {
        headword: headword.to_string(),
        label: label.to_string(),
        scope: ui.get_scope(),
    });
    sync_nav_state(ui, history);
}

/// Re-show a stored history page (for back/forward), restoring the dictionary
/// scope it was viewed under. Does not touch the history stack.
fn replay(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    blocks_model: &Rc<VecModel<DefBlock>>,
    entry: &NavEntry,
) {
    ui.set_scope(entry.scope);
    let filter = scope_filter(entry.scope, manager);
    let page = compute_page(
        &mut manager.borrow_mut(),
        &entry.headword,
        &entry.label,
        filter.as_deref(),
    );
    apply_page(ui, blocks_model, &page);
}

/// Mirror whether back/forward are possible onto the UI (drives the toolbar
/// buttons' enabled state).
fn sync_nav_state(ui: &AppWindow, history: &Rc<RefCell<NavHistory>>) {
    let h = history.borrow();
    ui.set_can_go_back(h.can_back());
    ui.set_can_go_forward(h.can_forward());
}

fn clear_conjugation(ui: &AppWindow) {
    ui.set_conjugation(ModelRc::from(Rc::new(VecModel::from(Vec::<ConjMood>::new()))));
    ui.set_conj_tab(0);
    ui.set_show_conjugation(false);
}

/// Look up `headword` and return (joined raw definition text, source name,
/// whether the entry is HTML). When `filter` is set, only that dictionary's
/// results are considered.
fn lookup_raw(
    manager: &mut DictionaryManager,
    headword: &str,
    filter: Option<&str>,
) -> (String, String, bool) {
    match manager.lookup(headword) {
        Ok(results) if !results.is_empty() => {
            let results: Vec<_> = results
                .into_iter()
                .filter(|r| filter.is_none_or(|name| r.dictionary == name))
                .collect();
            let Some(first) = results.first() else {
                return (String::new(), String::new(), false);
            };
            let source = first.dictionary.clone();
            // StarDict type 'h' = HTML (`sametypesequence=h`). Only show the source
            // dictionary's own entries: mixing a plain-text and an HTML dictionary
            // (the same headword in both) would leak one's markup into the other's
            // renderer, and the source pill names a single dictionary anyway.
            let html = first
                .entries
                .first()
                .and_then(|e| e.segments.first())
                .map(|s| s.type_.contains('h'))
                .unwrap_or(false);
            let mut parts = Vec::new();
            for r in results.iter().filter(|r| r.dictionary == source) {
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
    // Depth of nested `<font>` elements whose face is a symbol/dingbat: their
    // letters are decorative glyphs, not text (some HTML dictionaries use a
    // Wingdings "v" as a section divider), so we suppress all output inside them.
    let mut symbol_depth = 0usize;
    // Whether we're inside an `<a href="bword://…">` cross-reference. The tags are
    // stripped like everything else, but we wrap the link text in sentinel markers
    // (see `LINK_*`) so it survives flattening and can later become a clickable
    // `lookup://` link, while still reading as plain text for snippets.
    let mut link_open = false;
    while i < chars.len() {
        match chars[i] {
            '<' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '>' {
                    j += 1;
                }
                let tag: String = chars[start..j].iter().collect();
                let trimmed = tag.trim();
                if trimmed.eq_ignore_ascii_case("/font") {
                    symbol_depth = symbol_depth.saturating_sub(1);
                } else if symbol_depth > 0 {
                    // Inside a dingbat run: keep `<font>` nesting balanced; drop all else.
                    if tag_name(trimmed) == "font" {
                        symbol_depth += 1;
                    }
                } else if tag_name(trimmed) == "font" && is_symbol_font(&tag.to_ascii_lowercase()) {
                    symbol_depth += 1;
                } else if tag_name(trimmed) == "a" {
                    if trimmed.starts_with('/') {
                        // Closing </a>: end the link text.
                        if link_open {
                            cur.push(LINK_CLOSE);
                            link_open = false;
                        }
                    } else if let Some(target) = bword_target(trimmed) {
                        // Opening <a href="bword://target">: begin the link text.
                        if link_open {
                            cur.push(LINK_CLOSE);
                        }
                        cur.push(LINK_OPEN);
                        cur.push_str(&target);
                        cur.push(LINK_SEP);
                        link_open = true;
                    }
                    // Anchors without a bword href are dropped, keeping their text.
                } else if is_block_tag(&tag) {
                    if link_open {
                        cur.push(LINK_CLOSE);
                        link_open = false;
                    }
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
                        if symbol_depth == 0 {
                            cur.push_str(&s);
                        }
                    }
                    i = j + 1;
                } else {
                    if symbol_depth == 0 {
                        cur.push('&');
                    }
                    i += 1;
                }
            }
            c => {
                if symbol_depth == 0 {
                    cur.push(c);
                }
                i += 1;
            }
        }
    }
    if link_open {
        cur.push(LINK_CLOSE);
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

/// Lift the header lines out of an HTML entry's leading paragraphs and drop them
/// from the body. The first paragraph is the repeated headword line, e.g.
/// `headword [prɒn] adjective`: its first `[…]` is the phonetic and whatever
/// trails it is the part of speech. A following etymology line (opening with the
/// "étym." label) is lifted too. Returns `(pron, pos, etym)`; leaves a paragraph
/// in place when it doesn't look like a header line.
fn extract_html_header(paras: &mut Vec<String>, headword: &str) -> (String, String, String) {
    let (mut pron, mut pos) = (String::new(), String::new());
    if let Some(first) = paras.first() {
        match (first.find('['), first.find(']')) {
            (Some(open), Some(close)) if open < close => {
                pron = first[open + 1..close].trim().to_string();
                pos = first[close + 1..].trim().to_string();
                paras.remove(0);
            }
            // No bracketed phonetic: only drop the line if it's a bare repeat of
            // the headword, otherwise leave the body as-is.
            _ => {
                if first.trim().eq_ignore_ascii_case(headword.trim()) {
                    paras.remove(0);
                }
            }
        }
    }

    // The etymology line, when present, follows the header; lift it as well.
    let etym = if paras.first().is_some_and(|p| is_etym_line(p)) {
        paras.remove(0)
    } else {
        String::new()
    };

    (pron, pos, etym)
}

/// Whether a paragraph is an etymology line (opens with the "étym." label).
fn is_etym_line(para: &str) -> bool {
    let head = para.trim_start().to_lowercase();
    head.starts_with("étym") || head.starts_with("etym")
}

/// If a paragraph opens with a sense marker — a bullet glyph (`■ a sense…`) or a
/// leading sense number (`1 a sense…`) — split it off as the block's hanging
/// marker so it renders in the marker column (accent, aligned) instead of inline,
/// matching GCIDE's numbered senses. Returns `(marker, body)`.
fn split_sense_marker(para: &str) -> (String, String) {
    let trimmed = para.trim_start();
    let first = trimmed.chars().next();
    if first.is_some_and(is_sense_bullet) {
        let body = trimmed[first.unwrap().len_utf8()..].trim_start();
        return (first.unwrap().to_string(), body.to_string());
    }
    if let Some(split) = split_numeric_marker(trimmed) {
        return split;
    }
    (String::new(), para.to_string())
}

/// Whether `c` is a bullet glyph used to mark a sense (not part of running text).
fn is_sense_bullet(c: char) -> bool {
    matches!(
        c,
        '■' | '□' | '▪' | '▫' | '●' | '○' | '◆' | '◇' | '◊' | '♦' | '•' | '‣'
    )
}

/// Split a leading arabic sense number (one or two digits followed by a space,
/// e.g. `1 (1564) Tuer…`) off as the marker. Returns `None` when the line doesn't
/// open with such a marker, including the `1 250`-style case where the number is
/// really part of running text (the body would then start with another digit).
fn split_numeric_marker(s: &str) -> Option<(String, String)> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = s[digits.len()..].strip_prefix(' ')?.trim_start();
    if rest.is_empty() || rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some((digits, rest.to_string()))
}

/// Collapse whitespace in `cur`, push it as a paragraph if non-empty, and reset.
fn flush_paragraph(cur: &mut String, paras: &mut Vec<String>) {
    let p = collapse_ws(cur);
    if !p.is_empty() {
        paras.push(p);
    }
    cur.clear();
}

/// The lowercased element name of an HTML tag body (without `<`/`>`), e.g.
/// `/DIV style="…"` → `div`.
fn tag_name(tag: &str) -> String {
    tag.trim()
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Whether a (lowercased) `<font>` tag selects a symbol/dingbat face whose
/// letters are decorative glyphs rather than readable text (e.g. a Wingdings "v"
/// used as a section divider).
fn is_symbol_font(tag_lower: &str) -> bool {
    tag_lower.contains("wingdings")
        || tag_lower.contains("webdings")
        || tag_lower.contains("dingbat")
}

/// Whether an HTML tag is block-level (so it ends the current paragraph).
fn is_block_tag(tag: &str) -> bool {
    let name = tag_name(tag);
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

// ---- GCIDE markup parsing (display only; proper rendering is Phase 7) ----

/// A parsed GCIDE entry: pronunciation respelling, part of speech, and senses.
struct Parsed {
    pronunciation: String,
    pos: String,
    senses: Vec<Sense>,
}

/// One numbered (or unnumbered) sense: its definition `body` plus any indented
/// example/quotation lines GCIDE attaches to it, kept so the body pane can show
/// them in a lighter italic style instead of dropping them.
struct Sense {
    body: String,
    quotes: Vec<String>,
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

fn parse_entry(raw: &str) -> Parsed {
    // Keep braces ({...}) intact — they encode cross-references that are
    // later converted to clickable markdown links by convert_gcide_refs_to_links.
    let text = drop_markers(raw);

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
            .map(|s| Sense {
                body: decode_gcide(&s.body),
                quotes: s.quotes.iter().map(|q| decode_gcide(q)).collect(),
            })
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

/// Split a GCIDE entry into its numbered senses, keeping each sense's indented
/// example/quotation lines so the body pane can render them in italic.
fn parse_senses(text: &str) -> Vec<Sense> {
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
            // The marker line carries the start of the sense body; subsequent
            // lines (until the next marker) are continuation prose or quotations.
            let mut block = vec![c[1].as_ref()];
            i += 1;
            while i < lines.len() && !mark.is_match(lines[i]) {
                block.push(lines[i]);
                i += 1;
            }
            if let Some(sense) = split_body_quotes(&block) {
                senses.push(sense);
            }
        } else {
            i += 1;
        }
    }
    senses
}

/// Partition one sense's lines into its definition body (non-indented prose) and
/// its quotations (indented blocks / `-- attribution` lines), grouped by blank
/// lines. Returns `None` if the body is empty.
fn split_body_quotes(lines: &[&str]) -> Option<Sense> {
    let mut body = String::new();
    let mut quotes: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            if !cur.is_empty() {
                quotes.push(collapse_ws(&cur));
                cur.clear();
            }
            continue;
        }
        if is_quote(line) {
            cur.push(' ');
            cur.push_str(t);
        } else {
            if !body.is_empty() {
                body.push(' ');
            }
            body.push_str(t);
        }
    }
    if !cur.is_empty() {
        quotes.push(collapse_ws(&cur));
    }
    let body = collapse_ws(&body);
    if body.is_empty() {
        None
    } else {
        Some(Sense { body, quotes })
    }
}

/// For entries without numbered senses: strip the header (pronunciation,
/// etymology), returning the remaining prose as a single sense plus any
/// quotations attached to it.
fn unnumbered_sense(text: &str) -> Option<Sense> {
    static PRON: OnceLock<Regex> = OnceLock::new();
    static BR: OnceLock<Regex> = OnceLock::new();
    let no_pron = re(&PRON, r"\\[^\\]*\\").replace_all(text, "");
    let no_etym = re(&BR, r"(?s)\[[^\[\]]*\]").replace_all(&no_pron, "");
    let lines: Vec<&str> = no_etym.lines().collect();
    split_body_quotes(&lines)
}

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
        // HTML entry: drop the leading headword block so the preview shows the
        // definition text, not a repeat of the card title.
        let mut blocks = html_to_blocks(raw);
        if blocks.len() > 1 {
            blocks.remove(0);
        }
        strip_link_markers(&blocks.join(" "))
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

/// Convert GCIDE `{word}` braces to markdown `[word](lookup://word)` links.
/// Skips known non-reference labels (language codes, usage markers) and
/// punctuation-only fragments.
fn convert_gcide_refs_to_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '{' if depth == 0 => {
                out.push_str(&text[start..i]);
                depth = 1;
                start = i + c.len_utf8();
            }
            '{' => depth += 1,
            '}' if depth == 1 => {
                let inner = &text[start..i];
                depth = 0;
                start = i + c.len_utf8();
                let trimmed = inner.trim();
                if trimmed.is_empty()
                    || trimmed.len() < 2
                    || trimmed
                        .chars()
                        .all(|c| c.is_ascii_punctuation() || c.is_ascii_digit())
                    || is_skip_label(trimmed)
                {
                    out.push('{');
                    out.push_str(trimmed);
                    out.push('}');
                } else {
                    // Produce a clickable markdown link. The destination is
                    // wrapped in angle brackets so CommonMark accepts the spaces
                    // in multi-word references (e.g. "To go a-begging") — a bare
                    // `(lookup://To go)` is not parsed as a link.
                    let label = trimmed.trim_start_matches("See ").trim();
                    out.push('[');
                    out.push_str(label);
                    out.push_str("](<lookup://");
                    out.push_str(label);
                    out.push_str(">)");
                }
            }
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    out.push_str(&text[start..]);
    out
}

// Sentinel chars (Unicode private-use area, so they never appear in dictionary
// text) that `html_to_blocks` wraps around a cross-reference: the link text reads
// as plain text between them, while the markers let us recover the link target
// after the HTML has been flattened. Layout: OPEN target SEP label CLOSE.
const LINK_OPEN: char = '\u{E000}';
const LINK_SEP: char = '\u{E001}';
const LINK_CLOSE: char = '\u{E002}';

/// Extract the cross-reference target from an `<a …>` tag whose `href` uses the
/// StarDict `bword://` scheme, percent-decoded. `None` for anchors without such a
/// href (e.g. `<a name=…>` or external links), which are dropped without linking.
fn bword_target(tag: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    // `to_ascii_lowercase` preserves byte length, so indices map back to `tag`.
    let href = lower.find("href")?;
    let eq = tag[href..].find('=')? + href;
    let rest = tag[eq + 1..].trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let body = &rest[quote.len_utf8()..];
    let end = body.find(quote)?;
    let value = &body[..end];
    if !value.to_ascii_lowercase().starts_with("bword://") {
        return None;
    }
    let target = percent_decode(&value["bword://".len()..]);
    let target = target.trim();
    (!target.is_empty()).then(|| target.to_string())
}

/// Decode `%XX` escapes in a URL path into UTF-8 (cross-reference targets are
/// occasionally percent-encoded, e.g. for spaces or accents).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Walk text containing `LINK_*` sentinels, rendering each cross-reference either
/// as a markdown `[label](<lookup://target>)` link (`as_markdown`) or as its bare
/// label (for plain-text uses like snippets). Stray/unbalanced markers are dropped.
fn render_links(text: &str, as_markdown: bool) -> String {
    if !text.contains(LINK_OPEN) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != LINK_OPEN {
            // Drop stray separators/closers; copy everything else verbatim.
            if c != LINK_SEP && c != LINK_CLOSE {
                out.push(c);
            }
            continue;
        }
        let mut target = String::new();
        for c in chars.by_ref() {
            if c == LINK_SEP || c == LINK_CLOSE {
                break;
            }
            target.push(c);
        }
        let mut label = String::new();
        for c in chars.by_ref() {
            if c == LINK_CLOSE {
                break;
            }
            label.push(c);
        }
        if label.is_empty() {
            continue;
        }
        if as_markdown && !target.is_empty() {
            // Angle-bracketed destination so multi-word targets (with spaces) parse.
            out.push('[');
            out.push_str(&label);
            out.push_str("](<lookup://");
            out.push_str(&target);
            out.push_str(">)");
        } else {
            out.push_str(&label);
        }
    }
    out
}

/// Turn the cross-reference sentinels left by `html_to_blocks` into clickable
/// markdown links.
fn convert_html_refs_to_links(text: &str) -> String {
    render_links(text, true)
}

/// Strip the cross-reference sentinels, keeping the link text — for plain-text
/// uses (result snippets, the block's selectable text, header fields).
fn strip_link_markers(text: &str) -> String {
    render_links(text, false)
}

/// Known non-headword labels that should not become clickable links.
fn is_skip_label(s: &str) -> bool {
    let lower = s.to_lowercase();
    matches!(
        lower.as_str(),
        "f." | "l."
            | "sp."
            | "gr."
            | "it."
            | "nl."
            | "d."
            | "as."
            | "of."
            | "pg."
            | "g."
            | "obs."
            | "collog."
            | "vulgar"
            | "cant"
            | "slang"
            | "dial."
            | "prov."
            | "law"
            | "mus."
            | "med."
            | "chem."
            | "bot."
            | "zool."
            | "geol."
            | "astron."
            | "math."
            | "sc."
            | "sing."
            | "pl."
            | "fem."
            | "masc."
            | "neut."
            | "comp."
            | "superl."
            | "cf."
            | "i.e."
            | "e.g."
            | "etc."
            | "q.v."
            | "viz."
    ) || (s.len() <= 2 && s.ends_with('.'))
        || s.chars()
            .all(|c| c.is_ascii_punctuation() || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_entry_becomes_clean_paragraphs() {
        let html = "<DIV style=\"font-weight:bold\">headword</DIV> \
                    <DIV>A sample with <SPAN style=\"color: maroon\">colored</span> \
                    <SPAN style=\"font-style:italic\">italic</span> &laquo; m&acirc;cher &raquo;.</DIV>";
        let blocks = html_to_blocks(html);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], "headword");
        assert_eq!(blocks[1], "A sample with colored italic « mâcher ».");
        // no raw tags leak through
        assert!(blocks.iter().all(|b| !b.contains('<') && !b.contains('>')));
    }

    #[test]
    fn drops_wingdings_divider() {
        // Some HTML entries separate the header from the senses with a Wingdings
        // "v" glyph; stripped to text it must not leak as a literal "v" paragraph.
        let html = "<DIV>word</DIV> \
                    <P style=\"text-align: center\"><FONT FACE=\"Wingdings\">v</FONT></P> \
                    <DIV>The definition.</DIV>";
        let blocks = html_to_blocks(html);
        assert_eq!(blocks, vec!["word", "The definition."]);
        assert!(!blocks.iter().any(|b| b == "v"));
    }

    #[test]
    fn html_header_becomes_pron_pos_line() {
        let html = "<DIV>headword, fem [hɛdwɜːd] adjective and noun</DIV> \
                    <DIV>etym. 1500; from sample</DIV>";
        let mut paras = html_to_blocks(html);
        let (pron, pos, etym) = extract_html_header(&mut paras, "headword, fem");
        assert_eq!(pron, "hɛdwɜːd");
        assert_eq!(pos, "adjective and noun");
        // the headword and etymology lines are lifted into the header
        assert_eq!(etym, "etym. 1500; from sample");
        assert!(paras.is_empty());
    }

    #[test]
    fn splits_leading_sense_bullet() {
        let (marker, body) = split_sense_marker("■ A first sense.");
        assert_eq!(marker, "■");
        assert_eq!(body, "A first sense.");
        // Plain paragraphs keep no marker.
        let (marker, body) = split_sense_marker("etym. 1500; from sample");
        assert_eq!(marker, "");
        assert_eq!(body, "etym. 1500; from sample");
    }

    #[test]
    fn splits_leading_sense_number() {
        // Numbered senses get the number lifted into the marker column.
        let (marker, body) = split_sense_marker("1 (1564) A first sense.");
        assert_eq!(marker, "1");
        assert_eq!(body, "(1564) A first sense.");
        let (marker, _) = split_sense_marker("12 A later sense.");
        assert_eq!(marker, "12");
        // A number that's really part of the text (e.g. "1 250 inhabitants") is not
        // a marker, and neither is a three-digit/standalone year.
        assert_eq!(split_sense_marker("1 250 units.").0, "");
        assert_eq!(split_sense_marker("1564 was a year.").0, "");
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
        assert_eq!(parsed.senses.len(), 1);
        assert_eq!(parsed.senses[0].body, "The science of antiquities.");
        assert!(parsed.senses[0].quotes.is_empty());
    }

    #[test]
    fn keeps_quotations_per_sense() {
        // A numbered sense followed by an indented quotation with attribution.
        // The quote is kept (not dropped) and attached to its sense.
        let raw = "Test \\Test\\, n.\n   \
                   1. A first meaning.\n\
                   \x20         An illustrative quotation that\n\
                   \x20         spans two lines.              --Author.\n   \
                   2. A second meaning.\n";
        let parsed = parse_entry(raw);
        assert_eq!(parsed.senses.len(), 2);
        assert_eq!(parsed.senses[0].body, "A first meaning.");
        assert_eq!(
            parsed.senses[0].quotes,
            vec!["An illustrative quotation that spans two lines. --Author."]
        );
        assert_eq!(parsed.senses[1].body, "A second meaning.");
        assert!(parsed.senses[1].quotes.is_empty());
    }

    #[test]
    fn keeps_only_first_phonetic_variant() {
        // "either": two variants plus a reference number.
        let raw = "Either \\Ei\"ther\\ ([=e]\"[th][~e]r or [imac]\"[th][~e]r; 277), a.\n   \
                   1. One of two.\n";
        let parsed = parse_entry(raw);
        assert_eq!(parsed.pronunciation, "ē·thẽr");
    }

    #[test]
    fn html_bword_anchor_becomes_lookup_link() {
        // A StarDict cross-reference (`<a href="bword://…">`) survives flattening
        // as link sentinels, then converts to a clickable lookup link; the plain
        // text keeps just the label.
        let html = "<DIV>voir <A HREF=\"bword://alpha\">alpha</A> et \
                    <SPAN style=\"color: maroon\"><a href=\"bword://beta gamma\">beta gamma</a></SPAN>.</DIV>";
        let blocks = html_to_blocks(html);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            convert_html_refs_to_links(&blocks[0]),
            "voir [alpha](<lookup://alpha>) et [beta gamma](<lookup://beta gamma>)."
        );
        assert_eq!(strip_link_markers(&blocks[0]), "voir alpha et beta gamma.");
        // No sentinel chars leak into the plain text.
        let plain = strip_link_markers(&blocks[0]);
        assert!(
            !plain.contains(LINK_OPEN) && !plain.contains(LINK_SEP) && !plain.contains(LINK_CLOSE)
        );
    }

    #[test]
    fn non_bword_anchors_are_not_linked() {
        // Anchors without a bword href (external links, named anchors) keep their
        // text but produce no link.
        let html = "<DIV><a href=\"http://example.com\">site</a> <a name=\"x\">anchor</a></DIV>";
        let blocks = html_to_blocks(html);
        assert_eq!(blocks[0], "site anchor");
        assert_eq!(convert_html_refs_to_links(&blocks[0]), "site anchor");
    }

    #[test]
    fn bword_target_decodes_percent_escapes() {
        assert_eq!(
            bword_target("a href=\"bword://aller%20%C3%A0\"").as_deref(),
            Some("aller à")
        );
        assert_eq!(
            bword_target("A HREF=\"bword://chien\"").as_deref(),
            Some("chien")
        );
        assert_eq!(bword_target("a href=\"http://x\""), None);
        assert_eq!(bword_target("a name=\"y\""), None);
    }
}
