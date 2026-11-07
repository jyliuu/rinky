//! The document model shared by every format.
//!
//! - [`Document`]: one open book. Reflowable engines (`ink-html`, fed by `ink-formats`)
//!   and fixed-layout engines (`ink-mupdf`) implement it.
//! - [`Provider`]: opens files of some formats; `ink-app` asks each provider in turn.
//! - [`Position`]: crengine xpointer (reflowable books), page + quads (PDF, CBZ), or the
//!   phase 0 [`FlowAnchor`] (read for migration only).
//! - Notes: [`Link::footnote`] marks note references, [`Document::note`] returns the note.
//! - [`LayoutSettings`]: what a reflowable layout depends on (also the page-map cache key).
//! - [`ResourceLoader`]: images, stylesheets and fonts referenced by a book.
//!
//! Owner from wave A: the `html` agent (with `ink-html` and `tests/golden`).

#![warn(missing_docs)]

pub mod position;
pub mod timing;

pub use position::{FlowAnchor, Location, PagePosition, Position, Quad, Step, TextOffset, XPointer};

use ink_device::{Canvas, Point, Rect};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Errors from documents and providers.
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    /// The format or feature is not implemented (yet); the message says which and when.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// The file is damaged or not what its extension says.
    #[error("bad document: {0}")]
    Format(String),
    /// A position string or position that this document cannot use.
    #[error("bad position: {0}")]
    BadPosition(String),
    /// Page index past the end.
    #[error("page {0} out of range")]
    PageOutOfRange(usize),
    /// A resource inside the book is missing.
    #[error("resource not found: {0}")]
    NotFound(String),
    /// File system error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result alias for this crate.
pub type Result<T, E = DocError> = std::result::Result<T, E>;

/// Book metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metadata {
    /// Title (may be empty).
    pub title: String,
    /// Authors in order.
    pub authors: Vec<String>,
    /// Language tag (`dc:language`).
    pub language: Option<String>,
    /// Identifier (ISBN, UUID).
    pub identifier: Option<String>,
    /// Publisher.
    pub publisher: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Resource path of the cover image inside the book.
    pub cover: Option<String>,
    /// Series name and index.
    pub series: Option<(String, f32)>,
}

/// Reflowable or fixed layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    /// Text that is laid out for the screen (EPUB, FB2, MOBI, TXT, HTML).
    Reflowable,
    /// Pages with fixed geometry (PDF, CBZ).
    Fixed,
}

/// Settings a reflowable layout depends on. Fixed-layout documents use `width` and
/// `height` as the target size and ignore the rest.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutSettings {
    /// Page width in pixels.
    pub width: u32,
    /// Page height in pixels.
    pub height: u32,
    /// Left margin in pixels.
    pub margin_left: f32,
    /// Right margin in pixels.
    pub margin_right: f32,
    /// Top margin in pixels.
    pub margin_top: f32,
    /// Bottom margin in pixels (the reader's footer is drawn inside it).
    pub margin_bottom: f32,
    /// Body font size in pixels per em.
    pub font_px: f32,
    /// Line height as a multiple of the font size.
    pub line_spacing: f32,
    /// Justify paragraphs.
    pub justify: bool,
    /// Hyphenate words.
    pub hyphenate: bool,
    /// Apply the book's own CSS.
    pub publisher_styles: bool,
    /// Extra space after every paragraph, in em of the paragraph's font size (0 = the
    /// book's own spacing).
    pub paragraph_spacing: f32,
    /// First-line indent of every paragraph in em; `None` = the book's own CSS.
    pub paragraph_indent: Option<f32>,
    /// Extra stroke weight of drawn text: 0 normal, 1 and 2 darker. Changes drawing
    /// only; the line breaks stay the same.
    pub embolden: u8,
    /// Font family override (`None` = embedded Noto Serif).
    pub font_family: Option<String>,
}

impl LayoutSettings {
    /// The prototype's defaults for a screen of `width` x `height`: 70 px side and top
    /// margins, 110 px bottom margin, 40 px body text, line spacing 1.4, justified.
    pub fn for_screen(width: u32, height: u32) -> Self {
        LayoutSettings {
            width,
            height,
            margin_left: 70.0,
            margin_right: 70.0,
            margin_top: 70.0,
            margin_bottom: 110.0,
            font_px: 40.0,
            line_spacing: 1.4,
            justify: true,
            hyphenate: false,
            publisher_styles: true,
            paragraph_spacing: 0.0,
            paragraph_indent: None,
            embolden: 0,
            font_family: None,
        }
    }

    /// A string that changes whenever the layout result could change (page-map cache key).
    pub fn cache_key(&self) -> String {
        // The paragraph and weight values are appended only when set, so the key of a
        // default layout is the one older versions wrote.
        let mut extra = String::new();
        if self.paragraph_spacing != 0.0 {
            extra += &format!("-ps{}", self.paragraph_spacing);
        }
        if let Some(i) = self.paragraph_indent {
            extra += &format!("-pi{i}");
        }
        if self.embolden != 0 {
            extra += &format!("-w{}", self.embolden);
        }
        format!(
            "{}x{}-m{}_{}_{}_{}-f{}-l{}-j{}-h{}-p{}-{}{extra}",
            self.width,
            self.height,
            self.margin_left,
            self.margin_right,
            self.margin_top,
            self.margin_bottom,
            self.font_px,
            self.line_spacing,
            self.justify as u8,
            self.hyphenate as u8,
            self.publisher_styles as u8,
            self.font_family.as_deref().unwrap_or("default")
        )
    }
}

/// Page count, possibly while pagination is still running in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageCount {
    /// Pages known so far.
    pub pages: usize,
    /// True when every page is known.
    pub complete: bool,
}

/// One table-of-contents entry.
#[derive(Clone, Debug, PartialEq)]
pub struct TocEntry {
    /// Title as shown.
    pub title: String,
    /// Nesting depth, 0 for top level.
    pub level: u8,
    /// Where it points.
    pub position: Position,
    /// Page index once known.
    pub page: Option<usize>,
}

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    /// Start of the match.
    pub position: Position,
    /// Page index once known.
    pub page: Option<usize>,
    /// Text around the match.
    pub excerpt: String,
    /// Byte range of the match inside `excerpt`.
    pub matched: std::ops::Range<usize>,
}

/// Where a link goes.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    /// Inside the book.
    Internal(Position),
    /// A URL (shown, never opened by the reader).
    External(String),
}

/// A link on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// Link text.
    pub text: String,
    /// Target.
    pub target: LinkTarget,
    /// Where it is drawn on the page, if known.
    pub rect: Option<Rect>,
    /// Looks like a footnote reference.
    pub footnote: bool,
}

/// Footnote or endnote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    /// A note on the same page or in the same chapter (`epub:type="footnote"`, `aside`).
    Footnote,
    /// A note collected at the end of the book or chapter (`endnote`, `rearnote`,
    /// a notes chapter).
    Endnote,
}

/// The note a footnote reference points to.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    /// Footnote or endnote.
    pub kind: NoteKind,
    /// Where the note text starts.
    pub position: Position,
    /// The note text (for a popup), without its leading number or backlink: the text
    /// of [`Note::spans`] joined.
    pub text: String,
    /// The note's own number or mark as the book prints it before the text (`12`, `*`),
    /// without brackets or a trailing dot; `None` when the note has none.
    pub label: Option<String>,
    /// [`Note::text`] with its inline formatting (italic, bold), white space collapsed.
    pub spans: Vec<NoteSpan>,
}

/// A piece of a note's text in one style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoteSpan {
    /// The text.
    pub text: String,
    /// Bold (`b`, `strong`).
    pub bold: bool,
    /// Italic (`i`, `em`, `cite`, ...).
    pub italic: bool,
}

impl Note {
    /// A note of plain text (formats without markup).
    pub fn plain(kind: NoteKind, position: Position, text: impl Into<String>) -> Note {
        let text = text.into();
        let spans = if text.is_empty() { Vec::new() } else { vec![NoteSpan { text: text.clone(), ..Default::default() }] };
        Note { kind, position, text, label: None, spans }
    }

    /// Removes `label` (the reference's own text, such as `12` or `[12]`) from the start
    /// of the text when the note repeats it there as plain text (`12 The text`,
    /// `12. The text`, `[12] The text`), and sets [`Note::label`] if it was unset.
    /// Nothing changes when the text does not start with it. Returns true when it was
    /// removed.
    pub fn strip_label(&mut self, label: &str) -> bool {
        let want = note_mark(label);
        let Some(want) = want.filter(|w| !w.is_empty()) else { return false };
        let Some(first) = self.spans.first() else { return false };
        let t = first.text.trim_start();
        let (token, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
        if note_mark(token).as_deref() != Some(want.as_str()) {
            return false;
        }
        let rest = rest.trim_start().to_string();
        if rest.is_empty() {
            self.spans.remove(0);
            if let Some(s) = self.spans.first_mut() {
                s.text = s.text.trim_start().to_string();
            }
        } else {
            self.spans[0].text = rest;
        }
        self.spans.retain(|s| !s.text.is_empty());
        self.text = self.spans.iter().map(|s| s.text.as_str()).collect();
        self.label.get_or_insert(want);
        true
    }
}

/// The mark of a note number as written (`[12]`, `(12)`, `12.`, `12)`, `*`): the
/// number or mark itself; `None` for text that does not look like one (at most four
/// letters or digits, or a few of `*†‡§¶#`).
pub fn note_mark(s: &str) -> Option<String> {
    let t = s.trim();
    let t = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')).or_else(|| t.strip_prefix('(').and_then(|x| x.strip_suffix(')'))).unwrap_or(t);
    let t = t.trim_end_matches(['.', ')', ':']).trim();
    let ok = !t.is_empty() && t.chars().count() <= 4 && (t.chars().all(|c| c.is_alphanumeric()) || t.chars().all(|c| "*†‡§¶#".contains(c)));
    ok.then(|| t.to_string())
}

/// Selected text.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    /// Start.
    pub start: Position,
    /// End (exclusive).
    pub end: Position,
    /// The text.
    pub text: String,
    /// Page the selection was made on.
    pub page: usize,
    /// Highlight rectangles on that page.
    pub rects: Vec<Rect>,
}

/// An open document.
///
/// Page indices are 0-based. Methods a format cannot support return
/// [`DocError::Unsupported`] with a message, never panic.
pub trait Document {
    /// Title, authors, ...
    fn metadata(&self) -> &Metadata;

    /// Reflowable or fixed.
    fn kind(&self) -> DocKind;

    /// Lays the document out for `settings` (reflowable) or sets the target size
    /// (fixed). Positions stay valid across calls; page numbers do not.
    fn set_layout(&mut self, settings: &LayoutSettings) -> Result<()>;

    /// Zooms a fixed-layout page in (`steps` > 0) or out one step per unit; the pages
    /// are cut into more or fewer screens and the current position stays on screen.
    /// Returns the new zoom factor (1.0 = fit), or `None` when the document has no zoom
    /// (reflowable) or the zoom is already at its limit.
    fn zoom_step(&mut self, _steps: i32) -> Option<f32> {
        None
    }

    /// Current layout settings.
    fn layout(&self) -> &LayoutSettings;

    /// Number of pages for the current layout.
    fn page_count(&self) -> PageCount;

    /// Draws page `page` into `canvas` (which has the layout's size). Draws the page
    /// content only; the reader adds headers, footers and overlays.
    fn render_page(&mut self, page: usize, canvas: &mut Canvas) -> Result<()>;

    /// Position of the first character on `page`.
    fn position_of_page(&self, page: usize) -> Result<Position>;

    /// Page that contains `pos` for the current layout.
    fn page_of(&self, pos: &Position) -> Result<usize>;

    /// The page the reader shows (kept by the document so plugins can ask for it).
    fn current_page(&self) -> usize;

    /// Records the page the reader shows.
    fn set_current_page(&mut self, page: usize);

    /// Position of the current page's start.
    fn current_position(&self) -> Result<Position> {
        self.position_of_page(self.current_page())
    }

    /// Table of contents, flattened with levels.
    fn toc(&self) -> Vec<TocEntry> {
        Vec::new()
    }

    /// Up to `limit` matches of `query` (case-insensitive), in reading order.
    fn search(&mut self, _query: &str, _limit: usize) -> Result<Vec<SearchHit>> {
        Err(DocError::Unsupported("search (phase 2/3)".into()))
    }

    /// About `chars` characters of text centred on `pos`.
    fn read_around(&mut self, _pos: &Position, _chars: usize) -> Result<String> {
        Err(DocError::Unsupported("read_around (phase 2)".into()))
    }

    /// Links on the page that contains `pos`.
    fn links(&mut self, _pos: &Position) -> Result<Vec<Link>> {
        Ok(Vec::new())
    }

    /// Selects the text between two points on `page` and remembers it.
    fn select(&mut self, _page: usize, _from: Point, _to: Point) -> Result<Option<Selection>> {
        Err(DocError::Unsupported("selection (phase 3)".into()))
    }

    /// The remembered selection.
    fn selection(&self) -> Option<&Selection> {
        None
    }

    /// Forgets the selection.
    fn clear_selection(&mut self) {}

    /// Remembers `sel` as the selection (what [`Document::selection`] returns next): a
    /// selection the reader put together from `select` calls on several pages (a
    /// passage continued on the next page). Default: nothing is remembered.
    fn set_selection(&mut self, _sel: Selection) {}

    /// Rectangles on `page` covered by the text range `start..end` (end exclusive), in
    /// the same form as [`Selection::rects`] (one per line). Used to draw saved
    /// highlights after a reopen or a layout change. Empty when the range is not on
    /// `page`. Default: empty (the format cannot map positions back to the page).
    fn range_rects(&mut self, _page: usize, _start: &Position, _end: &Position) -> Vec<Rect> {
        Vec::new()
    }

    /// Rectangles of search hit `hit` on `page` (see [`Document::range_rects`]).
    /// Default: empty.
    fn hit_rects(&mut self, _page: usize, _hit: &SearchHit) -> Vec<Rect> {
        Vec::new()
    }

    /// Kindle-style location of `pos`, when the format has text offsets.
    fn location(&self, _pos: &Position) -> Option<Location> {
        None
    }

    /// Last location of the book.
    fn location_count(&self) -> Option<Location> {
        None
    }

    /// Printed page labels from the EPUB page list: `(label, position)`.
    fn page_labels(&self) -> Vec<(String, Position)> {
        Vec::new()
    }

    /// The footnote or endnote that `target` (a link target) points to, when the
    /// target looks like a note.
    fn note(&mut self, _target: &Position) -> Result<Option<Note>> {
        Ok(None)
    }

    /// Enables an on-disk page-map cache in `dir` for the book identified by `book_id`
    /// (KOReader's partial MD5). Call before `set_layout`. Default: no cache.
    fn set_page_cache(&mut self, _dir: &Path, _book_id: &str) {}

    /// Waits until pagination is complete (`page_count().complete`) or `timeout` passes.
    /// Returns true when complete. Default: always complete.
    fn wait_paginated(&mut self, _timeout: std::time::Duration) -> bool {
        true
    }

    /// A plausible total page count while pagination runs (`page_count().complete` is
    /// false), estimated from the sizes of the parts not paginated yet; show it marked
    /// as an estimate ("~1234"). `None` when the count is complete or no estimate exists.
    fn page_estimate(&self) -> Option<usize> {
        None
    }

    /// The page of `pos` when it is known without paginating more of the book (never
    /// blocks on pagination; parsing one chapter is allowed). Default: [`Document::page_of`].
    fn page_of_known(&self, pos: &Position) -> Option<usize> {
        self.page_of(pos).ok()
    }

    /// The position the next `set_layout` opens at: the engine lays out that part first,
    /// so a book opened at its saved position does not wait for the parts before it.
    /// Call before `set_layout`; read the page with `current_page()` after it. Default:
    /// nothing (read `page_of` after `set_layout`).
    fn open_at(&mut self, _pos: &Position) {}

    /// First page of every spine item (chapter file), in spine order, once pagination
    /// is complete; empty when unknown or not applicable (fixed layouts). Read from the
    /// page map, never by parsing chapters. Used for chapter starts when the TOC is flat.
    fn spine_starts(&self) -> Vec<usize> {
        Vec::new()
    }
}

/// Opens documents of some formats.
pub trait Provider {
    /// Short name, e.g. `formats` or `mupdf`.
    fn name(&self) -> &'static str;

    /// Lowercase file extensions handled (without the dot; `fb2.zip` style allowed).
    fn extensions(&self) -> &'static [&'static str];

    /// True when this provider handles `path` (by extension by default).
    fn can_open(&self, path: &Path) -> bool {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
        self.extensions().iter().any(|ext| name.strip_suffix(ext).is_some_and(|stem| stem.ends_with('.')))
    }

    /// Opens `path`.
    fn open(&self, path: &Path) -> Result<Box<dyn Document>>;
}

/// Opens `path` with the first provider that accepts it.
pub fn open_with(providers: &[&dyn Provider], path: &Path) -> Result<Box<dyn Document>> {
    match providers.iter().find(|p| p.can_open(path)) {
        // File system errors name the file ("No such file or directory" alone does not
        // say which one).
        Some(p) => p.open(path).map_err(|e| match e {
            DocError::Io(io) => DocError::Io(std::io::Error::new(io.kind(), format!("{}: {io}", path.display()))),
            other => other,
        }),
        None => Err(DocError::Unsupported(format!(
            "no provider for {} (known: {})",
            path.display(),
            providers.iter().flat_map(|p| p.extensions().iter().copied()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// Reads resources (images, CSS, fonts) referenced by a book, by path inside the book.
pub trait ResourceLoader {
    /// The bytes of `path` (already resolved against the referencing document).
    fn read(&self, path: &str) -> Result<Vec<u8>>;

    /// True when `path` exists.
    fn exists(&self, path: &str) -> bool {
        self.read(path).is_ok()
    }

    /// A loader for the same resources that another thread may use (background
    /// pagination reads image sizes). `None` when the loader cannot be shared.
    fn shared(&self) -> Option<std::sync::Arc<dyn ResourceLoader + Send + Sync>> {
        None
    }
}

/// Resources from a directory (unpacked books, HTML files).
#[derive(Clone, Debug)]
pub struct DirLoader {
    /// Base directory.
    pub root: PathBuf,
}

impl ResourceLoader for DirLoader {
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        let clean = resolve_href("", path);
        if clean.split('/').any(|p| p == "..") {
            return Err(DocError::NotFound(path.into()));
        }
        std::fs::read(self.root.join(&clean)).map_err(|_| DocError::NotFound(path.into()))
    }

    fn shared(&self) -> Option<std::sync::Arc<dyn ResourceLoader + Send + Sync>> {
        Some(std::sync::Arc::new(self.clone()))
    }
}

/// Resources held in memory (tests, generated books).
#[derive(Clone, Debug, Default)]
pub struct MemoryLoader(pub HashMap<String, Vec<u8>>);

impl ResourceLoader for MemoryLoader {
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.0.get(path).cloned().ok_or_else(|| DocError::NotFound(path.into()))
    }

    fn shared(&self) -> Option<std::sync::Arc<dyn ResourceLoader + Send + Sync>> {
        Some(std::sync::Arc::new(self.clone()))
    }
}

/// Resolves `href` (percent-encoded, may contain `..`, may carry `#fragment`) against
/// the directory `dir` of the referencing file. Returns a path without leading `/`.
pub fn resolve_href(dir: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or("");
    let href = percent_decode(href);
    let mut parts: Vec<String> =
        if href.starts_with('/') { Vec::new() } else { dir.split('/').filter(|p| !p.is_empty()).map(String::from).collect() };
    for p in href.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                if matches!(parts.last().map(String::as_str), None | Some("..")) {
                    // Leaves the book; keep it visible so callers can refuse it.
                    parts.push("..".into());
                } else {
                    parts.pop();
                }
            }
            _ => parts.push(p.to_string()),
        }
    }
    parts.join("/")
}

/// Decodes `%XX` escapes (invalid UTF-8 is replaced).
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_resolution() {
        assert_eq!(resolve_href("OEBPS/", "../Text/ch%201.xhtml#x"), "Text/ch 1.xhtml");
        assert_eq!(resolve_href("", "a/b.html"), "a/b.html");
        assert_eq!(resolve_href("OEBPS/Text", "./img/../c.png"), "OEBPS/Text/c.png");
        assert_eq!(resolve_href("a", "/root.css"), "root.css");
        assert_eq!(resolve_href("", "../../etc/passwd"), "../../etc/passwd");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%41%zz"), "A%zz");
    }

    #[test]
    fn loaders() {
        let mut m = MemoryLoader::default();
        m.0.insert("a.css".into(), b"p{}".to_vec());
        assert_eq!(m.read("a.css").unwrap(), b"p{}");
        assert!(!m.exists("b.css"));
        assert!(m.shared().unwrap().exists("a.css"));
        // INK_MANIFEST_DIR: the test binary runs on the emulator with the files copied there.
        let root = std::env::var_os("INK_MANIFEST_DIR").map(PathBuf::from).unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into());
        let d = DirLoader { root };
        assert!(d.exists("Cargo.toml"));
        assert!(d.shared().unwrap().exists("Cargo.toml"));
        assert!(matches!(d.read("../ink-doc/Cargo.toml"), Err(DocError::NotFound(_))));
    }

    struct Fake;
    impl Provider for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["fb2.zip", "txt"]
        }
        fn open(&self, path: &Path) -> Result<Box<dyn Document>> {
            Err(DocError::Unsupported(path.display().to_string()))
        }
    }

    #[test]
    fn provider_dispatch() {
        assert!(Fake.can_open(Path::new("/x/Book.FB2.zip")));
        assert!(!Fake.can_open(Path::new("/x/book.zip")));
        let err = open_with(&[&Fake], Path::new("a.pdf")).err().unwrap();
        assert!(err.to_string().contains("no provider"), "{err}");
        assert!(matches!(open_with(&[&Fake], Path::new("a.txt")), Err(DocError::Unsupported(_))));
    }

    #[test]
    fn layout_defaults_and_cache_key() {
        let a = LayoutSettings::for_screen(1272, 1696);
        let mut b = a.clone();
        assert_eq!(a.cache_key(), b.cache_key());
        b.font_px = 41.0;
        assert_ne!(a.cache_key(), b.cache_key());
        // The new typography values keep the default key and change it when set.
        let mut c = a.clone();
        assert_eq!((c.paragraph_spacing, c.paragraph_indent, c.embolden), (0.0, None, 0));
        c.paragraph_spacing = 0.5;
        let k1 = c.cache_key();
        c.paragraph_indent = Some(0.0);
        let k2 = c.cache_key();
        c.embolden = 1;
        let k3 = c.cache_key();
        assert!(a.cache_key() != k1 && k1 != k2 && k2 != k3);
    }
}
