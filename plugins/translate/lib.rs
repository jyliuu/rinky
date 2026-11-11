//! Translate plugin: translates the selected text with Google Translate, like KOReader's
//! translator.
//!
//! - Selection action **Translate** ([`ACTION`]): a toast while the request runs, then a
//!   card in the dictionary's card style: the selected text as the title, `_German to
//!   English_` as the subtitle, the translation, for a single word its dictionary
//!   (`noun: house, home`) or other translations, and the footer `— Google Translate`.
//!   Buttons Close and **Full view** (the Markdown viewer with the whole text, the
//!   dictionary, other translations and the source word's definitions).
//! - Text already in the target language gets a card that says so and where to change
//!   the target.
//! - The request runs on the plugin network worker ([`HostCtx::start_request`]), one at a
//!   time; the selection is cut at [`api::MAX_CHARS`] characters. Failures are a card
//!   with Retry (Wi-Fi off, timeout, Google refusing with HTTP 429), never a plugin error.
//!
//! Settings → Translate ([`SETTINGS_PAGE`]): the language to translate into (`to`,
//! default `en`) and from (`from`: `auto`, `book` for the book's language, or a code).

#![warn(missing_docs)]

pub mod api;

use std::time::Duration;

use api::Translation;
use ink_net::Request;
use ink_plugin::kit::{cut_at, md_escape, Requests};
use ink_plugin::serde_json::json;
use ink_plugin::{HostCtx, HttpEvent, Permission, Plugin, PluginManifest, Registrar, Result, StreamMode};

/// Plugin name.
pub const NAME: &str = "translate";
/// Selection action id (not `lookup`: that one is the dictionary's).
pub const ACTION: &str = "translate";
/// Settings page id.
pub const SETTINGS_PAGE: &str = "translate-settings";
/// Default target language.
pub const DEFAULT_TO: &str = "en";
/// Characters of the selected text in the card title.
pub const TITLE_CHARS: usize = 40;
/// Selections of at most this many words show other translations on the card.
pub const CARD_ALT_WORDS: usize = 3;

const ID_CARD: u32 = 1;
const ID_VIEW: u32 = 2;
const ID_SET: u32 = 3;
const ID_FROM: u32 = 4;
const ID_TO: u32 = 5;
const ID_CODE_FROM: u32 = 6;
const ID_CODE_TO: u32 = 7;

const CARD_BUTTONS: [(&str, &str); 2] = [("Close", "close"), ("Full view", "full")];
const RETRY_BUTTONS: [(&str, &str); 2] = [("Close", "close"), ("Retry", "retry")];
const CLOSE_BUTTONS: [(&str, &str); 1] = [("Close", "close")];

/// A translation asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// The text sent.
    pub text: String,
    /// Whether the selection was cut to [`api::MAX_CHARS`].
    pub cut: bool,
    /// Source code sent (`auto` or a code).
    pub from: String,
    /// Target code.
    pub to: String,
}

/// The Translate plugin.
pub struct TranslatePlugin {
    manifest: PluginManifest,
    /// The request in flight (at most one).
    running: Requests<Job>,
    /// The last job, for Retry.
    job: Option<Job>,
    /// The last answer, for Full view.
    last: Option<(Job, Translation)>,
}

impl Default for TranslatePlugin {
    /// A plugin with nothing loaded.
    fn default() -> Self {
        let mut manifest = PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Document, Permission::Ui, Permission::Storage, Permission::Network]);
        manifest.hosts = vec!["translate.googleapis.com".to_string()];
        TranslatePlugin { manifest, running: Requests::default(), job: None, last: None }
    }
}

/// The message for a failed request. `m` is the text of the network error.
pub fn net_message(m: &str) -> String {
    let l = m.to_lowercase();
    if l.contains("offline") || l.starts_with("http:") || l.contains("unreachable") || l.contains("lookup") || l.contains("resolve") {
        "Turn on Wi-Fi to translate. Wi-Fi is off or not connected.".to_string()
    } else if l.contains("timed out") {
        "Google Translate did not answer in time. Check that Wi-Fi is connected, then retry.".to_string()
    } else if l.starts_with("tls") || l.contains("certificate") {
        "The secure connection failed. Check the date and time of the Kindle.".to_string()
    } else {
        format!("Could not translate: {m}")
    }
}

/// The message for an HTTP status other than 200.
pub fn status_message(status: u16) -> String {
    match status {
        429 => "Google Translate refused the request: too many requests from this network. Try again later.".to_string(),
        st => format!("Google Translate answered HTTP {st}. Try again later."),
    }
}

/// `German to English`: the source (the detected one, else the one asked for) and target.
pub fn direction(t: &Translation, from: &str, to: &str) -> String {
    let src = t.source_lang.as_deref().filter(|_| from == "auto").unwrap_or(from);
    let src = if src == "auto" { "Detected language".to_string() } else { api::name(src) };
    format!("{src} to {}", api::name(to))
}

/// Whether the text was already in the target language (nothing was translated).
pub fn untranslated(t: &Translation, to: &str) -> bool {
    t.source_lang.as_deref().is_some_and(|s| api::same_lang(s, to))
}

/// The card: title, body in the host's card convention.
pub fn card(source: &str, t: &Translation, from: &str, to: &str) -> (String, String) {
    let mut lines = vec![format!("_{}_", direction(t, from, to))];
    if untranslated(t, to) {
        lines.push(format!("The text is already in {}. Choose the language to translate into under Settings › Plugins › Translate.", api::name(to)));
    } else {
        lines.push(t.text.clone());
        if !t.dictionary.is_empty() {
            for (pos, terms) in t.dictionary.iter().take(3) {
                let terms = terms.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
                lines.push(if pos.is_empty() { terms } else { format!("{pos}: {terms}") });
            }
        } else if !t.alternatives.is_empty() && source.split_whitespace().count() <= CARD_ALT_WORDS {
            // A sentence's alternative is a near copy of it; those stay in the full view.
            lines.push(format!("Also: {}", t.alternatives.iter().take(4).cloned().collect::<Vec<_>>().join(", ")));
        }
    }
    lines.push("— Google Translate".to_string());
    (cut_at(source, TITLE_CHARS), lines.join("\n"))
}

/// The full view: the source, the translation and everything else of the answer.
pub fn full_markdown(job: &Job, t: &Translation) -> String {
    let src_name = api::name(t.source_lang.as_deref().filter(|_| job.from == "auto").unwrap_or(&job.from));
    let mut md = format!(":::top\n\n## {}\n\n{}\n\n", md_escape(&src_name), md_escape(&job.text));
    if job.cut {
        md.push_str(&format!("*Only the first {} characters of the selection were translated.*\n\n", api::MAX_CHARS));
    }
    md.push_str(&format!("## {}\n\n{}\n\n", md_escape(&api::name(&job.to)), md_escape(&t.text)));
    if !t.dictionary.is_empty() {
        md.push_str("## Dictionary\n\n");
        for (pos, terms) in &t.dictionary {
            let terms = md_escape(&terms.join(", "));
            md.push_str(&if pos.is_empty() { format!("{terms}\n\n") } else { format!("**{}**: {terms}\n\n", md_escape(pos)) });
        }
    }
    if !t.alternatives.is_empty() {
        md.push_str(&format!("## Other translations\n\n{}\n\n", md_escape(&t.alternatives.join(", "))));
    }
    if !t.definitions.is_empty() {
        md.push_str(&format!("## Definitions ({})\n\n", md_escape(&src_name)));
        for (pos, glosses) in &t.definitions {
            if !pos.is_empty() {
                md.push_str(&format!("**{}**\n\n", md_escape(pos)));
            }
            for (i, g) in glosses.iter().enumerate() {
                md.push_str(&format!("{}\\. {}\n\n", i + 1, md_escape(g)));
            }
        }
    }
    md.push_str("*Translated by Google Translate*\n");
    md
}

fn request(url: String) -> Request {
    Request::get(url).header("Accept", "application/json").timeout(Duration::from_secs(10)).read_timeout(Duration::from_secs(15)).total_timeout(Some(Duration::from_secs(20)))
}

impl TranslatePlugin {
    // ---- settings ----

    /// The target code.
    fn to(cx: &mut HostCtx) -> String {
        cx.setting_str("to").map(|s| api::normalize_code(&s)).filter(|s| api::valid_code(s)).unwrap_or_else(|| DEFAULT_TO.to_string())
    }

    /// The source setting: `auto`, `book` or a code.
    fn from_setting(cx: &mut HostCtx) -> String {
        match cx.setting_str("from").map(|s| s.trim().to_string()) {
            Some(s) if s == "book" => s,
            Some(s) if api::valid_code(&api::normalize_code(&s)) => api::normalize_code(&s),
            _ => "auto".to_string(),
        }
    }

    /// The source code to send: the book's language for `book` (auto when it has none).
    fn from(cx: &mut HostCtx) -> String {
        match Self::from_setting(cx).as_str() {
            "book" => cx.document().ok().and_then(|d| d.metadata().language).and_then(|l| api::book_code(&l)).unwrap_or_else(|| "auto".into()),
            s => s.to_string(),
        }
    }

    fn from_label(setting: &str) -> String {
        match setting {
            "auto" => "Auto-detect".to_string(),
            "book" => "Book language".to_string(),
            c => format!("{} ({c})", api::name(c)),
        }
    }

    // ---- requests ----

    /// Translates `sel`.
    fn translate(&mut self, cx: &mut HostCtx, sel: &str) -> Result<()> {
        let (text, cut) = api::clean_text(sel);
        if text.is_empty() {
            return cx.toast("Select text to translate");
        }
        let job = Job { text, cut, from: Self::from(cx), to: Self::to(cx) };
        self.start(cx, job)
    }

    /// Starts `job`; a failure to start shows as a card, never as an error.
    fn start(&mut self, cx: &mut HostCtx, job: Job) -> Result<()> {
        self.running.cancel_all(cx);
        self.job = Some(job.clone());
        let url = api::url(&job.from, &job.to, &job.text);
        log::info!("translate: {} chars, {} -> {}", job.text.chars().count(), job.from, job.to);
        // The toast shows that something happens; the card replaces it.
        cx.toast("Translating…")?;
        match self.running.start(cx, request(url), StreamMode::Whole { limit: 1 << 20 }, job) {
            Ok(_) => Ok(()),
            Err(e) => {
                log::warn!("translate: cannot start the request: {e}");
                self.failed(cx, &net_message(&e.to_string()))
            }
        }
    }

    /// A card with the message and Retry.
    fn failed(&mut self, cx: &mut HostCtx, msg: &str) -> Result<()> {
        log::warn!("translate: failed: {msg}");
        let title = self.job.as_ref().map(|j| cut_at(&j.text, TITLE_CHARS)).unwrap_or_else(|| "Translate".into());
        cx.dialog(ID_CARD, &title, &format!("{msg}\n— Google Translate"), &RETRY_BUTTONS)
    }

    fn done(&mut self, cx: &mut HostCtx, job: Job, status: u16, body: &[u8]) -> Result<()> {
        if status != 200 {
            return self.failed(cx, &status_message(status));
        }
        match api::parse(body) {
            Ok(t) => {
                let (title, body) = card(&job.text, &t, &job.from, &job.to);
                let buttons: &[(&str, &str)] = if untranslated(&t, &job.to) { &CLOSE_BUTTONS } else { &CARD_BUTTONS };
                cx.dialog(ID_CARD, &title, &body, buttons)?;
                self.last = Some((job, t));
                Ok(())
            }
            Err(e) => {
                log::warn!("translate: bad answer: {e}");
                self.failed(cx, "Google Translate sent an answer rinky cannot read. Try again later.")
            }
        }
    }

    // ---- settings page ----

    fn show_settings(&mut self, cx: &mut HostCtx) -> Result<()> {
        let to = Self::to(cx);
        let from = Self::from_setting(cx);
        let items = vec![(format!("Translate to\t{} ({to})", api::name(&to)), "to"), (format!("Translate from\t{}", Self::from_label(&from)), "from")];
        cx.menu(ID_SET, "Translate", items)
    }

    /// A language picker: `extra` entries first, then every language, then a typed code.
    fn pick(cx: &mut HostCtx, id: u32, title: &str, extra: &[(&str, &str)]) -> Result<()> {
        let mut items: Vec<(String, String)> = extra.iter().map(|(l, a)| (l.to_string(), a.to_string())).collect();
        items.extend(api::LANGUAGES.iter().map(|(c, n)| (format!("{n}\t{c}"), format!("l:{c}"))));
        items.push(("Other language code…".into(), "other".into()));
        items.push(("< Translate".into(), "back".into()));
        cx.menu(id, title, items)
    }

    /// Saves a picked or typed code for `key` and shows the settings again.
    fn set_code(&mut self, cx: &mut HostCtx, key: &str, text: &str) -> Result<()> {
        let c = api::normalize_code(text);
        let bad = |t: &str| format!("“{}” is not a language code (examples: en, de, zh-CN)", cut_at(t, 20));
        let special = key == "from" && (c == "auto" || c == "book");
        if cx.set_from_input(key, text, |t| if special || api::valid_code(&c) { Ok(json!(c)) } else { Err(bad(t)) })? {
            log::info!("translate: {key} = {c}");
        }
        self.show_settings(cx)
    }
}

impl Plugin for TranslatePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.selection_action(ACTION, "Translate");
        reg.settings_page(SETTINGS_PAGE, "Translate");
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        match id {
            ACTION => {
                let sel = cx.document().ok().and_then(|d| d.selection());
                match sel {
                    Some(sel) => self.translate(cx, &sel.text),
                    None => cx.toast("Select text to translate"),
                }
            }
            a if a.strip_prefix("settings:") == Some(SETTINGS_PAGE) => self.show_settings(cx),
            _ => Ok(()),
        }
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        match (id, result) {
            (ID_CARD, "full") => match &self.last {
                Some((job, t)) => {
                    let md = full_markdown(job, t);
                    cx.view_markdown(ID_VIEW, "Translation", &md)
                }
                None => Ok(()),
            },
            (ID_CARD, "retry") => match self.job.clone() {
                Some(job) => self.start(cx, job),
                None => Ok(()),
            },
            (ID_CARD, _) => {
                // Closed: a translation still loading is dropped.
                self.running.cancel_all(cx);
                Ok(())
            }
            (ID_SET, "to") => {
                let title = format!("Translate to · now {}", api::name(&Self::to(cx)));
                Self::pick(cx, ID_TO, &title, &[])
            }
            (ID_SET, "from") => {
                let title = format!("Translate from · now {}", Self::from_label(&Self::from_setting(cx)));
                Self::pick(cx, ID_FROM, &title, &[("Auto-detect", "l:auto"), ("Book language (auto-detect if unknown)", "l:book")])
            }
            (ID_TO | ID_FROM, "back") => self.show_settings(cx),
            (ID_TO, "other") => {
                let cur = Self::to(cx);
                cx.input(ID_CODE_TO, "Language code to translate into (en, de, zh-CN)", &cur)
            }
            (ID_FROM, "other") => cx.input(ID_CODE_FROM, "Language code to translate from (en, de, zh-CN)", ""),
            (ID_TO, a) => match a.strip_prefix("l:") {
                Some(c) => self.set_code(cx, "to", c),
                None => Ok(()),
            },
            (ID_FROM, a) => match a.strip_prefix("l:") {
                Some(c) => self.set_code(cx, "from", c),
                None => Ok(()),
            },
            (ID_CODE_TO, text) => self.set_code(cx, "to", text),
            (ID_CODE_FROM, text) => self.set_code(cx, "from", text),
            _ => Ok(()),
        }
    }

    fn on_http(&mut self, cx: &mut HostCtx, id: u32, ev: &HttpEvent) -> Result<()> {
        // Events of a cancelled request find no job and are dropped.
        let Some((run, answer)) = self.running.event(id, ev) else { return Ok(()) };
        match answer {
            Ok(body) => self.done(cx, run.job, run.status, &body),
            Err(m) if m.starts_with("interrupted") => {
                // The device went to sleep; nothing to show on the screen saver.
                log::info!("translate: request interrupted by sleep");
                Ok(())
            }
            Err(m) => self.failed(cx, &net_message(&m)),
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
