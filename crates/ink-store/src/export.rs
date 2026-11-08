//! Highlights, notes and bookmarks of a book as a Markdown, Kindle clippings or JSON file.
//!
//! [`render_markdown`], [`render_clippings`] and [`render_json`] are pure functions: a
//! [`BookState`] plus the title and authors in, text out. [`write_export_as`],
//! [`export_entry_as`] and [`export_all_as`] write the text under `<root>/exports/`
//! ([`crate::Paths::exports`]) with [`crate::write_atomic`]: `<title>.md`,
//! `<title>.json`, or Kindle's `My Clippings.txt` layout (`<title>.txt` for one book,
//! one `My Clippings.txt` for the whole library). Nothing here writes the sidecar. The
//! callers log the paths (this crate has no logger).
//!
//! Entries are sorted by position. The position strings are not compared as text
//! (`DocFragment[10]` would come before `DocFragment[2]`): [`position_key`] turns them
//! into numbers. Page numbers come from the caller ([`Locator::page_of`]; the reader asks
//! its open document) or from the entry's own `page` (fixed-layout books). A book that is
//! not open has no page numbers for xpointers, so those entries show chapter and date
//! only.

use crate::sidecar::{Bookmark, Highlight};
use crate::{write_atomic, BookEntry, BookState, LibraryIndex, Paths, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The file format of an export.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportFormat {
    /// `<title>.md`: headings per chapter, quotes, notes, page and date lines.
    #[default]
    Markdown,
    /// Kindle's `My Clippings.txt` layout, read by Readwise, Clippings.io and the like.
    Clippings,
    /// `<title>.json`: every field, for scripts.
    Json,
}

impl ExportFormat {
    /// Every format, in the order the settings list them.
    pub const ALL: [ExportFormat; 3] = [ExportFormat::Markdown, ExportFormat::Clippings, ExportFormat::Json];

    /// The name stored in `settings.toml` and given to `--format`.
    pub fn name(self) -> &'static str {
        match self {
            ExportFormat::Markdown => "markdown",
            ExportFormat::Clippings => "clippings",
            ExportFormat::Json => "json",
        }
    }

    /// The format of a stored or typed name (`md`, `kindle` and `txt` are accepted too).
    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "markdown" | "md" => Some(ExportFormat::Markdown),
            "clippings" | "kindle" | "txt" => Some(ExportFormat::Clippings),
            "json" => Some(ExportFormat::Json),
            _ => None,
        }
    }

    /// File extension of a one-book export.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Markdown => "md",
            ExportFormat::Clippings => "txt",
            ExportFormat::Json => "json",
        }
    }
}

/// File name of the whole-library export in [`ExportFormat::Clippings`], as on a Kindle.
pub const CLIPPINGS_FILE: &str = "My Clippings.txt";

/// Where an entry is in the book, for the page and percent line.
pub struct Locator<'a> {
    /// 0-based page of a position string, if known (the reader asks its document).
    pub page_of: &'a dyn Fn(&str) -> Option<u32>,
    /// Number of pages, if known. Without it no percent is printed.
    pub total: Option<u32>,
}

impl Locator<'_> {
    /// Knows nothing: only the pages stored in the entries are used.
    pub fn none() -> Locator<'static> {
        Locator { page_of: &|_| None, total: None }
    }
}

/// Longest file name stem written, in bytes (file systems allow 255 with the `.md`).
const MAX_STEM_BYTES: usize = 200;

/// One entry before sorting.
enum Entry<'a> {
    Highlight(&'a Highlight),
    Bookmark(&'a Bookmark),
}

/// Sort key of a position string: a kind number, then the numbers in it.
/// Reflowable: the `[n]` of every step (1 when absent) and the `.offset` after it.
/// `page:N;x0,y0,...`: page, first top, first left. `flow:a.b.c`: a, b, c.
/// Anything else sorts last.
pub fn position_key(s: &str) -> Vec<u64> {
    let num = |t: &str| t.trim().parse::<f64>().ok().map(|v| v.max(0.0) as u64);
    if let Some(rest) = s.strip_prefix("page:") {
        let mut parts = rest.split(';');
        let page = parts.next().and_then(num).unwrap_or(0);
        let q: Vec<u64> = parts.next().map(|q| q.split(',').filter_map(num).collect()).unwrap_or_default();
        return vec![1, page, q.get(1).copied().unwrap_or(0), q.first().copied().unwrap_or(0)];
    }
    if let Some(rest) = s.strip_prefix("flow:") {
        let mut k = vec![2];
        k.extend(rest.split('.').filter_map(num));
        return k;
    }
    if let Some(rest) = s.strip_prefix('/') {
        let mut k = vec![0];
        for part in rest.split('/') {
            let (idx, tail) = match (part.find('['), part.find(']')) {
                (Some(a), Some(b)) if a < b => (part[a + 1..b].parse::<u64>().unwrap_or(1), &part[b + 1..]),
                _ => (1, part.strip_prefix("text()").unwrap_or(part)),
            };
            k.push(idx);
            if let Some(off) = tail.strip_prefix('.').and_then(|o| o.parse::<u64>().ok()) {
                k.push(off);
            }
        }
        return k;
    }
    vec![3]
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `> ` before every line; an empty line becomes `>`.
fn quote(text: &str) -> String {
    text.trim().lines().map(|l| if l.trim().is_empty() { ">".to_string() } else { format!("> {}", l.trim_end()) }).collect::<Vec<_>>().join("\n")
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The `page · percent · date` line (italic); `None` when nothing is known.
fn where_line(page: Option<u32>, total: Option<u32>, created: &str) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(p) = page {
        parts.push(format!("p. {}", p + 1));
        if let Some(t) = total.filter(|t| *t > 0) {
            let pct = ((p as f64 + 1.0) * 100.0 / t as f64).round().min(100.0);
            parts.push(format!("{pct}%"));
        }
    }
    let date = created.get(..10).unwrap_or(created);
    if !date.is_empty() {
        parts.push(date.to_string());
    }
    (!parts.is_empty()).then(|| format!("*{}*", parts.join(" · ")))
}

fn note_text(n: &str) -> String {
    n.trim().replace('\n', "\n\n")
}

/// Highlights and bookmarks of `state` sorted by position.
fn sorted_entries(state: &BookState) -> Vec<Entry<'_>> {
    let mut entries: Vec<(Vec<u64>, Entry)> = Vec::new();
    for h in &state.highlights {
        entries.push((position_key(&h.start), Entry::Highlight(h)));
    }
    for b in &state.bookmarks {
        entries.push((position_key(&b.position), Entry::Bookmark(b)));
    }
    // Stable: equal positions keep highlights first, then the saved order.
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|(_, e)| e).collect()
}

impl Entry<'_> {
    fn position(&self) -> &str {
        match self {
            Entry::Highlight(h) => &h.start,
            Entry::Bookmark(b) => &b.position,
        }
    }

    /// 0-based page: from the locator, else the one stored in the entry.
    fn page(&self, loc: &Locator) -> Option<u32> {
        let stored = match self {
            Entry::Highlight(h) => h.page,
            Entry::Bookmark(b) => b.page,
        };
        (loc.page_of)(self.position()).or(stored)
    }
}

fn has_entries(state: &BookState) -> bool {
    !state.highlights.is_empty() || !state.bookmarks.is_empty()
}

/// The Markdown text for one book, or `None` when it has no highlights, notes or
/// bookmarks (nothing to write). Entries are sorted by position; a `## chapter` heading
/// is printed when the chapter changes.
pub fn render_markdown(title: &str, authors: &[String], state: &BookState, loc: &Locator) -> Option<String> {
    if !has_entries(state) {
        return None;
    }
    let entries = sorted_entries(state);

    let title = one_line(title);
    let mut out = format!("# {}\n\n", if title.is_empty() { "Untitled" } else { &title });
    let authors: Vec<&str> = authors.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).collect();
    if !authors.is_empty() {
        out.push_str(&format!("*{}*\n\n", authors.join(", ")));
    }
    let notes = state.highlights.iter().filter(|h| h.has_note()).count();
    out.push_str(&format!(
        "{} · {} · {}\n",
        plural(state.highlights.len(), "highlight", "highlights"),
        plural(notes, "note", "notes"),
        plural(state.bookmarks.len(), "bookmark", "bookmarks")
    ));

    let mut chapter = String::new();
    for e in &entries {
        let (ch, created) = match e {
            Entry::Highlight(h) => (h.chapter.as_deref(), h.created.as_str()),
            Entry::Bookmark(b) => (b.chapter.as_deref(), b.created.as_str()),
        };
        if let Some(c) = ch.map(one_line).filter(|c| !c.is_empty() && *c != chapter) {
            out.push_str(&format!("\n## {c}\n"));
            chapter = c;
        }
        out.push('\n');
        match e {
            Entry::Highlight(h) => {
                let has_text = !h.text.trim().is_empty();
                if has_text {
                    out.push_str(&quote(&h.text));
                    out.push('\n');
                }
                if let Some(n) = h.note.as_deref().filter(|n| !n.trim().is_empty()) {
                    if has_text {
                        out.push('\n');
                    }
                    out.push_str(&format!("**Note:** {}\n", note_text(n)));
                }
            }
            Entry::Bookmark(b) => {
                let label = one_line(&b.label);
                out.push_str(&format!("**Bookmark:** {}\n", if label.is_empty() { "(no label)" } else { &label }));
                if let Some(n) = b.note.as_deref().filter(|n| !n.trim().is_empty()) {
                    out.push_str(&format!("\n**Note:** {}\n", note_text(n)));
                }
            }
        }
        if let Some(line) = where_line(e.page(loc), loc.total, created) {
            out.push_str(&format!("\n{line}\n"));
        }
    }
    Some(out)
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// `YYYY-MM-DD HH:MM:SS` (rinky's and KOReader's time stamps) as a Kindle writes it:
/// `Thursday, October 1, 2026 12:00:00 PM`. Anything else is returned as it is.
pub fn kindle_date(stamp: &str) -> String {
    let stamp = stamp.trim();
    let parse = || -> Option<String> {
        let (date, time) = stamp.split_once(' ').unwrap_or((stamp, "00:00:00"));
        let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
        let (y, m, day) = (d.next()??, d.next()??, d.next()??);
        let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
        let (h, min, s) = (t.next()??, t.next().flatten().unwrap_or(0), t.next().flatten().unwrap_or(0));
        if !(1..=12).contains(&m) || !(1..=31).contains(&day) || !(0..=23).contains(&h) || !(0..=59).contains(&min) || !(0..=60).contains(&s) {
            return None;
        }
        // Days since 1970-01-01 (Howard Hinnant's days_from_civil); that day was a Thursday.
        let yy = if m <= 2 { y - 1 } else { y };
        let era = yy.div_euclid(400);
        let yoe = yy - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + day - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        let weekday = WEEKDAYS[(days + 3).rem_euclid(7) as usize];
        let (h12, ampm) = match h {
            0 => (12, "AM"),
            1..=11 => (h, "AM"),
            12 => (12, "PM"),
            _ => (h - 12, "PM"),
        };
        Some(format!("{weekday}, {} {day}, {y} {h12}:{min:02}:{s:02} {ampm}", MONTHS[m as usize - 1]))
    };
    parse().unwrap_or_else(|| stamp.to_string())
}

/// Text lines with Windows line ends, as a Kindle writes them.
fn crlf(text: &str) -> String {
    text.trim().lines().map(str::trim_end).collect::<Vec<_>>().join("\r\n")
}

/// One clipping: title line, `- Your <kind> ... | Added on ...`, a blank line, the text,
/// and the `==========` separator.
fn clipping(out: &mut String, head: &str, kind: &str, page: Option<u32>, stamp: &str, text: &str) {
    let on_page = page.map(|p| format!(" on page {}", p + 1)).unwrap_or_default();
    out.push_str(&format!("{head}\r\n- Your {kind}{on_page} | Added on {}\r\n\r\n{}\r\n==========\r\n", kindle_date(stamp), crlf(text)));
}

/// The book's clippings in Kindle's `My Clippings.txt` layout, or `None` when it has
/// nothing to export. Each highlight is a `Highlight` clipping followed by a `Note`
/// clipping when it has a note (as a Kindle does); a bookmark is a `Bookmark` clipping
/// with an empty text (and a `Note` when it has one). The title line is `Title (Author;
/// Author)`. Lines end in CR LF; no byte order mark (the file writer adds one at the
/// start of the file). Entries are sorted by position; page numbers are 1-based.
pub fn render_clippings(title: &str, authors: &[String], state: &BookState, loc: &Locator) -> Option<String> {
    if !has_entries(state) {
        return None;
    }
    let title = one_line(title);
    let title = if title.is_empty() { "Untitled".to_string() } else { title };
    let authors: Vec<String> = authors.iter().map(|a| one_line(a)).filter(|a| !a.is_empty()).collect();
    let head = if authors.is_empty() { title } else { format!("{title} ({})", authors.join("; ")) };
    let mut out = String::new();
    for e in sorted_entries(state) {
        let page = e.page(loc);
        let (note, note_stamp) = match e {
            Entry::Highlight(h) => {
                if !h.text.trim().is_empty() {
                    clipping(&mut out, &head, "Highlight", page, &h.created, &h.text);
                }
                (h.note.as_deref(), h.updated.as_deref().unwrap_or(&h.created))
            }
            Entry::Bookmark(b) => {
                clipping(&mut out, &head, "Bookmark", page, &b.created, "");
                (b.note.as_deref(), b.created.as_str())
            }
        };
        if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
            clipping(&mut out, &head, "Note", page, note_stamp, n);
        }
    }
    Some(out)
}

/// One book in [`render_json`].
#[derive(Serialize)]
struct JsonBook<'a> {
    title: &'a str,
    authors: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    pages: Option<u32>,
    highlights: Vec<JsonHighlight<'a>>,
    bookmarks: Vec<JsonBookmark<'a>>,
}

#[derive(Serialize)]
struct JsonHighlight<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chapter: Option<&'a str>,
    /// 1-based.
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    created: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated: Option<&'a str>,
    style: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<&'a str>,
    start: &'a str,
    end: &'a str,
}

#[derive(Serialize)]
struct JsonBookmark<'a> {
    label: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chapter: Option<&'a str>,
    /// 1-based.
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
    created: &'a str,
    position: &'a str,
}

/// The book's highlights and bookmarks as pretty-printed JSON (`title`, `authors`,
/// `pages` when known, `highlights`, `bookmarks`, each list sorted by position, pages
/// 1-based), or `None` when it has nothing to export.
pub fn render_json(title: &str, authors: &[String], state: &BookState, loc: &Locator) -> Option<String> {
    if !has_entries(state) {
        return None;
    }
    let mut book = JsonBook { title, authors, pages: loc.total.filter(|t| *t > 0), highlights: Vec::new(), bookmarks: Vec::new() };
    for e in sorted_entries(state) {
        let page = e.page(loc).map(|p| p + 1);
        match e {
            Entry::Highlight(h) => book.highlights.push(JsonHighlight {
                text: &h.text,
                note: h.note.as_deref(),
                chapter: h.chapter.as_deref(),
                page,
                created: &h.created,
                updated: h.updated.as_deref(),
                style: &h.style,
                color: h.color.as_deref(),
                start: &h.start,
                end: &h.end,
            }),
            Entry::Bookmark(b) => {
                book.bookmarks.push(JsonBookmark { label: &b.label, note: b.note.as_deref(), chapter: b.chapter.as_deref(), page, created: &b.created, position: &b.position })
            }
        }
    }
    // Only strings and numbers: serialising cannot fail.
    let mut text = serde_json::to_string_pretty(&book).ok()?;
    text.push('\n');
    Some(text)
}

/// The text of one book in `format`.
pub fn render(format: ExportFormat, title: &str, authors: &[String], state: &BookState, loc: &Locator) -> Option<String> {
    match format {
        ExportFormat::Markdown => render_markdown(title, authors, state, loc),
        ExportFormat::Clippings => render_clippings(title, authors, state, loc),
        ExportFormat::Json => render_json(title, authors, state, loc),
    }
}

/// The bytes written for `text` in `format`: a clippings file starts with a UTF-8 byte
/// order mark, as a Kindle's does.
fn file_bytes(format: ExportFormat, text: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(text.len() + 3);
    if format == ExportFormat::Clippings {
        b.extend_from_slice("\u{feff}".as_bytes());
    }
    b.extend_from_slice(text.as_bytes());
    b
}

/// A file name stem from a title: `/ \ : * ? " < > |`, control characters and NUL become
/// `-` (the user store is FAT-like), runs of white space collapse, leading dots and
/// trailing dots and spaces go, other Unicode stays; at most 200 bytes (cut at a
/// character). `fallback` is used when nothing is left; then `book`.
pub fn sanitize_file_name(title: &str, fallback: &str) -> String {
    fn clean(s: &str) -> String {
        let mapped: String = s.chars().map(|c| if c.is_whitespace() { ' ' } else if c.is_control() || "/\\:*?\"<>|".contains(c) { '-' } else { c }).collect();
        let mut t = one_line(&mapped);
        while t.len() > MAX_STEM_BYTES {
            t.pop();
        }
        t.trim_start_matches('.').trim_end_matches(['.', ' ']).to_string()
    }
    let t = clean(title);
    if !t.is_empty() {
        return t;
    }
    let f = clean(fallback);
    if f.is_empty() {
        "book".to_string()
    } else {
        f
    }
}

fn file_stem_of(book: &Path) -> String {
    book.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Writes the Markdown export of one book to `paths.exports()/<stem>.md`; see
/// [`write_export_as`].
pub fn write_export(paths: &Paths, stem: &str, title: &str, authors: &[String], state: &BookState, loc: &Locator) -> Result<Option<PathBuf>> {
    write_export_as(paths, ExportFormat::Markdown, stem, title, authors, state, loc)
}

/// Writes `bytes` to `paths.exports()/<name>` (directory created if missing).
fn write_file(paths: &Paths, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let dir = paths.exports();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    write_atomic(&path, bytes)?;
    Ok(path)
}

/// Writes the export of one book in `format` to `paths.exports()/<stem>.<ext>`
/// (directory created if missing). Returns the path, or `None` when the book has
/// nothing to export (no file is written).
pub fn write_export_as(paths: &Paths, format: ExportFormat, stem: &str, title: &str, authors: &[String], state: &BookState, loc: &Locator) -> Result<Option<PathBuf>> {
    let Some(text) = render(format, title, authors, state, loc) else { return Ok(None) };
    write_file(paths, &format!("{stem}.{}", format.extension()), &file_bytes(format, &text)).map(Some)
}

/// Exports the book `entry` as Markdown; see [`export_entry_as`].
pub fn export_entry(paths: &Paths, entry: &BookEntry, loc: &Locator) -> Result<Option<PathBuf>> {
    export_entry_as(paths, entry, loc, ExportFormat::Markdown)
}

/// Exports the book `entry` in `format` (title and authors from the entry, state from
/// its sidecar).
pub fn export_entry_as(paths: &Paths, entry: &BookEntry, loc: &Locator, format: ExportFormat) -> Result<Option<PathBuf>> {
    let state = BookState::load(&entry.path)?;
    let stem = sanitize_file_name(&entry.title, &file_stem_of(&entry.path));
    write_export_as(paths, format, &stem, &entry.title, &entry.authors, &state, loc)
}

/// What [`export_all`] did.
#[derive(Debug, Default)]
pub struct ExportReport {
    /// Files written.
    pub written: Vec<PathBuf>,
    /// Books exported (one file each, or all in one `My Clippings.txt`).
    pub books: usize,
    /// Books without highlights, notes or bookmarks (no file).
    pub empty: usize,
    /// Books that failed: path and the error text.
    pub failed: Vec<(PathBuf, String)>,
}

/// Exports every book of `index` as Markdown; see [`export_all_as`].
pub fn export_all(paths: &Paths, index: &LibraryIndex) -> ExportReport {
    export_all_as(paths, index, ExportFormat::Markdown)
}

/// Exports every book of `index` that has something to export. Markdown and JSON: one
/// file each; books whose sanitised titles are equal (an EPUB and a PDF of one title)
/// get ` (format)` added, and ` (md5 prefix)` when that is still equal, so none
/// overwrites another; the names do not depend on the order of the index. Clippings:
/// one `exports/My Clippings.txt` with every book, ordered by file path, replaced on
/// each export.
pub fn export_all_as(paths: &Paths, index: &LibraryIndex, format: ExportFormat) -> ExportReport {
    let mut report = ExportReport::default();
    let mut books: Vec<&BookEntry> = index.books.iter().collect();
    books.sort_by(|a, b| a.path.cmp(&b.path));
    let mut loaded = Vec::new();
    for b in books {
        match BookState::load(&b.path) {
            Ok(s) if has_entries(&s) => loaded.push((b, s)),
            Ok(_) => report.empty += 1,
            Err(e) => report.failed.push((b.path.clone(), e.to_string())),
        }
    }
    if format == ExportFormat::Clippings {
        let mut text = String::new();
        for (b, state) in &loaded {
            if let Some(t) = render_clippings(&b.title, &b.authors, state, &Locator::none()) {
                text.push_str(&t);
                report.books += 1;
            }
        }
        if report.books > 0 {
            match write_file(paths, CLIPPINGS_FILE, &file_bytes(format, &text)) {
                Ok(p) => report.written.push(p),
                Err(e) => {
                    // Every book is in the one file that could not be written.
                    report.failed.extend(loaded.iter().map(|(b, _)| (b.path.clone(), e.to_string())));
                    report.books = 0;
                }
            }
        }
        return report;
    }
    // How many of `names` equal each one, ignoring case.
    let counts = |names: &[String]| {
        let mut tally: HashMap<String, usize> = HashMap::new();
        for n in names {
            *tally.entry(n.to_lowercase()).or_default() += 1;
        }
        names.iter().map(|n| tally[&n.to_lowercase()]).collect::<Vec<_>>()
    };
    let stems: Vec<String> = loaded.iter().map(|(b, _)| sanitize_file_name(&b.title, &file_stem_of(&b.path))).collect();
    let with_format: Vec<String> =
        loaded.iter().zip(&stems).zip(counts(&stems)).map(|(((b, _), s), n)| if n > 1 { format!("{s} ({})", b.format) } else { s.clone() }).collect();
    let final_names: Vec<String> = loaded
        .iter()
        .zip(&with_format)
        .zip(counts(&with_format))
        .map(|(((b, _), s), n)| {
            if n < 2 {
                return s.clone();
            }
            let md5 = b.md5.clone().or_else(|| crate::partial_md5(&b.path).ok()).unwrap_or_default();
            let id: String = md5.chars().take(6).collect();
            if id.is_empty() {
                s.clone()
            } else {
                format!("{s} ({id})")
            }
        })
        .collect();
    for ((b, state), stem) in loaded.iter().zip(&final_names) {
        match write_export_as(paths, format, stem, &b.title, &b.authors, state, &Locator::none()) {
            Ok(Some(p)) => {
                report.written.push(p);
                report.books += 1;
            }
            Ok(None) => report.empty += 1,
            Err(e) => report.failed.push((b.path.clone(), e.to_string())),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn hl(start: &str, text: &str, note: Option<&str>, chapter: Option<&str>) -> Highlight {
        Highlight {
            start: start.into(),
            end: start.into(),
            text: text.into(),
            note: note.map(String::from),
            chapter: chapter.map(String::from),
            created: "2026-10-01 12:00:00".into(),
            ..Default::default()
        }
    }

    #[test]
    fn golden_markdown() {
        let state = BookState {
            highlights: vec![
                hl("/body/DocFragment[10]/body/p[1]/text().0", "Late words.", None, Some("Chapter 10")),
                hl("/body/DocFragment[2]/body/p[3]/text().4", "First line\nsecond line", Some("my note\nmore"), Some("Chapter 2")),
                hl("/body/DocFragment[2]/body/p[1]/text().9", "Earlier.", None, Some("Chapter 2")),
            ],
            bookmarks: vec![Bookmark {
                position: "/body/DocFragment[2]/body/p[2]/text().0".into(),
                label: "Mark".into(),
                created: "2026-10-02 08:00:00".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let page_of = |s: &str| if s.contains("[10]") { Some(99) } else { Some(11) };
        let md = render_markdown("Romeo and Juliet", &["William Shakespeare".to_string()], &state, &Locator { page_of: &page_of, total: Some(200) }).unwrap();
        let want = "\
# Romeo and Juliet

*William Shakespeare*

3 highlights · 1 note · 1 bookmark

## Chapter 2

> Earlier.

*p. 12 · 6% · 2026-10-01*

**Bookmark:** Mark

*p. 12 · 6% · 2026-10-02*

> First line
> second line

**Note:** my note

more

*p. 12 · 6% · 2026-10-01*

## Chapter 10

> Late words.

*p. 100 · 50% · 2026-10-01*
";
        assert_eq!(md, want);
    }

    #[test]
    fn fixed_layout_uses_the_stored_page_and_sorts_by_page() {
        let mut a = hl("page:12;1,2,3,4", "b", None, None);
        a.page = Some(12);
        let mut b = hl("page:3;1,2,3,4", "a", None, None);
        b.page = Some(3);
        let state = BookState { highlights: vec![a, b], ..Default::default() };
        let md = render_markdown("T", &[], &state, &Locator::none()).unwrap();
        assert!(md.find("> a").unwrap() < md.find("> b").unwrap());
        assert!(md.contains("*p. 4 · 2026-10-01*") && md.contains("*p. 13 · 2026-10-01*"));
    }

    #[test]
    fn position_keys_compare_numbers() {
        assert!(position_key("/body/DocFragment[2]/body/p[1]/text().0") < position_key("/body/DocFragment[10]/body/p[1]/text().0"));
        assert!(position_key("/body/DocFragment[2]/body/p/text().5") < position_key("/body/DocFragment[2]/body/p[2]/text().1"));
        assert!(position_key("/body/DocFragment[2]/body/p[2]/text().9") < position_key("/body/DocFragment[2]/body/p[2]/text().10"));
        assert!(position_key("page:2;0,0,1,1") < position_key("page:10;0,0,1,1"));
        assert!(position_key("flow:1.9.0") < position_key("flow:1.10.0"));
    }

    #[test]
    fn empty_book_renders_nothing_and_writes_no_file() {
        let state = BookState::default();
        assert_eq!(render_markdown("T", &[], &state, &Locator::none()), None);
        let dir = TempDir::new("export-empty");
        let paths = Paths { root: dir.0.clone() };
        assert_eq!(write_export(&paths, "T", "T", &[], &state, &Locator::none()).unwrap(), None);
        assert!(!paths.exports().exists());
    }

    #[test]
    fn titles_are_sanitised() {
        assert_eq!(sanitize_file_name("AC/DC: Live? \"Now\"", "x"), "AC-DC- Live- -Now-");
        assert_eq!(sanitize_file_name("  ..hidden.. ", "x"), "hidden");
        assert_eq!(sanitize_file_name("Ørsted – Über 日本語", "x"), "Ørsted – Über 日本語");
        assert_eq!(sanitize_file_name("a\0b\nc", "x"), "a-b c");
        assert_eq!(sanitize_file_name("///", "stem"), "---");
        assert_eq!(sanitize_file_name("...", "stem"), "stem");
        assert_eq!(sanitize_file_name("", ""), "book");
        let long = sanitize_file_name(&"é".repeat(300), "x");
        assert!(long.len() <= 200 && long.chars().all(|c| c == 'é'));
    }

    #[test]
    fn writes_under_the_exports_dir_and_batches_do_not_collide() {
        let dir = TempDir::new("export-all");
        let paths = Paths { root: dir.0.join("home") };
        let mut idx = LibraryIndex::default();
        for (name, fmt) in [("a.epub", "epub"), ("a.pdf", "pdf"), ("b.epub", "epub"), ("c.epub", "epub")] {
            let path = dir.0.join(name);
            std::fs::write(&path, name).unwrap();
            let title = if name.starts_with('c') { "Plain" } else if name.starts_with('b') { "Same/Title" } else { "Same-Title" };
            if name != "c.epub" {
                BookState { highlights: vec![hl("/body/p[1]/text().0", name, None, None)], ..Default::default() }.save(&path).unwrap();
            }
            idx.books.push(BookEntry { path, title: title.into(), format: fmt.into(), ..Default::default() });
        }
        let r = export_all(&paths, &idx);
        assert_eq!(r.empty, 1);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        let mut names: Vec<String> = r.written.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names.len(), 3);
        // Three books share "Same-Title"; two of them are epubs, so those two also get an id.
        assert!(names.iter().all(|n| n.starts_with("Same-Title (")), "{names:?}");
        assert_eq!(names.iter().collect::<std::collections::HashSet<_>>().len(), 3, "{names:?}");
        assert!(r.written.iter().all(|p| p.starts_with(paths.exports())));
        assert_eq!(std::fs::read_dir(paths.exports()).unwrap().count(), 3, "no temp files left");
    }

    /// The book of `golden_markdown`, with fixed time stamps, for the other formats.
    fn sample_state() -> BookState {
        let mut noted = hl("/body/DocFragment[2]/body/p[3]/text().4", "First line\nsecond line", Some("my note"), Some("Chapter 2"));
        noted.updated = Some("2026-10-03 00:05:09".into());
        noted.style = "lighten".into();
        BookState {
            highlights: vec![hl("/body/DocFragment[10]/body/p[1]/text().0", "Late words.", None, Some("Chapter 10")), noted],
            bookmarks: vec![Bookmark {
                position: "/body/DocFragment[2]/body/p[2]/text().0".into(),
                label: "Mark".into(),
                created: "2026-10-02 18:30:00".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn kindle_dates() {
        assert_eq!(kindle_date("2026-10-01 12:00:00"), "Thursday, October 1, 2026 12:00:00 PM");
        assert_eq!(kindle_date("2026-10-02 18:30:00"), "Friday, October 2, 2026 6:30:00 PM");
        assert_eq!(kindle_date("2026-10-03 00:05:09"), "Saturday, October 3, 2026 12:05:09 AM");
        assert_eq!(kindle_date("2000-02-29 09:01:02"), "Tuesday, February 29, 2000 9:01:02 AM");
        assert_eq!(kindle_date("1969-12-31 23:59:59"), "Wednesday, December 31, 1969 11:59:59 PM");
        assert_eq!(kindle_date("2026-10-01"), "Thursday, October 1, 2026 12:00:00 AM");
        assert_eq!(kindle_date("yesterday"), "yesterday");
        assert_eq!(kindle_date("2026-13-01 00:00:00"), "2026-13-01 00:00:00");
    }

    #[test]
    fn golden_clippings() {
        let page_of = |s: &str| if s.contains("[10]") { Some(99) } else { Some(11) };
        let text = render_clippings("Romeo and Juliet", &["William Shakespeare".to_string(), "Ed. Someone".into()], &sample_state(), &Locator { page_of: &page_of, total: Some(200) }).unwrap();
        let want = "\
Romeo and Juliet (William Shakespeare; Ed. Someone)\r
- Your Bookmark on page 12 | Added on Friday, October 2, 2026 6:30:00 PM\r
\r
\r
==========\r
Romeo and Juliet (William Shakespeare; Ed. Someone)\r
- Your Highlight on page 12 | Added on Thursday, October 1, 2026 12:00:00 PM\r
\r
First line\r
second line\r
==========\r
Romeo and Juliet (William Shakespeare; Ed. Someone)\r
- Your Note on page 12 | Added on Saturday, October 3, 2026 12:05:09 AM\r
\r
my note\r
==========\r
Romeo and Juliet (William Shakespeare; Ed. Someone)\r
- Your Highlight on page 100 | Added on Thursday, October 1, 2026 12:00:00 PM\r
\r
Late words.\r
==========\r
";
        assert_eq!(text, want);
        // Without page numbers the "on page" part is left out; no authors, no brackets.
        let bare = render_clippings("T", &[], &BookState { highlights: vec![hl("/body/p[1]/text().0", "x", None, None)], ..Default::default() }, &Locator::none()).unwrap();
        assert_eq!(bare, "T\r\n- Your Highlight | Added on Thursday, October 1, 2026 12:00:00 PM\r\n\r\nx\r\n==========\r\n");
        assert_eq!(render_clippings("T", &[], &BookState::default(), &Locator::none()), None);
    }

    #[test]
    fn golden_json() {
        let page_of = |s: &str| if s.contains("[10]") { Some(99) } else { None };
        let text = render_json("Romeo and Juliet", &["William Shakespeare".to_string()], &sample_state(), &Locator { page_of: &page_of, total: Some(200) }).unwrap();
        let want = r#"{
  "title": "Romeo and Juliet",
  "authors": [
    "William Shakespeare"
  ],
  "pages": 200,
  "highlights": [
    {
      "text": "First line\nsecond line",
      "note": "my note",
      "chapter": "Chapter 2",
      "created": "2026-10-01 12:00:00",
      "updated": "2026-10-03 00:05:09",
      "style": "lighten",
      "start": "/body/DocFragment[2]/body/p[3]/text().4",
      "end": "/body/DocFragment[2]/body/p[3]/text().4"
    },
    {
      "text": "Late words.",
      "chapter": "Chapter 10",
      "page": 100,
      "created": "2026-10-01 12:00:00",
      "style": "",
      "start": "/body/DocFragment[10]/body/p[1]/text().0",
      "end": "/body/DocFragment[10]/body/p[1]/text().0"
    }
  ],
  "bookmarks": [
    {
      "label": "Mark",
      "created": "2026-10-02 18:30:00",
      "position": "/body/DocFragment[2]/body/p[2]/text().0"
    }
  ]
}
"#;
        assert_eq!(text, want);
        assert_eq!(render_json("T", &[], &BookState::default(), &Locator::none()), None);
    }

    #[test]
    fn format_names_round_trip() {
        for f in ExportFormat::ALL {
            assert_eq!(ExportFormat::from_name(f.name()), Some(f));
        }
        assert_eq!(ExportFormat::from_name(" MD "), Some(ExportFormat::Markdown));
        assert_eq!(ExportFormat::from_name("kindle"), Some(ExportFormat::Clippings));
        assert_eq!(ExportFormat::from_name("pdf"), None);
        assert_eq!(ExportFormat::default(), ExportFormat::Markdown);
    }

    #[test]
    fn one_book_in_each_format() {
        let dir = TempDir::new("export-formats");
        let paths = Paths { root: dir.0.clone() };
        let state = sample_state();
        let md = write_export_as(&paths, ExportFormat::Markdown, "B", "B", &[], &state, &Locator::none()).unwrap().unwrap();
        let txt = write_export_as(&paths, ExportFormat::Clippings, "B", "B", &[], &state, &Locator::none()).unwrap().unwrap();
        let json = write_export_as(&paths, ExportFormat::Json, "B", "B", &[], &state, &Locator::none()).unwrap().unwrap();
        assert_eq!([md.file_name().unwrap(), txt.file_name().unwrap(), json.file_name().unwrap()], ["B.md", "B.txt", "B.json"]);
        let bytes = std::fs::read(&txt).unwrap();
        assert!(bytes.starts_with("\u{feff}B\r\n- Your Bookmark".as_bytes()), "BOM then the first clipping");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
        assert_eq!(v["highlights"].as_array().unwrap().len(), 2);
        assert!(std::fs::read_to_string(&md).unwrap().starts_with("# B\n"));
    }

    #[test]
    fn library_clippings_go_to_one_file() {
        let dir = TempDir::new("export-clippings");
        let paths = Paths { root: dir.0.join("home") };
        let mut idx = LibraryIndex::default();
        for (name, title) in [("b.epub", "Second"), ("a.epub", "First"), ("c.epub", "Empty")] {
            let path = dir.0.join(name);
            std::fs::write(&path, name).unwrap();
            if name != "c.epub" {
                BookState { highlights: vec![hl("/body/p[1]/text().0", &format!("from {title}"), None, None)], ..Default::default() }.save(&path).unwrap();
            }
            idx.books.push(BookEntry { path, title: title.into(), authors: vec!["A. Writer".into()], format: "epub".into(), ..Default::default() });
        }
        let r = export_all_as(&paths, &idx, ExportFormat::Clippings);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!((r.books, r.empty), (2, 1));
        assert_eq!(r.written, vec![paths.exports().join(CLIPPINGS_FILE)]);
        let text = std::fs::read_to_string(&r.written[0]).unwrap();
        let text = text.strip_prefix('\u{feff}').expect("one BOM at the start");
        assert!(!text.contains('\u{feff}'));
        assert_eq!(text.matches("==========\r\n").count(), 2);
        assert!(text.starts_with("First (A. Writer)\r\n") && text.find("from First").unwrap() < text.find("from Second").unwrap(), "{text}");
        // JSON: one file per book, like Markdown.
        let r = export_all_as(&paths, &idx, ExportFormat::Json);
        let mut names: Vec<String> = r.written.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!((names, r.books), (vec!["First.json".to_string(), "Second.json".to_string()], 2));
    }
}
