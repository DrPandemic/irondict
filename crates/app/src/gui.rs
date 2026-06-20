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
use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};
use slint::private_unstable_api::re_exports::{parse_markdown, StyledText};
use slint::{Color, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use irondict_core::{
    bundled_gcide_config, download, search, Config, ConjugatorRegistry, DictionaryManager,
    IndexProgress, Language, Preferences, Progress, SearchEngine, SearchMode, ThemeMode,
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

/// The word of the moment shown this session: its headword and the source
/// dictionary to scope the lookup to (`None` = any). Held in a cell so the
/// cleared-search and scope-change handlers stay in sync on what to show.
type SessionWotm = Rc<RefCell<Option<(String, Option<String>)>>>;

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
#[derive(Serialize, Deserialize)]
struct RenderedBlock {
    marker: String,
    text: String,
    md: String,
    /// True for an indented example/quotation block (rendered italic + greyed).
    quote: bool,
    /// True for a section heading (a part of speech like "Verb", or
    /// "Etymology") — rendered bold and larger, with no marker.
    heading: bool,
    /// List nesting depth (0 = top level); drives the body's left indent so
    /// sub-senses sit under their parent.
    indent: i32,
    /// True for the trailing "Conjugation" button block, rendered at the end of
    /// a verb entry (opens the conjugation overlay). When set, the other fields
    /// are unused.
    conj: bool,
}

/// An intermediate body block, shared by the GCIDE and HTML rendering paths
/// before the markdown/styled text is built. `text` still carries `LINK_*` and
/// `EMPH_*` sentinels.
struct BlockSpec {
    marker: String,
    text: String,
    quote: bool,
    heading: bool,
    indent: i32,
}

impl BlockSpec {
    /// A plain, markerless body paragraph.
    fn plain(text: String) -> Self {
        BlockSpec {
            marker: String::new(),
            text,
            quote: false,
            heading: false,
            indent: 0,
        }
    }

    /// A section heading (a part of speech, "Etymology"): bold, larger, no marker.
    fn heading(text: String) -> Self {
        BlockSpec {
            marker: String::new(),
            text,
            quote: false,
            heading: true,
            indent: 0,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RenderedConjForm {
    label: String,
    text: String,
}

#[derive(Serialize, Deserialize)]
struct RenderedConjSection {
    label: String,
    forms: Vec<RenderedConjForm>,
}

/// The body of a rendered page: either a real entry or a plain message.
#[derive(Serialize, Deserialize)]
enum PageBody {
    Entry {
        pron: String,
        etym: String,
        blocks: Vec<RenderedBlock>,
        conjugation: Vec<RenderedConjSection>,
    },
    Message {
        body: String,
    },
}

/// A fully-computed definition page, ready for `apply_page` to push into the UI.
#[derive(Serialize, Deserialize)]
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
    /// changed). `gen` tags that rebuild so the UI can drop index messages from a
    /// superseded build (only meaningful when `rebuild` is set).
    Reload { rebuild: bool, gen: u64 },
    /// Stop the worker (on shutdown).
    Shutdown,
}

/// A message from the search worker back to the UI thread.
enum WorkerMsg {
    /// The dictionary set finished loading on the worker thread; this is a cheap
    /// copy (sharing the parsed indexes) for the UI thread's synchronous lookups,
    /// sent before the search index is opened so the UI can populate the moment
    /// the parse completes — the window itself is already on screen.
    ManagerReady {
        manager: Box<DictionaryManager>,
    },
    Results {
        gen: u64,
        results: Box<RenderedResults>,
    },
    /// A search arrived before the index was ready.
    NoEngine {
        gen: u64,
    },
    /// Periodic progress while the index is (re)building, for the loading bar.
    /// `gen` tags the build so the UI can drop messages from a superseded one.
    IndexProgress {
        gen: u64,
        indexed: u64,
        total: u64,
    },
    IndexReady {
        gen: u64,
    },
    BuildError {
        gen: u64,
        msg: String,
    },
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
                c.dictionaries.push(bundled_gcide_config());
                let _ = c.save_to(&path);
                c
            } else {
                Config::load_from(&path).unwrap_or_default()
            }
        }
        Err(_) => {
            let mut c = Config::default();
            c.dictionaries.push(bundled_gcide_config());
            c
        }
    };
    let (manager, errors) = DictionaryManager::from_config(&config);
    for e in errors {
        eprintln!("warning: failed to load {}: {}", e.path.display(), e.error);
    }
    manager
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
    progress: impl FnMut(IndexProgress),
) -> Result<Option<SearchEngine>, String> {
    let dir = search::default_index_dir().map_err(|e| e.to_string())?;
    if let Ok(engine) = SearchEngine::open(&dir, manager) {
        warm_up(&engine);
        return Ok(Some(engine));
    }
    match SearchEngine::build_cancellable(&dir, manager, cancel, progress)
        .map_err(|e| e.to_string())?
    {
        Some(engine) => {
            warm_up(&engine);
            Ok(Some(engine))
        }
        None => Ok(None),
    }
}

/// The search worker: owns its own manager + index and serves search/lookup work
/// off the UI thread. It loads the dictionary set itself (the heavy
/// `.idx`/`.syn` parse happens here, off the UI thread, so the window can appear
/// instantly) and immediately hands the UI a cheap `reopen`ed copy sharing that
/// parse via `ManagerReady`. It then builds/opens the index (reporting
/// `IndexReady` or `BuildError`) and processes requests until the UI drops the
/// request channel or sends `Shutdown`. The manager is reloaded from the saved
/// config on `Reload`, and the index rebuilt when the dictionary set changed.
fn run_worker(req_rx: mpsc::Receiver<WorkerReq>, resp_tx: mpsc::Sender<WorkerMsg>) {
    let mut manager = load_manager();
    // Hand the UI thread a copy sharing this one parse, so it can do synchronous
    // lookups without reparsing. Falls back to a fresh parse only if reopening
    // somehow fails, so the UI never gets stuck on the loading screen.
    let ui_copy = manager.reopen().unwrap_or_else(|e| {
        eprintln!("warning: sharing the loaded dictionaries with the UI failed ({e}); reparsing");
        load_manager()
    });
    let _ = resp_tx.send(WorkerMsg::ManagerReady {
        manager: Box::new(ui_copy),
    });
    let mut engine: Option<SearchEngine> = None;
    // A control request pulled off the channel while a build was running, to be
    // handled before blocking for the next one.
    let mut pending: Option<WorkerReq> = None;
    // The generation of the current index build, stamped on every index message so
    // the UI can drop messages from a build that a newer rebuild has superseded.
    // The initial build is generation 0.
    let mut build_gen: u64 = 0;
    rebuild_index(
        &mut engine,
        &mut manager,
        &req_rx,
        &resp_tx,
        &mut pending,
        build_gen,
    );

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
            WorkerReq::Reload { rebuild, gen } => {
                // Pick up any language pins / dictionary-set changes the UI saved.
                manager = load_manager();
                if rebuild {
                    build_gen = gen;
                    rebuild_index(
                        &mut engine,
                        &mut manager,
                        &req_rx,
                        &resp_tx,
                        &mut pending,
                        build_gen,
                    );
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
    gen: u64,
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
    let progress = |p: IndexProgress| {
        let _ = resp_tx.send(WorkerMsg::IndexProgress {
            gen,
            indexed: p.indexed,
            total: p.total,
        });
    };
    match prepare_engine(manager, cancel, progress) {
        Ok(Some(e)) => {
            *engine = Some(e);
            let _ = resp_tx.send(WorkerMsg::IndexReady { gen });
        }
        // Cancelled: a superseding request is now pending, which will drive the
        // next (correct) rebuild. Keep the previous engine state untouched.
        Ok(None) => {}
        Err(msg) => {
            let _ = resp_tx.send(WorkerMsg::BuildError { gen, msg });
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
pub fn run(initial: Option<String>, scope: Option<String>) -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    // The accent (indigo) and light default live in the .slint; the OS values are
    // detected and applied below, off the startup path.

    // Boot instantly: start with no dictionaries loaded. The expensive
    // `.idx`/`.syn` parse runs on the search-worker thread, which hands back a
    // cheap shared copy (`WorkerMsg::ManagerReady`) once done — so the window
    // appears immediately instead of after a multi-second parse.
    let manager = Rc::new(RefCell::new(DictionaryManager::new()));
    // The dictionary set isn't loaded yet, but its preferences (theme, accent)
    // live in the config and are cheap to read, so apply them now for a correct
    // first paint rather than flashing the default theme.
    if let Some(prefs) = Config::default_path()
        .ok()
        .filter(|p| p.exists())
        .and_then(|p| Config::load_from(&p).ok())
        .map(|c| c.preferences)
    {
        *manager.borrow_mut().preferences_mut() = prefs;
    }
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

    // Apply the theme (persisted override, else OS detection) from the cheaply
    // loaded preferences, without blocking startup.
    apply_appearance(&ui, &manager);

    // The word of the moment is *chosen at the previous launch* and rendered to a
    // cache file, so this launch can show it instantly — before the dictionaries
    // finish loading in the background. That's the boot cheat: no "loading" page,
    // the window comes up already showing a real word. `seed` only matters as a
    // fallback for drawing a word live (first launch, scope changes).
    let seed = random_seed();
    // The word shown this session. Seeded from the cached page so clearing the
    // search box returns to the same word; scope changes refresh it.
    let session_wotm: SessionWotm = Rc::new(RefCell::new(None));
    match load_cached_wotm() {
        // The previous launch's pick: show it immediately, with no hint that the
        // dictionaries are still loading.
        Some(page) => {
            *session_wotm.borrow_mut() = wotm_identity(&page);
            apply_page(&ui, &blocks_model, &page);
        }
        // First launch (or a cleared cache): nothing cached yet. Leave the pane
        // blank rather than advertising the load; `apply_loaded_manager` fills in
        // the word the instant the dictionaries are ready.
        None => apply_page(&ui, &blocks_model, &message_page("", "", "")),
    }

    // Spin up the search worker. It builds/opens the index and runs every search
    // and first-result render off the UI thread, so typing never blocks. Requests
    // go out on `req_tx`; results come back on `resp_rx`, drained by a fast timer.
    // `gen` tags each search so stale responses (the user kept typing) are dropped.
    let (req_tx, req_rx) = mpsc::channel::<WorkerReq>();
    let (resp_tx, resp_rx) = mpsc::channel::<WorkerMsg>();
    let req_tx = Rc::new(req_tx);
    let gen = Rc::new(Cell::new(0u64));
    // Generation of the latest requested index build, bumped on every rebuild so
    // stale index messages from a superseded build can be dropped. Starts at 0 to
    // match the worker's initial build.
    let build_gen = Rc::new(Cell::new(0u64));
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
        let build_gen = build_gen.clone();
        let dict_items = dict_items.clone();
        let scopes = scopes.clone();
        let session_wotm = session_wotm.clone();
        // When the current index build began, so the time-remaining estimate can
        // extrapolate from the elapsed time. `None` between builds.
        let mut build_start: Option<Instant> = None;
        resp_timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
            while let Ok(msg) = resp_rx.try_recv() {
                let ui = ui_weak.unwrap();
                match msg {
                    WorkerMsg::ManagerReady { manager: loaded } => {
                        // The background load finished: adopt the real dictionary
                        // set and populate the UI (lists, scope, first page).
                        *manager.borrow_mut() = *loaded;
                        apply_loaded_manager(
                            &ui,
                            &manager,
                            &dict_items,
                            &scopes,
                            &blocks_model,
                            &history,
                            &session_wotm,
                            initial.as_deref(),
                            scope.as_deref(),
                            seed,
                        );
                    }
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
                    WorkerMsg::IndexProgress {
                        gen: g,
                        indexed,
                        total,
                    } => {
                        // Drop progress from a build a newer rebuild has superseded.
                        if g != build_gen.get() {
                            continue;
                        }
                        let start = *build_start.get_or_insert_with(Instant::now);
                        let fraction = if total > 0 {
                            (indexed as f32 / total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        ui.set_indexing(true);
                        ui.set_index_progress(fraction);
                        ui.set_index_status(index_status(start.elapsed(), fraction).into());
                    }
                    WorkerMsg::IndexReady { gen: g } => {
                        // A stale "ready" must not clear the loading state while a
                        // newer rebuild is still pending or running.
                        if g != build_gen.get() {
                            continue;
                        }
                        build_start = None;
                        ui.set_indexing(false);
                        ui.set_index_progress(1.0);
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
                    WorkerMsg::BuildError { gen: g, msg } => {
                        if g != build_gen.get() {
                            continue;
                        }
                        build_start = None;
                        ui.set_indexing(false);
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
        let session_wotm = session_wotm.clone();
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
                // Return to the session's word of the moment (the one booted on),
                // drawing one live only if the session hasn't picked one yet.
                let cached = session_wotm.borrow().clone();
                let (wotm, wotm_src) = match cached {
                    Some(p) => p,
                    None => {
                        let p = word_of_the_moment(&manager, ui.get_scope(), seed);
                        *session_wotm.borrow_mut() = Some(p.clone());
                        p
                    }
                };
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
        let session_wotm = session_wotm.clone();
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
                *session_wotm.borrow_mut() = Some((wotm.clone(), wotm_src.clone()));
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
            // Keep the next-launch word aligned with the now-selected dictionary,
            // which is also what the next launch will restore as its scope.
            cache_next_wotm(&manager, idx);
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
        let build_gen = build_gen.clone();
        ui.on_toggle_dict(move |row| {
            let ui = ui_weak.unwrap();
            let name = nth_dict_name(&manager, row);
            if let Some(name) = name {
                let now = !is_enabled(&manager, &name);
                manager.borrow_mut().set_enabled(&name, now);
                save_config(&manager);
                refresh_lists(&ui, &manager, &dict_items, &scopes);
                request_rebuild(&ui, &req_tx, &build_gen);
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
                let _ = req_tx.send(WorkerReq::Reload {
                    rebuild: false,
                    gen: 0,
                });
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
        let build_gen = build_gen.clone();
        ui.on_remove_dict(move |row| {
            let ui = ui_weak.unwrap();
            if let Some(name) = nth_dict_name(&manager, row) {
                manager.borrow_mut().remove(&name);
                save_config(&manager);
                refresh_lists(&ui, &manager, &dict_items, &scopes);
                request_rebuild(&ui, &req_tx, &build_gen);
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
        let build_gen = build_gen.clone();
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
                    request_rebuild(&ui, &req_tx, &build_gen);
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
        let build_gen = build_gen.clone();
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
                                request_rebuild(&ui, &req_tx, &build_gen);
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
                                        // Skip companions not yet published as a
                                        // catalog asset (e.g. en-conj/it-conj).
                                        .filter(|c| download::find(c).is_some())
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
        let build_gen = build_gen.clone();
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
            request_rebuild(&ui, &req_tx, &build_gen);
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
    Progress {
        index: Option<usize>,
        fraction: f32,
    },
    Done {
        index: Option<usize>,
        ifo: PathBuf,
        language: Language,
    },
    Failed {
        index: Option<usize>,
        error: String,
    },
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

/// Build the search-box status line shown while indexing, e.g.
/// `Indexing… 42% · about 5 s left`. The estimate extrapolates the remaining
/// fraction from the elapsed time; it's omitted until there's enough signal (a
/// little progress and a little time) to avoid a wild first guess.
fn index_status(elapsed: Duration, fraction: f32) -> String {
    let percent = (fraction * 100.0).round() as u32;
    if fraction <= 0.02 || elapsed < Duration::from_millis(400) {
        return format!("Indexing… {percent}%");
    }
    let remaining = elapsed.as_secs_f32() * (1.0 - fraction) / fraction;
    format!(
        "Indexing… {percent}% · about {} left",
        human_duration(remaining)
    )
}

/// Render a rough seconds estimate as `5 s` / `1 min 20 s`, kept coarse since
/// it's only an extrapolation.
fn human_duration(secs: f32) -> String {
    let secs = secs.ceil().max(1.0) as u64;
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min {} s", secs / 60, secs % 60)
    }
}

/// Ask the worker to rebuild the index (the dictionary set changed). The worker
/// keeps serving the old index until the new one is ready; `indexing` clears
/// again via the `IndexReady` message.
fn request_rebuild(
    ui: &AppWindow,
    req_tx: &Rc<mpsc::Sender<WorkerReq>>,
    build_gen: &Rc<Cell<u64>>,
) {
    // Show the loading bar immediately, at empty. Every caller changes the
    // dictionary set, so the signature never matches the cached manifest and the
    // worker always does a real build — but its first `IndexProgress` only lands
    // after it has reloaded the manager (re-parsing every dictionary) and started
    // building. Without flipping `indexing` here the bar wouldn't appear until
    // then, leaving the stale page on screen with no loading state in between.
    ui.set_index_progress(0.0);
    ui.set_index_status("Indexing…".into());
    ui.set_indexing(true);
    // Bump the build generation so any index message still in flight from a
    // previous build (e.g. its `IndexReady`) is recognised as stale and ignored,
    // rather than clearing the loading bar we just showed.
    build_gen.set(build_gen.get() + 1);
    let _ = req_tx.send(WorkerReq::Reload {
        rebuild: true,
        gen: build_gen.get(),
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
/// Populate the UI from the freshly-loaded dictionary set once the background
/// load finishes: rebuild the scope/settings lists and theme choices, restore
/// the active scope, and render the first page (the launcher word, or the word of
/// the moment). Until this runs the window is already on screen showing a loading
/// page, so all the dictionary-dependent work is deferred to here.
#[allow(clippy::too_many_arguments)]
fn apply_loaded_manager(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    dict_items: &Rc<VecModel<DictRow>>,
    scopes: &Rc<VecModel<SharedString>>,
    blocks_model: &Rc<VecModel<DefBlock>>,
    history: &Rc<RefCell<NavHistory>>,
    session_wotm: &SessionWotm,
    initial: Option<&str>,
    scope: Option<&str>,
    seed: usize,
) {
    refresh_lists(ui, manager, dict_items, scopes);
    {
        let prefs = manager.borrow().preferences().clone();
        ui.set_theme_mode(theme_mode_index(prefs.theme_mode));
        ui.set_accent_choice(accent_choice_index(&prefs));
    }
    apply_appearance(ui, manager);

    // Pick the active dictionary scope (refresh_lists reset it to "All").
    match scope {
        // Launched scoped to one dictionary (e.g. a per-language launcher trigger).
        // Falls back to "All" if the name no longer matches an enabled dictionary.
        Some(name) => ui.set_scope(scope_index_for(manager, Some(name))),
        // Opened with a word but no scope: keep "All" so the lookup spans every
        // dictionary.
        None if initial.is_some() => {}
        // Normal launch: restore the last-used scope.
        None => {
            let last = manager.borrow().preferences().last_scope.clone();
            ui.set_scope(scope_index_for(manager, last.as_deref()));
        }
    }

    match initial.map(str::trim).filter(|w| !w.is_empty()) {
        // Opened with a word (e.g. from a launcher): show its definition and seed
        // the search box so the result list fills with it once the index is ready.
        Some(word) => {
            ui.set_query(word.into());
            let filter = scope_filter(ui.get_scope(), manager);
            navigate(ui, manager, blocks_model, history, word, "", filter.as_deref());
        }
        // No launcher word: the window is already showing the word of the moment
        // the previous launch cached. Only draw one here if there was nothing to
        // show (first launch / cleared cache) and the user hasn't started typing.
        None => {
            if session_wotm.borrow().is_none() && ui.get_query().trim().is_empty() {
                let (wotm, wotm_src) = word_of_the_moment(manager, ui.get_scope(), seed);
                *session_wotm.borrow_mut() = Some((wotm.clone(), wotm_src.clone()));
                navigate(
                    ui,
                    manager,
                    blocks_model,
                    history,
                    &wotm,
                    "WORD OF THE MOMENT",
                    wotm_src.as_deref(),
                );
            }
        }
    }

    // Now that the dictionaries are loaded, pick and render the word the *next*
    // launch will show, scoped to the dictionary this launch restored.
    cache_next_wotm(manager, ui.get_scope());
}

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
        Language::Italian => 3,
    }
}

fn index_to_lang(index: i32) -> Language {
    match index {
        1 => Language::English,
        2 => Language::French,
        3 => Language::Italian,
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

/// A fresh, effectively-random seed for drawing a word of the moment.
fn random_seed() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
}

/// Path to the cached word-of-the-moment page: a rendered page the *previous*
/// launch picked, so this launch can show it before the dictionaries finish
/// loading. Lives in the cache dir alongside the index/`.idx` caches.
fn wotm_cache_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "irondict")?;
    Some(dirs.cache_dir().join("wotm.json"))
}

/// Load the word of the moment the previous launch cached, if any.
fn load_cached_wotm() -> Option<RenderedPage> {
    let bytes = std::fs::read(wotm_cache_path()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Cache a rendered word-of-the-moment page for the next launch to show. Best
/// effort: failures (no cache dir, write error) are ignored — the next launch
/// just falls back to a blank pane until the dictionaries load.
fn save_cached_wotm(page: &RenderedPage) {
    let Some(path) = wotm_cache_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(page) {
        let _ = std::fs::write(path, bytes);
    }
}

/// The (headword, source) a cached entry page looks up, or `None` for a
/// non-entry (message) page. Lets the session reuse the cached word so clearing
/// the search box returns to the same word it booted on.
fn wotm_identity(page: &RenderedPage) -> Option<(String, Option<String>)> {
    match page.body {
        PageBody::Entry { .. } => Some((
            page.headword.clone(),
            (!page.source.is_empty()).then(|| page.source.clone()),
        )),
        PageBody::Message { .. } => None,
    }
}

/// Draw and render a fresh word of the moment for `scope`, then cache it for the
/// *next* launch — the "chosen at the previous boot" half of the boot cheat. The
/// current launch never blocks on this; it runs once the dictionaries are loaded.
fn cache_next_wotm(manager: &Rc<RefCell<DictionaryManager>>, scope: i32) {
    let (word, src) = word_of_the_moment(manager, scope, random_seed());
    let page = compute_page(
        &mut manager.borrow_mut(),
        &word,
        "WORD OF THE MOMENT",
        src.as_deref(),
    );
    save_cached_wotm(&page);
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
    let lower = name.to_lowercase();
    if lower.contains("gcide") {
        return "GCIDE".to_string();
    }
    if lower.contains("wiktionnaire") || lower.contains("wiktionary") {
        // "Wiktionnaire Français-Français" -> "Wiki Français": the xxyzz releases
        // are monolingual, so collapse the repeated language pair, and abbreviate
        // the long dictionary name to keep the tab short while keeping the language.
        // "Wiktionary — Italiano" -> "Wiki Italiano": the DrPandemic releases
        // use an em-dash separator with a single language name.
        if let Some((_, lang)) = name.split_once(" \u{2014} ") {
            return format!("Wiki {lang}");
        }
        // "Wiktionnaire Français-Français" -> "Wiki Français": the xxyzz releases
        // are monolingual, so collapse the repeated language pair, and abbreviate
        // the long dictionary name to keep the tab short while keeping the language.
        if let Some((_, pair)) = name.rsplit_once(' ') {
            if let Some((a, b)) = pair.split_once('-') {
                if a.eq_ignore_ascii_case(b) {
                    return format!("Wiki {a}");
                }
            }
        }
    }
    name.to_string()
}

fn is_companion_dict(path: &std::path::Path) -> bool {
    download::path_is_companion(path)
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

    // Build the body blocks (marker + text + quote/heading flags + indent), plus
    // the header fields. A quote block is an indented example/quotation, shown in
    // lighter italic under the sense it belongs to.
    let (pron, pos, etym, specs): (String, String, String, Vec<BlockSpec>) = if html {
        // HTML entry: lift the pronunciation onto the grey line, keep the part of
        // speech / "Etymology" headings and the nested-list sense numbering, and
        // convert the rest to plain-text paragraphs so tags don't show and the
        // body can be virtualized.
        let mut paras = html_to_blocks(&raw);
        let (pron, pos, etym) = extract_html_header(&mut paras, headword);
        let blocks = paras
            .into_iter()
            .map(|b| {
                // Some HTML dictionaries embed the sense marker in the text (a
                // leading bullet/number) rather than as list structure; lift it
                // when the list walk didn't already supply one.
                let (marker, text) = if b.marker.is_empty() && !b.quote && !b.heading {
                    split_sense_marker(&b.text)
                } else {
                    (b.marker, b.text)
                };
                BlockSpec {
                    marker,
                    text,
                    quote: b.quote,
                    heading: b.heading,
                    indent: b.indent,
                }
            })
            .collect();
        // The header fields are shown as plain text, so drop any sentinels.
        (
            strip_link_markers(&pron),
            strip_link_markers(&pos),
            strip_link_markers(&etym),
            blocks,
        )
    } else {
        let parsed = parse_entry(&raw);
        let sections = parse_sections(&raw);
        let has_senses = sections.iter().any(|s| !s.senses.is_empty());
        let blocks = if !has_senses {
            // Fall back to lightly-cleaned text when we couldn't split senses.
            vec![BlockSpec::plain(cleaned_plain(&raw))]
        } else {
            // Each part-of-speech sub-entry contributes a heading followed by its
            // senses, numbered restarting at 1, so the POS sits above the part of
            // the description it governs (a noun+verb homograph reads "noun … verb").
            let mut blocks: Vec<BlockSpec> = Vec::new();
            for section in sections {
                if section.senses.is_empty() {
                    continue;
                }
                if !section.pos.is_empty() {
                    blocks.push(BlockSpec::heading(section.pos));
                }
                for (i, s) in section.senses.into_iter().enumerate() {
                    blocks.push(BlockSpec {
                        marker: format!("{}.", i + 1),
                        text: s.body,
                        quote: false,
                        heading: false,
                        indent: 0,
                    });
                    blocks.extend(s.quotes.into_iter().map(|q| BlockSpec {
                        marker: String::new(),
                        text: q,
                        quote: true,
                        heading: false,
                        indent: 0,
                    }));
                }
            }
            blocks
        };
        // `parsed.pos` is the joined POS, kept only to gate the conjugation button;
        // it is no longer shown at the top (the per-section headings carry it).
        (parsed.pronunciation, parsed.pos, String::new(), blocks)
    };

    // Hide the pronunciation line when it merely echoes the headword: GCIDE's
    // respelling of a word with no phonetic diacritics (e.g. "Hel·lo" for "Hello")
    // adds nothing over the title, while genuine phonetics (vowel diacritics, IPA)
    // differ from the headword and are kept.
    let pron = if pron_echoes_headword(&pron, headword) {
        String::new()
    } else {
        pron
    };

    // Convert each block's text to markdown with clickable cross-reference links.
    // GCIDE `{word}` braces and HTML `bword://` anchors (carried as `LINK_*`
    // sentinels) both become `[word](lookup://word)` links, and HTML `<b>`/`<i>`
    // emphasis (carried as `EMPH_*` sentinels) becomes markdown bold/italic; the
    // block's plain `text` keeps the labels but not the markup.
    let mut blocks: Vec<RenderedBlock> = specs
        .into_iter()
        .map(|b| {
            let (md, text) = if html {
                (
                    convert_html_refs_to_links(&b.text),
                    strip_link_markers(&b.text),
                )
            } else {
                (convert_gcide_refs_to_links(&b.text), b.text)
            };
            RenderedBlock {
                marker: b.marker,
                text,
                md,
                quote: b.quote,
                heading: b.heading,
                indent: b.indent,
                conj: false,
            }
        })
        .collect();

    // Offer conjugation whenever a verb is among the entry's parts of speech, so
    // the button agrees with the POS headings the user sees: a noun+verb homograph
    // like "jump"/"love"/"mouse" conjugates again, while a pure noun like "table"
    // (no verb POS) gets none. compute_conjugation stays unforced, so its
    // verb-evidence guard still vetoes a bogus grid.
    let conjugation = if pos_is_verb(&pos) {
        compute_conjugation(manager, headword, &raw, &source)
    } else {
        Vec::new()
    };

    // The "Conjugation" button sits just under the first verb section heading, so
    // it rides with the verb content it conjugates, above that section's senses.
    // It's a body block so it flows and scrolls with the content; fall back to the
    // end if no verb heading exists.
    if !conjugation.is_empty() {
        let button = RenderedBlock {
            marker: String::new(),
            text: String::new(),
            md: String::new(),
            quote: false,
            heading: false,
            indent: 0,
            conj: true,
        };
        match blocks.iter().position(|b| b.heading && pos_is_verb(&b.text)) {
            Some(idx) => blocks.insert(idx + 1, button),
            None => blocks.push(button),
        }
    }

    RenderedPage {
        section_label: label.to_string(),
        headword: headword.to_string(),
        source,
        body: PageBody::Entry {
            pron,
            etym,
            blocks,
            conjugation,
        },
    }
}

/// Compute `headword`'s conjugation, sourcing from the conjugation companion
/// for the source dictionary's language when it is installed, falling back to
/// the current entry's text otherwise.
fn compute_conjugation(
    manager: &mut DictionaryManager,
    headword: &str,
    raw: &str,
    source: &str,
) -> Vec<RenderedConjSection> {
    let language = manager
        .dictionaries()
        .iter()
        .find(|d| d.name() == source)
        .map(|d| d.language)
        .unwrap_or(Language::Auto);

    let companion_text = manager.companion_text(headword, language);

    let def = companion_text
        .as_deref()
        .or(if !raw.is_empty() { Some(raw) } else { None });

    // Not forced: the backend declines non-verbs (e.g. the noun "mouse"), so an
    // empty result hides the Conjugation button rather than generating a bogus
    // grid for every headword in an English dictionary.
    let Some(conj) = ConjugatorRegistry::new().conjugate(headword, def, language, false) else {
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
            ui.set_def_etym("".into());
            blocks_model.set_vec(vec![DefBlock {
                marker: "".into(),
                text: body.as_str().into(),
                styled: Default::default(),
                quote: false,
                heading: false,
                indent: 0,
                conj: false,
            }]);
            clear_conjugation(ui);
        }
        PageBody::Entry {
            pron,
            etym,
            blocks,
            conjugation,
        } => {
            ui.set_def_pron(pron.as_str().into());
            ui.set_def_etym(etym.as_str().into());
            let blocks: Vec<DefBlock> = blocks
                .iter()
                .map(|b| DefBlock {
                    marker: b.marker.as_str().into(),
                    text: b.text.as_str().into(),
                    styled: parse_markdown::<StyledText>(&b.md, &[]),
                    quote: b.quote,
                    heading: b.heading,
                    indent: b.indent,
                    conj: b.conj,
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

/// The mood tab a conjugation section belongs to: sections group by mood
/// (`Indicatif présent` → `Indicatif`, `Indicative present` → `Indicative`);
/// any label without a known mood prefix forms its own tab.
fn conj_group(label: &str) -> &str {
    const MOODS: &[&str] = &[
        // English
        "Indicative",
        "Conditional",
        "Non-finite",
        // French
        "Indicatif",
        "Subjonctif",
        "Conditionnel",
        "Impératif",
        "Infinitif",
        "Gérondif",
        "Participe",
        // Italian
        "Indicativo",
        "Congiuntivo",
        "Condizionale",
        "Imperativo",
        "Infinito",
        "Gerundio",
        "Participio",
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
    // to the selected dictionary (or all, when no scope is active). The engine
    // already includes and ranks the exact match first (accent-insensitively) and
    // orders completions shortest-first, so no extra exact pass is needed here.
    let mut hits = engine
        .search_scoped(needle, SearchMode::Prefix, 80, filter)
        .unwrap_or_default();
    if hits.is_empty() {
        // Fuzzy hits are already ranked by edit distance — keep that order.
        hits = engine
            .search_scoped(needle, SearchMode::Fuzzy, 40, filter)
            .unwrap_or_default();
    }
    hits.truncate(40);

    // The index no longer stores definitions, so read each row's preview snippet
    // on demand from the dictionary it was found in. Result lists are short
    // (truncated above), so the per-row lookup is bounded.
    let items: Vec<RenderedItem> = hits
        .iter()
        .map(|h| RenderedItem {
            headword: h.headword.clone(),
            snippet: fetch_snippet(manager, &h.dictionary, &h.headword),
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
    ui.set_conjugation(ModelRc::from(Rc::new(VecModel::from(
        Vec::<ConjMood>::new(),
    ))));
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

/// One block parsed from an HTML dictionary entry: its hanging sense marker
/// (derived from list nesting), its text (with `LINK_*`/`EMPH_*` sentinels still
/// embedded), and flags for usage examples (`quote`) and section headings.
struct HtmlBlock {
    marker: String,
    text: String,
    quote: bool,
    heading: bool,
    /// List nesting depth (0 = top level); drives the body's left indent.
    indent: i32,
}

/// Convert an HTML dictionary entry into render-ready blocks: split on
/// block-level tags, lift list structure into sense markers, keep `<b>`/`<i>`
/// emphasis and `bword://` links as sentinels, strip the rest, and decode
/// entities. Long paragraphs are chopped so each ListView delegate stays small
/// (and the body renders at 60fps).
// `flush!` clears `link_open` after the final flush, where the value is no longer
// read — an expected dead store given the macro is reused mid-stream too.
#[allow(unused_assignments)]
fn html_to_blocks(html: &str) -> Vec<HtmlBlock> {
    let chars: Vec<char> = html.chars().collect();
    let mut out: Vec<HtmlBlock> = Vec::new();
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
    // Open list frames: `(ordered, items-so-far)`. Drives the hanging sense
    // markers ("1.", "a.", "i.", "•") and the body indent, mirroring how
    // Wiktionary nests its senses in `<ol>`/`<ul>`.
    let mut lists: Vec<(bool, usize)> = Vec::new();
    // Marker staged by the most recent `<li>`, hung on its first flushed block.
    let mut pending_marker = String::new();
    // Nesting of `<dd>` (usage examples / quotations) and `<h1>`..`<h6>` headings,
    // so flushed blocks inside them can be flagged.
    let mut dd_depth = 0usize;
    let mut heading_depth = 0usize;
    // Open `<b>`/`<i>` emphasis sentinels and the byte offset where each was
    // pushed, so an empty `<b></b>` can be dropped instead of emitting bare `**`.
    let mut emph: Vec<(char, usize)> = Vec::new();

    // Flush the accumulated text as one block, closing any open emphasis/link so
    // the markdown stays balanced. `indent` is the current list depth.
    macro_rules! flush {
        () => {{
            close_emphasis(&mut cur, &mut emph);
            if link_open {
                cur.push(LINK_CLOSE);
                link_open = false;
            }
            let text = collapse_ws(&cur);
            cur.clear();
            if !text.is_empty() {
                out.push(HtmlBlock {
                    marker: std::mem::take(&mut pending_marker),
                    text,
                    quote: dd_depth > 0,
                    heading: heading_depth > 0,
                    indent: lists.len().saturating_sub(1) as i32,
                });
            }
        }};
    }

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
                let closing = trimmed.starts_with('/');
                let name = tag_name(trimmed);
                if trimmed.eq_ignore_ascii_case("/font") {
                    symbol_depth = symbol_depth.saturating_sub(1);
                } else if symbol_depth > 0 {
                    // Inside a dingbat run: keep `<font>` nesting balanced; drop all else.
                    if name == "font" {
                        symbol_depth += 1;
                    }
                } else if name == "font" && is_symbol_font(&tag.to_ascii_lowercase()) {
                    symbol_depth += 1;
                } else if name == "a" {
                    if closing {
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
                } else if matches!(name.as_str(), "b" | "strong") {
                    // Drop inline bold: keep the text plain to match GCIDE's
                    // formatting. Wiktionary bolds the headword and every
                    // inflected form inside its examples, which renders as noise.
                } else if matches!(name.as_str(), "i" | "em") {
                    toggle_emphasis(&mut cur, &mut emph, EMPH_ITAL, closing);
                } else if name == "ol" || name == "ul" {
                    flush!();
                    if closing {
                        lists.pop();
                    } else {
                        lists.push((name == "ol", 0));
                    }
                } else if name == "li" {
                    // End the previous item, then stage the new item's marker.
                    flush!();
                    if !closing {
                        let depth = lists.len();
                        if let Some((ordered, count)) = lists.last_mut() {
                            *count += 1;
                            pending_marker = list_marker(*ordered, depth, *count);
                        }
                    }
                } else if name == "dd" || name == "dt" {
                    if closing {
                        flush!();
                        dd_depth = dd_depth.saturating_sub(1);
                    } else {
                        dd_depth += 1;
                        flush!();
                    }
                } else if matches!(name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
                    if closing {
                        flush!();
                        heading_depth = heading_depth.saturating_sub(1);
                    } else {
                        heading_depth += 1;
                        flush!();
                    }
                } else if is_block_tag(&tag) {
                    flush!();
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
    flush!();

    // Long paragraphs are chopped so each ListView delegate stays small (60fps),
    // carrying their block's flags onto every piece.
    let mut split = Vec::new();
    for b in out {
        if b.text.chars().count() > 900 {
            let mut first = true;
            for piece in split_long(&b.text, 800) {
                split.push(HtmlBlock {
                    marker: if first {
                        b.marker.clone()
                    } else {
                        String::new()
                    },
                    text: piece,
                    quote: b.quote,
                    heading: b.heading,
                    indent: b.indent,
                });
                first = false;
            }
        } else {
            split.push(b);
        }
    }
    split
}

/// Open or close an emphasis run (`<b>`/`<i>`), tracking its position so an empty
/// run (`<b></b>`) leaves no stray markdown. On close, only the matching
/// innermost run is emitted, keeping the sentinel pairs balanced.
fn toggle_emphasis(cur: &mut String, emph: &mut Vec<(char, usize)>, sentinel: char, closing: bool) {
    if closing {
        if matches!(emph.last(), Some((c, _)) if *c == sentinel) {
            let (ch, pos) = emph.pop().unwrap();
            emit_or_drop_emphasis(cur, ch, pos);
        }
    } else {
        emph.push((sentinel, cur.len()));
        cur.push(sentinel);
    }
}

/// Close every still-open emphasis run (e.g. at a block boundary).
fn close_emphasis(cur: &mut String, emph: &mut Vec<(char, usize)>) {
    while let Some((ch, pos)) = emph.pop() {
        emit_or_drop_emphasis(cur, ch, pos);
    }
}

/// Emit the closing emphasis sentinel, or — when the run wrapped no real text —
/// remove the opening one by truncating back to `pos`.
fn emit_or_drop_emphasis(cur: &mut String, ch: char, pos: usize) {
    let body_is_empty = cur[pos + ch.len_utf8()..]
        .chars()
        .all(|c| c.is_whitespace() || is_sentinel(c));
    if body_is_empty {
        cur.truncate(pos);
    } else {
        cur.push(ch);
    }
}

/// The hanging marker for a list item: a bullet for `<ul>`, or a number/letter/
/// roman numeral cycling by depth for `<ol>` (so nested senses read 1 → a → i).
fn list_marker(ordered: bool, depth: usize, count: usize) -> String {
    if !ordered {
        return "•".to_string();
    }
    match (depth - 1) % 3 {
        0 => format!("{count}."),
        1 => format!("{}.", to_alpha(count)),
        _ => format!("{}.", to_roman(count)),
    }
}

/// Lower-case letter sequence for an ordinal: 1→a, 26→z, 27→aa, …
fn to_alpha(n: usize) -> String {
    let mut n = n;
    let mut s = String::new();
    while n > 0 {
        n -= 1;
        s.insert(0, (b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    s
}

/// Lower-case roman numeral for an ordinal (1→i, 4→iv, …).
fn to_roman(mut n: usize) -> String {
    const TABLE: [(usize, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut s = String::new();
    for (value, sym) in TABLE {
        while n >= value {
            s.push_str(sym);
            n -= value;
        }
    }
    s
}

/// Lift the header lines out of an HTML entry's leading paragraphs and drop them
/// from the body. The first paragraph is the repeated headword line, e.g.
/// `headword [prɒn] adjective`: its first `[…]` is the phonetic and whatever
/// trails it is the part of speech. A following etymology line (opening with the
/// "étym." label) is lifted too. Returns `(pron, pos, etym)`; leaves a paragraph
/// in place when it doesn't look like a header line.
fn extract_html_header(paras: &mut Vec<HtmlBlock>, headword: &str) -> (String, String, String) {
    // Wiktionary lists pronunciations as `IPA: /…/` items scattered through the
    // entry; lift them onto the grey pron line and drop them from the body.
    let mut prons: Vec<String> = Vec::new();
    paras.retain(|b| {
        if let Some(p) = ipa_pron(&b.text) {
            if !prons.contains(&p) {
                prons.push(p);
            }
            false
        } else {
            true
        }
    });
    let pron_from_ipa = prons.join("  ·  ");

    let mut pron = String::new();

    // French Wiktionary structure: each part-of-speech section is a heading
    // ("Verbe", "Nom commun") immediately followed by a headword line that
    // repeats the headword with its `\…\` phonetic and grammatical detail. That
    // line is redundant and visually noisy, so drop it from every section — but
    // keep the POS heading in the body, so it labels the senses that follow
    // (the POS sits above its part of the description, not in a chip at the top).
    // The first section's phonetic is lifted onto the grey line; the joined POS is
    // returned only to gate the conjugation button.
    let mut pos_list: Vec<String> = Vec::new();
    let mut i = 0;
    let mut pron_lifted = false;
    while i + 1 < paras.len() {
        if paras[i].heading && !paras[i + 1].heading && !paras[i + 1].quote {
            if let Some(p) = slash_pron(&paras[i + 1].text) {
                push_distinct(&mut pos_list, paras[i].text.to_lowercase());
                if !pron_lifted {
                    // Drop Wiktionary's "Prononciation ?" placeholder (a real
                    // phonetic never contains a question mark).
                    pron = if p.contains('?') { String::new() } else { p };
                    pron_lifted = true;
                }
                paras.remove(i + 1);
                i += 1;
                continue;
            }
        }
        i += 1;
    }
    let mut pos = pos_list.join(" · ");

    // Whether the POS already appears as headings in the body (Wiktionary). A
    // single-header dictionary instead carries the POS inline in the first
    // paragraph; we lift it and inject it as a heading below, so both layouts put
    // the POS above the senses.
    let pos_inline = !pos_list.is_empty();
    if !pos_inline {
        if let Some(first) = paras.first().filter(|b| !b.heading && !b.quote) {
            let text = first.text.clone();
            match (text.find('['), text.find(']')) {
                (Some(open), Some(close)) if open < close => {
                    pron = text[open + 1..close].trim().to_string();
                    pos = text[close + 1..].trim().to_string();
                    paras.remove(0);
                }
                // No bracketed phonetic: only drop the line if it's a bare repeat
                // of the headword, otherwise leave the body as-is.
                _ => {
                    if text.trim().eq_ignore_ascii_case(headword.trim()) {
                        paras.remove(0);
                    }
                }
            }
        }
    }
    if pron.is_empty() {
        pron = pron_from_ipa;
    }

    // The etymology line, when present, follows the header; lift it as well.
    let etym = if paras
        .first()
        .is_some_and(|b| !b.heading && is_etym_line(&b.text))
    {
        paras.remove(0).text
    } else {
        String::new()
    };

    // Single-header dictionary: the lifted POS isn't a body heading yet, so inject
    // one above the (now header-free) senses to match the Wiktionary layout.
    if !pos_inline && !pos.is_empty() {
        paras.insert(
            0,
            HtmlBlock {
                marker: String::new(),
                text: pos.clone(),
                quote: false,
                heading: true,
                indent: 0,
            },
        );
    }

    (pron, pos, etym)
}

/// Extract the IPA pronunciation from a Wiktionary pronunciation block (`… IPA:
/// /ɹʌn/`). Returns the slash- or bracket-delimited transcription, or `None` when
/// the block isn't a pronunciation line.
fn ipa_pron(text: &str) -> Option<String> {
    if !text.contains("IPA") {
        return None;
    }
    let between = |open: char, close: char| {
        let start = text.find(open)? + open.len_utf8();
        let end = text[start..].find(close)? + start;
        let inner = text[start..end].trim();
        (!inner.is_empty()).then(|| inner.to_string())
    };
    between('/', '/').or_else(|| between('[', ']'))
}

/// Extract the phonetic from a French Wiktionary headword line, which carries its
/// pronunciation as a backslash-delimited respelling (`avoir \a.vwaʁ\ …`). Returns
/// the first transcription without the slashes, or `None` when there isn't one.
fn slash_pron(text: &str) -> Option<String> {
    let start = text.find('\\')?;
    let rest = &text[start + 1..];
    let end = rest.find('\\')?;
    let inner = rest[..end].trim();
    (!inner.is_empty()).then(|| inner.to_string())
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

/// One part-of-speech sub-entry: its POS heading (e.g. "noun", "verb
/// (intransitive)") and the senses it governs. GCIDE leads a homograph with
/// repeated `\Word\, n.` / `\Word\, v. i.` headers; each becomes a section so the
/// POS heads its own senses in the body instead of a single chip at the top.
struct PosSection {
    pos: String,
    senses: Vec<Sense>,
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

    // Collect every sub-entry's part of speech, not just the first: GCIDE leads a
    // noun+verb homograph (`jump`, `swim`, `love`) with repeated `\Word\, n.` /
    // `\Word\, v. i.` headers, and we want the chip to show all of them
    // (`noun · verb (intransitive)`) so the verb sense isn't hidden behind a
    // noun-first ordering.
    static POS: OnceLock<Regex> = OnceLock::new();
    let mut pos_list: Vec<String> = Vec::new();
    for c in re(&POS, r"\\[^\\]+\\[^,]*,\s*([A-Za-z]\.(?:\s*[A-Za-z]\.)*)").captures_iter(&text) {
        push_distinct(&mut pos_list, expand_pos(c[1].trim()));
    }
    let pos = pos_list.join(" · ");

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

/// Split a GCIDE entry into its part-of-speech sub-entries. GCIDE leads each
/// sense group with a flush-left header line (`Word \respelling\…, pos.`); we cut
/// the entry at every such line, label the section with its expanded POS, and
/// parse the senses that follow. An entry with no POS header yields a single
/// untitled section, so a noun+verb homograph shows "noun" then "verb" inline
/// while a plain entry still renders uniformly.
fn parse_sections(raw: &str) -> Vec<PosSection> {
    let text = drop_markers(raw);
    static HEADER: OnceLock<Regex> = OnceLock::new();
    // A flush-left (`^\S`) header line carrying `\respelling\` then `, pos.`.
    let header = re(
        &HEADER,
        r"^\S.*\\[^\\]+\\[^,]*,\s*([A-Za-z]\.(?:\s*[A-Za-z]\.)*)",
    );
    let lines: Vec<&str> = text.lines().collect();
    let heads: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| header.is_match(l))
        .map(|(i, _)| i)
        .collect();

    let decode_senses = |segment: &str| -> Vec<Sense> {
        parse_senses(segment)
            .into_iter()
            .map(|s| Sense {
                body: decode_gcide(&s.body),
                quotes: s.quotes.iter().map(|q| decode_gcide(q)).collect(),
            })
            .collect()
    };

    if heads.is_empty() {
        return vec![PosSection {
            pos: String::new(),
            senses: decode_senses(&text),
        }];
    }

    heads
        .iter()
        .enumerate()
        .map(|(k, &start)| {
            let end = heads.get(k + 1).copied().unwrap_or(lines.len());
            let segment = lines[start..end].join("\n");
            let pos = header
                .captures(lines[start])
                .map(|c| expand_pos(c[1].trim()))
                .unwrap_or_default();
            PosSection {
                pos,
                senses: decode_senses(&segment),
            }
        })
        .collect()
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

/// Whether `pron` is just the headword respelled with syllable breaks and no
/// phonetic diacritics, comparing only the letters/digits of each (case-folded).
/// GCIDE's "Hel·lo" for "Hello" echoes the word, so the card drops it; respellings
/// that add information ("är·kē·ŏl·ō·gy", IPA "(h)ɛ.lo") differ and are kept.
fn pron_echoes_headword(pron: &str, headword: &str) -> bool {
    let letters = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    !pron.is_empty() && letters(pron) == letters(headword)
}

/// Whether **any** part of speech in the (`·`-joined) chip names a verb, so the
/// Conjugation button appears whenever a verb sense is among the entry's POS —
/// including noun-led homographs like `noun · verb`. Matches GCIDE's expanded
/// `verb`/`verb (transitive)`/`verb (intransitive)` and HTML dictionaries' lower-
/// cased `verb`(e) headings, while rejecting `noun`/`adverb`/`pronoun`/`proper noun`.
fn pos_is_verb(pos: &str) -> bool {
    pos.split('·')
        .any(|p| p.trim().to_ascii_lowercase().starts_with("verb"))
}

/// Append `value` to a part-of-speech list, skipping empties and duplicates so a
/// repeated heading (or a verb listed transitive *and* intransitive) doesn't
/// produce a chip like `verb · verb`. Order is preserved (document order).
fn push_distinct(list: &mut Vec<String>, value: String) {
    if !value.is_empty() && !list.contains(&value) {
        list.push(value);
    }
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

/// For entries without numbered senses: drop the headword line, strip the
/// pronunciation and etymology, and return the remaining prose as a single sense
/// plus any quotations attached to it.
fn unnumbered_sense(text: &str) -> Option<Sense> {
    static PRON: OnceLock<Regex> = OnceLock::new();
    static BR: OnceLock<Regex> = OnceLock::new();
    // Drop the GCIDE headword line(s) — `Headword \respelling\ (phon), pos.` — so
    // the body doesn't repeat the word and pronunciation the card already shows in
    // its header. Usually the definition is on the indented lines that follow; if
    // header and definition share a single line, strip just the leading
    // `Headword \respelling\ (phon)` prefix instead so the definition survives.
    let kept: Vec<&str> = text.lines().filter(|l| !is_header_line(l)).collect();
    let src = if kept.iter().any(|l| !l.trim().is_empty()) {
        kept.join("\n")
    } else {
        strip_header_prefix(text)
    };
    let no_pron = re(&PRON, r"\\[^\\]*\\").replace_all(&src, "");
    let no_etym = re(&BR, r"(?s)\[[^\[\]]*\]").replace_all(&no_pron, "");
    let lines: Vec<&str> = no_etym.lines().collect();
    split_body_quotes(&lines)
}

/// A GCIDE headword line: flush-left (definition and quotation lines are
/// indented) and carrying a `\respelling\`. Used to drop the redundant
/// headword/pronunciation line from an unnumbered entry's body.
fn is_header_line(line: &str) -> bool {
    !line.trim().is_empty() && !line.starts_with(char::is_whitespace) && line.contains('\\')
}

/// Strip a leading `Headword \respelling\ (phon)` prefix from GCIDE text, keeping
/// the part of speech and definition that follow. Used for one-line entries where
/// the headword/pronunciation can't be dropped as a whole line.
fn strip_header_prefix(text: &str) -> String {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    re(&HEADER, r"^[^\\\n]*\\[^\\]*\\\s*(\([^()]*\)\s*)?")
        .replace(text, "")
        .into_owned()
}

fn cleaned_plain(raw: &str) -> String {
    let mut joined = decode_gcide(&drop_markers(&strip_braces(raw)));
    while joined.contains("\n\n\n") {
        joined = joined.replace("\n\n\n", "\n\n");
    }
    joined.trim().to_string()
}

/// Leading slice of a definition scanned when building a result-list snippet.
/// Generous because HTML entries can open with a few hundred characters of
/// inline-CSS tags before any visible text; `make_snippet` strips the markup and
/// then truncates to a display length.
const SNIPPET_SCAN_LEN: usize = 800;

/// Fetch a result-list preview snippet for `headword` from the dictionary it was
/// found in. The search index no longer stores definitions (see `SearchHit`), so
/// the snippet is read on demand from the source dictionary and bounded before
/// the (potentially HTML/GCIDE-heavy) `make_snippet` processing.
fn fetch_snippet(manager: &mut DictionaryManager, dictionary: &str, headword: &str) -> String {
    let entries = manager.lookup_in(dictionary, headword).unwrap_or_default();
    let raw: String = entries
        .iter()
        .flat_map(|e| e.segments.iter())
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let bounded = match raw.char_indices().nth(SNIPPET_SCAN_LEN) {
        Some((idx, _)) => &raw[..idx],
        None => &raw,
    };
    make_snippet(bounded, headword)
}

/// A one-line preview for the results list, built from the same parse the card
/// body uses so the two agree: the headword line and its pronunciation are
/// dropped, leaving the first sense/definition. `headword` lets the HTML path
/// drop the matching headword line.
fn make_snippet(raw: &str, headword: &str) -> String {
    let tail = if raw.contains('<') {
        // HTML entry: drop the headword/pronunciation line(s) like the card does,
        // then show the remaining definition text.
        let mut blocks = html_to_blocks(raw);
        extract_html_header(&mut blocks, headword);
        let joined = blocks
            .iter()
            .filter(|b| !b.heading)
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        strip_link_markers(&joined)
    } else {
        // GCIDE: the parsed first sense already has the header stripped (numbered
        // or not); fall back to lightly-cleaned text if parsing finds no sense.
        match parse_entry(raw).senses.into_iter().next() {
            Some(sense) => sense.body,
            None => collapse_ws(&decode_gcide(&strip_braces(raw))),
        }
    };
    let tail = tail.trim_start();
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

// Emphasis sentinels (also private-use), inserted in pairs around `<b>`/`<i>`
// runs so the bold/italic survives flattening: they become markdown `**`/`*` for
// the styled body, and are dropped for plain-text uses.
const EMPH_BOLD: char = '\u{E003}';
const EMPH_ITAL: char = '\u{E004}';

/// Whether `c` is one of the private-use sentinels (never present in real text).
fn is_sentinel(c: char) -> bool {
    matches!(c, LINK_OPEN | LINK_SEP | LINK_CLOSE | EMPH_BOLD | EMPH_ITAL)
}

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
    if !text.chars().any(is_sentinel) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != LINK_OPEN {
            match c {
                // Emphasis: markdown bold/italic, or nothing for plain text.
                EMPH_BOLD if as_markdown => out.push_str("**"),
                EMPH_ITAL if as_markdown => out.push('*'),
                EMPH_BOLD | EMPH_ITAL => {}
                // Drop stray separators/closers; copy everything else verbatim.
                LINK_SEP | LINK_CLOSE => {}
                _ => out.push(c),
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
        assert_eq!(blocks[0].text, "headword");
        assert_eq!(blocks[1].text, "A sample with colored italic « mâcher ».");
        // no raw tags leak through
        assert!(blocks
            .iter()
            .all(|b| !b.text.contains('<') && !b.text.contains('>')));
    }

    #[test]
    fn drops_wingdings_divider() {
        // Some HTML entries separate the header from the senses with a Wingdings
        // "v" glyph; stripped to text it must not leak as a literal "v" paragraph.
        let html = "<DIV>word</DIV> \
                    <P style=\"text-align: center\"><FONT FACE=\"Wingdings\">v</FONT></P> \
                    <DIV>The definition.</DIV>";
        let blocks = html_to_blocks(html);
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, vec!["word", "The definition."]);
        assert!(!texts.iter().any(|t| *t == "v"));
    }

    #[test]
    fn wiktionary_nested_senses_get_hierarchical_markers() {
        // A part-of-speech heading, then a top-level sense with two sub-senses,
        // one carrying a usage example in a <dd>.
        let html = "<h4>Verb</h4><ol>\
            <li>To move swiftly.\
              <ol>\
                <li>To run on foot.\
                  <dl><dd><i>She <b>ran</b> home.</i></dd></dl>\
                </li>\
                <li>To flow.</li>\
              </ol>\
            </li></ol>";
        let blocks = html_to_blocks(html);
        let summary: Vec<(&str, &str, bool, bool, i32)> = blocks
            .iter()
            .map(|b| {
                (
                    b.marker.as_str(),
                    b.text.as_str(),
                    b.quote,
                    b.heading,
                    b.indent,
                )
            })
            .collect();
        // Strip sentinels for the comparison of example text.
        let example = strip_link_markers(&blocks[3].text);
        assert_eq!(summary[0], ("", "Verb", false, true, 0));
        assert_eq!(summary[1], ("1.", "To move swiftly.", false, false, 0));
        assert_eq!(summary[2].0, "a.");
        assert_eq!(summary[2].4, 1); // sub-sense is indented one level
        assert!(summary[3].2); // the example is a quote
        assert_eq!(summary[3].4, 1); // and hangs under its sub-sense
        assert_eq!(example, "She ran home.");
        // The next sibling sub-sense numbers as "b.".
        assert!(blocks
            .iter()
            .any(|b| b.marker == "b." && b.text == "To flow."));
    }

    #[test]
    fn wiktionary_emphasis_becomes_markdown() {
        let html = "<p>see <b>bold</b> and <i>italic</i> and <b></b> empty</p>";
        let blocks = html_to_blocks(html);
        assert_eq!(blocks.len(), 1);
        // Bold is dropped to plain text (matching GCIDE — Wiktionary bolds the
        // headword and every inflected form, which reads as noise); only italic
        // converts to markdown, and the empty <b></b> leaves nothing.
        assert_eq!(
            convert_html_refs_to_links(&blocks[0].text),
            "see bold and *italic* and empty"
        );
        // Plain text keeps the words without any markup or sentinels; the empty
        // <b></b> leaves no trace (the surrounding spaces collapse to one).
        assert_eq!(
            strip_link_markers(&blocks[0].text),
            "see bold and italic and empty"
        );
    }

    #[test]
    fn wiktionary_ipa_is_lifted_to_pron_line() {
        let html = "<h4>Verb</h4>\
            <ul><li>IPA: /ɹʌn/</li><li>IPA: /ɹʊn/</li></ul>\
            <ol><li>To move swiftly.</li></ol>";
        let mut blocks = html_to_blocks(html);
        let (pron, pos, _etym) = extract_html_header(&mut blocks, "run");
        assert_eq!(pron, "ɹʌn  ·  ɹʊn");
        assert_eq!(pos, "");
        // The pronunciation list is gone; the heading and sense remain.
        assert!(blocks.iter().all(|b| !b.text.contains("IPA")));
        assert!(blocks.iter().any(|b| b.heading && b.text == "Verb"));
        assert!(blocks.iter().any(|b| b.marker == "1."));
    }

    #[test]
    fn gcide_homograph_splits_into_pos_sections() {
        // A noun+verb homograph yields one section per POS, each with its senses
        // numbered from 1, so the POS heads its own part of the description.
        let raw = "Mouse \\Mouse\\, n.\n   1. (Zool.) A small rodent.\n\
                   2. A computer pointing device.\n\
                   Mouse \\Mouse\\, v. i.\n   1. To hunt for mice.\n";
        let sections = parse_sections(raw);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].pos, "noun");
        assert_eq!(sections[0].senses.len(), 2);
        assert_eq!(sections[1].pos, "verb (intransitive)");
        assert_eq!(sections[1].senses.len(), 1);
        assert_eq!(sections[1].senses[0].body, "To hunt for mice.");
    }

    #[test]
    fn gcide_single_pos_is_one_section() {
        let raw = "Run \\Run\\, v. i.\n   1. To move swiftly.\n";
        let sections = parse_sections(raw);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].pos, "verb (intransitive)");
        assert_eq!(sections[0].senses.len(), 1);
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
        // the lifted POS is injected as a body heading above the senses
        assert_eq!(paras.len(), 1);
        assert!(paras[0].heading && paras[0].text == "adjective and noun");
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
        assert!(blocks.iter().all(|b| b.text.chars().count() <= 800));
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
    fn conjugation_button_follows_displayed_pos() {
        // The gate mirrors the POS chip the user sees.
        assert!(pos_is_verb("verb"));
        assert!(pos_is_verb("verb (transitive)"));
        assert!(pos_is_verb("verb (intransitive)"));
        assert!(!pos_is_verb("noun"));
        assert!(!pos_is_verb("adverb"));
        assert!(!pos_is_verb("pronoun"));
        assert!(!pos_is_verb("proper noun"));
        assert!(!pos_is_verb(""));

        // The gate fires when any segment of a `·`-joined chip is a verb.
        assert!(pos_is_verb("noun · verb (intransitive)"));
        assert!(pos_is_verb("adjective · verb"));
        assert!(!pos_is_verb("noun · adjective"));

        // A noun-led homograph now shows all its parts of speech, so its verb
        // sense is no longer hidden and conjugation is offered again (the "mouse"
        // complaint): GCIDE leads "mouse" with `n.`, then a `v. i.` sub-entry.
        let noun = "Mouse \\Mouse\\, n.\n   1. (Zool.) A small rodent.\n\
                    Mouse \\Mouse\\, v. i.\n   1. To hunt for mice.\n";
        assert_eq!(parse_entry(noun).pos, "noun · verb (intransitive)");
        assert!(pos_is_verb(&parse_entry(noun).pos));

        // A pure noun (no verb sub-entry) still offers no conjugation.
        let pure_noun = "Table \\Table\\, n.\n   1. A piece of furniture.\n";
        assert_eq!(parse_entry(pure_noun).pos, "noun");
        assert!(!pos_is_verb(&parse_entry(pure_noun).pos));

        // A verb-led entry still offers conjugation.
        let verb = "Run \\Run\\, v. i.\n   1. To move swiftly.\n";
        assert_eq!(parse_entry(verb).pos, "verb (intransitive)");
        assert!(pos_is_verb(&parse_entry(verb).pos));
    }

    #[test]
    fn unnumbered_entry_drops_headword_and_pronunciation() {
        // An entry with no numbered senses: the body must start at the definition,
        // not repeat the headword line (`Hello \Hel*lo"\, interj. & n.`) whose word
        // and pronunciation the card already shows in its header.
        let raw = "Hello \\Hel*lo\"\\, interj. & n.\n   \
                   An exclamation used as a greeting.\n   \
                   [1913 Webster +PJC]\n";
        let parsed = parse_entry(raw);
        assert_eq!(parsed.pronunciation, "Hel·lo");
        assert_eq!(parsed.senses.len(), 1);
        assert_eq!(parsed.senses[0].body, "An exclamation used as a greeting.");
    }

    #[test]
    fn pronunciation_echoing_headword_is_hidden() {
        // GCIDE's syllabified respelling of the word adds nothing over the title.
        assert!(pron_echoes_headword("Hel·lo", "Hello"));
        assert!(pron_echoes_headword("Dic·tion·a·ry", "Dictionary"));
        // Phonetic diacritics and IPA carry information, so they're kept.
        assert!(!pron_echoes_headword("är·kē·ŏl·ō·gy", "Archaeology"));
        assert!(!pron_echoes_headword("(h)ɛ.lo", "hello"));
        // An empty pronunciation is not treated as an echo.
        assert!(!pron_echoes_headword("", "Hello"));
    }

    #[test]
    fn one_line_entry_drops_headword_and_pronunciation() {
        // Header and definition share a line: drop just the `word \respelling\`
        // prefix so the definition (and its part of speech) survive.
        let raw = "cecal \\cecal\\ adj. of, pertaining to, or like the cecum.\n   [1913 Webster]\n";
        let parsed = parse_entry(raw);
        assert_eq!(parsed.senses.len(), 1);
        assert_eq!(
            parsed.senses[0].body,
            "adj. of, pertaining to, or like the cecum."
        );
    }

    #[test]
    fn snippet_skips_headword_for_unnumbered_entry() {
        // The result-list preview agrees with the card body: no repeated headword
        // or `\respelling\` pronunciation, just the definition.
        let raw = "cecal \\cecal\\ adj. of, pertaining to, or like the cecum.\n";
        assert_eq!(
            make_snippet(raw, "cecal"),
            "adj. of, pertaining to, or like the cecum."
        );
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
            convert_html_refs_to_links(&blocks[0].text),
            "voir [alpha](<lookup://alpha>) et [beta gamma](<lookup://beta gamma>)."
        );
        assert_eq!(
            strip_link_markers(&blocks[0].text),
            "voir alpha et beta gamma."
        );
        // No sentinel chars leak into the plain text.
        let plain = strip_link_markers(&blocks[0].text);
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
        assert_eq!(blocks[0].text, "site anchor");
        assert_eq!(convert_html_refs_to_links(&blocks[0].text), "site anchor");
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

    // The HTML contract produced by the `wikitionary-dictionaries` generator for
    // the Italian (`it-it`) monolingual dictionary. These strings are the actual
    // shape the generator emits (a `<h4>` POS heading, a `\…\` headword line,
    // `<ol><li>` senses with `<dd>` examples, `bword://` inflection links). They
    // guard that irondict keeps rendering `it-it` like the French Wiktionnaire.

    #[test]
    fn italian_lemma_lifts_pos_and_ipa() {
        // A noun lemma: POS heading + backslash phonetic headword line + senses.
        let html = "<h4>Sostantivo</h4><p>cane \\ˈkaːne\\</p>\
            <ol><li>animale domestico<dd>quel cane difende il padrone</dd></li>\
            <li>persona vile</li></ol><p>Etimologia: dal latino canis</p>";
        let mut blocks = html_to_blocks(html);
        let (pron, pos, _etym) = extract_html_header(&mut blocks, "cane");
        assert_eq!(pos, "sostantivo");
        assert_eq!(pron, "ˈkaːne");
        // The redundant headword line is gone; senses + etymology remain.
        assert!(blocks.iter().all(|b| !b.text.contains('\\')));
        assert!(blocks
            .iter()
            .any(|b| b.marker == "1." && b.text.starts_with("animale")));
        assert!(blocks
            .iter()
            .any(|b| b.quote && b.text.contains("difende il padrone")));
        assert!(blocks.iter().any(|b| b.text.starts_with("Etimologia:")));
    }

    #[test]
    fn italian_multi_pos_keeps_later_headings() {
        // `bello` = adjective + noun: every section's POS is collected onto the
        // grey line, the first section's phonetic is lifted, and the second keeps
        // its `<h4>` heading but drops its redundant headword line.
        let html = "<h4>Aggettivo</h4><p>bello \\ˈbɛllo\\</p><ol><li>gradevole</li></ol>\
            <h4>Sostantivo</h4><p>bello \\ˈbɛllo\\</p><ol><li>ciò che è bello</li></ol>";
        let mut blocks = html_to_blocks(html);
        let (pron, pos, _etym) = extract_html_header(&mut blocks, "bello");
        assert_eq!(pos, "aggettivo · sostantivo");
        assert_eq!(pron, "ˈbɛllo");
        // The second POS heading survives in the body; no headword line remains.
        assert!(blocks.iter().any(|b| b.heading && b.text == "Sostantivo"));
        assert!(blocks.iter().all(|b| !b.text.contains('\\')));
    }

    #[test]
    fn french_verb_homograph_offers_conjugation() {
        // A noun+verb homograph lists both sections; the joined chip names a verb,
        // so conjugation is offered even though the noun section comes first.
        let html = "<h4>Nom commun</h4><p>aide \\ɛd\\</p><ol><li>assistance</li></ol>\
            <h4>Verbe</h4><p>aide \\ɛd\\</p><ol><li>forme du verbe aider</li></ol>";
        let mut blocks = html_to_blocks(html);
        let (_pron, pos, _etym) = extract_html_header(&mut blocks, "aide");
        assert_eq!(pos, "nom commun · verbe");
        assert!(pos_is_verb(&pos));
    }

    #[test]
    fn italian_form_entry_links_to_lemma() {
        // An inflected form: no phonetic to lift, and the lemma is a clickable link.
        let html = "<h4>Voce verbale</h4><ol><li>1ª persona di \
            <a href=\"bword://correre\">correre</a></li></ol>";
        let mut blocks = html_to_blocks(html);
        let (pron, pos, _etym) = extract_html_header(&mut blocks, "corro");
        assert_eq!(pos, "");
        assert_eq!(pron, "");
        assert!(blocks.iter().any(|b| b.heading && b.text == "Voce verbale"));
        let sense = blocks
            .iter()
            .find(|b| b.marker == "1.")
            .expect("a numbered sense");
        assert_eq!(
            convert_html_refs_to_links(&sense.text),
            "1ª persona di [correre](<lookup://correre>)"
        );
    }
}
