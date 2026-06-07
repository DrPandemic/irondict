// Phase 6 step 1: a static, interactive visual prototype of the "toolbar layout".
// Sample data only — the real DictionaryManager + SearchEngine wiring comes next.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ModelRc, VecModel};

slint::include_modules!();

/// A sample dictionary entry used to populate the prototype.
struct Entry {
    headword: &'static str,
    pos: &'static str,
    snippet: &'static str,
    body: &'static str,
}

const SOURCE: &str = "GCIDE";

fn sample_entries() -> Vec<Entry> {
    vec![
        Entry {
            headword: "Petrichor",
            pos: "noun",
            snippet: "a pleasant, earthy smell after rain",
            body: "The pleasant, earthy scent produced when rain falls on dry soil \
                   after a warm, dry spell.",
        },
        Entry {
            headword: "Dictionary",
            pos: "noun",
            snippet: "a book of words and their meanings",
            body: "A book containing the words of a language, alphabetically arranged, \
                   with explanations of their meanings, etymologies, and pronunciations.",
        },
        Entry {
            headword: "Diction",
            pos: "noun",
            snippet: "manner of word choice and expression",
            body: "Choice of words and the manner of expression in speech or writing.",
        },
        Entry {
            headword: "Dictionaries",
            pos: "noun",
            snippet: "plural of dictionary",
            body: "Plural of dictionary.",
        },
        Entry {
            headword: "Lexicon",
            pos: "noun",
            snippet: "the vocabulary of a language",
            body: "The vocabulary of a person, language, or branch of knowledge.",
        },
        Entry {
            headword: "Lexicography",
            pos: "noun",
            snippet: "the art of compiling dictionaries",
            body: "The practice and principles of compiling dictionaries.",
        },
        Entry {
            headword: "Etymology",
            pos: "noun",
            snippet: "the origin and history of words",
            body: "The study of the origin of words and the way their meanings have \
                   changed throughout history.",
        },
        Entry {
            headword: "Vocabulary",
            pos: "noun",
            snippet: "the words used in a language",
            body: "The body of words used in a particular language, or known to an \
                   individual person.",
        },
        Entry {
            headword: "Glossary",
            pos: "noun",
            snippet: "a list of terms with definitions",
            body: "An alphabetical list of terms in a particular domain with their \
                   definitions.",
        },
        Entry {
            headword: "Thesaurus",
            pos: "noun",
            snippet: "a book of synonyms",
            body: "A reference work that lists words grouped together according to \
                   similarity of meaning.",
        },
    ]
}

fn show_entry(ui: &AppWindow, e: &Entry, label: &str) {
    ui.set_section_label(label.into());
    ui.set_def_headword(e.headword.into());
    ui.set_def_pos(e.pos.into());
    ui.set_def_body(e.body.into());
    ui.set_def_source(SOURCE.into());
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    ui.set_accent(slint::Color::from_rgb_u8(0x4f, 0x46, 0xe5));
    ui.set_accent_tint(slint::Color::from_rgb_u8(0xee, 0xf0, 0xfd));

    let entries = Rc::new(sample_entries());
    let results_model: Rc<VecModel<ResultItem>> = Rc::new(VecModel::default());
    ui.set_results(ModelRc::from(results_model.clone()));
    // Maps a results-row index back to its entry index, so selection can show the
    // full definition.
    let row_to_entry: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));

    // Fill the results column with every entry (the idle/browse state).
    let populate_all = {
        let entries = entries.clone();
        let results_model = results_model.clone();
        let row_to_entry = row_to_entry.clone();
        move || {
            let rows: Vec<ResultItem> = entries
                .iter()
                .map(|e| ResultItem {
                    headword: e.headword.into(),
                    snippet: e.snippet.into(),
                    source: SOURCE.into(),
                })
                .collect();
            results_model.set_vec(rows);
            *row_to_entry.borrow_mut() = (0..entries.len()).collect();
        }
    };

    // "Word of the moment": one fixed entry chosen per launch; it does not change
    // when the search is cleared.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    let wotm_index = seed % entries.len();

    // Initial state: list shows everything, nothing highlighted, word of the
    // moment in the definition pane.
    populate_all();
    ui.set_selected_index(-1);
    show_entry(&ui, &entries[wotm_index], "WORD OF THE MOMENT");

    // Live filter as the user types.
    {
        let ui_weak = ui.as_weak();
        let entries = entries.clone();
        let results_model = results_model.clone();
        let row_to_entry = row_to_entry.clone();
        let populate_all = populate_all.clone();
        ui.on_query_changed(move |q| {
            let ui = ui_weak.unwrap();
            let needle = q.trim().to_lowercase();

            if needle.is_empty() {
                // Back to the browse state with the same word of the moment.
                ui.set_searching(false);
                populate_all();
                ui.set_selected_index(-1);
                show_entry(&ui, &entries[wotm_index], "WORD OF THE MOMENT");
                return;
            }

            let mut rows = Vec::new();
            let mut map = Vec::new();
            for (i, e) in entries.iter().enumerate() {
                if e.headword.to_lowercase().contains(&needle) {
                    rows.push(ResultItem {
                        headword: e.headword.into(),
                        snippet: e.snippet.into(),
                        source: SOURCE.into(),
                    });
                    map.push(i);
                }
            }

            ui.set_searching(true);
            results_model.set_vec(rows);
            *row_to_entry.borrow_mut() = map;

            if let Some(&first) = row_to_entry.borrow().first() {
                ui.set_selected_index(0);
                show_entry(&ui, &entries[first], "");
            } else {
                ui.set_selected_index(-1);
                ui.set_section_label("".into());
                ui.set_def_headword("No results".into());
                ui.set_def_pos("".into());
                ui.set_def_body(format!("Nothing matches \u{201c}{}\u{201d}.", q).into());
                ui.set_def_source("".into());
            }
        });
    }

    // Click a result to show its definition.
    {
        let ui_weak = ui.as_weak();
        let entries = entries.clone();
        let row_to_entry = row_to_entry.clone();
        ui.on_select(move |row| {
            let ui = ui_weak.unwrap();
            if let Some(&entry_idx) = row_to_entry.borrow().get(row.max(0) as usize) {
                ui.set_selected_index(row);
                show_entry(&ui, &entries[entry_idx], "");
            }
        });
    }

    // Scope toggle (cosmetic in the prototype).
    {
        let ui_weak = ui.as_weak();
        ui.on_scope_changed(move |idx| {
            ui_weak.unwrap().set_scope(idx);
        });
    }

    ui.run()
}
