//! Wikipedia plugin tests: recorded-shape fixtures written by hand from the documented
//! API, a fake request runner (no threads, no network) and a temporary data folder.

use ink_plugin::serde_json::json;
use ink_plugin::testing::{selection, FakeBook, TempDir};
use ink_plugin::{DocumentAccess, HostCtx, MemoryStorage, NetHost, PluginError, Registrar, Registry, UiQueue, UiRequest};

use super::*;

// ---- fixtures (shapes of the documented responses) ----

const SUMMARY: &str = r#"{
  "type": "standard",
  "title": "Mutiny on the Bounty",
  "displaytitle": "<i>Mutiny on the Bounty</i>",
  "description": "1789 insurrection aboard a Royal Navy ship",
  "extract": "The mutiny on the Bounty occurred in the south Pacific Ocean on 28 April 1789 aboard HMS Bounty. Disaffected crewmen, led by acting-Lieutenant Fletcher Christian, seized control of the ship from their captain, Lieutenant William Bligh.",
  "extract_html": "<p>The mutiny on the <i>Bounty</i> occurred...</p>",
  "content_urls": {"desktop": {"page": "https://en.wikipedia.org/wiki/Mutiny_on_the_Bounty"}, "mobile": {"page": "https://en.m.wikipedia.org/wiki/Mutiny_on_the_Bounty"}},
  "lang": "en"
}"#;

const SUMMARY_DE: &str = r#"{"type":"standard","title":"Meuterei auf der Bounty","description":"Meuterei 1789","extract":"Die Meuterei auf der Bounty fand am 28. April 1789 statt.","content_urls":{"desktop":{"page":"https://de.wikipedia.org/wiki/Meuterei_auf_der_Bounty"}}}"#;

const NOT_FOUND: &str = r#"{"type":"https://mediawiki.org/wiki/HyperSwitch/errors/not_found","title":"Not found.","method":"get","detail":"Page or revision not found.","uri":"/en.wikipedia.org/v1/page/summary/Mutinyy"}"#;

const SEARCH: &str = r#"{
  "batchcomplete": true,
  "query": {
    "searchinfo": {"totalhits": 2},
    "search": [
      {"ns": 0, "title": "Mutiny on the Bounty", "pageid": 1, "size": 1, "wordcount": 1, "snippet": "The <span class=\"searchmatch\">mutiny</span> on the <span class=\"searchmatch\">Bounty</span> occurred &quot;in&quot; 1789 &amp; more", "timestamp": "2026-01-01T00:00:00Z"},
      {"ns": 0, "title": "Mutiny", "pageid": 2, "size": 1, "wordcount": 1, "snippet": "A <span class=\"searchmatch\">mutiny</span> is a revolt", "timestamp": "2026-01-01T00:00:00Z"}
    ]
  }
}"#;

const SEARCH_EMPTY: &str = r#"{"batchcomplete":true,"query":{"searchinfo":{"totalhits":0},"search":[]}}"#;

const SEARCH_SUGGEST: &str = r#"{"batchcomplete":true,"query":{"searchinfo":{"totalhits":0,"suggestion":"mutiny","suggestionsnippet":"<em>mutiny</em>"},"search":[]}}"#;

const ARTICLE: &str = r#"{"batchcomplete":true,"query":{"normalized":[{"fromencoded":false,"from":"mutiny on the Bounty","to":"Mutiny on the Bounty"}],"pages":[{"pageid":1,"ns":0,"title":"Mutiny on the Bounty","extract":"The mutiny on the Bounty occurred in 1789.\n\n\n== Background ==\nThe ship sailed to Tahiti. 1. Not a list - really *star*\n\n\n=== Voyage ===\nIt took ten months.\n\n\n== See also =="}]}}"#;

const ARTICLE_MISSING: &str = r#"{"batchcomplete":true,"query":{"pages":[{"ns":0,"title":"Nope","missing":true}]}}"#;

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
            return Err(PluginError::Unsupported("no network in this context".into()));
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
    p: WikipediaPlugin,
    ui: UiQueue,
    st: MemoryStorage,
    jobs: FakeJobs,
    book: FakeBook,
    root: TempDir,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root = TempDir::new(&format!("wikipedia-{tag}"));
        let mut st = MemoryStorage { dir: root.join("plugins/wikipedia/data"), ..MemoryStorage::default() };
        st.settings.insert("save_dir".into(), json!(root.join("Books").to_str().unwrap()));
        Env { p: WikipediaPlugin::default(), ui: UiQueue::default(), st, jobs: FakeJobs::default(), book: FakeBook::titled("A Book"), root }
    }

    fn call(&mut self, f: impl FnOnce(&mut WikipediaPlugin, &mut HostCtx) -> Result<()>) {
        let perms = self.p.manifest().permissions.clone();
        let mut cx = HostCtx::new(&mut self.ui, &mut self.st, &perms).with_doc(Some(&mut self.book as &mut dyn DocumentAccess)).with_jobs(Some(&mut self.jobs as &mut dyn NetHost));
        f(&mut self.p, &mut cx).expect("a hook never fails");
    }

    /// Selects `text` and triggers the selection action.
    fn select(&mut self, text: &str) {
        self.book.selection = Some(selection(text));
        self.call(|p, cx| p.on_action(cx, ACTION_LOOKUP));
    }

    /// The newest request: (id, url).
    fn last_req(&self) -> (u32, String) {
        let (id, r) = self.jobs.reqs.last().expect("a request");
        (*id, r.url.clone())
    }

    /// Answers the newest request with `status` and `body`.
    fn reply(&mut self, status: u16, body: &str) {
        let (id, _) = self.last_req();
        self.call(|p, cx| p.on_http(cx, id, &HttpEvent::Response { status, headers: vec![] }));
        self.call(|p, cx| p.on_http(cx, id, &HttpEvent::Done { body: Some(body.as_bytes().to_vec()) }));
    }

    fn fail(&mut self, msg: &str) {
        let (id, _) = self.last_req();
        self.call(|p, cx| p.on_http(cx, id, &HttpEvent::Error(msg.into())));
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

fn labels(b: &[(String, String)]) -> Vec<&str> {
    b.iter().map(|(l, _)| l.as_str()).collect()
}

// ---- pure parts ----

#[test]
fn registers_the_entries() {
    let mut p = WikipediaPlugin::default();
    let mut reg = Registry::default();
    p.on_load(&mut Registrar { plugin: NAME, registry: &mut reg }).unwrap();
    assert!(reg.selection_actions.iter().any(|a| a.id == ACTION_LOOKUP && a.label == "Wikipedia"));
    assert!(reg.selection_actions.iter().all(|a| a.id != "lookup"), "the dictionary owns `lookup`");
    assert!(reg.menu_items.iter().any(|m| m.id == ACTION_TYPE && m.label == "Wikipedia"));
    assert!(reg.settings_pages.iter().any(|s| s.id == SETTINGS_PAGE && s.title == "Wikipedia"), "{:?}", reg.settings_pages);
    assert_eq!(p.manifest().hosts, ["wikipedia.org"]);
}

#[test]
fn urls_and_language_codes() {
    assert_eq!(api::summary_url("en", "Albert Einstein"), "https://en.wikipedia.org/api/rest_v1/page/summary/Albert_Einstein");
    assert_eq!(api::summary_url("de", "AC/DC"), "https://de.wikipedia.org/api/rest_v1/page/summary/AC%2FDC");
    assert_eq!(api::summary_url("en", "Zoë & co?"), "https://en.wikipedia.org/api/rest_v1/page/summary/Zo%C3%AB_%26_co%3F");
    assert_eq!(
        api::search_url("da", "mutiny bounty"),
        "https://da.wikipedia.org/w/api.php?action=query&list=search&srsearch=mutiny%20bounty&srlimit=10&format=json&formatversion=2"
    );
    let a = api::article_url("fr", "Mutiny on the Bounty");
    assert!(a.starts_with("https://fr.wikipedia.org/w/api.php?action=query&prop=extracts&explaintext=1"), "{a}");
    assert!(a.contains("redirects=1") && a.contains("formatversion=2") && a.ends_with("&formatversion=2") && a.contains("titles=Mutiny%20on%20the%20Bounty"), "{a}");
    for ok in ["en", "da", "zh-yue", "simple", "be-tarask"] {
        assert!(api::valid_lang(ok), "{ok}");
    }
    for bad in ["", "e", "EN", "en.evil.com", "en/x", "en-", "-en", "en--x", "1en", "en wiki", "abcdefghijklmn", "en@x"] {
        assert!(!api::valid_lang(bad), "{bad:?}");
    }
}

#[test]
fn selection_text_becomes_a_search_term() {
    assert_eq!(api::clean_query("  “Mutiny\n on the  Bounty”, "), "Mutiny on the Bounty");
    assert_eq!(api::clean_query("(Paris)."), "Paris");
    assert_eq!(api::clean_query("..."), "");
    assert_eq!(api::clean_query(&"x".repeat(500)).chars().count(), 120);
}

#[test]
fn parses_the_summary() {
    let s = api::parse_summary(SUMMARY.as_bytes()).unwrap();
    assert_eq!(s.title, "Mutiny on the Bounty");
    assert_eq!(s.description.as_deref(), Some("1789 insurrection aboard a Royal Navy ship"));
    assert!(s.extract.starts_with("The mutiny on the Bounty occurred"));
    assert_eq!(s.url.as_deref(), Some("https://en.wikipedia.org/wiki/Mutiny_on_the_Bounty"));
    assert_eq!(s.kind, "standard");
    // Minimal: only a title (a "no-extract" page has no description either).
    let m = api::parse_summary(br#"{"type":"no-extract","title":"Foo"}"#).unwrap();
    assert_eq!((m.title.as_str(), m.description, m.extract.as_str(), m.kind.as_str()), ("Foo", None, "", "no-extract"));
    // Broken shapes are errors, not panics.
    assert!(api::parse_summary(b"<html>502</html>").is_err());
    assert!(api::parse_summary(NOT_FOUND.replace("\"title\":\"Not found.\",", "").as_bytes()).is_err());
}

#[test]
fn parses_search_and_cleans_snippets() {
    let hits = api::parse_search(SEARCH.as_bytes()).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].0, "Mutiny on the Bounty");
    assert_eq!(hits[0].1, "The mutiny on the Bounty occurred \"in\" 1789 & more");
    assert_eq!(hits[1], ("Mutiny".to_string(), "A mutiny is a revolt".to_string()));
    assert!(api::parse_search(SEARCH_EMPTY.as_bytes()).unwrap().is_empty());
    assert!(api::parse_search(br#"{"error":{"code":"readapidenied","info":"You need read permission"}}"#).unwrap_err().contains("read permission"));
    assert!(api::parse_search(b"{}").is_err());
    assert_eq!(api::strip_html("a &#39;b&#x27; &lt;i&gt; &unknown; & c"), "a 'b' <i> &unknown; & c");
}

#[test]
fn parses_the_article_and_builds_text() {
    let (title, text) = api::parse_article(ARTICLE.as_bytes()).unwrap().unwrap();
    assert_eq!(title, "Mutiny on the Bounty");
    assert!(text.contains("== Background =="));
    assert_eq!(api::parse_article(ARTICLE_MISSING.as_bytes()).unwrap(), None);
    assert_eq!(api::parse_article(br#"{"query":{"pages":[]}}"#).unwrap(), None);
    assert!(api::parse_article(b"nope").is_err());

    let md = api::article_markdown(&text, Some("https://en.wikipedia.org/wiki/X"));
    assert!(md.starts_with(":::top\n\n"), "{md}");
    assert!(md.contains("\n## Background\n"), "{md}");
    assert!(md.contains("\n### Voyage\n"), "{md}");
    assert!(md.contains("\n## See also\n"), "{md}");
    // Markdown punctuation in the text is escaped, a leading "1. " is not a list.
    assert!(md.contains("The ship sailed to Tahiti. 1"), "{md}");
    assert!(md.contains("Not a list \\- really \\*star\\*"), "{md}");
    assert!(md.trim_end().ends_with("*https://en.wikipedia.org/wiki/X*"), "{md}");

    let html = api::article_html("en", "A <b> & C", Some("desc"), &text, "https://en.wikipedia.org/wiki/X");
    assert!(html.contains("<meta charset=\"utf-8\"><title>A &lt;b&gt; &amp; C</title>"), "{html}");
    assert!(html.contains("<h2>Background</h2>") && html.contains("<h3>Voyage</h3>") && html.contains("<p><i>desc</i></p>"), "{html}");
    assert!(html.contains("CC BY-SA"), "{html}");
    assert_eq!(api::file_name("AC/DC: live?", "de"), "AC_DC_ live_ (Wikipedia de).html");
    // No space where the title is cut or after a leading dot.
    assert_eq!(api::file_name(&format!("{} yyy", "x".repeat(79)), "en"), format!("{} (Wikipedia en).html", "x".repeat(79)));
    assert_eq!(api::file_name(" . Hidden", "en"), "Hidden (Wikipedia en).html");
    assert_eq!(api::file_name(" .. ", "en"), "Article (Wikipedia en).html");
}

#[test]
fn long_articles_are_cut_at_a_line() {
    let line = "word ".repeat(19);
    let text = vec![line.trim(); 6000].join("\n");
    assert!(text.chars().count() > api::ARTICLE_CHARS);
    let md = api::article_markdown(&text, None);
    assert!(md.contains("*The article is cut here.*"));
    assert!(md.chars().count() < api::ARTICLE_CHARS * 2 + 200);
}

#[test]
fn card_text_follows_the_card_convention() {
    let s = api::parse_summary(SUMMARY.as_bytes()).unwrap();
    let (title, body) = summary_card(&s, "en", 1, 1);
    assert_eq!(title, "Mutiny on the Bounty");
    let lines: Vec<&str> = body.lines().collect();
    // `_subtitle_` first, `— footer` last, the extract between (cut, one paragraph).
    assert_eq!(lines[0], "_1789 insurrection aboard a Royal Navy ship_");
    assert_eq!(lines.last().copied(), Some("— Wikipedia · en"));
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1], s.extract, "a short extract is not cut");
    // A long extract is cut on a word at CARD_CHARS with an ellipsis, as one paragraph.
    let long = Summary { extract: "word ".repeat(200), ..s.clone() };
    let (_, body) = summary_card(&long, "en", 1, 1);
    let cut = body.lines().nth(1).unwrap();
    assert!(cut.ends_with("word…") && cut.chars().count() <= CARD_CHARS + 1, "{cut}");
    // Several languages: the paged footer `N/M · `.
    let (_, body) = summary_card(&s, "de", 2, 3);
    assert_eq!(body.lines().last(), Some("— 2/3 · Wikipedia · de"));
    // No description: the body starts with the extract, not a subtitle.
    let plain = Summary { description: None, ..s };
    assert!(!summary_card(&plain, "en", 1, 1).1.starts_with('_'));
    let (t, b) = missing_card("Mutinyy", "en", 1, 1);
    assert_eq!(t, "Mutinyy");
    assert_eq!(b, "No article found for “Mutinyy” on en.wikipedia.org.\n— Wikipedia · en");
}

#[test]
fn network_errors_get_clear_messages() {
    for m in ["offline: Wi-Fi is off", "http: io: failed to lookup address information: Name or service not known", "http: io: Network is unreachable (os error 101)"] {
        assert!(net_message(m).starts_with("Turn on Wi-Fi"), "{m}");
    }
    assert!(net_message("timed out").contains("did not answer in time"));
    assert!(net_message("tls: invalid peer certificate: NotValidYet").contains("date and time"));
    assert!(net_message("response larger than 3145728 bytes").contains("too large"));
    assert_eq!(net_message("weird"), "Could not load Wikipedia: weird");
}

#[test]
fn cache_keeps_fifty_and_survives_a_restart() {
    let dir = TempDir::new("wikipedia-cache");
    let path = dir.join("cache.json");
    let mut c = Cache::default();
    for i in 0..60 {
        c.put("en", &format!("Query {i}"), &Summary { title: format!("Title {i}"), extract: format!("Extract {i}"), kind: "standard".into(), ..Summary::default() });
    }
    assert_eq!(c.len(), api::CACHE_SIZE);
    assert!(c.get("en", "query 5").is_none(), "the oldest left");
    assert_eq!(c.get("en", "  QUERY 59 ").unwrap().title, "Title 59");
    assert_eq!(c.get("en", "title 58").unwrap().extract, "Extract 58", "the page title is a key, too");
    assert!(c.get("de", "query 59").is_none(), "the language is part of the key");
    // The same page again replaces its entry.
    c.put("en", "other words", &Summary { title: "Title 58".into(), extract: "new".into(), kind: "standard".into(), ..Summary::default() });
    assert_eq!(c.len(), api::CACHE_SIZE);
    c.save(&path).unwrap();
    let mut back = Cache::load(&path);
    assert_eq!(back, c);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("\"description\"") && !text.contains("\"url\"") && !text.contains("null"), "absent fields are left out: {text}");
    let mut one = Cache::default();
    one.put("da", "q", &Summary { title: "T".into(), description: Some("D".into()), url: Some("https://da.wikipedia.org/wiki/T".into()), kind: "standard".into(), ..Summary::default() });
    one.save(&path).unwrap();
    assert_eq!(Cache::load(&path), one);
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"description\":\"D\""));
    assert_eq!(back.get("en", "Other  words").unwrap().extract, "new");
    // A damaged file is an empty cache.
    std::fs::write(&path, "{ not json").unwrap();
    assert!(Cache::load(&path).is_empty());
    assert!(Cache::load(&dir.join("missing.json")).is_empty());
}

// ---- the plugin ----

#[test]
fn selection_to_card_and_cache() {
    let mut e = Env::new("card");
    e.select("“mutiny on the bounty”.");
    // One request on the worker, a toast, no card yet.
    assert_eq!(e.jobs.reqs.len(), 1);
    let (id, url) = e.last_req();
    assert_eq!(url, "https://en.wikipedia.org/api/rest_v1/page/summary/mutiny_on_the_bounty");
    let req = &e.jobs.reqs[0].1;
    assert_eq!((req.timeout.as_secs(), req.total_timeout.map(|t| t.as_secs()), req.max_redirects), (10, Some(20), 10));
    assert_eq!(e.toasts().len(), 1);
    assert!(!e.ui.0.iter().any(|r| matches!(r, UiRequest::Dialog { .. })));
    // The summary arrives: the card.
    e.reply(200, SUMMARY);
    let (title, body, buttons) = e.card();
    assert_eq!(title, "Mutiny on the Bounty");
    assert!(body.starts_with("_1789 insurrection") && body.ends_with("\n— Wikipedia · en"), "{body}");
    assert_eq!(buttons, [("Close".to_string(), "close".to_string()), ("Save".into(), "save".into()), ("Full view".into(), "full".into())]);
    // Cached on disk.
    let cache = e.st.dir.join("cache.json");
    assert!(cache.exists());
    assert_eq!(Cache::load(&cache).len(), 1);
    // The same word again (a new plugin, as after a restart): no request, the card at once.
    e.p = WikipediaPlugin::default();
    e.ui.0.clear();
    e.select("Mutiny on the Bounty");
    assert_eq!(e.jobs.reqs.len(), 1, "answered from the cache");
    assert!(e.toasts().is_empty());
    assert_eq!(e.card().0, "Mutiny on the Bounty");
    // The old request's late event is ignored.
    let n = e.ui.0.len();
    e.call(|p, cx| p.on_http(cx, id, &HttpEvent::Done { body: Some(SUMMARY.as_bytes().to_vec()) }));
    assert_eq!(e.ui.0.len(), n);
}

#[test]
fn no_selection_and_empty_term_only_toast() {
    let mut e = Env::new("empty");
    e.call(|p, cx| p.on_action(cx, ACTION_LOOKUP));
    e.select("...");
    assert!(e.jobs.reqs.is_empty());
    assert_eq!(e.toasts(), ["Select a word to look up", "Select a word to look up"]);
}

#[test]
fn a_404_searches_and_the_hits_are_a_menu() {
    let mut e = Env::new("search");
    e.select("Mutinyy");
    e.reply(404, NOT_FOUND);
    // The second request is the search, with the term as typed.
    assert_eq!(e.jobs.reqs.len(), 2);
    assert_eq!(e.last_req().1, "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch=Mutinyy&srlimit=10&format=json&formatversion=2");
    assert!(!e.ui.0.iter().any(|r| matches!(r, UiRequest::Dialog { .. })), "no card for a 404");
    e.reply(200, SEARCH);
    let (title, items) = e.menu();
    assert!(title.contains("Mutinyy"), "{title}");
    assert_eq!(items, [("Mutiny on the Bounty".to_string(), "h0".to_string()), ("Mutiny".into(), "h1".into())]);
    // Pick the second hit: its summary is requested, then shown.
    e.answer(3, "h1");
    assert_eq!(e.last_req().1, "https://en.wikipedia.org/api/rest_v1/page/summary/Mutiny");
    e.reply(200, r#"{"type":"standard","title":"Mutiny","description":"revolt","extract":"A mutiny is a revolt.","content_urls":{"desktop":{"page":"https://en.wikipedia.org/wiki/Mutiny"}}}"#);
    let (title, body, _) = e.card();
    assert_eq!(title, "Mutiny");
    assert!(body.contains("A mutiny is a revolt."));
    // Cached under the picked title: picking it again needs no request.
    assert_eq!(e.jobs.reqs.len(), 3);
    e.select("Mutiny");
    assert_eq!(e.jobs.reqs.len(), 3, "the picked page is cached under its title");
    e.select("Mutinyy");
    assert_eq!(e.jobs.reqs.len(), 4, "the misspelt term was not cached, only the page");
}

#[test]
fn a_spelling_suggestion_is_offered_when_nothing_matches() {
    assert_eq!(api::parse_suggestion(SEARCH_SUGGEST.as_bytes()).as_deref(), Some("mutiny"));
    assert_eq!(api::parse_suggestion(SEARCH_EMPTY.as_bytes()), None);
    let mut e = Env::new("suggest");
    e.select("Mutinyy");
    e.reply(404, NOT_FOUND);
    e.reply(200, SEARCH_SUGGEST);
    assert_eq!(e.menu().1, [("mutiny".to_string(), "h0".to_string())]);
    e.answer(3, "h0");
    assert_eq!(e.last_req().1, "https://en.wikipedia.org/api/rest_v1/page/summary/mutiny");
}

#[test]
fn a_404_without_hits_shows_a_missing_card() {
    let mut e = Env::new("nohits");
    e.select("qzxv");
    e.reply(404, NOT_FOUND);
    e.reply(200, SEARCH_EMPTY);
    let (title, body, buttons) = e.card();
    assert_eq!(title, "qzxv");
    assert_eq!(body, "No article found for “qzxv” on en.wikipedia.org.\n— Wikipedia · en");
    assert_eq!(labels(&buttons), ["Close"]);
}

#[test]
fn wifi_off_shows_the_hint_and_retry_works() {
    let mut e = Env::new("wifi");
    e.select("Paris");
    e.fail("http: io: failed to lookup address information: Name or service not known");
    let (title, body, buttons) = e.card();
    assert_eq!(title, "Paris");
    assert!(body.starts_with("Turn on Wi-Fi to look this up on Wikipedia.") && body.ends_with("\n— Wikipedia · en"), "{body}");
    assert_eq!(labels(&buttons), ["Close", "Retry"]);
    // Retry sends the request again.
    e.answer(1, "retry");
    assert_eq!(e.jobs.reqs.len(), 2);
    assert_eq!(e.last_req().1, e.jobs.reqs[0].1.url);
    // A timeout, then a server error.
    e.fail("timed out");
    assert!(e.card().1.contains("did not answer in time"));
    e.answer(1, "retry");
    e.reply(503, "{}");
    assert!(e.card().1.starts_with("Could not load Wikipedia: Wikipedia answered HTTP 503"), "{}", e.card().1);
    // Garbage with status 200.
    e.answer(1, "retry");
    e.reply(200, "<html>captive portal</html>");
    assert!(e.card().1.contains("bad answer"), "{}", e.card().1);
}

#[test]
fn a_runner_that_refuses_shows_a_card_not_an_error() {
    let mut e = Env::new("refuse");
    e.jobs.refuse = true;
    e.select("Paris");
    assert!(e.card().1.contains("Could not load Wikipedia"), "{}", e.card().1);
    // Sleep interrupts a request: nothing is shown.
    e.jobs.refuse = false;
    e.ui.0.clear();
    e.select("Rome");
    let n = e.ui.0.len();
    e.fail("interrupted: the device went to sleep");
    assert_eq!(e.ui.0.len(), n);
}

#[test]
fn closing_the_card_cancels_the_request() {
    let mut e = Env::new("close");
    e.select("Paris");
    let (id, _) = e.last_req();
    e.answer(1, "");
    assert_eq!(e.jobs.cancelled, [id]);
    // A new lookup cancels the one before it.
    e.select("Rome");
    let (id2, _) = e.last_req();
    e.select("Oslo");
    assert_eq!(e.jobs.cancelled, [id, id2]);
}

#[test]
fn languages_swipe_like_the_dictionary_card() {
    let mut e = Env::new("langs");
    e.st.settings.insert("language".into(), json!("DA "));
    e.st.settings.insert("languages".into(), json!(["de", "da", "de", "bad lang", "fr"]));
    e.select("Bounty");
    assert!(e.last_req().1.starts_with("https://da.wikipedia.org/api/rest_v1/page/summary/Bounty"));
    e.reply(200, SUMMARY);
    // Primary, then de and fr (no repeats, bad codes dropped): 3 pages.
    assert_eq!(e.card().1.lines().last(), Some("— 1/3 · Wikipedia · da"));
    // Swipe: de is fetched.
    e.answer(1, "next");
    assert!(e.last_req().1.starts_with("https://de.wikipedia.org/api/rest_v1/page/summary/Bounty"));
    e.reply(200, SUMMARY_DE);
    let (title, body, _) = e.card();
    assert_eq!(title, "Meuterei auf der Bounty");
    assert_eq!(body.lines().last(), Some("— 2/3 · Wikipedia · de"));
    // fr has no article: 404, then an empty search: a missing card on page 3.
    e.answer(1, "next");
    assert!(e.last_req().1.starts_with("https://fr.wikipedia.org/api/rest_v1/"));
    e.reply(404, NOT_FOUND);
    assert!(e.last_req().1.starts_with("https://fr.wikipedia.org/w/api.php?"));
    e.reply(200, SEARCH_EMPTY);
    let (_, body, _) = e.card();
    assert!(body.starts_with("No article found for “Bounty” on fr.wikipedia.org.") && body.ends_with("— 3/3 · Wikipedia · fr"), "{body}");
    // Wraps to page 1 without a request; prev goes back to page 3 (kept in memory).
    let n = e.jobs.reqs.len();
    e.answer(1, "next");
    assert_eq!(e.card().1.lines().last(), Some("— 1/3 · Wikipedia · da"));
    e.answer(1, "prev");
    assert!(e.card().1.ends_with("— 3/3 · Wikipedia · fr"));
    assert_eq!(e.jobs.reqs.len(), n);
    // The Full view opens the page that is shown; fr has none, so nothing happens.
    e.answer(1, "full");
    assert_eq!(e.jobs.reqs.len(), n);
}

#[test]
fn one_language_does_not_page() {
    let mut e = Env::new("single");
    e.select("Bounty");
    e.reply(200, SUMMARY);
    e.answer(1, "next");
    assert_eq!(e.jobs.reqs.len(), 1);
    assert!(!e.card().1.contains("1/1"));
}

#[test]
fn full_view_loads_then_replaces_the_text() {
    let mut e = Env::new("full");
    e.select("Bounty");
    e.reply(200, SUMMARY);
    e.answer(1, "full");
    // The viewer opens with a loading text at once; the article request is on the worker.
    assert!(e.viewer().contains("Loading"));
    let url = e.last_req().1;
    assert!(url.contains("prop=extracts") && url.contains("explaintext=1") && url.contains("titles=Mutiny%20on%20the%20Bounty"), "{url}");
    assert_eq!(e.jobs.reqs[1].1.total_timeout.map(|t| t.as_secs()), Some(45));
    e.reply(200, ARTICLE);
    let md = e.viewer();
    assert!(md.starts_with(":::top") && md.contains("## Background") && md.contains("The mutiny on the Bounty occurred in 1789"), "{md}");
    let views = e.ui.0.iter().filter(|r| matches!(r, UiRequest::TextViewer { id: 2, .. })).count();
    assert_eq!(views, 2, "loading text and the article, same viewer id");
    // Closing the viewer while loading cancels the request and drops the text.
    let mut e = Env::new("full2");
    e.select("Bounty");
    e.reply(200, SUMMARY);
    e.answer(1, "full");
    let (id, _) = e.last_req();
    e.answer(2, "closed");
    assert_eq!(e.jobs.cancelled, [id]);
    // A failure shows in the viewer.
    let mut e = Env::new("full3");
    e.select("Bounty");
    e.reply(200, SUMMARY);
    e.answer(1, "full");
    e.fail("timed out");
    assert!(e.viewer().contains("did not answer in time"));
    // A missing article.
    let mut e = Env::new("full4");
    e.select("Bounty");
    e.reply(200, SUMMARY);
    e.answer(1, "full");
    e.reply(200, ARTICLE_MISSING);
    assert!(e.viewer().contains("no text"));
}

#[test]
fn save_writes_an_html_book_once_and_rescans() {
    let mut e = Env::new("save");
    e.select("Bounty");
    e.reply(200, SUMMARY);
    e.answer(1, "save");
    assert!(e.last_req().1.contains("prop=extracts"));
    e.reply(200, ARTICLE);
    let file = e.root.join("Books/Mutiny on the Bounty (Wikipedia en).html");
    let html = std::fs::read_to_string(&file).expect("the saved file");
    assert!(html.contains("<title>Mutiny on the Bounty</title>") && html.contains("<h2>Background</h2>") && html.contains("https://en.wikipedia.org/wiki/Mutiny_on_the_Bounty"), "{html}");
    assert!(!e.root.join("Books/Mutiny on the Bounty (Wikipedia en).html.part").exists());
    assert!(e.ui.0.iter().any(|r| matches!(r, UiRequest::LibraryRescan)));
    assert!(e.toasts().iter().any(|t| t.starts_with("Saved “Mutiny on the Bounty”")), "{:?}", e.toasts());
    // Again: the article is in memory, the file is not overwritten.
    std::fs::write(&file, "mine").unwrap();
    e.answer(1, "save");
    assert_eq!(e.jobs.reqs.len(), 2);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
    assert!(e.toasts().last().unwrap().contains("already saved"));
}

#[test]
fn settings_page_sets_language_and_extra_languages() {
    let mut e = Env::new("settings");
    e.call(|p, cx| p.on_action(cx, &format!("settings:{SETTINGS_PAGE}")));
    let (title, items) = e.menu();
    assert_eq!(title, "Wikipedia");
    assert_eq!(items[0], ("Language\ten".to_string(), "lang".to_string()));
    assert_eq!(items[1], ("Extra languages\tnone".to_string(), "extra".to_string()));
    assert_eq!(items.len(), 2);
    // Language: a keyboard prompt with the current value, then the saved code.
    e.answer(5, "lang");
    assert!(matches!(e.ui.0.last(), Some(UiRequest::TextInput { id: 6, initial, .. }) if initial == "en"));
    e.answer(6, " Da ");
    assert_eq!(e.st.settings.get("language"), Some(&json!("da")));
    assert_eq!(e.menu().1[0].0, "Language\tda");
    // Invalid input is refused with a toast and nothing is stored.
    e.answer(6, "en.evil.com");
    assert_eq!(e.st.settings.get("language"), Some(&json!("da")));
    assert!(e.toasts().last().unwrap().contains("not a language code"));
    // Extra languages.
    e.answer(5, "extra");
    e.answer(7, "de, fr da de");
    assert_eq!(e.st.settings.get("languages"), Some(&json!(["de", "fr", "da"])));
    let (_, items) = e.menu();
    assert_eq!(items[1].0, "Extra languages\tde, fr, da");
    assert_eq!(labels(&items).last(), Some(&"Remove extra languages"));
    e.answer(7, "de, x");
    assert_eq!(e.st.settings.get("languages"), Some(&json!(["de", "fr", "da"])), "one bad code: nothing changes");
    e.answer(5, "clear");
    assert_eq!(e.st.settings.get("languages"), Some(&json!([])));
    // A lookup now uses da only (the extra "da" is the primary anyway).
    e.select("Bounty");
    assert!(e.last_req().1.starts_with("https://da.wikipedia.org/"));
}

#[test]
fn typed_search_from_the_reader_menu() {
    let mut e = Env::new("typed");
    e.call(|p, cx| p.on_action(cx, ACTION_TYPE));
    assert!(matches!(e.ui.0.last(), Some(UiRequest::TextInput { id: 4, .. })));
    e.answer(4, "  Mutiny on the Bounty ");
    assert_eq!(e.last_req().1, "https://en.wikipedia.org/api/rest_v1/page/summary/Mutiny_on_the_Bounty");
}
