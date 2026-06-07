// Phase 6 step 2: the "toolbar layout" wired to the real backend
// (DictionaryManager + SearchEngine over the bundled GCIDE).
//
// The search index takes a few seconds to build on first run, so it is built on
// a worker thread; the window appears immediately (showing the word of the
// moment) and search becomes live once the index is ready.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use slint::{ModelRc, Timer, TimerMode, VecModel};

use irondict_core::{
    bundled_gcide_path, search, Config, DictionaryConfig, DictionaryManager, SearchEngine,
    SearchMode,
};

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
                c.dictionaries.push(DictionaryConfig {
                    path: bundled_gcide_path(),
                    enabled: true,
                });
                let _ = c.save_to(&path);
                c
            } else {
                Config::load_from(&path).unwrap_or_default()
            }
        }
        Err(_) => {
            let mut c = Config::default();
            c.dictionaries.push(DictionaryConfig {
                path: bundled_gcide_path(),
                enabled: true,
            });
            c
        }
    };
    let (manager, errors) = DictionaryManager::from_config(&config);
    for e in errors {
        eprintln!("warning: failed to load {}: {}", e.path.display(), e.error);
    }
    manager
}

/// Open the cached index, or build it (slow) if there isn't one yet. The engine
/// is warmed before returning so the user's first keystroke is snappy.
fn open_or_build_index() -> Result<SearchEngine, String> {
    let dir = search::default_index_dir().map_err(|e| e.to_string())?;
    let engine = match SearchEngine::open(&dir) {
        Ok(engine) => engine,
        Err(_) => {
            let mut manager = load_manager();
            SearchEngine::build(&dir, &mut manager).map_err(|e| e.to_string())?
        }
    };
    warm_up(&engine);
    Ok(engine)
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
    ui.set_accent(slint::Color::from_rgb_u8(0x4f, 0x46, 0xe5));
    ui.set_accent_tint(slint::Color::from_rgb_u8(0xee, 0xf0, 0xfd));

    let manager = Rc::new(RefCell::new(load_manager()));
    let engine: Rc<RefCell<Option<SearchEngine>>> = Rc::new(RefCell::new(None));
    let results_model: Rc<VecModel<ResultItem>> = Rc::new(VecModel::default());
    ui.set_results(ModelRc::from(results_model.clone()));
    let rows: Rc<RefCell<Vec<RowData>>> = Rc::new(RefCell::new(Vec::new()));

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
    show_word(&ui, &manager, &wotm, "WORD OF THE MOMENT");

    // Build/open the index off the UI thread; deliver it via a channel.
    let (tx, rx) = mpsc::channel::<Result<SearchEngine, String>>();
    std::thread::spawn(move || {
        let _ = tx.send(open_or_build_index());
    });

    // Poll for the finished index and switch search on when it arrives.
    let index_timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let engine = engine.clone();
        let results_model = results_model.clone();
        let rows = rows.clone();
        index_timer.start(TimerMode::Repeated, Duration::from_millis(120), move || {
            match rx.try_recv() {
                Ok(Ok(e)) => {
                    let ui = ui_weak.unwrap();
                    *engine.borrow_mut() = Some(e);
                    ui.set_index_ready(true);
                    let q = ui.get_query();
                    if !q.trim().is_empty() {
                        run_search(&ui, &manager, &engine, &results_model, &rows, &q);
                    }
                }
                Ok(Err(msg)) => {
                    let ui = ui_weak.unwrap();
                    ui.set_def_headword("Couldn't build index".into());
                    ui.set_def_body(msg.into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        });
    }

    // Live search as the user types.
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let engine = engine.clone();
        let results_model = results_model.clone();
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
                show_word(&ui, &manager, &wotm, "WORD OF THE MOMENT");
                return;
            }
            let ui_weak = ui_weak.clone();
            let manager = manager.clone();
            let engine = engine.clone();
            let results_model = results_model.clone();
            let rows = rows.clone();
            debounce.start(
                TimerMode::SingleShot,
                Duration::from_millis(140),
                move || {
                    let ui = ui_weak.unwrap();
                    run_search(&ui, &manager, &engine, &results_model, &rows, &ui.get_query());
                },
            );
        });
    }

    // Click a result to show its definition.
    {
        let ui_weak = ui.as_weak();
        let manager = manager.clone();
        let rows = rows.clone();
        ui.on_select(move |row| {
            let ui = ui_weak.unwrap();
            let headword = rows
                .borrow()
                .get(row.max(0) as usize)
                .map(|r| r.headword.clone());
            if let Some(headword) = headword {
                ui.set_selected_index(row);
                show_word(&ui, &manager, &headword, "");
            }
        });
    }

    // Scope toggle (cosmetic until multi-dictionary support lands).
    {
        let ui_weak = ui.as_weak();
        ui.on_scope_changed(move |idx| {
            ui_weak.unwrap().set_scope(idx);
        });
    }

    let r = ui.run();
    drop(index_timer);
    r
}

/// Run a query through the engine and update the list + definition pane.
fn run_search(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    engine: &Rc<RefCell<Option<SearchEngine>>>,
    results_model: &Rc<VecModel<ResultItem>>,
    rows: &Rc<RefCell<Vec<RowData>>>,
    query: &str,
) {
    let needle = query.trim();
    let eng_ref = engine.borrow();
    let Some(eng) = eng_ref.as_ref() else {
        // Index still building.
        ui.set_section_label("".into());
        ui.set_def_headword("Preparing dictionary…".into());
        ui.set_def_pos("".into());
        ui.set_def_body("Building the search index — one moment.".into());
        ui.set_def_source("".into());
        return;
    };

    // Prefix (autocomplete) first; fall back to fuzzy for typos.
    let mut hits = eng
        .search(needle, SearchMode::Prefix, 80)
        .unwrap_or_default();
    if hits.is_empty() {
        // Fuzzy hits are already ranked by edit distance — keep that order.
        hits = eng.search(needle, SearchMode::Fuzzy, 40).unwrap_or_default();
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
        show_word(ui, manager, &first.headword, "");
    } else {
        ui.set_selected_index(-1);
        ui.set_section_label("".into());
        ui.set_def_headword("No results".into());
        ui.set_def_pos("".into());
        ui.set_def_body(format!("Nothing matches \u{201c}{needle}\u{201d}.").into());
        ui.set_def_source("".into());
    }
}

/// Resolve `headword` to its full definition and show it in the definition pane.
fn show_word(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    headword: &str,
    label: &str,
) {
    let (body, source) = {
        let mut m = manager.borrow_mut();
        match m.lookup(headword) {
            Ok(results) if !results.is_empty() => {
                let source = results[0].dictionary.clone();
                let mut parts = Vec::new();
                for r in &results {
                    for e in &r.entries {
                        parts.push(e.segments.iter().map(|s| s.text.as_str()).collect::<String>());
                    }
                }
                (clean_body(&parts.join("\n\n")), source)
            }
            _ => (String::new(), String::new()),
        }
    };
    ui.set_section_label(label.into());
    ui.set_def_headword(headword.into());
    ui.set_def_pos("".into());
    ui.set_def_body(body.into());
    ui.set_def_source(source.into());
}

/// Drop StarDict `{cross-reference}` braces (keep the inner text).
fn strip_braces(s: &str) -> String {
    s.chars().filter(|&c| c != '{' && c != '}').collect()
}

/// Light cleanup of GCIDE definition text for display: drop the editorial
/// `[1913 Webster]` / `[PJC]` marker lines, strip cross-ref braces, and collapse
/// runs of blank lines. (Proper rich rendering is Phase 7.)
fn clean_body(raw: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t == "[1913 Webster]"
            || t == "[PJC]"
            || t.starts_with("[Webster 1913")
            || t.starts_with("[Century")
        {
            continue;
        }
        out.push(line.trim_end().to_string());
    }
    let mut joined = strip_braces(&out.join("\n"));
    while joined.contains("\n\n\n") {
        joined = joined.replace("\n\n\n", "\n\n");
    }
    joined.trim().to_string()
}

/// A one-line preview for the results list: prefer the first numbered sense.
fn make_snippet(raw: &str) -> String {
    let flat: String = strip_braces(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let start = flat.find(" 1. ").map(|i| i + 4).unwrap_or(0);
    let tail = &flat[start..];
    let mut chars = tail.chars();
    let head: String = chars.by_ref().take(90).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}
