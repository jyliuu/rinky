//! Highlights, notes and bookmarks of a book as a Markdown file.
//!
//! [`render_markdown`] is a pure function: a [`BookState`] plus the title and authors
//! in, text out. [`write_export`], [`export_entry`] and [`export_all`] write the text to
//! `<root>/exports/<title>.md` ([`crate::Paths::exports`]) with [`crate::write_atomic`].
//! Nothing here writes the sidecar. The callers log the paths (this crate has no logger).
//!
//! Entries are sorted by position. The position strings are not compared as text
//! (`DocFragment[10]` would come before `DocFragment[2]`): [`position_key`] turns them
//! into numbers. Page numbers come from the caller ([`Locator::page_of`]; the reader asks
//! its open document) or from the entry's own `page` (fixed-layout books). A book that is
//! not open has no page numbers for xpointers, so those entries show chapter and date
//! only.

use crate::sidecar::{Bookmark, Highlight};
use crate::{write_atomic, BookEntry, BookState, LibraryIndex, Paths, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// The Markdown text for one book, or `None` when it has no highlights, notes or
/// bookmarks (nothing to write). Entries are sorted by position; a `## chapter` heading
/// is printed when the chapter changes.
pub fn render_markdown(title: &str, authors: &[String], state: &BookState, loc: &Locator) -> Option<String> {
    if state.highlights.is_empty() && state.bookmarks.is_empty() {
        return None;
    }
    let mut entries: Vec<(Vec<u64>, Entry)> = Vec::new();
    for h in &state.highlights {
        entries.push((position_key(&h.start), Entry::Highlight(h)));
    }
    for b in &state.bookmarks {
        entries.push((position_key(&b.position), Entry::Bookmark(b)));
    }
    // Stable: equal positions keep highlights first, then the saved order.
    entries.sort_by(|a, b| a.0.cmp(&b.0));

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
    for (_, e) in &entries {
        let (ch, pos, stored_page, created) = match e {
            Entry::Highlight(h) => (h.chapter.as_deref(), h.start.as_str(), h.page, h.created.as_str()),
            Entry::Bookmark(b) => (b.chapter.as_deref(), b.position.as_str(), b.page, b.created.as_str()),
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
        let page = (loc.page_of)(pos).or(stored_page);
        if let Some(line) = where_line(page, loc.total, created) {
            out.push_str(&format!("\n{line}\n"));
        }
    }
    Some(out)
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

/// Writes the export of one book to `paths.exports()/<stem>.md` (directory created if
/// missing). Returns the path, or `None` when the book has nothing to export (no file is
/// written).
pub fn write_export(paths: &Paths, stem: &str, title: &str, authors: &[String], state: &BookState, loc: &Locator) -> Result<Option<PathBuf>> {
    let Some(text) = render_markdown(title, authors, state, loc) else { return Ok(None) };
    let dir = paths.exports();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{stem}.md"));
    write_atomic(&path, text.as_bytes())?;
    Ok(Some(path))
}

/// Exports the book `entry` (title and authors from the entry, state from its sidecar).
pub fn export_entry(paths: &Paths, entry: &BookEntry, loc: &Locator) -> Result<Option<PathBuf>> {
    let state = BookState::load(&entry.path)?;
    let stem = sanitize_file_name(&entry.title, &file_stem_of(&entry.path));
    write_export(paths, &stem, &entry.title, &entry.authors, &state, loc)
}

/// What [`export_all`] did.
#[derive(Debug, Default)]
pub struct ExportReport {
    /// Files written.
    pub written: Vec<PathBuf>,
    /// Books without highlights, notes or bookmarks (no file).
    pub empty: usize,
    /// Books that failed: path and the error text.
    pub failed: Vec<(PathBuf, String)>,
}

/// Exports every book of `index` that has something to export, one file each. Books
/// whose sanitised titles are equal (an EPUB and a PDF of one title) get ` (format)`
/// added, and ` (md5 prefix)` when that is still equal, so none overwrites another; the
/// names do not depend on the order of the index.
pub fn export_all(paths: &Paths, index: &LibraryIndex) -> ExportReport {
    let mut report = ExportReport::default();
    let mut books: Vec<&BookEntry> = index.books.iter().collect();
    books.sort_by(|a, b| a.path.cmp(&b.path));
    let mut loaded = Vec::new();
    for b in books {
        match BookState::load(&b.path) {
            Ok(s) if !s.highlights.is_empty() || !s.bookmarks.is_empty() => loaded.push((b, s)),
            Ok(_) => report.empty += 1,
            Err(e) => report.failed.push((b.path.clone(), e.to_string())),
        }
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
        match write_export(paths, stem, &b.title, &b.authors, state, &Locator::none()) {
            Ok(Some(p)) => report.written.push(p),
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
}
