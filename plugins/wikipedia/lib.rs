//! Wikipedia plugin: looks up the selected word or phrase on Wikipedia.
//!
//! - Selection action **Wikipedia** ([`ACTION_LOOKUP`]) and reader menu item "Wikipedia"
//!   ([`ACTION_TYPE`], type a search term): the summary of the page
//!   (`/api/rest_v1/page/summary/<title>`, redirects followed) shows as a card in the
//!   dictionary's card style: title, the page description as the muted subtitle, the
//!   first paragraph, the footer `— Wikipedia · en`, buttons Close, **Save** and
//!   **Full view**.
//! - A missing page (HTTP 404) falls back to the search API; its top hits come as a menu
//!   to pick from. No hit: a card says so.
//! - **Full view** opens the plain-text article (`prop=extracts&explaintext=1`) in the
//!   Markdown viewer (it pages by half-page jumps); a loading text shows until the text
//!   arrives. **Save** writes the article as a small HTML book into the books folder
//!   (`save_dir`, default `/mnt/us/Books`; never over an existing file) and asks the
//!   library to rescan.
//! - With extra languages the card is paged like the dictionary card, one page per
//!   language (`— 2/3 · Wikipedia · de`): a swipe answers `next` or `prev`; a language
//!   not looked at yet is fetched then (a toast shows while it loads).
//! - Every request runs on the plugin network worker ([`HostCtx::start_request`]); the UI
//!   never waits. Timeouts: 10 s per phase, 20 s in all (45 s for an article). Failures
//!   show as a card (Wi-Fi off: "Turn on Wi-Fi ..."), never as a plugin error.
//! - The last 50 summaries are kept in `plugins/wikipedia/data/cache.json` and answer
//!   repeated lookups at once, also with Wi-Fi off.
//!
//! Settings → Wikipedia ([`SETTINGS_PAGE`]): the language code and the extra languages
//! (`[plugins.wikipedia] language = "en"`, `languages = ["de", "fr"]`, `save_dir`).

#![warn(missing_docs)]

pub mod api;

use std::path::{Path, PathBuf};
use std::time::Duration;

use api::{Cache, Summary};
use ink_net::Request;
use ink_plugin::serde_json::json;
use ink_plugin::{HostCtx, HttpEvent, Permission, Plugin, PluginManifest, Registrar, Result, StreamMode};

/// Plugin name.
pub const NAME: &str = "wikipedia";
/// Selection action id (not `lookup`: that one is the dictionary's).
pub const ACTION_LOOKUP: &str = "wikipedia";
/// Reader menu item id.
pub const ACTION_TYPE: &str = "wikipedia-search";
/// Settings page id.
pub const SETTINGS_PAGE: &str = "wikipedia-settings";
/// Default language code.
pub const DEFAULT_LANG: &str = "en";
/// Default folder of saved articles.
pub const DEFAULT_SAVE_DIR: &str = "/mnt/us/Books";
/// Characters of the extract on the card (the card keeps eight lines).
pub const CARD_CHARS: usize = 400;

const ID_CARD: u32 = 1;
const ID_VIEW: u32 = 2;
const ID_HITS: u32 = 3;
const ID_INPUT: u32 = 4;
const ID_SET: u32 = 5;
const ID_LANG: u32 = 6;
const ID_EXTRA: u32 = 7;

const CARD_BUTTONS: [(&str, &str); 3] = [("Close", "close"), ("Save", "save"), ("Full view", "full")];
const RETRY_BUTTONS: [(&str, &str); 2] = [("Close", "close"), ("Retry", "retry")];
const MISSING_BUTTONS: [(&str, &str); 1] = [("Close", "close")];

/// What a request is for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Job {
    /// REST summary of `title` in language `lang` (index into the session's languages).
    Summary { lang: usize, title: String },
    /// Search fallback for `query`.
    Search { lang: usize, query: String },
    /// Plain-text article of `sum`.
    Article { lang: usize, sum: Summary, save: bool },
}

/// The request in flight (at most one).
struct Running {
    id: u32,
    job: Job,
    status: u16,
}

/// One language's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Page {
    Found(Summary),
    Missing,
}

/// The lookup the card shows: one page per language.
struct Session {
    query: String,
    langs: Vec<String>,
    pages: Vec<Option<Page>>,
    shown: usize,
    /// Titles of the hit menu on screen, and its language.
    hits: Vec<String>,
    hits_lang: usize,
}

/// The article text of the last Full view (for Save).
struct FullText {
    lang: String,
    title: String,
    text: String,
}

/// The Wikipedia plugin.
pub struct WikipediaPlugin {
    manifest: PluginManifest,
    cache: Option<Cache>,
    cache_path: Option<PathBuf>,
    session: Option<Session>,
    running: Option<Running>,
    view_open: bool,
    full: Option<FullText>,
}

impl Default for WikipediaPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// The footer of a card: `Wikipedia · en`, or `2/3 · Wikipedia · de` with several pages.
pub fn footer(lang: &str, page: usize, pages: usize) -> String {
    if pages >= 2 {
        format!("{page}/{pages} · Wikipedia · {lang}")
    } else {
        format!("Wikipedia · {lang}")
    }
}

/// The card of a summary in the host's card convention: title, `_description_`, the first
/// paragraph cut at [`CARD_CHARS`], the footer. `page` is 1-based.
pub fn summary_card(s: &Summary, lang: &str, page: usize, pages: usize) -> (String, String) {
    let mut lines: Vec<String> = Vec::new();
    if let Some(d) = &s.description {
        lines.push(format!("_{}_", d.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    let extract = s.extract.split_whitespace().collect::<Vec<_>>().join(" ");
    lines.push(if extract.is_empty() { "No summary for this page. Open the full view.".to_string() } else { api::cut_at(&extract, CARD_CHARS) });
    lines.push(format!("— {}", footer(lang, page, pages)));
    (s.title.clone(), lines.join("\n"))
}

/// The card of a language without an article.
pub fn missing_card(query: &str, lang: &str, page: usize, pages: usize) -> (String, String) {
    (query.to_string(), format!("No article found for “{query}” on {}.\n— {}", api::host(lang), footer(lang, page, pages)))
}

/// The message for a failed request. `m` is the text of the network error.
pub fn net_message(m: &str) -> String {
    let l = m.to_lowercase();
    if l.starts_with("offline") || l.starts_with("http:") || l.contains("unreachable") || l.contains("lookup") || l.contains("resolve") {
        "Turn on Wi-Fi to look this up on Wikipedia. Wi-Fi is off or not connected.".to_string()
    } else if l.contains("timed out") {
        "Wikipedia did not answer in time. Check that Wi-Fi is connected, then retry.".to_string()
    } else if l.starts_with("tls") || l.contains("certificate") {
        "The secure connection failed. Check the date and time of the Kindle.".to_string()
    } else if l.contains("larger than") {
        "The answer is too large to load.".to_string()
    } else {
        format!("Could not load Wikipedia: {m}")
    }
}

fn request(url: String, total: u64) -> Request {
    Request::get(url).header("Accept", "application/json").timeout(Duration::from_secs(10)).read_timeout(Duration::from_secs(15)).total_timeout(Some(Duration::from_secs(total)))
}

impl WikipediaPlugin {
    /// A plugin with nothing loaded.
    pub fn new() -> Self {
        let mut manifest = PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Document, Permission::Ui, Permission::Storage, Permission::Network]);
        manifest.hosts = vec!["wikipedia.org".to_string()];
        WikipediaPlugin { manifest, cache: None, cache_path: None, session: None, running: None, view_open: false, full: None }
    }

    // ---- settings ----

    fn language(cx: &mut HostCtx) -> String {
        let v = cx.storage().ok().and_then(|s| s.get("language"));
        v.as_ref().and_then(|v| v.as_str()).map(|s| s.trim().to_lowercase()).filter(|s| api::valid_lang(s)).unwrap_or_else(|| DEFAULT_LANG.to_string())
    }

    fn extra_languages(cx: &mut HostCtx) -> Vec<String> {
        let v = cx.storage().ok().and_then(|s| s.get("languages"));
        let raw: Vec<String> = match v {
            Some(ink_plugin::serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
            Some(ink_plugin::serde_json::Value::String(s)) => s.split([',', ' ']).map(str::to_string).collect(),
            _ => Vec::new(),
        };
        let mut out: Vec<String> = Vec::new();
        for l in raw {
            let l = l.trim().to_lowercase();
            if api::valid_lang(&l) && !out.contains(&l) {
                out.push(l);
            }
        }
        out
    }

    /// The language and the extra ones, in swipe order, without repeats.
    fn languages(cx: &mut HostCtx) -> Vec<String> {
        let first = Self::language(cx);
        let mut all = vec![first.clone()];
        all.extend(Self::extra_languages(cx).into_iter().filter(|l| *l != first));
        all
    }

    // ---- cache ----

    fn cache(&mut self, cx: &mut HostCtx) -> &mut Cache {
        if self.cache.is_none() {
            let path = cx.data_file("cache.json").ok();
            self.cache = Some(path.as_deref().map(Cache::load).unwrap_or_default());
            self.cache_path = path;
        }
        self.cache.as_mut().expect("set above")
    }

    fn cache_put(&mut self, cx: &mut HostCtx, lang: &str, query: &str, s: &Summary) {
        self.cache(cx).put(lang, query, s);
        if let (Some(c), Some(p)) = (&self.cache, &self.cache_path) {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = c.save(p) {
                log::warn!("wikipedia: cannot write {}: {e}", p.display());
            }
        }
    }

    // ---- requests ----

    fn cancel(&mut self, cx: &mut HostCtx) {
        if let Some(r) = self.running.take() {
            log::debug!("wikipedia: cancel request {}", r.id);
            cx.cancel_request(r.id);
        }
    }

    /// Starts `job`; a failure to start shows as a card or toast, never as an error.
    fn start(&mut self, cx: &mut HostCtx, job: Job) -> Result<()> {
        self.cancel(cx);
        let Some(s) = self.session.as_ref() else { return Ok(()) };
        let (url, total, limit) = match &job {
            Job::Summary { lang, title } => (api::summary_url(&s.langs[*lang], title), 20, 1 << 20),
            Job::Search { lang, query } => (api::search_url(&s.langs[*lang], query), 20, 1 << 20),
            Job::Article { lang, sum, .. } => (api::article_url(&s.langs[*lang], &sum.title), 45, 3 << 20),
        };
        log::info!("wikipedia: GET {url}");
        match cx.start_request(request(url, total), StreamMode::Whole { limit }) {
            Ok(id) => {
                self.running = Some(Running { id, job, status: 0 });
                Ok(())
            }
            Err(e) => {
                log::warn!("wikipedia: cannot start the request: {e}");
                self.failed(cx, &job, &format!("{e}"))
            }
        }
    }

    /// A lookup for `text`: a fresh session over the configured languages.
    fn lookup(&mut self, cx: &mut HostCtx, text: &str) -> Result<()> {
        let query = api::clean_query(text);
        if query.is_empty() {
            cx.ui()?.toast("Select a word to look up");
            return Ok(());
        }
        let langs = Self::languages(cx);
        log::info!("wikipedia: lookup {query:?} in {langs:?}");
        self.cancel(cx);
        self.session = Some(Session { query, pages: vec![None; langs.len()], langs, shown: 0, hits: Vec::new(), hits_lang: 0 });
        self.fetch_page(cx, 0)
    }

    /// Shows language `i`'s page: from the cache, from memory, or after a request.
    fn fetch_page(&mut self, cx: &mut HostCtx, i: usize) -> Result<()> {
        let Some(s) = self.session.as_mut() else { return Ok(()) };
        s.shown = i;
        if s.pages[i].is_some() {
            return self.show_card(cx);
        }
        let (lang, query) = (s.langs[i].clone(), s.query.clone());
        if let Some(sum) = self.cache(cx).get(&lang, &query) {
            log::info!("wikipedia: {query:?} in {lang} from the cache");
            if let Some(s) = self.session.as_mut() {
                s.pages[i] = Some(Page::Found(sum));
            }
            return self.show_card(cx);
        }
        // A toast: the next card replaces it, and it shows that something happens.
        cx.ui()?.toast(&format!("Looking up “{}” on {}…", api::cut_at(&query, 30), api::host(&lang)));
        self.start(cx, Job::Summary { lang: i, title: query })
    }

    /// Shows the card of the shown language.
    fn show_card(&mut self, cx: &mut HostCtx) -> Result<()> {
        let Some(s) = self.session.as_ref() else { return Ok(()) };
        let (page, pages, lang) = (s.shown + 1, s.langs.len(), &s.langs[s.shown]);
        match &s.pages[s.shown] {
            Some(Page::Found(sum)) => {
                let (title, body) = summary_card(sum, lang, page, pages);
                cx.ui()?.dialog(ID_CARD, &title, &body, &CARD_BUTTONS);
            }
            _ => {
                let (title, body) = missing_card(&s.query, lang, page, pages);
                cx.ui()?.dialog(ID_CARD, &title, &body, &MISSING_BUTTONS);
            }
        }
        Ok(())
    }

    /// A failed request: a card with Retry for lookups, a message in the viewer or a toast
    /// for articles.
    fn failed(&mut self, cx: &mut HostCtx, job: &Job, raw: &str) -> Result<()> {
        let msg = net_message(raw);
        log::warn!("wikipedia: request failed: {raw} -> {msg:?}");
        match job {
            Job::Article { save: false, .. } if self.view_open => {
                cx.ui()?.view_markdown(ID_VIEW, "Wikipedia", &format!(":::top\n\n{}\n", api::md_escape(&msg)));
            }
            Job::Article { .. } => cx.ui()?.toast(&msg),
            _ => {
                let Some(s) = self.session.as_ref() else { return Ok(()) };
                let (page, pages, lang) = (s.shown + 1, s.langs.len(), &s.langs[s.shown]);
                let body = format!("{msg}\n— {}", footer(lang, page, pages));
                cx.ui()?.dialog(ID_CARD, &s.query, &body, &RETRY_BUTTONS);
            }
        }
        Ok(())
    }

    fn turn(&mut self, cx: &mut HostCtx, step: isize) -> Result<()> {
        let Some(s) = self.session.as_ref() else { return Ok(()) };
        let n = s.langs.len();
        if n < 2 {
            return Ok(());
        }
        let to = (s.shown as isize + step).rem_euclid(n as isize) as usize;
        log::info!("wikipedia: card page {} -> {} of {n}", s.shown + 1, to + 1);
        self.fetch_page(cx, to)
    }

    fn pick_hit(&mut self, cx: &mut HostCtx, n: usize) -> Result<()> {
        let Some(s) = self.session.as_mut() else { return Ok(()) };
        let Some(title) = s.hits.get(n).cloned() else { return Ok(()) };
        let (i, lang) = (s.hits_lang, s.langs[s.hits_lang].clone());
        s.shown = i;
        if let Some(sum) = self.cache(cx).get(&lang, &title) {
            if let Some(s) = self.session.as_mut() {
                s.pages[i] = Some(Page::Found(sum));
            }
            return self.show_card(cx);
        }
        cx.ui()?.toast(&format!("Loading “{}”…", api::cut_at(&title, 30)));
        self.start(cx, Job::Summary { lang: i, title })
    }

    // ---- answers ----

    fn done(&mut self, cx: &mut HostCtx, run: Running, body: &[u8]) -> Result<()> {
        match run.job.clone() {
            Job::Summary { lang, title } => match run.status {
                200 => match api::parse_summary(body) {
                    Ok(sum) => {
                        let Some(s) = self.session.as_mut() else { return Ok(()) };
                        let l = s.langs[lang].clone();
                        s.pages[lang] = Some(Page::Found(sum.clone()));
                        s.shown = lang;
                        self.cache_put(cx, &l, &title, &sum);
                        self.show_card(cx)
                    }
                    Err(e) => self.failed(cx, &run.job, &format!("bad answer: {e}")),
                },
                404 => {
                    log::info!("wikipedia: no page {title:?}, searching");
                    self.start(cx, Job::Search { lang, query: title })
                }
                st => self.failed(cx, &run.job, &format!("Wikipedia answered HTTP {st}")),
            },
            Job::Search { lang, query } => {
                if run.status != 200 {
                    return self.failed(cx, &run.job, &format!("Wikipedia answered HTTP {}", run.status));
                }
                // No hit but a spelling suggestion (`mutinyy` -> `mutiny`): offer it as the hit.
                let parsed = api::parse_search(body).map(|h| if h.is_empty() { api::parse_suggestion(body).map(|s| vec![(s, String::new())]).unwrap_or_default() } else { h });
                match parsed {
                    Ok(hits) if !hits.is_empty() => {
                        let Some(s) = self.session.as_mut() else { return Ok(()) };
                        s.hits = hits.iter().map(|(t, _)| t.clone()).collect();
                        s.hits_lang = lang;
                        let items: Vec<(String, String)> = hits.iter().enumerate().map(|(i, (t, _))| (t.clone(), format!("h{i}"))).collect();
                        let refs: Vec<(&str, &str)> = items.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                        let title = format!("No page “{}”. Did you mean", api::cut_at(&query, 24));
                        cx.ui()?.menu(ID_HITS, &title, &refs);
                        Ok(())
                    }
                    Ok(_) => {
                        let Some(s) = self.session.as_mut() else { return Ok(()) };
                        s.pages[lang] = Some(Page::Missing);
                        s.shown = lang;
                        self.show_card(cx)
                    }
                    Err(e) => self.failed(cx, &run.job, &format!("bad answer: {e}")),
                }
            }
            Job::Article { lang, sum, save } => {
                if run.status != 200 {
                    return self.failed(cx, &run.job, &format!("Wikipedia answered HTTP {}", run.status));
                }
                let l = self.session.as_ref().map(|s| s.langs[lang].clone()).unwrap_or_else(|| DEFAULT_LANG.into());
                let text = match api::parse_article(body) {
                    Ok(Some((_, t))) if !t.is_empty() => t,
                    Ok(_) => return self.failed(cx, &run.job, "The article has no text."),
                    Err(e) => return self.failed(cx, &run.job, &format!("bad answer: {e}")),
                };
                let url = sum.url.clone().unwrap_or_else(|| format!("https://{}/wiki/{}", api::host(&l), api::title_segment(&sum.title)));
                if save {
                    self.save_article(cx, &l, &sum, &text, &url)?;
                } else if self.view_open {
                    cx.ui()?.view_markdown(ID_VIEW, &sum.title, &api::article_markdown(&text, Some(&url)));
                }
                self.full = Some(FullText { lang: l, title: sum.title, text, });
                Ok(())
            }
        }
    }

    fn save_article(&mut self, cx: &mut HostCtx, lang: &str, sum: &Summary, text: &str, url: &str) -> Result<()> {
        let dir = cx.storage().ok().and_then(|s| s.get("save_dir")).and_then(|v| v.as_str().map(PathBuf::from)).unwrap_or_else(|| PathBuf::from(DEFAULT_SAVE_DIR));
        let name = api::file_name(&sum.title, lang);
        let html = api::article_html(lang, &sum.title, sum.description.as_deref(), text, url);
        match write_new(&dir, &name, html.as_bytes()) {
            Ok(true) => {
                log::info!("wikipedia: saved {}", dir.join(&name).display());
                cx.ui()?.toast(&format!("Saved “{}” to the library", sum.title));
                cx.ui()?.rescan_library();
            }
            Ok(false) => cx.ui()?.toast(&format!("“{}” is already saved", sum.title)),
            Err(e) => {
                log::warn!("wikipedia: cannot save {}: {e}", dir.join(&name).display());
                cx.ui()?.toast(&format!("Cannot save the article: {e}"));
            }
        }
        Ok(())
    }

    /// The summary the card shows and its language index.
    fn shown_summary(&self) -> Option<(usize, Summary)> {
        let s = self.session.as_ref()?;
        match &s.pages[s.shown] {
            Some(Page::Found(sum)) => Some((s.shown, sum.clone())),
            _ => None,
        }
    }

    fn full_view(&mut self, cx: &mut HostCtx) -> Result<()> {
        let Some((lang, sum)) = self.shown_summary() else { return Ok(()) };
        self.view_open = true;
        cx.ui()?.view_markdown(ID_VIEW, &sum.title, "*Loading the article…*");
        self.start(cx, Job::Article { lang, sum, save: false })
    }

    fn save(&mut self, cx: &mut HostCtx) -> Result<()> {
        let Some((lang, sum)) = self.shown_summary() else { return Ok(()) };
        let l = self.session.as_ref().map(|s| s.langs[lang].clone()).unwrap_or_default();
        if let Some(f) = self.full.as_ref().filter(|f| f.lang == l && f.title == sum.title) {
            let (text, url) = (f.text.clone(), sum.url.clone().unwrap_or_else(|| format!("https://{}/wiki/{}", api::host(&l), api::title_segment(&sum.title))));
            return self.save_article(cx, &l, &sum, &text, &url);
        }
        cx.ui()?.toast(&format!("Saving “{}”…", api::cut_at(&sum.title, 30)));
        self.start(cx, Job::Article { lang, sum, save: true })
    }

    // ---- settings page ----

    fn show_settings(&mut self, cx: &mut HostCtx) -> Result<()> {
        let lang = Self::language(cx);
        let extra = Self::extra_languages(cx);
        let extra_text = if extra.is_empty() { "none".to_string() } else { extra.join(", ") };
        let mut items = vec![(format!("Language\t{lang}"), "lang"), (format!("Extra languages\t{extra_text}"), "extra")];
        if !extra.is_empty() {
            items.push(("Remove extra languages".to_string(), "clear"));
        }
        let refs: Vec<(&str, &str)> = items.iter().map(|(a, b)| (a.as_str(), *b)).collect();
        cx.ui()?.menu(ID_SET, "Wikipedia", &refs);
        Ok(())
    }

    fn set_language(&mut self, cx: &mut HostCtx, text: &str) -> Result<()> {
        let l = text.trim().to_lowercase();
        if api::valid_lang(&l) {
            cx.storage()?.set("language", json!(l))?;
            log::info!("wikipedia: language {l}");
        } else {
            cx.ui()?.toast(&format!("“{}” is not a language code (examples: en, de, da)", api::cut_at(text.trim(), 20)));
        }
        self.show_settings(cx)
    }

    fn set_extra(&mut self, cx: &mut HostCtx, text: &str) -> Result<()> {
        let mut out: Vec<String> = Vec::new();
        let mut bad: Vec<&str> = Vec::new();
        for w in text.split([',', ' ', ';']).map(str::trim).filter(|w| !w.is_empty()) {
            let l = w.to_lowercase();
            if !api::valid_lang(&l) {
                bad.push(w);
            } else if !out.contains(&l) {
                out.push(l);
            }
        }
        if bad.is_empty() {
            cx.storage()?.set("languages", json!(out))?;
            log::info!("wikipedia: extra languages {out:?}");
        } else {
            cx.ui()?.toast(&format!("Not a language code: {}", api::cut_at(&bad.join(", "), 30)));
        }
        self.show_settings(cx)
    }
}

/// Writes `data` to `<dir>/<name>` through `<name>.part`; `Ok(false)` when the file exists.
fn write_new(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<bool> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    if path.exists() {
        return Ok(false);
    }
    let part = dir.join(format!("{name}.part"));
    std::fs::write(&part, data)?;
    std::fs::rename(&part, &path)?;
    Ok(true)
}

impl Plugin for WikipediaPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.selection_action(ACTION_LOOKUP, "Wikipedia");
        reg.menu_item(ACTION_TYPE, "Wikipedia", 41);
        reg.settings_page(SETTINGS_PAGE, "Wikipedia");
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        match id {
            ACTION_LOOKUP => {
                let sel = cx.document().ok().and_then(|d| d.selection());
                match sel {
                    Some(sel) => self.lookup(cx, &sel.text),
                    None => {
                        cx.ui()?.toast("Select a word to look up");
                        Ok(())
                    }
                }
            }
            ACTION_TYPE => {
                cx.ui()?.input(ID_INPUT, "Search Wikipedia", "");
                Ok(())
            }
            a if a.strip_prefix("settings:") == Some(SETTINGS_PAGE) => self.show_settings(cx),
            _ => Ok(()),
        }
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        match (id, result) {
            (ID_CARD, "next") => self.turn(cx, 1),
            (ID_CARD, "prev") => self.turn(cx, -1),
            (ID_CARD, "full") => self.full_view(cx),
            (ID_CARD, "save") => self.save(cx),
            (ID_CARD, "retry") => {
                let i = self.session.as_ref().map(|s| s.shown).unwrap_or(0);
                self.fetch_page(cx, i)
            }
            (ID_CARD, _) => {
                // Closed: a lookup still loading is dropped (an article keeps going).
                if self.running.as_ref().is_some_and(|r| !matches!(r.job, Job::Article { .. })) {
                    self.cancel(cx);
                }
                Ok(())
            }
            (ID_VIEW, "closed") => {
                self.view_open = false;
                if self.running.as_ref().is_some_and(|r| matches!(r.job, Job::Article { save: false, .. })) {
                    self.cancel(cx);
                }
                Ok(())
            }
            (ID_HITS, a) => match a.strip_prefix('h').and_then(|n| n.parse::<usize>().ok()) {
                Some(n) => self.pick_hit(cx, n),
                None => Ok(()),
            },
            (ID_INPUT, text) if !text.trim().is_empty() => self.lookup(cx, text),
            (ID_SET, "lang") => {
                let cur = Self::language(cx);
                cx.ui()?.input(ID_LANG, "Wikipedia language code (en, de, da, fr)", &cur);
                Ok(())
            }
            (ID_SET, "extra") => {
                let cur = Self::extra_languages(cx).join(" ");
                cx.ui()?.input(ID_EXTRA, "Extra languages to swipe between (de fr)", &cur);
                Ok(())
            }
            (ID_SET, "clear") => {
                cx.storage()?.set("languages", json!([]))?;
                self.show_settings(cx)
            }
            (ID_LANG, text) if !text.trim().is_empty() => self.set_language(cx, text),
            (ID_EXTRA, text) if !text.trim().is_empty() => self.set_extra(cx, text),
            (ID_LANG | ID_EXTRA, _) => self.show_settings(cx),
            _ => Ok(()),
        }
    }

    fn on_http(&mut self, cx: &mut HostCtx, id: u32, ev: &HttpEvent) -> Result<()> {
        if self.running.as_ref().is_none_or(|r| r.id != id) {
            log::debug!("wikipedia: event of an old request {id} ignored");
            return Ok(());
        }
        match ev {
            HttpEvent::Response { status, .. } => {
                if let Some(r) = self.running.as_mut() {
                    r.status = *status;
                }
                Ok(())
            }
            HttpEvent::Done { body } => {
                let Some(run) = self.running.take() else { return Ok(()) };
                self.done(cx, run, body.as_deref().unwrap_or_default())
            }
            HttpEvent::Error(m) => {
                let Some(run) = self.running.take() else { return Ok(()) };
                if m.starts_with("interrupted") {
                    // The device went to sleep; nothing to show on the screen saver.
                    log::info!("wikipedia: request interrupted by sleep");
                    return Ok(());
                }
                self.failed(cx, &run.job, m)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
