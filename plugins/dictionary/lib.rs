//! Dictionary plugin: looks up the selected word in StarDict dictionaries.
//!
//! - Selection action **Look up**: the reader's selection is cleaned and looked up
//!   ([`ink_dict::Library::lookup`]: exact, lowercase, possessive, English inflections).
//!   A card shows the headword, pronunciation and part of speech, the first sense and the
//!   dictionary, with a close button, **Add to vocabulary** and **Full view** (the card
//!   body convention of the app's plugin host, see [`popup_body`]); **Full view** opens
//!   the Markdown viewer with one dictionary at a time and a row of dictionary tabs
//!   (links `dict:N`) when there are several.
//! - With hits in several dictionaries the card is paged, one page per dictionary
//!   ([`popup_page`], footer `— 2/3 · Dictionary`): the host reports a swipe on the card
//!   as the answer `next` or `prev` and the plugin sends the other dictionary's page as a
//!   new card with the same id (wrapping around). Full view opens on the page shown.
//! - **Add to vocabulary** writes the word to the Vocabulary screen's word list
//!   (`ink_stats` table `vocabulary`). Lookups themselves are always logged (table
//!   `lookup`); setting `auto_add = true` also adds every found word to the list.
//! - Action [`ACTION_TYPE`]: type a word on the keyboard and look it up; reader menu
//!   item "Dictionary".
//! - Every lookup made from a book is recorded (word, found form, book MD5 and title,
//!   time, text around it) in `ink_store::vocab` (`<root>/vocabulary.sqlite3`) on a
//!   worker thread, for the vocabulary builder.
//!
//! Dictionaries: `<root>/dict/<name>/*.ifo` (+ `.idx`/`.idx.gz`, `.dict`/`.dict.dz`,
//! `.syn`), where `<root>` is the rinky home (`/mnt/us/rinky` on the Kindle,
//! `$TMPDIR/rinky-sim` in the simulator). Plugin settings (`[plugins.dictionary]` in
//! `settings.toml` or `plugins/dictionary/settings.json`):
//!
//! | Key | Default | Meaning |
//! | --- | --- | --- |
//! | `dir` | `<root>/dict` | dictionary folder |
//! | `vocab_db` | `<root>/vocabulary.sqlite3` | lookup database |
//! | `record` | `true` | record lookups |
//! | `auto_add` | `false` | add every found word to the vocabulary list |
//! | `order` | folder order | dictionary names (`.ifo` file stems) in lookup order |
//! | `disabled` | none | dictionary names left out of lookups |
//!
//! Settings → Dictionaries ([`SETTINGS_PAGE`]) lists the dictionaries in lookup order;
//! a dictionary's menu turns it on or off and moves it up, down or to the top.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use ink_dict::{Dictionary, Hit, Library, LookupResult};
use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Registrar, Result};
use ink_store::vocab::{Lookup, VocabDb};

/// Plugin name.
pub const NAME: &str = "dictionary";
/// Selection action id.
pub const ACTION_LOOKUP: &str = "lookup";
/// Reader menu item id.
pub const ACTION_TYPE: &str = "lookup-typed";
/// Characters of the definition shown in the popup.
pub const POPUP_CHARS: usize = 200;
/// Characters of one sense line in the popup (parsed entries).
pub const SENSE_CHARS: usize = 160;
/// Sense lines in the popup (parsed entries).
pub const POPUP_SENSES: usize = 3;

const ID_POPUP: u32 = 1;
const ID_VIEW: u32 = 2;
const ID_INPUT: u32 = 3;
const ID_DICTS: u32 = 4;
const ID_DICT: u32 = 5;

/// Settings page id (Settings → Dictionaries): turn dictionaries on and off, order them.
pub const SETTINGS_PAGE: &str = "dictionaries";
/// Storage key: dictionary names ([`ink_dict::Dictionary::name`]) in lookup order.
pub const KEY_ORDER: &str = "order";
/// Storage key: names of the dictionaries turned off.
pub const KEY_OFF: &str = "disabled";

/// The book a lookup was made in.
#[derive(Clone)]
struct BookRef {
    md5: Option<String>,
    title: Option<String>,
    context: Option<String>,
}

/// The dictionary plugin.
pub struct DictionaryPlugin {
    manifest: PluginManifest,
    /// The dictionaries turned on, in lookup order.
    lib: Option<Library>,
    /// The dictionaries turned off (kept open: turning one on reads no file).
    off: Vec<Dictionary>,
    /// Every dictionary in the user's order: (name, title, on).
    order: Vec<(String, String, bool)>,
    /// The dictionary whose menu is open (Settings → Dictionaries → one dictionary).
    editing: Option<String>,
    /// The last popup: its lookup (for "Full view" and its tabs) and its vocabulary
    /// entry (for "Add to vocabulary").
    last: Option<Last>,
    writers: Vec<JoinHandle<()>>,
}

impl Default for DictionaryPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// What the last popup showed.
struct Last {
    result: LookupResult,
    word: ink_stats::Lookup,
    /// The dictionary page the card shows (index into [`dict_names`]).
    shown: usize,
}

/// Buttons of the lookup card.
const POPUP_BUTTONS: [(&str, &str); 3] = [("Close", "close"), ("Add to vocabulary", "add"), ("Full view", "full")];

/// The rinky home for a plugin data folder `<root>/plugins/dictionary/data`.
fn root_of(data_dir: &Path) -> PathBuf {
    if data_dir.ends_with(Path::new("plugins").join(NAME).join("data")) {
        if let Some(r) = data_dir.ancestors().nth(3) {
            return r.to_path_buf();
        }
    }
    PathBuf::from("/mnt/us/rinky")
}

/// Escapes Markdown punctuation so definition text shows as typed.
fn md_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#' | '|' | '-' | '+' | '!') {
            out.push('\\');
        }
        out.push(c);
    }
    // "1. " at the start would become a numbered list.
    if let Some(dot) = out.find(". ") {
        if dot > 0 && out[..dot].chars().all(|c| c.is_ascii_digit()) {
            out.insert(dot, '\\');
        }
    }
    out
}

/// Parts-of-speech words that open an entry or a new sense.
const POS: &[&str] = &[
    "noun", "verb", "adjective", "adverb", "pronoun", "preposition", "conjunction", "interjection", "exclamation", "determiner", "abbreviation",
    "prefix", "suffix", "article", "numeral", "phrase", "n.", "v.", "adj.", "adv.", "prep.", "conj.", "pron.", "interj.",
];

fn is_pos(word: &str) -> bool {
    let w = word.trim_matches(|c: char| c == ',' || c == ';' || c == ':').to_ascii_lowercase();
    POS.contains(&w.as_str())
}

/// The parts of a dictionary entry the popup shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// Pronunciation without its delimiters (`/…/`, `|…|`, `[…]`).
    pub pron: Option<String>,
    /// Part of speech and grammar note (`noun (plural mutinies)`).
    pub pos: Option<String>,
    /// The first sense.
    pub sense: String,
}

/// Splits a pronunciation in `/…/`, `|…|` or `[…]` off `meta`.
fn take_pron(meta: &str) -> (Option<String>, String) {
    for (open, close) in [('/', '/'), ('|', '|'), ('[', ']')] {
        if let Some(a) = meta.find(open) {
            if let Some(len) = meta[a + 1..].find(close) {
                let p = meta[a + 1..a + 1 + len].trim().to_string();
                if !p.is_empty() {
                    let rest = format!("{} {}", &meta[..a], &meta[a + 2 + len..]);
                    return (Some(p), rest.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }
        }
    }
    (None, meta.trim().to_string())
}

/// Parses `hit` into pronunciation, part of speech and first sense. A first line that is
/// the headword (with a short note after it) or starts with a part of speech is the
/// entry's head; the first sense is the next line, without a leading sense number.
pub fn parse_entry(hit: &Hit) -> Entry {
    let mut lines: Vec<&str> = hit.text.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    let mut e = Entry::default();
    if let Some(first) = lines.first().copied() {
        let head = first.len() >= hit.headword.len() && first.is_char_boundary(hit.headword.len()) && first[..hit.headword.len()].eq_ignore_ascii_case(&hit.headword);
        let rest = if head { first[hit.headword.len()..].trim() } else { first };
        let starts_pos = rest.split_whitespace().next().is_some_and(is_pos) || rest.starts_with(['/', '|', '[']);
        // The head line: the headword alone or with a short note, or a short line that
        // starts with a part of speech and has more lines after it.
        if (head && (rest.is_empty() || (rest.chars().count() <= 60 && starts_pos))) || (starts_pos && lines.len() > 1 && rest.chars().count() <= 60) {
            let (pron, pos) = take_pron(rest);
            e.pron = pron;
            e.pos = Some(pos).filter(|p| !p.is_empty());
            lines.remove(0);
        }
    }
    let sense = lines.first().copied().unwrap_or("");
    // "1 a house…", "1. a house…"
    let sense = match sense.split_once(' ') {
        Some((n, rest)) if !n.is_empty() && n.trim_end_matches('.').chars().all(|c| c.is_ascii_digit()) => rest,
        _ => sense,
    };
    e.sense = sense.trim_start_matches("• ").trim().to_string();
    e
}

/// Cuts `text` at [`POPUP_CHARS`] on a word boundary with an ellipsis.
fn cut(text: &str) -> String {
    cut_at(text, POPUP_CHARS)
}

/// Cuts `text` at `max` characters on a word boundary with an ellipsis.
fn cut_at(text: &str, max: usize) -> String {
    let mut body: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        if let Some(sp) = body.rfind(' ').filter(|n| *n > max / 2) {
            body.truncate(sp);
        }
        let keep = body.trim_end_matches([' ', '•', ',', ';', ':']).len();
        body.truncate(keep);
        body.push('…');
    }
    body
}

/// Footer of the "no definition" card: how many dictionaries were searched.
fn searched(n: usize) -> String {
    format!("Searched {n} {}", if n == 1 { "dictionary" } else { "dictionaries" })
}

/// The popup body in the host's card convention: `_pronunciation · part of speech_`
/// (when known), the definition, and `— dictionary` (with `· +N more` for further
/// hits). A parsed entry ([`Hit::entry`]) shows up to [`POPUP_SENSES`] numbered senses
/// of the first part of speech (one sense: its first example in italics), then the first
/// sense of the other parts of speech; other entries show the first sense cut at
/// [`POPUP_CHARS`].
pub fn popup_body(hit: &Hit, more: usize) -> String {
    let tail = if more > 0 { format!("\n— {} · +{more} more", hit.dict) } else { format!("\n— {}", hit.dict) };
    card_text(hit) + &tail
}

/// The card's title and body for dictionary page `tab` (index into [`dict_names`], taken
/// modulo their number) of `r`: with one dictionary, [`popup_body`] of the first hit with
/// `+N more` for the other hits; with several, that dictionary's first hit and the footer
/// `— N/M · Dictionary` (`· +K more` for its other hits), which pages the host's card.
/// `None` without hits.
pub fn popup_page(r: &LookupResult, tab: usize) -> Option<(String, String)> {
    let names = dict_names(r);
    if names.len() < 2 {
        let first = r.hits.first()?;
        return Some((first.headword.clone(), popup_body(first, r.hits.len() - 1)));
    }
    let tab = tab % names.len();
    let mut hits = r.hits.iter().filter(|h| h.dict == names[tab]);
    let first = hits.next()?;
    let more = hits.count();
    let more = if more > 0 { format!(" · +{more} more") } else { String::new() };
    Some((first.headword.clone(), format!("{}\n— {}/{} · {}{more}", card_text(first), tab + 1, names.len(), first.dict)))
}

/// The card lines of one hit without the footer (see [`popup_body`]).
fn card_text(hit: &Hit) -> String {
    if let Some(e) = &hit.entry {
        return entry_popup(e);
    }
    let e = parse_entry(hit);
    let mut out = String::new();
    let meta: Vec<String> = e.pron.iter().map(|p| format!("/{p}/")).chain(e.pos.clone()).collect();
    if !meta.is_empty() {
        out.push_str(&format!("_{}_\n", meta.join(" · ")));
    }
    out.push_str(&cut(&e.sense));
    out
}

/// The popup lines of a parsed entry (see [`popup_body`]).
fn entry_popup(e: &ink_dict::Entry) -> String {
    let mut lines: Vec<String> = Vec::new();
    let first = &e.groups[0];
    let meta: Vec<String> = e.pron.iter().map(|p| format!("/{p}/")).chain(Some(first.pos.clone()).filter(|p| !p.is_empty())).collect();
    if !meta.is_empty() {
        lines.push(format!("_{}_", meta.join(" · ")));
    }
    let main: Vec<&ink_dict::entry::Sense> = first.senses.iter().filter(|s| !s.sub).collect();
    for (i, s) in main.iter().take(POPUP_SENSES).enumerate() {
        let def = cut_at(&s.def, SENSE_CHARS);
        lines.push(if main.len() > 1 { format!("{}. {def}", i + 1) } else { def });
    }
    if main.len() == 1 {
        if let Some(x) = main[0].examples.first() {
            lines.push(format!("_{}_", cut_at(x, SENSE_CHARS)));
        }
    }
    // The other parts of speech, while there is room.
    let room = POPUP_SENSES.saturating_sub(main.len().min(POPUP_SENSES)).max(1);
    let others: Vec<&ink_dict::entry::Group> = e.groups[1..].iter().filter(|g| !g.pos.is_empty() && g.pos != first.pos).collect();
    for g in others.iter().take(room) {
        if let Some(s) = g.senses.iter().find(|s| !s.sub) {
            lines.push(format!("{}: {}", g.pos, cut_at(&s.def, SENSE_CHARS)));
        }
    }
    lines.join("\n")
}

/// The definition saved with a vocabulary word: part of speech and the first sense.
pub fn vocab_definition(hit: &Hit) -> String {
    if let Some((pos, s)) = hit.entry.as_ref().and_then(|e| e.main_senses().next()) {
        return if pos.is_empty() { cut(&s.def) } else { format!("{pos} · {}", cut(&s.def)) };
    }
    let e = parse_entry(hit);
    match e.pos {
        Some(pos) => format!("{pos} · {}", cut(&e.sense)),
        None => cut(&e.sense),
    }
}

/// The dictionaries of `r` in hit order.
pub fn dict_names(r: &LookupResult) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for h in &r.hits {
        if !v.contains(&h.dict) {
            v.push(h.dict.clone());
        }
    }
    v
}

/// The full view of dictionary `tab` (index into [`dict_names`]). With several
/// dictionaries a tab row comes first (after `:::top`, so the viewer shows a new tab
/// from its first page): the shown dictionary in bold, the others as
/// `dict:N` links.
pub fn full_markdown_tab(r: &LookupResult, tab: usize) -> String {
    let names = dict_names(r);
    if names.len() < 2 {
        return full_markdown(r);
    }
    let tab = tab.min(names.len() - 1);
    let row: Vec<String> =
        names.iter().enumerate().map(|(i, n)| if i == tab { format!("**{}**", md_escape(n)) } else { format!("[{}](dict:{i})", md_escape(n)) }).collect();
    let only = LookupResult { hits: r.hits.iter().filter(|h| h.dict == names[tab]).cloned().collect(), ..r.clone() };
    // `:::top`: a tab opens at its first page.
    format!(":::top\n{}\n\n---\n\n{}", row.join("  ·  "), full_markdown(&only))
}

/// The full view: one section per dictionary, entries in order.
pub fn full_markdown(r: &LookupResult) -> String {
    let mut md = String::new();
    let mut dict = "";
    for h in &r.hits {
        if h.dict != dict {
            dict = &h.dict;
            md.push_str(&format!("## {}\n\n", md_escape(dict)));
        }
        if let Some(e) = &h.entry {
            md.push_str(&entry_markdown(&h.headword, e));
            continue;
        }
        md.push_str(&format!("**{}**\n\n", md_escape(&h.headword)));
        for line in h.text.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
            match line.strip_prefix("• ") {
                Some(item) => md.push_str(&format!("- {}\n", md_escape(item))),
                None => md.push_str(&format!("{}\n\n", md_escape(line))),
            }
        }
        md.push('\n');
    }
    if !r.form.is_empty() && !r.form.eq_ignore_ascii_case(&r.query) {
        md.push_str(&format!("*Looked up “{}” as “{}”.*\n", md_escape(&r.query), md_escape(&r.form)));
    }
    md
}

/// Markdown of one parsed entry: the headword and pronunciation, each part of speech in
/// italics with its senses as a numbered list (subsenses and notes nested, examples in
/// italics after the definition), then the sections with bold labels.
pub fn entry_markdown(headword: &str, e: &ink_dict::Entry) -> String {
    let mut md = format!("**{}**", md_escape(headword));
    if let Some(p) = &e.pron {
        md.push_str(&format!("  /{}/", md_escape(p)));
    }
    md.push_str("\n\n");
    let examples = |s: &ink_dict::entry::Sense| -> String {
        let xs: Vec<String> = s.examples.iter().map(|x| format!("*{}*", md_escape(x))).collect();
        if xs.is_empty() {
            String::new()
        } else if s.def.is_empty() {
            xs.join(" · ")
        } else if s.def.ends_with(['.', '!', '?', ':']) {
            format!(" {}", xs.join(" · "))
        } else {
            format!(": {}", xs.join(" · "))
        }
    };
    for g in &e.groups {
        if !g.pos.is_empty() {
            md.push_str(&format!("*{}*\n\n", md_escape(&g.pos)));
        }
        let numbered = g.senses.iter().filter(|s| !s.sub).count() > 1;
        let mut n = 0;
        // Nested lines are indented to the content of their parent item (CommonMark):
        // 3 spaces under `1. `, 4 under `10. `, 2 under `- `.
        let mut parent = 2;
        for s in &g.senses {
            let item = format!("{}{}", md_escape(&s.def), examples(s));
            let indent = if s.sub {
                md.push_str(&format!("{}- {item}\n", " ".repeat(parent)));
                " ".repeat(parent + 2)
            } else {
                n += 1;
                let marker = if numbered { format!("{n}. ") } else { "- ".to_string() };
                md.push_str(&format!("{marker}{item}\n"));
                parent = marker.len();
                " ".repeat(parent)
            };
            for note in &s.notes {
                md.push_str(&format!("{indent}- {}\n", md_escape(note)));
            }
        }
        md.push('\n');
    }
    for sec in &e.sections {
        md.push_str(&format!("**{}**\n\n", md_escape(&sec.label)));
        let list = sec.lines.len() > 1;
        for (term, text) in &sec.lines {
            let line = match (term.is_empty(), text.is_empty()) {
                (true, _) => md_escape(text),
                (false, true) => format!("**{}**", md_escape(term)),
                (false, false) => format!("**{}** {}", md_escape(term), md_escape(text)),
            };
            if list {
                md.push_str(&format!("- {line}\n"));
            } else {
                md.push_str(&format!("{line}\n"));
            }
        }
        md.push('\n');
    }
    md
}

/// The Vocabulary screen's entry for the first hit of `r` (headword, book, context,
/// [`vocab_definition`]); `None` without hits.
fn vocab_word(r: &LookupResult, book: Option<&BookRef>) -> Option<ink_stats::Lookup> {
    let h = r.hits.first()?;
    Some(ink_stats::Lookup {
        word: h.headword.clone(),
        title: book.and_then(|b| b.title.clone()),
        book_md5: book.and_then(|b| b.md5.clone()),
        prev_context: book.and_then(|b| b.context.clone()),
        next_context: None,
        definition: Some(vocab_definition(h)).filter(|d| !d.is_empty()),
    })
}

impl DictionaryPlugin {
    /// A plugin with nothing loaded; dictionaries open on the first lookup.
    pub fn new() -> Self {
        DictionaryPlugin { manifest: PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Document, Permission::Ui, Permission::Storage]), lib: None, off: Vec::new(), order: Vec::new(), editing: None, last: None, writers: Vec::new() }
    }

    /// Waits until every lookup has been written to the database (tests).
    pub fn wait_recorded(&mut self) {
        for h in self.writers.drain(..) {
            let _ = h.join();
        }
    }

    fn setting_path(cx: &mut HostCtx, key: &str, default: &str) -> Result<PathBuf> {
        let st = cx.storage()?;
        let root = root_of(&st.data_dir());
        Ok(match st.get(key).as_ref().and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
            _ => root.join(default),
        })
    }

    fn library(&mut self, cx: &mut HostCtx) -> Result<&Library> {
        let dir = Self::setting_path(cx, "dir", "dict")?;
        if self.lib.as_ref().is_none_or(|l| l.dir != dir || l.dicts.len() + self.off.len() == 0) {
            let lib = Library::open_dir(&dir);
            log::info!("dictionary: {} dictionaries in {}", lib.dicts.len(), dir.display());
            for (p, e) in &lib.errors {
                log::warn!("dictionary: {}: {e}", p.display());
            }
            self.off.clear();
            self.lib = Some(lib);
            self.arrange(cx)?;
        }
        Ok(self.lib.as_ref().expect("set above"))
    }

    /// A list of names from storage.
    fn names(cx: &mut HostCtx, key: &str) -> Result<Vec<String>> {
        let v = cx.storage()?.get(key);
        Ok(v.as_ref().and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect()).unwrap_or_default())
    }

    /// Puts the open dictionaries in the stored order (unknown ones after, in folder
    /// order) and moves the turned-off ones out of the lookup.
    fn arrange(&mut self, cx: &mut HostCtx) -> Result<()> {
        let (order, off) = (Self::names(cx, KEY_ORDER)?, Self::names(cx, KEY_OFF)?);
        let Some(lib) = self.lib.as_mut() else { return Ok(()) };
        let mut all: Vec<Dictionary> = lib.dicts.drain(..).chain(self.off.drain(..)).collect();
        all.sort_by_key(|d| order.iter().position(|n| *n == d.name).unwrap_or(usize::MAX));
        self.order = all.iter().map(|d| (d.name.clone(), d.title().to_string(), !off.contains(&d.name))).collect();
        let (on, rest): (Vec<Dictionary>, Vec<Dictionary>) = all.into_iter().partition(|d| !off.contains(&d.name));
        lib.dicts = on;
        self.off = rest;
        log::info!(
            "dictionary: order {:?}",
            self.order.iter().map(|(n, _, on)| if *on { n.clone() } else { format!("{n} (off)") }).collect::<Vec<_>>()
        );
        Ok(())
    }

    /// Settings → Dictionaries: every dictionary in lookup order, on or off.
    fn show_dicts(&mut self, cx: &mut HostCtx) -> Result<()> {
        let dir = self.library(cx)?.dir.clone();
        if self.order.is_empty() {
            let body = format!("No dictionaries found. Copy StarDict dictionaries (.ifo, .idx, .dict.dz) to {}/<name>/.", dir.display());
            cx.ui()?.dialog(0, "Dictionaries", &body, &[("OK", "ok")]);
            return Ok(());
        }
        let items: Vec<(String, String)> =
            self.order.iter().enumerate().map(|(i, (name, title, on))| (format!("{}. {title}\t{}", i + 1, if *on { "on" } else { "off" }), name.clone())).collect();
        let refs: Vec<(&str, &str)> = items.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        cx.ui()?.menu(ID_DICTS, "Dictionaries", &refs);
        Ok(())
    }

    /// The menu of one dictionary: on/off and its place in the order.
    fn show_dict(&mut self, cx: &mut HostCtx, name: &str) -> Result<()> {
        let Some(i) = self.order.iter().position(|(n, _, _)| n == name) else { return self.show_dicts(cx) };
        let (title, on) = (self.order[i].1.clone(), self.order[i].2);
        let mut items = vec![(if on { "Turn off" } else { "Turn on" }, "toggle")];
        if i > 0 {
            items.push(("Move up", "up"));
        }
        if i + 1 < self.order.len() {
            items.push(("Move down", "down"));
        }
        if i > 1 {
            items.push(("Move to top", "top"));
        }
        items.push(("< Dictionaries", "back"));
        self.editing = Some(name.to_string());
        let title = format!("{title} · {} of {}", i + 1, self.order.len());
        cx.ui()?.menu(ID_DICT, &title, &items);
        Ok(())
    }

    /// Applies a choice from [`Self::show_dict`], saves the order and shows the list again.
    fn edit_dict(&mut self, cx: &mut HostCtx, name: &str, what: &str) -> Result<()> {
        let Some(i) = self.order.iter().position(|(n, _, _)| n == name) else { return self.show_dicts(cx) };
        match what {
            "toggle" => self.order[i].2 = !self.order[i].2,
            "up" if i > 0 => self.order.swap(i, i - 1),
            "down" if i + 1 < self.order.len() => self.order.swap(i, i + 1),
            "top" => {
                let d = self.order.remove(i);
                self.order.insert(0, d);
            }
            _ => return self.show_dicts(cx),
        }
        let order: Vec<&str> = self.order.iter().map(|(n, _, _)| n.as_str()).collect();
        let off: Vec<&str> = self.order.iter().filter(|(_, _, on)| !on).map(|(n, _, _)| n.as_str()).collect();
        log::info!("dictionary: {what} {name}");
        let st = cx.storage()?;
        st.set(KEY_ORDER, ink_plugin::serde_json::json!(order))?;
        st.set(KEY_OFF, ink_plugin::serde_json::json!(off))?;
        self.arrange(cx)?;
        // A move keeps the dictionary's menu open for the next move.
        if what == "toggle" {
            self.show_dicts(cx)
        } else {
            self.show_dict(cx, name)
        }
    }

    /// Looks `text` up and shows the popup; records it when `from_book`.
    fn lookup(&mut self, cx: &mut HostCtx, text: &str, from_book: Option<BookRef>) -> Result<()> {
        let lib = self.library(cx)?;
        let dir = lib.dir.clone();
        let ndicts = lib.dicts.len();
        let r = lib.lookup(text);
        log::info!("dictionary: lookup {:?} -> form {:?}, {} hits", r.query, r.form, r.hits.len());
        if r.query.is_empty() {
            cx.ui()?.toast("Select a word to look up");
            return Ok(());
        }
        if ndicts == 0 {
            self.last = None;
            // The `— ` footer line makes the host draw its card at the bottom of the
            // screen, clear of the selection card (a plain dialog sits in the middle).
            let body = if self.off.is_empty() {
                format!("No dictionaries found. Copy StarDict dictionaries (.ifo, .idx, .dict.dz) to {}/<name>/.\n— No dictionaries installed", dir.display())
            } else {
                "All dictionaries are turned off. Turn one on in Settings → Dictionaries.\n— No dictionary turned on".to_string()
            };
            cx.ui()?.dialog(ID_POPUP, &r.query, &body, &[("Close", "close")]);
            return Ok(());
        }
        // Recorded once a dictionary exists, found or not.
        if let Some(book) = from_book.clone() {
            self.record(cx, &r, book)?;
        }
        let book = from_book;
        let auto_add = cx.storage()?.get("auto_add").and_then(|v| v.as_bool()).unwrap_or(false);
        let Some((title, body)) = popup_page(&r, 0) else {
            self.last = None;
            let body = format!("No definition found for “{}”.\n— {}", r.query, searched(ndicts));
            cx.ui()?.dialog(ID_POPUP, &r.query, &body, &[("Close", "close")]);
            return Ok(());
        };
        let pages = dict_names(&r).len();
        self.last = Some(Last { word: vocab_word(&r, book.as_ref()).expect("a hit"), result: r, shown: 0 });
        if auto_add {
            self.add_word(cx, false)?;
        }
        log::debug!("dictionary: card {title:?}, {pages} dictionary page(s)");
        cx.ui()?.dialog(ID_POPUP, &title, &body, &POPUP_BUTTONS);
        Ok(())
    }

    /// Shows the next (`step` 1) or previous (`step` -1) dictionary page of the last
    /// card, wrapping around. Nothing with one dictionary.
    fn turn_card(&mut self, cx: &mut HostCtx, step: isize) -> Result<()> {
        let Some(l) = self.last.as_mut() else { return Ok(()) };
        let n = dict_names(&l.result).len();
        if n < 2 {
            log::debug!("dictionary: card swipe with {n} dictionary page(s): nothing to switch");
            return Ok(());
        }
        let from = l.shown;
        l.shown = (l.shown as isize + step).rem_euclid(n as isize) as usize;
        let Some((title, body)) = popup_page(&l.result, l.shown) else { return Ok(()) };
        log::info!("dictionary: card page {} -> {} of {n} ({})", from + 1, l.shown + 1, dict_names(&l.result)[l.shown]);
        cx.ui()?.dialog(ID_POPUP, &title, &body, &POPUP_BUTTONS);
        Ok(())
    }

    /// Writes the last popup's word to the vocabulary list (worker thread); `toast`
    /// confirms it.
    fn add_word(&mut self, cx: &mut HostCtx, toast: bool) -> Result<()> {
        let Some(w) = self.last.as_ref().map(|l| l.word.clone()) else { return Ok(()) };
        let path = Self::setting_path(cx, "vocab_db", "vocabulary.sqlite3")?;
        if toast {
            cx.ui()?.toast(&format!("Added “{}” to vocabulary", w.word));
        }
        let now = ink_store::now_secs();
        self.spawn(move || {
            if let Err(e) = ink_stats::VocabDb::open_or_recover(&path).and_then(|(db, _)| db.record_lookup(&w, now)) {
                log::warn!("dictionary: vocabulary {:?} in {}: {e}", w.word, path.display());
            }
        });
        Ok(())
    }

    /// Runs a database write off the UI thread (SQLite on the Kindle's FUSE store takes
    /// about 70 ms per write).
    fn spawn(&mut self, f: impl FnOnce() + Send + 'static) {
        self.writers.retain(|h| !h.is_finished());
        self.writers.push(std::thread::spawn(f));
    }

    fn record(&mut self, cx: &mut HostCtx, r: &LookupResult, book: BookRef) -> Result<()> {
        let on = cx.storage()?.get("record").and_then(|v| v.as_bool()).unwrap_or(true);
        if !on {
            return Ok(());
        }
        let path = Self::setting_path(cx, "vocab_db", "vocabulary.sqlite3")?;
        let l = Lookup { word: r.query.clone(), form: r.form.clone(), book_md5: book.md5, book_title: book.title, time: ink_store::now_secs(), found: !r.hits.is_empty(), context: book.context };
        self.spawn(move || {
            if let Err(e) = VocabDb::open_or_recover(&path).and_then(|(mut db, _)| db.record(&l)) {
                log::warn!("dictionary: recording {:?} in {}: {e}", l.word, path.display());
            }
        });
        Ok(())
    }
}

impl Plugin for DictionaryPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.selection_action(ACTION_LOOKUP, "Look up");
        reg.menu_item(ACTION_TYPE, "Dictionary", 40);
        reg.settings_page(SETTINGS_PAGE, "Dictionaries");
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        match id {
            ACTION_LOOKUP => {
                let doc = cx.document()?;
                let Some(sel) = doc.selection() else {
                    cx.ui()?.toast("Select a word to look up");
                    return Ok(());
                };
                let snap = BookRef { md5: doc.book_id(), title: Some(doc.metadata().title).filter(|t| !t.is_empty()), context: doc.read_around(&sel.start, 200).ok().map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")) };
                self.lookup(cx, &sel.text, Some(snap))
            }
            ACTION_TYPE => {
                cx.ui()?.input(ID_INPUT, "Look up a word", "");
                Ok(())
            }
            a if a.strip_prefix("settings:") == Some(SETTINGS_PAGE) => self.show_dicts(cx),
            _ => Ok(()),
        }
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        match (id, result) {
            (ID_POPUP, "next") => self.turn_card(cx, 1)?,
            (ID_POPUP, "prev") => self.turn_card(cx, -1)?,
            (ID_POPUP, "full") => {
                if let Some(l) = &self.last {
                    let title = l.word.word.clone();
                    // The full view opens on the dictionary the card shows.
                    let md = full_markdown_tab(&l.result, l.shown);
                    cx.ui()?.view_markdown(ID_VIEW, &title, &md);
                }
            }
            (ID_POPUP, "add") => self.add_word(cx, true)?,
            (ID_VIEW, tab) if tab.starts_with("dict:") => {
                // A dictionary tab: the open viewer takes the new text.
                if let (Some(l), Ok(i)) = (&self.last, tab["dict:".len()..].parse::<usize>()) {
                    let title = l.word.word.clone();
                    let md = full_markdown_tab(&l.result, i);
                    cx.ui()?.view_markdown(ID_VIEW, &title, &md);
                }
            }
            (ID_DICTS, name) if !name.is_empty() && name != "closed" => self.show_dict(cx, name)?,
            (ID_DICT, "back") => self.show_dicts(cx)?,
            (ID_DICT, what) if !what.is_empty() && what != "closed" => {
                if let Some(name) = self.editing.clone() {
                    self.edit_dict(cx, &name, what)?;
                }
            }
            (ID_INPUT, text) if !text.trim().is_empty() => {
                // A typed word is recorded with the open book, if any.
                let snap = cx.document().ok().map(|d| BookRef { md5: d.book_id(), title: Some(d.metadata().title).filter(|t| !t.is_empty()), context: None });
                self.lookup(cx, text, snap)?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
