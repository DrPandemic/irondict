// Phase 6 step 2: the "toolbar layout" wired to the real backend
// (DictionaryManager + SearchEngine over the bundled GCIDE).
//
// The search index takes a few seconds to build on first run, so it is built on
// a worker thread; the window appears immediately (showing the word of the
// moment) and search becomes live once the index is ready.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use regex::Regex;
use slint::{ModelRc, SharedString, Timer, TimerMode, VecModel};

use irondict_core::{
    bundled_gcide_path, search, Config, DictionaryConfig, DictionaryManager, SearchEngine,
    SearchMode,
};

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
    let senses_model: Rc<VecModel<SharedString>> = Rc::new(VecModel::default());
    ui.set_senses(ModelRc::from(senses_model.clone()));
    let rows: Rc<RefCell<Vec<RowData>>> = Rc::new(RefCell::new(Vec::new()));

    // Apply the OS accent color (falling back to indigo) without blocking startup.
    theme::apply_os_accent(ui.as_weak());

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
    show_word(&ui, &manager, &senses_model, &wotm, "WORD OF THE MOMENT");

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
        let senses_model = senses_model.clone();
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
                            &senses_model,
                            &rows,
                            &q,
                        );
                    }
                }
                Ok(Err(msg)) => {
                    let ui = ui_weak.unwrap();
                    ui.set_def_headword("Couldn't build index".into());
                    ui.set_def_body(msg.into());
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
        let senses_model = senses_model.clone();
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
                show_word(&ui, &manager, &senses_model, &wotm, "WORD OF THE MOMENT");
                return;
            }
            let ui_weak = ui_weak.clone();
            let manager = manager.clone();
            let engine = engine.clone();
            let results_model = results_model.clone();
            let senses_model = senses_model.clone();
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
                        &senses_model,
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
        let senses_model = senses_model.clone();
        let rows = rows.clone();
        ui.on_select(move |row| {
            let ui = ui_weak.unwrap();
            let headword = rows
                .borrow()
                .get(row.max(0) as usize)
                .map(|r| r.headword.clone());
            if let Some(headword) = headword {
                ui.set_selected_index(row);
                show_word(&ui, &manager, &senses_model, &headword, "");
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

/// Show a plain text message (no entry) in the definition pane.
fn show_message(
    ui: &AppWindow,
    senses_model: &Rc<VecModel<SharedString>>,
    title: &str,
    body: &str,
) {
    ui.set_section_label("".into());
    ui.set_def_headword(title.into());
    ui.set_def_pron("".into());
    ui.set_def_pos("".into());
    senses_model.set_vec(Vec::new());
    ui.set_def_body(body.into());
    ui.set_def_source("".into());
}

/// Run a query through the engine and update the list + definition pane.
fn run_search(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    engine: &Rc<RefCell<Option<SearchEngine>>>,
    results_model: &Rc<VecModel<ResultItem>>,
    senses_model: &Rc<VecModel<SharedString>>,
    rows: &Rc<RefCell<Vec<RowData>>>,
    query: &str,
) {
    let needle = query.trim();
    let eng_ref = engine.borrow();
    let Some(eng) = eng_ref.as_ref() else {
        show_message(
            ui,
            senses_model,
            "Preparing dictionary…",
            "Building the search index — one moment.",
        );
        return;
    };

    // Prefix (autocomplete) first; fall back to fuzzy for typos.
    let mut hits = eng
        .search(needle, SearchMode::Prefix, 80)
        .unwrap_or_default();
    if hits.is_empty() {
        // Fuzzy hits are already ranked by edit distance — keep that order.
        hits = eng
            .search(needle, SearchMode::Fuzzy, 40)
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
        show_word(ui, manager, senses_model, &first.headword, "");
    } else {
        ui.set_selected_index(-1);
        show_message(
            ui,
            senses_model,
            "No results",
            &format!("Nothing matches \u{201c}{needle}\u{201d}."),
        );
    }
}

/// Resolve `headword` to its full definition, parse it, and show it.
fn show_word(
    ui: &AppWindow,
    manager: &Rc<RefCell<DictionaryManager>>,
    senses_model: &Rc<VecModel<SharedString>>,
    headword: &str,
    label: &str,
) {
    let (raw, source) = lookup_raw(manager, headword);
    if raw.is_empty() {
        show_message(ui, senses_model, headword, "No definition found.");
        ui.set_section_label(label.into());
        return;
    }

    let parsed = parse_entry(&raw);
    ui.set_section_label(label.into());
    ui.set_def_headword(headword.into());
    ui.set_def_pron(parsed.pronunciation.into());
    ui.set_def_pos(parsed.pos.into());
    ui.set_def_source(source.into());

    if parsed.senses.is_empty() {
        // Fall back to lightly-cleaned text when we couldn't split senses.
        senses_model.set_vec(Vec::new());
        ui.set_def_body(cleaned_plain(&raw).into());
    } else {
        ui.set_def_body("".into());
        let senses: Vec<SharedString> = parsed.senses.into_iter().map(SharedString::from).collect();
        senses_model.set_vec(senses);
    }
}

/// Look up `headword` and return (joined raw definition text, source name).
fn lookup_raw(manager: &Rc<RefCell<DictionaryManager>>, headword: &str) -> (String, String) {
    let mut m = manager.borrow_mut();
    match m.lookup(headword) {
        Ok(results) if !results.is_empty() => {
            let source = results[0].dictionary.clone();
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
            (parts.join("\n\n"), source)
        }
        _ => (String::new(), String::new()),
    }
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
        .map(|c| c[1].to_string())
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

/// A one-line preview for the results list: prefer the first numbered sense.
fn make_snippet(raw: &str) -> String {
    let flat = collapse_ws(&strip_braces(raw));
    let start = flat.find(" 1. ").map(|i| i + 4).unwrap_or(0);
    let tail = decode_gcide(&flat[start..]);
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
        _ => return None,
    })
}
