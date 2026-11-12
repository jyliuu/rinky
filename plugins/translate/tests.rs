//! Translate plugin tests: answers written by hand in the shape of the endpoint's JSON
//! arrays (the slots KOReader reads), a fake request runner (no threads, no network).

use ink_plugin::serde_json::json;
use ink_plugin::testing::{selection, FakeBook};
use ink_plugin::{DocumentAccess, HostCtx, MemoryStorage, NetHost, PluginError, Registrar, Registry, UiQueue, UiRequest};

use super::*;

// ---- fixtures ----

/// `Haus`, de -> en, with dictionary, alternatives and definitions.
const WORD: &str = r#"[[["House","Haus",null,null,10]],[["noun",["house","home","building","household"],[["house",["Haus","Gebäude"],null,0.6],["home",["Haus"],null,0.1]],"Haus",1]],"de",null,null,[["Haus",null,[["House",1000,true,false,[10]],["home",0,true,false,[8]]],[[0,4]],"Haus",0,0]],1,[],[["de"],null,[1],["de"]],null,null,null,[["Substantiv",[["Gebäude, das Menschen zum Wohnen dient.","m_de_0001","Ein Haus bauen."],["Familie, Haushalt.","m_de_0002"]],"Haus"]]]"#;

/// Two sentences: two segments, a romanization row, alternatives per segment.
const SENTENCES: &str = r#"[[["He walked home slowly. ","Er ging langsam nach Hause. ",null,null,10],["It was raining.","Es regnete.",null,null,10],[null,null,"Er ging"]],null,"de",null,null,[["Er ging langsam nach Hause.",null,[["He walked home slowly.",0,true,false,[10]],["He went home slowly.",0,true,false,[8]]],[[0,27]],"Er ging langsam nach Hause.",0,0],["Es regnete.",null,[["It was raining.",0,true,false,[10]]],[[0,11]],"Es regnete.",0,0]],0.9,[],[["de"],null,[0.9],["de"]]]"#;

/// English text translated into English.
const ENGLISH: &str = r#"[[["house","house",null,null,10]],null,"en",null,null,null,1,[],[["en"],null,[1],["en"]]]"#;

/// Google's refusal page (HTTP 429).
const SORRY: &str = "<html><head><title>Sorry...</title></head><body>automated queries</body></html>";

// ---- fakes ----

/// Records requests; no thread runs.
#[derive(Default)]
struct FakeJobs {
    reqs: Vec<(u32, Request)>,
    cancelled: Vec<u32>,
    refuse: bool,
}

impl NetHost for FakeJobs {
    fn start(&mut self, req: Request, mode: StreamMode) -> Result<u32> {
        assert!(matches!(mode, StreamMode::Whole { .. }));
        if self.refuse {
            return Err(PluginError::Unsupported("offline: no network".into()));
        }
        let id = self.reqs.len() as u32 + 100;
        self.reqs.push((id, req));
        Ok(id)
    }
    fn cancel(&mut self, id: u32) {
        self.cancelled.push(id);
    }
}

struct Env {
    p: TranslatePlugin,
    ui: UiQueue,
    st: MemoryStorage,
    jobs: FakeJobs,
    book: FakeBook,
}

impl Env {
    fn new() -> Env {
        Env { p: TranslatePlugin::default(), ui: UiQueue::default(), st: MemoryStorage::default(), jobs: FakeJobs::default(), book: FakeBook::titled("Ein Buch") }
    }

    fn call(&mut self, f: impl FnOnce(&mut TranslatePlugin, &mut HostCtx) -> Result<()>) {
        let perms = self.p.manifest().permissions.clone();
        let hosts = self.p.manifest().hosts.clone();
        let mut cx = HostCtx::new(&mut self.ui, &mut self.st, &perms)
            .with_doc(Some(&mut self.book as &mut dyn DocumentAccess))
            .with_jobs(Some(&mut self.jobs as &mut dyn NetHost))
            .with_hosts(&hosts);
        f(&mut self.p, &mut cx).expect("a hook never fails");
    }

    fn select(&mut self, text: &str) {
        self.book.selection = Some(selection(text));
        self.call(|p, cx| p.on_action(cx, ACTION));
    }

    fn last_req(&self) -> (u32, String) {
        let (id, r) = self.jobs.reqs.last().expect("a request");
        (*id, r.url.clone())
    }

    fn reply(&mut self, status: u16, body: &str) {
        let (id, _) = self.last_req();
        self.call(|p, cx| p.on_http(cx, id, &HttpEvent::Response { status, headers: vec![] }));
        self.call(|p, cx| p.on_http(cx, id, &HttpEvent::Done { body: Some(body.as_bytes().to_vec()) }));
    }

    fn answer(&mut self, id: u32, result: &str) {
        self.call(|p, cx| p.on_ui_result(cx, id, result));
    }

    fn card(&self) -> (String, String, Vec<(String, String)>) {
        self.ui
            .0
            .iter()
            .rev()
            .find_map(|r| if let UiRequest::Dialog { id: 1, title, body, buttons } = r { Some((title.clone(), body.clone(), buttons.clone())) } else { None })
            .expect("a card")
    }

    fn menu(&self) -> (String, Vec<(String, String)>) {
        self.ui.0.iter().rev().find_map(|r| if let UiRequest::Menu { title, items, .. } = r { Some((title.clone(), items.clone())) } else { None }).expect("a menu")
    }

    fn toasts(&self) -> Vec<String> {
        self.ui.0.iter().filter_map(|r| if let UiRequest::Toast { text } = r { Some(text.clone()) } else { None }).collect()
    }

    fn viewer(&self) -> String {
        self.ui.0.iter().rev().find_map(|r| if let UiRequest::TextViewer { markdown, .. } = r { Some(markdown.clone()) } else { None }).expect("a viewer")
    }
}

fn actions(buttons: &[(String, String)]) -> Vec<&str> {
    buttons.iter().map(|(_, a)| a.as_str()).collect()
}

// ---- api ----

#[test]
fn url_asks_for_every_part_and_encodes_the_text() {
    let u = api::url("auto", "zh-CN", "Er ging & kam");
    assert!(u.starts_with("https://translate.googleapis.com/translate_a/single?client=gtx&"), "{u}");
    for p in ["sl=auto", "tl=zh-CN", "dt=t", "dt=bd", "dt=at", "dt=md", "q=Er%20ging%20%26%20kam"] {
        assert!(u.contains(p), "{p} in {u}");
    }
}

#[test]
fn parses_a_word_with_dictionary_alternatives_and_definitions() {
    let t = api::parse(WORD.as_bytes()).unwrap();
    assert_eq!(t.text, "House");
    assert_eq!(t.source_lang.as_deref(), Some("de"));
    assert_eq!(t.dictionary, vec![("noun".to_string(), vec!["house".into(), "home".into(), "building".into(), "household".into()])]);
    // The main translation is not repeated among the alternatives.
    assert_eq!(t.alternatives, vec!["home".to_string()]);
    assert_eq!(t.definitions.len(), 1);
    assert_eq!(t.definitions[0].0, "Substantiv");
    assert_eq!(t.definitions[0].1, vec!["Gebäude, das Menschen zum Wohnen dient.".to_string(), "Familie, Haushalt.".into()]);
}

#[test]
fn parses_sentences_joined_without_romanization_or_alternatives() {
    let t = api::parse(SENTENCES.as_bytes()).unwrap();
    assert_eq!(t.text, "He walked home slowly. It was raining.");
    assert!(t.alternatives.is_empty(), "alternatives of one segment are not those of the text");
    assert!(t.dictionary.is_empty() && t.definitions.is_empty());
}

#[test]
fn refuses_answers_without_a_translation() {
    assert!(api::parse(SORRY.as_bytes()).is_err());
    assert!(api::parse(b"[]").is_err());
    assert!(api::parse(b"[[[null,null,\"x\"]],null,\"de\"]").is_err());
    assert!(api::parse(b"{\"error\":1}").is_err());
}

#[test]
fn clean_text_drops_soft_hyphens_and_cuts_long_selections() {
    assert_eq!(api::clean_text("  Mut\u{00AD}terspra\u{00AD}che\n ist "), ("Muttersprache ist".to_string(), false));
    let long = "Wort ".repeat(400);
    let (t, cut) = api::clean_text(&long);
    assert!(cut && t.chars().count() <= api::MAX_CHARS && t.ends_with("Wort"), "{} chars", t.chars().count());
}

#[test]
fn language_codes() {
    for ok in ["de", "en", "haw", "zh-CN", "zh-TW", "sr-Latn"] {
        assert!(api::valid_code(ok), "{ok}");
    }
    for bad in ["", "d", "German", "DE", "de-", "de-12345", "e1"] {
        assert!(!api::valid_code(bad), "{bad}");
    }
    assert_eq!(api::normalize_code(" ZH-cn "), "zh-CN");
    assert_eq!(api::book_code("en-US").as_deref(), Some("en"));
    assert_eq!(api::book_code("de").as_deref(), Some("de"));
    assert_eq!(api::book_code("zh-Hant").as_deref(), Some("zh-TW"));
    assert_eq!(api::book_code("zh").as_deref(), Some("zh-CN"));
    assert_eq!(api::book_code("und"), None);
    assert_eq!(api::book_code("eng"), None);
    assert_eq!(api::book_code(""), None);
    assert_eq!(api::name("de"), "German");
    assert_eq!(api::name("he"), "Hebrew");
    assert_eq!(api::name("xx"), "xx");
    assert!(api::same_lang("en", "en") && api::same_lang("zh-CN", "zh") && api::same_lang("he", "iw"));
    assert!(!api::same_lang("zh-CN", "zh-TW") && !api::same_lang("de", "en"));
}

// ---- the plugin ----

#[test]
fn registers_a_selection_action_that_is_not_lookup() {
    let mut reg = Registry::default();
    let mut p = TranslatePlugin::default();
    p.on_load(&mut Registrar { plugin: NAME, registry: &mut reg }).unwrap();
    assert!(reg.selection_actions.iter().any(|a| a.id == ACTION && a.label == "Translate"));
    assert!(reg.selection_actions.iter().all(|a| a.id != "lookup"), "the dictionary owns `lookup`");
    assert!(reg.settings_pages.iter().any(|s| s.id == SETTINGS_PAGE && s.title == "Translate"));
    assert_eq!(p.manifest().hosts, ["translate.googleapis.com"]);
}

#[test]
fn a_selected_word_shows_a_card_with_its_dictionary() {
    let mut e = Env::new();
    e.select("Haus");
    let (_, url) = e.last_req();
    assert!(url.contains("sl=auto") && url.contains("tl=en") && url.contains("q=Haus"), "{url}");
    assert_eq!(e.toasts(), vec!["Translating…".to_string()]);
    e.reply(200, WORD);
    let (title, body, buttons) = e.card();
    assert_eq!(title, "Haus");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines, vec!["_German to English_", "House", "noun: house, home, building, household", "— Google Translate"]);
    assert_eq!(actions(&buttons), vec!["close", "full"]);
}

#[test]
fn full_view_has_the_source_translation_and_definitions() {
    let mut e = Env::new();
    e.select("Haus");
    e.reply(200, WORD);
    e.answer(1, "full");
    let md = e.viewer();
    assert!(md.starts_with(":::top\n\n## German\n\nHaus\n\n## English\n\nHouse\n\n"), "{md}");
    assert!(md.contains("## Dictionary\n\n**noun**: house, home, building, household"), "{md}");
    assert!(md.contains("## Other translations\n\nhome"), "{md}");
    assert!(md.contains("## Definitions (German)\n\n**Substantiv**\n\n1\\. Gebäude, das Menschen zum Wohnen dient."), "{md}");
    assert!(md.trim_end().ends_with("*Translated by Google Translate*"), "{md}");
}

#[test]
fn sentences_show_the_whole_translation_and_a_cut_title() {
    let mut e = Env::new();
    e.select("Er ging ganz langsam nach Hause. Es regnete.");
    e.reply(200, SENTENCES);
    let (title, body, _) = e.card();
    assert!(title.ends_with('…') && title.chars().count() <= TITLE_CHARS + 1, "{title}");
    assert_eq!(body.lines().nth(1), Some("He walked home slowly. It was raining."));
    assert_eq!(body.lines().count(), 3, "{body}");
}

#[test]
fn other_translations_show_on_the_card_only_for_short_selections() {
    let t = Translation { text: "Er ging nach Hause".into(), source_lang: Some("en".into()), alternatives: vec!["Er lief heim".into()], ..Default::default() };
    let (_, body) = card("He went home", &t, "auto", "de");
    assert!(body.contains("Also: Er lief heim"), "{body}");
    let (_, body) = card("He went home after the rain", &t, "auto", "de");
    assert!(!body.contains("Also:"), "{body}");
}

#[test]
fn text_already_in_the_target_language_says_so() {
    let mut e = Env::new();
    e.select("house");
    e.reply(200, ENGLISH);
    let (_, body, buttons) = e.card();
    assert!(body.contains("already in English") && body.contains("Settings › Plugins › Translate"), "{body}");
    assert_eq!(actions(&buttons), vec!["close"]);
}

#[test]
fn settings_choose_the_languages_sent() {
    let mut e = Env::new();
    e.st.settings.insert("to".into(), json!("da"));
    e.st.settings.insert("from".into(), json!("book"));
    e.book.metadata.language = Some("de-DE".into());
    e.select("Haus");
    let (_, url) = e.last_req();
    assert!(url.contains("sl=de&") && url.contains("tl=da&"), "{url}");
    // A book without a language falls back to auto-detection.
    e.book.metadata.language = None;
    e.select("Haus");
    assert!(e.last_req().1.contains("sl=auto&"));
    // A broken setting falls back to the defaults.
    e.st.settings.insert("to".into(), json!("Danish"));
    e.st.settings.insert("from".into(), json!(""));
    e.select("Haus");
    let (_, url) = e.last_req();
    assert!(url.contains("sl=auto&") && url.contains("tl=en&"), "{url}");
}

#[test]
fn a_refusal_shows_a_card_with_retry() {
    let mut e = Env::new();
    e.select("Haus");
    let (_, first) = e.last_req();
    e.reply(429, SORRY);
    let (title, body, buttons) = e.card();
    assert_eq!(title, "Haus");
    assert!(body.contains("too many requests"), "{body}");
    assert_eq!(actions(&buttons), vec!["close", "retry"]);
    e.answer(1, "retry");
    assert_eq!(e.jobs.reqs.len(), 2);
    assert_eq!(e.last_req().1, first);
    e.reply(200, WORD);
    assert_eq!(e.card().1.lines().nth(1), Some("House"));
}

#[test]
fn network_failures_are_cards_not_errors() {
    let mut e = Env::new();
    e.jobs.refuse = true;
    e.select("Haus");
    assert!(e.card().1.contains("Turn on Wi-Fi"), "{}", e.card().1);
    e.jobs.refuse = false;
    e.select("Haus");
    let (id, _) = e.last_req();
    e.call(|p, cx| p.on_http(cx, id, &HttpEvent::Error("timed out reading".into())));
    assert!(e.card().1.contains("did not answer in time"));
    e.select("Haus");
    e.reply(200, "not json");
    assert!(e.card().1.contains("cannot read"));
    // Sleep interrupts quietly: no new card.
    let cards = e.ui.0.len();
    e.select("Haus");
    let (id, _) = e.last_req();
    e.call(|p, cx| p.on_http(cx, id, &HttpEvent::Error("interrupted: the device went to sleep".into())));
    assert_eq!(e.ui.0.len(), cards + 1, "only the toast");
}

#[test]
fn closing_the_card_drops_the_request() {
    let mut e = Env::new();
    e.select("Haus");
    let (id, _) = e.last_req();
    e.answer(1, "close");
    assert_eq!(e.jobs.cancelled, vec![id]);
    let n = e.ui.0.len();
    e.reply(200, WORD);
    assert_eq!(e.ui.0.len(), n, "a dropped answer shows nothing");
}

#[test]
fn an_empty_selection_asks_for_text() {
    let mut e = Env::new();
    e.select(" \u{00AD} ");
    assert!(e.jobs.reqs.is_empty());
    assert_eq!(e.toasts(), vec!["Select text to translate".to_string()]);
}

#[test]
fn the_request_is_allowed_by_the_manifest_hosts() {
    // `with_hosts` in Env holds the manifest's hosts; a refused host would fail `start`.
    let mut e = Env::new();
    e.select("Haus");
    assert_eq!(e.jobs.reqs.len(), 1);
}

#[test]
fn settings_page_picks_languages() {
    let mut e = Env::new();
    e.call(|p, cx| p.on_action(cx, &format!("settings:{SETTINGS_PAGE}")));
    let (title, items) = e.menu();
    assert_eq!(title, "Translate");
    assert_eq!(items[0].0, "Translate to\tEnglish (en)");
    assert_eq!(items[1].0, "Translate from\tAuto-detect");
    e.answer(ID_SET, "to");
    let (_, items) = e.menu();
    assert!(items.iter().any(|(l, a)| l == "German\tde" && a == "l:de"));
    e.answer(ID_TO, "l:de");
    assert_eq!(e.st.settings.get("to"), Some(&json!("de")));
    assert_eq!(e.menu().1[0].0, "Translate to\tGerman (de)");
    e.answer(ID_SET, "from");
    e.answer(ID_FROM, "l:book");
    assert_eq!(e.st.settings.get("from"), Some(&json!("book")));
    assert_eq!(e.menu().1[1].0, "Translate from\tBook language");
    // A typed code is checked; a bad one leaves the setting as it was.
    e.answer(ID_SET, "from");
    e.answer(ID_FROM, "other");
    e.answer(ID_CODE_FROM, "Klingon");
    assert_eq!(e.st.settings.get("from"), Some(&json!("book")));
    assert!(e.toasts().last().unwrap().contains("not a language code"));
    e.answer(ID_CODE_FROM, "ZH-cn");
    assert_eq!(e.st.settings.get("from"), Some(&json!("zh-CN")));
    assert_eq!(e.menu().1[1].0, "Translate from\tChinese (Simplified) (zh-CN)");
}
