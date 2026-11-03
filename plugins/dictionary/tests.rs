//! Plugin tests with a fake book, an in-memory UI queue and a dictionary written to a
//! temporary rinky home.

use std::path::PathBuf;

use ink_doc::{Metadata, Position, Selection, TocEntry};
use ink_plugin::serde_json::json;
use ink_plugin::{DocumentAccess, HostCtx, MemoryStorage, Plugin, PluginStorage, Registrar, Registry, Result, UiQueue, UiRequest};
use ink_store::vocab::VocabDb;

use super::*;

struct Book {
    selection: Option<Selection>,
}

impl DocumentAccess for Book {
    fn metadata(&self) -> Metadata {
        Metadata { title: "Romeo and Juliet".into(), ..Metadata::default() }
    }
    fn book_id(&self) -> Option<String> {
        Some("0123456789abcdef0123456789abcdef".into())
    }
    fn toc(&self) -> Vec<TocEntry> {
        Vec::new()
    }
    fn search(&mut self, _q: &str, _l: usize) -> Result<Vec<ink_doc::SearchHit>> {
        Ok(Vec::new())
    }
    fn read_around(&mut self, _p: &Position, _c: usize) -> Result<String> {
        Ok("Two households, both alike\n in dignity,".into())
    }
    fn links(&mut self, _p: &Position) -> Result<Vec<ink_doc::Link>> {
        Ok(Vec::new())
    }
    fn current_position(&self) -> Result<Position> {
        Ok("/body/DocFragment[3]/body/p[1]/text().0".parse().unwrap())
    }
    fn selection(&self) -> Option<Selection> {
        self.selection.clone()
    }
}

fn sel(text: &str) -> Selection {
    let p: Position = "/body/DocFragment[3]/body/p[1]/text().4".parse().unwrap();
    Selection { start: p.clone(), end: p, text: text.into(), page: 0, rects: vec![] }
}

/// A home with `dict/sample/` and the plugin data folder; returns (home, storage).
fn home(tag: &str, with_dict: bool) -> (PathBuf, MemoryStorage) {
    let h = std::env::temp_dir().join(format!("ink-dictionary-plugin-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&h);
    if with_dict {
        ink_dict::writer::write(&h.join("dict/sample"), &ink_dict::writer::sample_english()).unwrap();
    }
    let data = h.join("plugins/dictionary/data");
    std::fs::create_dir_all(&data).unwrap();
    (h, MemoryStorage { dir: data, ..MemoryStorage::default() })
}

fn run(p: &mut DictionaryPlugin, st: &mut MemoryStorage, book: Option<&mut Book>, f: impl FnOnce(&mut DictionaryPlugin, &mut HostCtx)) -> Vec<UiRequest> {
    let mut ui = UiQueue::default();
    let perms = p.manifest().permissions.clone();
    let mut cx = HostCtx::new(&mut ui, st, &perms).with_doc(book.map(|b| b as &mut dyn DocumentAccess));
    f(p, &mut cx);
    ui.0
}

#[test]
fn registers_the_selection_action() {
    let mut p = DictionaryPlugin::new();
    let mut reg = Registry::default();
    p.on_load(&mut Registrar { plugin: NAME, registry: &mut reg }).unwrap();
    assert!(reg.selection_actions.iter().any(|a| a.id == ACTION_LOOKUP && a.label == "Look up"));
    assert!(reg.menu_items.iter().any(|m| m.id == ACTION_TYPE && m.label == "Dictionary"), "{:?}", reg.menu_items);
}

#[test]
fn lookup_popup_full_view_and_record() {
    let (h, mut st) = home("popup", true);
    let mut p = DictionaryPlugin::new();
    let mut book = Book { selection: Some(sel("“Households,”")) };
    let reqs = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_action(cx, ACTION_LOOKUP).unwrap());
    let UiRequest::Dialog { id, title, body, buttons } = &reqs[0] else { panic!("{reqs:?}") };
    assert_eq!(title, "household");
    assert!(body.starts_with("_noun_\na house and its occupants"), "{body}");
    assert!(body.ends_with("\n— Sample English Dictionary"), "{body}");
    let b = |l: &str, a: &str| (l.to_string(), a.to_string());
    assert_eq!(buttons, &vec![b("Close", "close"), b("Add to vocabulary", "add"), b("Full view", "full")]);
    let id = *id;
    let reqs = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, id, "full").unwrap());
    let UiRequest::TextViewer { title, markdown, .. } = &reqs[0] else { panic!("{reqs:?}") };
    assert_eq!(title, "household");
    assert!(markdown.starts_with("## Sample English Dictionary\n\n**household**\n\n"), "{markdown}");
    assert!(markdown.contains("a house and its occupants"));
    assert!(!markdown.contains("Looked up"), "same word: no note");
    // "close" shows nothing more.
    assert!(run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, id, "close").unwrap()).is_empty());
    // "add" puts the word on the vocabulary list.
    let reqs = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, id, "add").unwrap());
    assert!(matches!(&reqs[0], UiRequest::Toast { text } if text.contains("household")), "{reqs:?}");

    p.wait_recorded();
    let db = VocabDb::open(&h.join("vocabulary.sqlite3")).unwrap();
    let rows = db.lookups(10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].word, "Households");
    assert_eq!(rows[0].form, "Households");
    assert_eq!(rows[0].book_title.as_deref(), Some("Romeo and Juliet"));
    assert_eq!(rows[0].book_md5.as_deref(), Some("0123456789abcdef0123456789abcdef"));
    assert_eq!(rows[0].context.as_deref(), Some("Two households, both alike in dignity,"));
    assert!(rows[0].found && rows[0].time > 1_700_000_000);
    assert_eq!(db.words().unwrap()[0].word, "households");
    // The Vocabulary screen's table (ink-stats) in the same file.
    let vocab = ink_stats::VocabDb::open(&h.join("vocabulary.sqlite3")).unwrap();
    let w = vocab.get("household").unwrap().expect("vocabulary row");
    assert_eq!(w.book_md5.as_deref(), Some("0123456789abcdef0123456789abcdef"));
    assert!(w.definition.as_deref().is_some_and(|d| d.starts_with("noun · a house") && !d.contains("Sample English")), "{w:?}");
}

#[test]
fn inflected_list_entry_and_not_found() {
    let (h, mut st) = home("forms", true);
    let mut p = DictionaryPlugin::new();
    let mut book = Book { selection: Some(sel("mutinies")) };
    let reqs = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_action(cx, ACTION_LOOKUP).unwrap());
    assert!(matches!(&reqs[0], UiRequest::Dialog { title, .. } if title == "mutiny"), "{reqs:?}");
    run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, 1, "add").unwrap());
    let md = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, 1, "full").unwrap());
    assert!(matches!(&md[0], UiRequest::TextViewer { markdown, .. } if markdown.ends_with("*Looked up “mutinies” as “mutiny”.*\n")), "{md:?}");
    let mut book = Book { selection: Some(sel("dignity")) };
    run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_action(cx, ACTION_LOOKUP).unwrap());
    run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, 1, "add").unwrap());
    let md = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, 1, "full").unwrap());
    let UiRequest::TextViewer { markdown, .. } = &md[0] else { panic!() };
    assert!(markdown.contains("\n- the state or quality of being worthy"), "{markdown}");
    let mut book = Book { selection: Some(sel("xyzzy")) };
    let reqs = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_action(cx, ACTION_LOOKUP).unwrap());
    let UiRequest::Dialog { body, buttons, .. } = &reqs[0] else { panic!("{reqs:?}") };
    assert_eq!(body, "No definition found for “xyzzy”.\n— Searched 1 dictionary");
    assert_eq!(buttons.len(), 1);
    // "full" and "add" after a miss do nothing.
    assert!(run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, 1, "add").unwrap()).is_empty());
    assert!(run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_ui_result(cx, 1, "full").unwrap()).is_empty());
    p.wait_recorded();
    let db = VocabDb::open(&h.join("vocabulary.sqlite3")).unwrap();
    let rows = db.lookups(10).unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|r| r.word == "mutinies" && r.form == "mutiny"));
    assert!(rows.iter().any(|r| r.word == "xyzzy" && !r.found));
    // Only found words go to the Vocabulary screen's table.
    let vocab = ink_stats::VocabDb::open(&h.join("vocabulary.sqlite3")).unwrap();
    assert_eq!(vocab.count().unwrap(), 2);
    assert!(vocab.get("mutiny").unwrap().is_some() && vocab.get("xyzzy").unwrap().is_none());
}

#[test]
fn no_dictionaries_no_selection_settings() {
    let (h, mut st) = home("empty", false);
    let mut p = DictionaryPlugin::new();
    let mut book = Book { selection: Some(sel("word")) };
    let reqs = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_action(cx, ACTION_LOOKUP).unwrap());
    let UiRequest::Dialog { body, .. } = &reqs[0] else { panic!("{reqs:?}") };
    assert!(body.contains("No dictionaries found") && body.contains(&h.join("dict").display().to_string()), "{body}");

    let mut none = Book { selection: None };
    let reqs = run(&mut p, &mut st, Some(&mut none), |p, cx| p.on_action(cx, ACTION_LOOKUP).unwrap());
    assert!(matches!(&reqs[0], UiRequest::Toast { .. }));

    // `dir` and `vocab_db` settings, `record = false`.
    let other = h.join("elsewhere");
    ink_dict::writer::write(&other.join("x"), &ink_dict::writer::Spec::plain("x", "Other", &[("word", "a unit of language")])).unwrap();
    st.settings.insert("dir".into(), json!(other.display().to_string()));
    st.settings.insert("record".into(), json!(false));
    let reqs = run(&mut p, &mut st, Some(&mut book), |p, cx| p.on_action(cx, ACTION_LOOKUP).unwrap());
    assert!(matches!(&reqs[0], UiRequest::Dialog { body, .. } if body.starts_with("a unit of language")), "{reqs:?}");
    p.wait_recorded();
    assert!(!h.join("vocabulary.sqlite3").exists());
}

#[test]
fn typed_word_action() {
    let (h, mut st) = home("typed", true);
    let mut p = DictionaryPlugin::new();
    let reqs = run(&mut p, &mut st, None, |p, cx| p.on_action(cx, ACTION_TYPE).unwrap());
    let UiRequest::TextInput { id, .. } = &reqs[0] else { panic!("{reqs:?}") };
    let id = *id;
    // Without a book: looked up, not recorded.
    let reqs = run(&mut p, &mut st, None, |p, cx| p.on_ui_result(cx, id, "Verona").unwrap());
    assert!(matches!(&reqs[0], UiRequest::Dialog { title, .. } if title == "Verona"), "{reqs:?}");
    assert!(run(&mut p, &mut st, None, |p, cx| p.on_ui_result(cx, id, "").unwrap()).is_empty());
    p.wait_recorded();
    assert!(!h.join("vocabulary.sqlite3").exists());
}

#[test]
fn popup_cut_and_markdown_escape() {
    let long = Hit { dict: "D".into(), headword: "w".into(), text: "word ".repeat(200), entry: None };
    let b = popup_body(&long, 2);
    assert!(b.contains('…') && b.ends_with("\n— D · +2 more"), "{b}");
    assert!(b.chars().count() < POPUP_CHARS + 20);
    let rep = Hit { dict: "D".into(), headword: "cat".into(), text: "Cat\na small animal".into(), entry: None };
    assert_eq!(popup_body(&rep, 0), "a small animal\n— D");
    let noad = Hit { dict: "D".into(), headword: "mutiny".into(), text: "mutiny | ˈmjuːtɪni | noun (plural mutinies)\n1 an open rebellion".into(), entry: None };
    let e = parse_entry(&noad);
    assert_eq!((e.pron.as_deref(), e.pos.as_deref(), e.sense.as_str()), (Some("ˈmjuːtɪni"), Some("noun (plural mutinies)"), "an open rebellion"));
    assert_eq!(popup_body(&noad, 0), "_/ˈmjuːtɪni/ · noun (plural mutinies)_\nan open rebellion\n— D");
    assert_eq!(md_escape("a*b_[c]"), "a\\*b\\_\\[c\\]");
    assert_eq!(md_escape("1. first"), "1\\. first");
    assert_eq!(root_of(std::path::Path::new("/x/home/plugins/dictionary/data")), std::path::PathBuf::from("/x/home"));
    assert_eq!(root_of(std::path::Path::new("/tmp/other")), std::path::PathBuf::from("/mnt/us/rinky"));
}

#[test]
fn full_view_tabs_with_two_dictionaries() {
    let hit = |d: &str, t: &str| Hit { dict: d.into(), headword: "w".into(), text: t.into(), entry: None };
    let r = LookupResult { query: "w".into(), form: "w".into(), hits: vec![hit("A", "first"), hit("B", "second")] };
    assert_eq!(dict_names(&r), vec!["A".to_string(), "B".to_string()]);
    let md = full_markdown_tab(&r, 1);
    assert!(md.starts_with(":::top\n[A](dict:0)  ·  **B**\n\n---\n\n## B\n"), "{md}");
    assert!(md.contains("second") && !md.contains("first"), "{md}");
    let one = LookupResult { hits: vec![hit("A", "first")], ..r.clone() };
    assert_eq!(full_markdown_tab(&one, 0), full_markdown(&one));
}

#[test]
fn parsed_entry_card_and_full_view() {
    use ink_dict::entry::{Entry, Group, Section, Sense};
    let sense = |d: &str, x: &[&str], sub: bool| Sense { def: d.into(), examples: x.iter().map(|s| s.to_string()).collect(), notes: Vec::new(), sub };
    let e = Entry {
        pron: Some("blɪp".into()),
        groups: vec![
            Group { pos: "noun".into(), senses: vec![sense("a dot", &["a blip on the screen"], false), sense("a small dot", &[], true), sense("a sound", &[], false)] },
            Group { pos: "verb".into(), senses: vec![sense("make a blip", &[], false)] },
        ],
        sections: vec![Section { label: "Origin".into(), lines: vec![(String::new(), "1990s: imitative.".into())] }],
    };
    let hit = Hit { dict: "D".into(), headword: "blip".into(), text: String::new(), entry: Some(e.clone()) };
    assert_eq!(popup_body(&hit, 1), "_/blɪp/ · noun_\n1. a dot\n2. a sound\nverb: make a blip\n— D · +1 more");
    assert_eq!(vocab_definition(&hit), "noun · a dot");
    let md = entry_markdown("blip", &e);
    assert_eq!(md, "**blip**  /blɪp/\n\n*noun*\n\n1. a dot: *a blip on the screen*\n   \\- a small dot\n2. a sound\n\n*verb*\n\n- make a blip\n\n**Origin**\n\n1990s: imitative.\n\n".replace("   \\- ", "   - "));
    // The viewer's parser sees a numbered list with a nested item.
    let blocks = ink_plugin::markdown::parse(&md);
    assert!(blocks.iter().any(|b| matches!(b, ink_plugin::markdown::Block::ListItem { number: Some(2), depth: 0, .. })), "{blocks:?}");
    assert!(blocks.iter().any(|b| matches!(b, ink_plugin::markdown::Block::ListItem { number: None, depth: 1, .. })), "{blocks:?}");
    // One sense: its first example follows in italics.
    let one = Entry { groups: vec![Group { pos: "noun".into(), senses: vec![sense("a dot", &["a blip"], false)] }], ..Entry::default() };
    let hit = Hit { entry: Some(one), ..hit };
    assert_eq!(popup_body(&hit, 0), "_noun_\na dot\n_a blip_\n— D");
}

#[test]
fn settings_page_turns_dictionaries_off_and_orders_them() {
    let (h, mut st) = home("settings-page", false);
    let spec = |stem: &str, name: &str, def: &str| ink_dict::writer::Spec::plain(stem, name, &[("word", def)]);
    ink_dict::writer::write(&h.join("dict/a"), &spec("a", "Alpha", "from alpha")).unwrap();
    ink_dict::writer::write(&h.join("dict/b"), &spec("b", "Beta", "from beta")).unwrap();
    let mut reg = Registry::default();
    let mut p = DictionaryPlugin::new();
    p.on_load(&mut Registrar { plugin: NAME, registry: &mut reg }).unwrap();
    assert!(reg.settings_pages.iter().any(|s| s.id == SETTINGS_PAGE && s.title == "Dictionaries"));
    let menu = |ui: &[UiRequest]| -> Vec<(String, String)> {
        match ui.last() {
            Some(UiRequest::Menu { items, .. }) => items.clone(),
            other => panic!("no menu: {other:?}"),
        }
    };
    let dicts_of = |p: &mut DictionaryPlugin, st: &mut MemoryStorage| -> Vec<String> {
        let ui = run(p, st, None, |p, cx| p.lookup(cx, "word", None).unwrap());
        match ui.last() {
            Some(UiRequest::Dialog { body, .. }) => body.clone(),
            other => panic!("{other:?}"),
        }
        .lines()
        .filter_map(|l| l.strip_prefix("— ").map(str::to_string))
        .collect()
    };
    // Folder order, both on.
    let ui = run(&mut p, &mut st, None, |p, cx| p.on_action(cx, "settings:dictionaries").unwrap());
    assert_eq!(menu(&ui), vec![("1. Alpha\ton".to_string(), "a".to_string()), ("2. Beta\ton".to_string(), "b".to_string())]);
    // Two dictionaries with a hit: the card is paged ("1/2 · Alpha").
    assert!(dicts_of(&mut p, &mut st)[0].starts_with("1/2 · Alpha"));
    // Beta: move to top (the menu stays on Beta, now first), then turn Alpha off.
    let ui = run(&mut p, &mut st, None, |p, cx| p.on_ui_result(cx, ID_DICTS, "b").unwrap());
    assert!(menu(&ui).iter().any(|(_, a)| a == "up"));
    let ui = run(&mut p, &mut st, None, |p, cx| p.on_ui_result(cx, ID_DICT, "up").unwrap());
    assert!(matches!(ui.last(), Some(UiRequest::Menu { id: ID_DICT, title, .. }) if title == "Beta · 1 of 2"), "{ui:?}");
    assert!(dicts_of(&mut p, &mut st)[0].starts_with("1/2 · Beta"));
    run(&mut p, &mut st, None, |p, cx| p.on_ui_result(cx, ID_DICTS, "a").unwrap());
    let ui = run(&mut p, &mut st, None, |p, cx| p.on_ui_result(cx, ID_DICT, "toggle").unwrap());
    assert_eq!(menu(&ui), vec![("1. Beta\ton".to_string(), "b".to_string()), ("2. Alpha\toff".to_string(), "a".to_string())]);
    assert_eq!(st.get(KEY_ORDER), Some(json!(["b", "a"])));
    assert_eq!(st.get(KEY_OFF), Some(json!(["a"])));
    // A new plugin (restart) keeps both choices; the lookup uses only Beta.
    let mut p2 = DictionaryPlugin::new();
    let ui = run(&mut p2, &mut st, None, |p, cx| p.lookup(cx, "word", None).unwrap());
    let Some(UiRequest::Dialog { body, .. }) = ui.last() else { panic!("{ui:?}") };
    assert!(body.contains("from beta") && !body.contains("from alpha") && !body.contains("more"), "{body}");
    // Everything off: the lookup says so instead of "no dictionaries installed".
    run(&mut p2, &mut st, None, |p, cx| p.on_ui_result(cx, ID_DICTS, "b").unwrap());
    run(&mut p2, &mut st, None, |p, cx| p.on_ui_result(cx, ID_DICT, "toggle").unwrap());
    let ui = run(&mut p2, &mut st, None, |p, cx| p.lookup(cx, "word", None).unwrap());
    let Some(UiRequest::Dialog { body, .. }) = ui.last() else { panic!("{ui:?}") };
    assert!(body.contains("All dictionaries are turned off"), "{body}");
}

#[test]
fn swipes_page_the_card_through_the_dictionaries_with_a_hit() {
    let (h, mut st) = home("paged-card", false);
    let spec = |stem: &str, name: &str, def: &str| ink_dict::writer::Spec::plain(stem, name, &[("word", def)]);
    ink_dict::writer::write(&h.join("dict/a"), &spec("a", "Alpha", "from alpha")).unwrap();
    ink_dict::writer::write(&h.join("dict/b"), &spec("b", "Beta", "from beta")).unwrap();
    ink_dict::writer::write(&h.join("dict/c"), &spec("c", "Gamma", "from gamma")).unwrap();
    let mut p = DictionaryPlugin::new();
    let card = |ui: &[UiRequest]| -> (u32, String) {
        match ui.last() {
            Some(UiRequest::Dialog { id, body, buttons, .. }) => {
                assert_eq!(buttons.len(), 3, "{buttons:?}");
                (*id, body.clone())
            }
            other => panic!("no card: {other:?}"),
        }
    };
    let (id, body) = card(&run(&mut p, &mut st, None, |p, cx| p.lookup(cx, "word", None).unwrap()));
    assert!(body.starts_with("from alpha") && body.ends_with("\n— 1/3 · Alpha"), "{body}");
    for (answer, want) in [("next", "2/3 · Beta"), ("next", "3/3 · Gamma"), ("next", "1/3 · Alpha"), ("prev", "3/3 · Gamma")] {
        let (rid, body) = card(&run(&mut p, &mut st, None, |p, cx| p.on_ui_result(cx, id, answer).unwrap()));
        assert_eq!(rid, id, "same request id: the host replaces the card");
        assert!(body.ends_with(&format!("\n— {want}")), "{answer}: {body}");
    }
    // Full view opens on the dictionary shown (Gamma).
    let ui = run(&mut p, &mut st, None, |p, cx| p.on_ui_result(cx, id, "full").unwrap());
    let Some(UiRequest::TextViewer { markdown, .. }) = ui.last() else { panic!("{ui:?}") };
    assert!(markdown.contains("**Gamma**") && markdown.contains("from gamma") && !markdown.contains("from alpha"), "{markdown}");
    // One dictionary: no pages, a swipe sends nothing.
    let hit = |d: &str, t: &str| Hit { dict: d.into(), headword: "w".into(), text: t.into(), entry: None };
    let one = LookupResult { query: "w".into(), form: "w".into(), hits: vec![hit("A", "first"), hit("A", "second")] };
    assert_eq!(popup_page(&one, 0).unwrap().1, "first\n— A · +1 more");
    let two = LookupResult { hits: vec![hit("A", "first"), hit("A", "again"), hit("B", "second")], ..one.clone() };
    assert_eq!(popup_page(&two, 0).unwrap().1, "first\n— 1/2 · A · +1 more");
    assert_eq!(popup_page(&two, 3).unwrap().1, "second\n— 2/2 · B");
    let (_, mut st1) = home("paged-card-one", true);
    let mut p1 = DictionaryPlugin::new();
    let mut book = Book { selection: Some(sel("household")) };
    run(&mut p1, &mut st1, Some(&mut book), |p, cx| p.on_action(cx, ACTION_LOOKUP).unwrap());
    assert!(run(&mut p1, &mut st1, Some(&mut book), |p, cx| p.on_ui_result(cx, 1, "next").unwrap()).is_empty());
}
