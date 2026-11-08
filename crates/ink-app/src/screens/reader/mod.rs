//! Reader screen (owner from wave B: the `reader` agent).
//!
//! Phase 0 behaviour is the prototype's `run` loop:
//! - the saved page with a GC16 flash (no "Opening" screen in between: the screen below
//!   stays until the page is ready);
//! - taps, swipes, pinches and the other gestures run the actions of the user's gesture
//!   map ([`actions`]; default: tap left third = previous page, the rest = next page,
//!   middle = menu; no hidden exit corner);
//! - the power key = exit;
//! - page turns use REAGL, every 8th turn flashes (GC16);
//! - the position is saved after every page shown (`<stem>.sdr/rinky.<ext>.json`).
//!
//! Page Next/Prev keys (host simulator) turn pages too. powerd events are logged.
//!
//! Phase 3 (wave B `reader`) adds overlays over the page ([`overlay`]): a tap in the
//! middle of the page (or the top band, or the Menu key) opens the menu ([`menu`]): top
//! bar with back, title, clock, battery and the tools; bottom bar with chapter, page,
//! percent and a page slider. The page is kept in a buffer so closing an overlay puts it
//! back without rendering it again.
//!
//! A book that cannot be opened (missing file, DRM, KFX, broken file) shows "Cannot open
//! this book", the file name and the error text; a tap, swipe or key then pops the
//! reader (back to the library, or out of the app when the reader was started directly).

pub mod aa;
pub mod actions;
pub mod footer;
pub mod goto;
pub mod light_gestures;
pub mod menu;
pub mod overlay;
pub mod search;
pub mod selection;

#[cfg(test)]
mod tests;

use crate::router::{AppCtx, AppScreen, Route, ScreenRegistry, Transition};
use ink_device::{Canvas, Key, Point, Rect, RefreshClass, Size};
use ink_doc::{Document, LayoutSettings, Link, LinkTarget, Position, SearchHit, Selection, TocEntry};
use ink_store::sidecar::{BookSettings, Bookmark, Highlight};
use ink_store::BookState;
use ink_text::{Fonts, Style};
use ink_ui::refresh::Change;
use ink_ui::{Damage, EventResult, Gesture, PaintCx, UiEvent};
use menu::MenuInfo;
use overlay::Overlay;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Registers the reader for [`Route::Reader`].
pub fn register(reg: &mut ScreenRegistry) {
    reg.register("reader", |route| match route {
        Route::Reader(path) => Ok(Box::new(ReaderScreen::new(path.clone()))),
        other => anyhow::bail!("reader cannot open {other:?}"),
    });
}

/// Layout settings for a screen of `w` x `h` from the user's reader settings, with the
/// top and bottom margins measured from the status bars ([`footer::text_layout`]).
pub fn layout_for(settings: &ink_store::Settings, w: u32, h: u32) -> LayoutSettings {
    footer::text_layout(stored_layout(settings, w, h), &settings.status_bars)
}

/// [`layout_for`] with the margins as stored (not yet placed against the status bars):
/// what the Aa sheet shows and saves.
pub fn stored_layout(settings: &ink_store::Settings, w: u32, h: u32) -> LayoutSettings {
    let r = &settings.reader;
    LayoutSettings {
        margin_left: r.margin_x,
        margin_right: r.margin_x,
        margin_top: r.margin_top,
        margin_bottom: r.margin_bottom,
        font_px: r.font_px,
        line_spacing: r.line_spacing,
        justify: r.justify,
        hyphenate: r.hyphenate,
        publisher_styles: r.publisher_styles,
        paragraph_spacing: r.paragraph_spacing,
        paragraph_indent: indent_em(r.paragraph_indent),
        embolden: r.font_weight.min(ink_store::settings::WEIGHT_MAX),
        ..LayoutSettings::for_screen(w, h)
    }
}

/// The layout's first-line indent for a stored value: negative keeps the book's own.
pub fn indent_em(stored: f32) -> Option<f32> {
    (stored >= 0.0 && stored.is_finite()).then_some(stored)
}

pub use footer::draw_footer;

/// Plugin status-bar texts (`hooks::status_text`): left, center, right.
fn status_texts(cx: &AppCtx) -> [String; 3] {
    use crate::plugin_host::hooks::status_text;
    use ink_plugin::registry::StatusSlot;
    [status_text(cx, StatusSlot::Left), status_text(cx, StatusSlot::Center), status_text(cx, StatusSlot::Right)]
}

thread_local! {
    /// Status bars of the open reader, for the control center (both run on the UI
    /// thread): `None` no book open, else whether the bars are hidden
    /// ([`ReaderScreen::status_bars_hidden`]).
    static READER_BARS: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    /// The control center asked the reader under it to show or hide its status bars.
    static BARS_TOGGLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the open reader hides its status bars; `None` when no book is open.
pub fn reader_bars_hidden() -> Option<bool> {
    READER_BARS.with(|c| c.get())
}

/// Asks the open reader to flip its status bars when it is shown again (the control
/// center's "Status bars" button over a book). Returns the new hidden state.
pub fn request_status_bars_toggle() -> Option<bool> {
    let hidden = !reader_bars_hidden()?;
    BARS_TOGGLE.with(|c| c.set(!c.get()));
    READER_BARS.with(|c| c.set(Some(hidden)));
    Some(hidden)
}

fn publish_bars(hidden: Option<bool>) {
    READER_BARS.with(|c| c.set(hidden));
}

/// A highlight made this session as it was drawn on one page: kept for documents that
/// cannot map a saved range back to rectangles (`range_rects` empty: fixed layouts).
struct SessionMark {
    /// Layout key ([`LayoutSettings::cache_key`]) the rectangles are for.
    layout: String,
    page: usize,
    rects: Vec<Rect>,
    /// Start of the saved highlight.
    start: Position,
}

impl SessionMark {
    /// True when the mark is drawn on `page` of layout `layout`.
    fn on(&self, layout: &str, page: usize) -> bool {
        self.page == page && self.layout == layout
    }
}

/// Draws the highlights on `page` (drawn on `canvas`): saved ones mapped to it in the
/// current layout, and this session's that the document could not map (fixed layouts).
fn draw_highlights(doc: &mut dyn Document, page: usize, canvas: &mut Canvas, saved: &[(Position, Position)], session: &[SessionMark]) {
    let mut drawn: Vec<&Position> = Vec::new();
    for (start, end) in saved {
        let rects = doc.range_rects(page, start, end);
        if !rects.is_empty() {
            selection::lighten(canvas, &rects);
            drawn.push(start);
        }
    }
    if session.is_empty() {
        return;
    }
    let key = doc.layout().cache_key();
    for m in session.iter().filter(|m| m.on(&key, page) && !drawn.contains(&&m.start)) {
        selection::lighten(canvas, &m.rects);
    }
}

/// How often the reader looks whether pagination finished while its bars show an
/// estimated total.
const COUNTING_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// Longest a percent jump waits for pagination to finish.
const PERCENT_WAIT: Duration = Duration::from_secs(10);

/// The page at `pct` % of the book. While pagination runs the page count is partial
/// (90 % of it is far too early), so this waits for the rest (a jump right after the
/// book opened), at most [`PERCENT_WAIT`]; then it goes by the estimated total, laying
/// out pages up to the target.
fn percent_page(doc: &mut dyn Document, pct: f32) -> usize {
    let f = (pct / 100.0).clamp(0.0, 1.0);
    if !doc.page_count().complete && !doc.wait_paginated(PERCENT_WAIT) {
        if let Some(total) = doc.page_estimate() {
            let page = (f * total.saturating_sub(1) as f32).round() as usize;
            log::info!("reader: {pct}% while pagination runs: page {} of about {total}", page + 1);
            // The document lays out pages up to (a few past) its current page.
            doc.set_current_page(page);
            return page.min(doc.page_count().pages.saturating_sub(1));
        }
    }
    let last = doc.page_count().pages.saturating_sub(1);
    (f * last as f32).round() as usize
}

/// Renders page `index` with the default status bars (what `rinky render` writes; the
/// reader screen uses the user's bars, device values and reading speed).
pub fn render_page(doc: &mut dyn Document, index: usize, canvas: &mut Canvas, fonts: &mut Fonts) -> anyhow::Result<()> {
    doc.render_page(index, canvas)?;
    footer::draw_default_bars(doc, index, canvas, fonts);
    Ok(())
}

/// Opens `path` with the first matching provider and lays it out for `w` x `h`.
pub fn open_book(
    providers: &[&dyn ink_doc::Provider],
    settings: &ink_store::Settings,
    path: &Path,
    w: u32,
    h: u32,
) -> anyhow::Result<Box<dyn Document>> {
    open_traced(providers, path, &layout_for(settings, w, h), None, None, None, &mut crate::perf::OpenTrace::start())
}

/// Opens `path` with the first matching provider, enables the page-map cache in
/// `cache` (when given, keyed by the book's partial MD5) and lays it out with `layout`,
/// with the steps recorded in `trace`: provider open (zip, container, spine), page-map
/// cache id, layout of the first page. `md5` is the book's
/// partial MD5 when the caller has it already; `at` is the position to open at (laid
/// out first, see [`Document::open_at`]).
pub fn open_traced(
    providers: &[&dyn ink_doc::Provider],
    path: &Path,
    layout: &LayoutSettings,
    cache: Option<&Path>,
    md5: Option<&str>,
    at: Option<&Position>,
    trace: &mut crate::perf::OpenTrace,
) -> anyhow::Result<Box<dyn Document>> {
    let t = Instant::now();
    let mut doc = ink_doc::open_with(providers, path)?;
    let t_parse = t.elapsed();
    trace.step_with_engine("provider open");
    if let Some(dir) = cache {
        match md5.map(|m| Ok(m.to_string())).unwrap_or_else(|| ink_store::md5::partial_md5(path)) {
            Ok(id) => doc.set_page_cache(dir, &id),
            Err(e) => log::warn!("no page cache for '{}': {e}", path.display()),
        }
    }
    if let Some(p) = at {
        doc.open_at(p);
    }
    trace.step("page cache id");
    doc.set_layout(layout)?;
    trace.step_with_engine("set_layout");
    let count = doc.page_count();
    log::info!(
        "'{}': {}{} pages (parse {:?}, layout {:?})",
        doc.metadata().title,
        count.pages,
        if count.complete { "" } else { " known" },
        t_parse,
        t.elapsed() - t_parse
    );
    if count.pages == 0 {
        anyhow::bail!("book produced no pages");
    }
    Ok(doc)
}

/// Font size of the error screen (the prototype's message size).
const ERROR_PX: f32 = 44.0;
/// At most this many lines of error text are drawn.
const ERROR_MAX_LINES: usize = 10;

/// Splits `text` into lines no wider than `max_w` at `px` (words longer than a line are
/// cut between characters). At most `max_lines` lines; a cut text ends with "...".
pub fn wrap_text(fonts: &mut Fonts, text: &str, px: f32, max_w: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if fonts.text_width(Style::Regular, &cand, px) <= max_w {
            cur = cand;
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        // The word alone: cut it between characters while it is too wide.
        for ch in word.chars() {
            let mut next = cur.clone();
            next.push(ch);
            if !cur.is_empty() && fonts.text_width(Style::Regular, &next, px) > max_w {
                lines.push(std::mem::take(&mut cur));
                cur.push(ch);
            } else {
                cur = next;
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            last.push_str(" ...");
        }
    }
    lines
}

/// Reading progress 0..=1 of `page`: Kindle locations when the format has them (stable
/// while pagination runs), else the page fraction.
pub fn progress(doc: &dyn Document, page: usize) -> f32 {
    if let (Ok(pos), Some(total)) = (doc.position_of_page(page), doc.location_count()) {
        if let Some(l) = doc.location(&pos) {
            if total.0 > 1 {
                return ((l.0.saturating_sub(1)) as f32 / (total.0 - 1) as f32).clamp(0.0, 1.0);
            }
        }
    }
    let total = doc.page_count().pages.max(1);
    ((page + 1) as f32 / total as f32).clamp(0.0, 1.0)
}

/// Progress written to the sidecar for `page`: [`progress`] (location of the page's
/// first character, stable while pagination still counts pages), and 1.0 on the last
/// page once the page count is complete.
pub fn saved_percent(doc: &dyn Document, page: usize) -> f32 {
    let count = doc.page_count();
    if count.complete && page + 1 >= count.pages {
        return 1.0;
    }
    progress(doc, page)
}

/// Index of the TOC entry `page` is in: the last entry that starts on or before it.
/// Entries whose page is not known yet (background pagination) are skipped: asking
/// `page_of` for them waited for the paginators (menu, Go to, bookmarks and sidecar
/// writes use only known pages; 2026-11-02).
pub fn current_chapter(doc: &dyn Document, toc: &[TocEntry], page: usize) -> Option<usize> {
    let mut best = None;
    for (i, e) in toc.iter().enumerate() {
        let p = e.page.or_else(|| doc.page_of_known(&e.position));
        if let Some(p) = p {
            if p <= page {
                best = Some(i);
            } else if best.is_some() {
                break;
            }
        }
    }
    best
}

/// A bookmark with its position parsed once: whether a page is marked is asked for
/// every page drawn (the dog-ear, the status bar's bookmark).
#[derive(Clone, Debug)]
struct Mark {
    bookmark: Bookmark,
    at: Option<Position>,
}

impl Mark {
    fn new(bookmark: Bookmark) -> Self {
        let at = bookmark.position.parse().ok();
        Mark { bookmark, at }
    }

    /// Page in the current layout; else the page saved with it (fixed layouts).
    fn page(&self, doc: &dyn Document) -> Option<usize> {
        self.at.as_ref().and_then(|p| doc.page_of_known(p)).or(self.bookmark.page.map(|p| p as usize))
    }
}

/// True when one of `marks` is on `page`.
fn page_marked(marks: &[Mark], doc: &dyn Document, page: usize) -> bool {
    marks.iter().any(|m| m.page(doc) == Some(page))
}

/// (start, end) of saved highlights whose positions parse.
fn saved_ranges(hl: &[Highlight]) -> Vec<(Position, Position)> {
    hl.iter().filter_map(|h| Some((h.start.parse::<Position>().ok()?, h.end.parse::<Position>().ok()?))).collect()
}

/// Now as `YYYY-MM-DD HH:MM:SS` local time: when a bookmark or highlight was made.
fn timestamp() -> String {
    let t = ink_device::clock::TimeZone::local().at(std::time::SystemTime::now()).format();
    t.get(..19).map_or_else(|| t.clone(), str::to_string)
}

/// `page` for a fixed-layout book (its marks keep the page: positions there are pages),
/// `None` for a reflowable one.
fn fixed_page(doc: &dyn Document, page: usize) -> Option<u32> {
    (doc.kind() == ink_doc::DocKind::Fixed).then_some(page as u32)
}

/// Copies the pixels of `r` from `src` to `dst` (two canvases of one size; `r` inside
/// them): the page buffer and the screen canvas.
fn copy_rect(dst: &mut Canvas, src: &Canvas, r: Rect) {
    let (w, n) = (dst.width, r.w as usize);
    for y in r.y..r.bottom() {
        let i = y as usize * w + r.x as usize;
        dst.pixels[i..i + n].copy_from_slice(&src.pixels[i..i + n]);
    }
}

/// True when `p` is within `slop` pixels of `r` (targets widened for fingers).
fn near(r: &Rect, p: Point, slop: i32) -> bool {
    Rect::new(r.x - slop, r.y - slop, r.w + 2 * slop as u32, r.h + 2 * slop as u32).contains(p)
}

/// Side of the bookmark mark in the top-right corner of a bookmarked page.
pub const DOG_EAR: i32 = 64;

/// Draws the bookmark mark: a black triangle in the top-right corner.
pub fn draw_dog_ear(canvas: &mut Canvas) {
    let w = canvas.width as i32;
    for y in 0..DOG_EAR {
        canvas.fill_rect(w - DOG_EAR + y, y, DOG_EAR - y, 1, 0);
    }
}

/// Where the "Back to p. N" pill of a followed link or footnote is drawn on a `size`
/// screen: a 104 px pill in the bottom-left corner (the footer bar starts right of it).
pub fn back_rect(size: Size) -> Rect {
    // 400 px: "Back to p. 1234" at the label size fits next to the icon.
    Rect::new(24, size.h as i32 - 108, 400, 104)
}

/// Caption of the back pill for the 0-based `page` (`None`: page unknown).
pub fn back_label(page: Option<usize>) -> String {
    match page {
        Some(p) => format!("Back to p. {}", p + 1),
        None => "Back".into(),
    }
}

/// Draws the "Back to p. N" pill (after following an in-book link or footnote).
fn draw_back(canvas: &mut Canvas, fonts: &mut Fonts, theme: &ink_ui::Theme, page: Option<usize>) {
    let r = back_rect(canvas.size());
    ink_ui::draw::fill_round_rect(canvas, r, r.h as f32 / 2.0, theme.bg);
    let mut p = PaintCx::new(canvas, fonts, theme);
    ink_ui::paint_button(&mut p, r, &back_label(page), Some(ink_ui::Icon::Back), ink_ui::ButtonKind::Outlined, true, false);
}

/// Text of a note popup: at most `max` characters, cut at a word with "...".
pub fn note_excerpt(text: &str, max: usize) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= max {
        return t;
    }
    let cut: String = t.chars().take(max).collect();
    let cut = cut.rsplit_once(' ').map(|(a, _)| a.to_string()).unwrap_or(cut);
    format!("{cut} ...")
}

/// Prepares a note for its popup: removes the reference's own number from the start of
/// the text when the book repeats it there (`<a>12</a>` pointing at `12 The text`), and
/// returns the popup title, `Footnote 12` (or `Note 12` for an endnote; without a number
/// when neither the note nor the reference has one).
pub fn note_popup_title(note: &mut ink_doc::Note, reference: &str) -> String {
    let mark = ink_doc::note_mark(reference);
    if note.label.is_none() {
        if let Some(m) = &mark {
            note.strip_label(m);
        }
    }
    let kind = match note.kind {
        ink_doc::NoteKind::Footnote => "Footnote",
        ink_doc::NoteKind::Endnote => "Note",
    };
    match note.label.clone().or(mark) {
        Some(l) => format!("{kind} {l}"),
        None => kind.to_string(),
    }
}

/// The styled text of a note popup: at most `max` characters, cut at a word with "..."
/// like [`note_excerpt`].
pub fn note_spans(note: &ink_doc::Note, max: usize) -> Vec<(String, Style)> {
    let style = |s: &ink_doc::NoteSpan| match (s.bold, s.italic) {
        (true, true) => Style::BoldItalic,
        (true, false) => Style::Bold,
        (false, true) => Style::Italic,
        (false, false) => Style::Regular,
    };
    let mut out = Vec::new();
    let mut left = max;
    for s in &note.spans {
        let n = s.text.chars().count();
        if n <= left {
            out.push((s.text.clone(), style(s)));
            left -= n;
            continue;
        }
        let cut: String = s.text.chars().take(left).collect();
        let cut = match cut.rsplit_once(' ') {
            Some((a, _)) => a.to_string(),
            None if out.is_empty() => cut,
            None => String::new(),
        };
        out.push((format!("{cut} ..."), style(s)));
        break;
    }
    out
}

/// Labels of the plugin menu entries.
fn plugin_labels(cx: &AppCtx) -> Vec<String> {
    plugin_menu(cx.plugins).into_iter().map(|m| m.label).collect()
}

/// Plugin entries of the reader menu, sorted by order then label.
pub fn plugin_menu(plugins: &crate::plugin_host::PluginHost) -> Vec<ink_plugin::registry::MenuItem> {
    let mut items = plugins.menu_items().to_vec();
    items.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.label.cmp(&b.label)));
    items
}

/// Id of Insightful's selection action (its plugin button opens its action sheet); the
/// selection card's Insightful button runs it.
const INSIGHTFUL_ACTION: &str = "menu";

/// Labels of the plugin selection actions (`plugin-sel:I`) and the index of Insightful's
/// among them (the selection card's Insightful button; `None` when it is not loaded).
fn selection_entries(cx: &AppCtx) -> (Vec<String>, Option<usize>) {
    let acts = cx.plugins.selection_actions();
    let insightful = acts.iter().position(|a| a.plugin == insightful::plugin::NAME && a.id == INSIGHTFUL_ACTION);
    (acts.iter().map(|a| a.label.clone()).collect(), insightful)
}

/// Prefix of the actions of a plugin card shown in a reader overlay
/// ([`ReaderScreen::dict_card`]): `pcard:<the plugin's button action>`.
const PCARD: &str = "pcard:";

/// A place on the way back: where a jump started.
#[derive(Clone, Debug)]
struct Return {
    position: Position,
    /// The jump followed an in-book link or footnote (the back gesture and key, the
    /// "Back to p. N" pill and the Aa sheet use only these).
    link: bool,
}

/// The reader.
pub struct ReaderScreen {
    path: PathBuf,
    doc: Option<Box<dyn Document>>,
    page: usize,
    /// The whole screen must be shown again with this class.
    dirty: Option<RefreshClass>,
    /// The page must be rendered again (page or layout changed) before it is shown.
    rerender: bool,
    /// Why the book could not be opened (the screen then shows it until a tap).
    error: Option<String>,
    /// The page as last rendered (content + footer), to put back when an overlay closes.
    page_buf: Option<Canvas>,
    /// The open overlay (menu, panels, popups).
    overlay: Option<Overlay>,
    /// Rects of overlays that closed or were replaced since the last paint: the next
    /// paint puts the page back there from `page_buf` and shows only these rects (plus
    /// the new overlay's own damage), not the full screen.
    reveal: Vec<Rect>,
    /// An overlay was replaced by one of the same kind (an Aa tab switch): the next
    /// partial paint refreshes only the pixels that changed, so a tall sheet is not
    /// promoted to a full-screen flash.
    swap_same: bool,
    /// A full-screen overlay (Goto, Search, Note) closed: the next paint shows the full
    /// page with `Change::FullOverlayClose` (REAGL plus ghost debt, no GC16 flash).
    full_close: bool,
    /// A highlight or bookmark changed: the page is drawn again but only the pixels that
    /// changed (highlight lines, dog-ear, bar icon, closed popup) are refreshed (GL16).
    mark_change: bool,
    /// The finger lifted after a selection drag: GL16 of this selection box brings back
    /// the anti-aliasing DU crushed while dragging.
    sel_settle: Option<Rect>,
    /// The book's table of contents (read once).
    toc: Option<Vec<TocEntry>>,
    /// Every overlay action handled, in order (tests and logs).
    actions: Vec<String>,
    /// An overlay closed during a touch: the gestures of that touch (a slider drag ends
    /// as a swipe) must not turn pages. Cleared by the next finger down.
    swallow_gestures: bool,
    /// An edge slide (brightness or warmth follows the finger) in progress.
    slide: Option<light_gestures::Slide>,
    /// The book's bookmarks (loaded on open, saved on every change).
    marks: Vec<Mark>,
    /// Bookmarks in the order the Go to panel lists them.
    listed_marks: Vec<Bookmark>,
    /// The book's own reading settings (Aa panel), over the global ones.
    book_settings: BookSettings,
    /// Links of a page (page, links), read when a tap needs them.
    links: Option<(usize, Vec<Link>)>,
    /// Where jumps came from, latest last: the menu's "Go back" returns to the last one,
    /// the back gesture and key only when it was a link.
    back: Vec<Return>,
    /// The "Back to p. N" pill is drawn on the page: only after a footnote jump. Other
    /// in-book links (a contents page, cross references) keep the return place for the
    /// Aa sheet's Back button and the back gesture, without a pill on the page.
    back_pill: bool,
    /// Target of the footnote popup ("Go to note").
    note_target: Option<Position>,
    /// Text of the search field.
    query: String,
    /// Results of the last search (None before the first).
    hits: Option<Vec<SearchHit>>,
    /// The hit shown last.
    hit: usize,
    /// Where the finger started a selection (while it is down).
    selecting: Option<Point>,
    /// The current selection.
    sel: Option<Selection>,
    /// The word the selection started on.
    sel_word: Option<selection::Word>,
    /// Bounds of the word the selection was last extended to, and the finger point of
    /// that extension (moves inside it, or within a few pixels, are skipped).
    sel_end: Option<(Rect, Point)>,
    /// Page of the word the selection started on (`sel_word`); the selection can
    /// continue on later or earlier pages ([`selection::span_select`]).
    sel_anchor_page: usize,
    /// Every page of the selection with its rectangles there (one entry unless the drag
    /// turned the page).
    sel_pages: Vec<(usize, Vec<Rect>)>,
    /// The dragged end rests in a page-turn zone ([`selection::edge_zone`]): (direction,
    /// since). The page turns after [`selection::EDGE_DWELL`].
    sel_edge: Option<(i8, Instant)>,
    /// The drag turned the page: no further turn until the finger leaves the zone.
    sel_edge_lock: bool,
    /// A selection drag went on half a page ([`selection::Bridge`]): the screen shows the
    /// lower half of `page` and the upper half of the next. Only while the selection is
    /// up; when its card closes the reader shows the page the selection ended on.
    bridge: Option<selection::Bridge>,
    /// The first line's top and the last line's bottom of a page (page, box), for the
    /// turn zones.
    sel_text_box: Option<(usize, Option<(i32, i32)>)>,
    /// Text of the note editor (selection card "Note").
    note_text: String,
    /// The selection rectangles inverted on the panel now.
    sel_shown: Vec<Rect>,
    /// The selection changed and must be drawn.
    sel_dirty: bool,
    /// Highlights made in this session, per page drawn. Drawn only when the document
    /// cannot map the saved range (`range_rects` empty: fixed layouts).
    session_hl: Vec<SessionMark>,
    /// Saved highlight ranges of the book (start, end), drawn with `range_rects`.
    saved_hl: Vec<(Position, Position)>,
    /// The selection is a saved highlight that was tapped: its card offers "Remove
    /// highlight" instead of "Highlight".
    sel_highlight: bool,
    /// Plugin status texts (left, center, right) drawn on the shown page.
    status_shown: [String; 3],
    /// Chapter starts for the footer (rebuilt when the layout changes).
    chapter_map: Option<footer::ChapterMap>,
    /// The bars on screen were drawn while pagination still counted pages (an estimated
    /// total, no chapter ticks or pages left): they are drawn again once it is done.
    counting_shown: bool,
    /// Reading speed for "min left in chapter" (ink-stats, else 250 wpm).
    speed: footer::Speed,
    /// Hide both status bars (the `status_bars` action, [`ReaderScreen::toggle_status_bars`]).
    pub status_bars_hidden: bool,
    /// Device values and minute tick of the status bars.
    bars: footer::BarsState,
    /// The hardware page-turn wave of the turn waiting to be shown (`reader.page_animation`
    /// on): the turn's one update sweeps across the screen. Jumps have none.
    turn_wave: Option<ink_device::Wave>,
    /// Open-time trace until the first page is on the panel (`perf.rs`).
    open_trace: Option<crate::perf::OpenTrace>,
    /// `on_book_open` is still to be sent (after the first page is shown).
    plugins_pending: bool,
    /// The plugin dialog shown as a card in the selection overlay (`(plugin, request
    /// id)`; the widget is `ids["dict"]`): the dictionary card of a long-press lookup.
    dict_card: Option<(String, u32)>,
    /// Cards that went away without a button (a tap on the page, Back, another overlay):
    /// they get the empty answer, like a plugin card tapped outside.
    dict_cancel: Vec<(String, u32)>,
    /// The position shown is not in the sidecar yet: (first unsaved page shown, last
    /// page shown). Written by [`ReaderScreen::flush_position`] when the page has rested
    /// [`POSITION_SAVE_IDLE`] (at most [`POSITION_SAVE_MAX`] after the first unsaved
    /// turn), before the reader closes, and on power events (sleep, cover, USB).
    position_unsaved: Option<(Instant, Instant)>,
    /// The page to render comes from a page turn or a relayout (font size, margins,
    /// rotation: the saved position is still on the page): its position is saved later.
    turned: bool,
    /// The page to show comes from a jump (TOC, Go to, link, search hit, bookmark): its
    /// class is `Change::Jump` (REAGL; GC16 per the user's "full refresh every N pages"),
    /// not a flash of its own.
    jumped: bool,
    /// The way the last page turn went (+1 next, -1 previous): the page expected next.
    turn_dir: i64,
    /// When the page after the one shown is drawn ahead ([`ReaderScreen::prefetch_next`]):
    /// set when a page is drawn, once no input came for [`PREFETCH_IDLE`].
    prefetch_at: Option<Instant>,
}

/// The sidecar gets the position once the page has rested this long. Page turns do no
/// file I/O: a sidecar load and save (two fsyncs) per turn held the UI thread before
/// the next tap was read (2026-11-02).
pub const POSITION_SAVE_IDLE: Duration = Duration::from_secs(2);
/// While pages keep turning, the position is still saved this often.
pub const POSITION_SAVE_MAX: Duration = Duration::from_secs(10);
/// A page drawn and no input for this long: the document draws the next page ahead
/// (PDF and CBZ pages take most of a second on the Kindle). Short against reading a
/// page, long enough that a quick second tap is read first.
pub const PREFETCH_IDLE: Duration = Duration::from_millis(250);

impl ReaderScreen {
    /// A reader for `path` (opened in `on_enter`).
    pub fn new(path: PathBuf) -> Self {
        ReaderScreen {
            path,
            doc: None,
            page: 0,
            dirty: None,
            rerender: true,
            error: None,
            page_buf: None,
            reveal: Vec::new(),
            swap_same: false,
            full_close: false,
            mark_change: false,
            sel_settle: None,
            overlay: None,
            toc: None,
            actions: Vec::new(),
            swallow_gestures: false,
            slide: None,
            marks: Vec::new(),
            listed_marks: Vec::new(),
            book_settings: BookSettings::default(),
            links: None,
            back: Vec::new(),
            back_pill: false,
            note_target: None,
            query: String::new(),
            hits: None,
            hit: 0,
            selecting: None,
            sel: None,
            sel_word: None,
            sel_end: None,
            sel_anchor_page: 0,
            sel_pages: Vec::new(),
            sel_edge: None,
            sel_edge_lock: false,
            bridge: None,
            sel_text_box: None,
            note_text: String::new(),
            sel_shown: Vec::new(),
            sel_dirty: false,
            session_hl: Vec::new(),
            saved_hl: Vec::new(),
            sel_highlight: false,
            status_shown: Default::default(),
            chapter_map: None,
            counting_shown: false,
            speed: footer::Speed::default(),
            status_bars_hidden: false,
            bars: footer::BarsState::default(),
            turn_wave: None,
            open_trace: None,
            plugins_pending: false,
            dict_card: None,
            dict_cancel: Vec::new(),
            position_unsaved: None,
            turned: false,
            jumped: false,
            turn_dir: 1,
            prefetch_at: None,
        }
    }

    /// The error shown instead of the book, if it could not be opened.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The 0-based page shown.
    pub fn page(&self) -> usize {
        self.page
    }

    /// The open overlay, if any.
    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref()
    }

    /// Overlay actions handled so far.
    pub fn actions(&self) -> &[String] {
        &self.actions
    }

    fn draw_error(&self, cx: &mut AppCtx) {
        let Some(err) = self.error.as_deref() else { return };
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.display().to_string());
        let max_w = cx.canvas.width as f32 * 0.86;
        let mut lines = vec!["Cannot open this book".to_string()];
        lines.extend(wrap_text(cx.fonts, &name, ERROR_PX, max_w, 2));
        lines.push(String::new());
        lines.extend(wrap_text(cx.fonts, err, ERROR_PX, max_w, ERROR_MAX_LINES));
        lines.push(String::new());
        lines.push("Tap to go back".to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        ink_ui::message::draw_message(cx.canvas, cx.fonts, &refs);
    }

    fn turn(&mut self, step: i64, cx: &mut AppCtx) {
        let Some(doc) = self.doc.as_ref() else { return };
        let total = doc.page_count().pages as i64;
        let next = (self.page as i64 + step).clamp(0, (total - 1).max(0)) as usize;
        self.bridge = None;
        if next != self.page {
            self.page = next;
            self.rerender = true;
            self.turned = true;
            self.turn_dir = step.signum();
            self.dirty = Some(cx.refresh.page_turn());
            // No right-to-left books yet: a next page sweeps left.
            self.turn_wave = ink_store::prefs::page_animation_on(&cx.settings.reader.page_animation).then(|| ink_device::Wave::for_turn(step > 0, false));
        }
    }

    /// Shows page `page` (clamped) as a jump (`Change::Jump`) without the footnote pill.
    /// Keeps the way back as it is; a jump the reader asked for goes through
    /// [`Self::navigate_to_page`].
    fn go_to_page(&mut self, page: usize) {
        self.back_pill = false;
        self.show_page(page);
    }

    /// A jump that is not a link (Contents, Go to, scrubber, search result, bookmark,
    /// plugin): remembers where the reader was, for the menu's "Go back".
    fn navigate_to_page(&mut self, page: usize) {
        self.remember_here(page, false);
        self.go_to_page(page);
    }

    /// Puts the shown page's position on the way back, unless the jump goes to `to` =
    /// this page. At most 50 places are kept.
    fn remember_here(&mut self, to: usize, link: bool) {
        if to == self.page {
            return;
        }
        let Some(doc) = self.doc.as_deref() else { return };
        if let Ok(position) = doc.position_of_page(self.page) {
            self.back.push(Return { position, link });
            if self.back.len() > 50 {
                self.back.remove(0);
            }
        }
    }

    /// Shows page `page` (clamped) as a jump (`Change::Jump`: REAGL, a GC16 per the
    /// user's flash count), keeping the link return place.
    fn show_page(&mut self, page: usize) {
        let Some(doc) = self.doc.as_ref() else { return };
        let last = doc.page_count().pages.saturating_sub(1);
        self.page = page.min(last);
        self.bridge = None;
        self.rerender = true;
        self.turned = false;
        // The class is picked when the page is shown (paint_screen: Change::Jump).
        self.raise_dirty(RefreshClass::Reagl);
        self.jumped = true;
        self.turn_wave = None;
    }

    /// The screen must be shown again with at least `class`.
    fn raise_dirty(&mut self, class: RefreshClass) {
        self.dirty = Some(self.dirty.map_or(class, |d| d.max(class)));
    }

    /// A highlight or bookmark changed: the page is drawn again and only the pixels that
    /// changed are refreshed (`mark_change`).
    fn marks_changed(&mut self) {
        self.rerender = true;
        self.mark_change = true;
    }

    /// The pages moved (relayout, zoom): page numbers kept for the TOC, the links and
    /// the search hits are wrong now.
    fn forget_page_numbers(&mut self) {
        self.toc = None;
        self.links = None;
        for h in self.hits.iter_mut().flatten() {
            h.page = None;
        }
    }

    fn save_position(&self) {
        let Some(doc) = self.doc.as_ref() else { return };
        let Ok(pos) = doc.position_of_page(self.page) else { return };
        let percent = saved_percent(doc.as_ref(), self.page);
        // The chapter title for the Home tab's hero card (library/home.rs). Only when the
        // TOC is already loaded (the footer loads it); saving never reads the TOC itself.
        let chapter = self.toc.as_deref().map(|toc| current_chapter(doc.as_ref(), toc, self.page).map(|i| toc[i].title.trim().to_string()).filter(|t| !t.is_empty()));
        self.update_sidecar("position", |state| {
            state.position = Some(pos.to_string());
            state.percent = Some(percent);
            match chapter {
                Some(Some(t)) => {
                    state.extra.insert("chapter".into(), t.into());
                }
                Some(None) => {
                    state.extra.remove("chapter");
                }
                None => {}
            }
        });
    }

    /// Loads the book's sidecar, changes it with `change` and writes it back. A failed
    /// write is logged ("cannot save `what`"); returns true when the write succeeded.
    fn update_sidecar(&self, what: &str, change: impl FnOnce(&mut BookState)) -> bool {
        let mut state = BookState::load(&self.path).unwrap_or_default();
        change(&mut state);
        match state.save(&self.path) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("cannot save {what}: {e}");
                false
            }
        }
    }

    /// The page shown is the reading position: logged now, written to the sidecar by
    /// [`ReaderScreen::flush_position`] later.
    fn note_position(&mut self) {
        let Some(doc) = self.doc.as_ref() else { return };
        let percent = saved_percent(doc.as_ref(), self.page);
        // tests/e2e/arch-reader-gestures.sh reads this line.
        log::info!("reader position page {} percent {percent:.4}", self.page + 1);
        let now = Instant::now();
        let first = self.position_unsaved.map_or(now, |(first, _)| first);
        self.position_unsaved = Some((first, now));
    }

    /// When the unsaved position is due to be written.
    fn position_due(&self) -> Option<Instant> {
        self.position_unsaved.map(|(first, last)| (last + POSITION_SAVE_IDLE).min(first + POSITION_SAVE_MAX))
    }

    /// Draws the page after the one shown, in the direction of the last turn, into the
    /// document's page cache (bounded by the document: 16 MiB of screens for MuPDF), so
    /// the next turn shows it at once. The target comes from the page shown now: a turn
    /// made since the timer was set moved the timer on, so a stale page is never drawn.
    /// Not while a selection is dragged or a half-page bridge is shown.
    fn prefetch_next(&mut self) {
        self.prefetch_at = None;
        if self.selecting.is_some() || self.bridge.is_some() {
            return;
        }
        let Some(doc) = self.doc.as_deref_mut() else { return };
        let Some(next) = self.page.checked_add_signed(self.turn_dir as isize) else { return };
        if next >= doc.page_count().pages {
            return;
        }
        let t = Instant::now();
        if doc.prefetch_page(next) {
            log::debug!("reader: page {} prefetched in {:.2} ms", next + 1, t.elapsed().as_secs_f64() * 1000.0);
        }
    }

    /// Writes the reading position to the sidecar if it changed since the last write.
    fn flush_position(&mut self) {
        if self.position_unsaved.take().is_none() {
            return;
        }
        let t = Instant::now();
        self.save_position();
        log::debug!("reader: position page {} saved in {} ms", self.page + 1, t.elapsed().as_millis());
    }

    /// Reads the table of contents into `self.toc` unless it is there (it is dropped
    /// when page numbers change). Callers borrow `self.toc` and `self.doc` side by side.
    fn load_toc(&mut self) {
        if self.toc.is_none() {
            self.toc = Some(self.doc.as_ref().map(|d| d.toc()).unwrap_or_default());
        }
    }

    /// What the menu bars show for the current page.
    fn menu_info(&mut self) -> MenuInfo {
        let page = self.page;
        self.load_toc();
        let toc = self.toc.as_deref().unwrap_or_default();
        let Some(doc) = self.doc.as_deref() else { return MenuInfo::default() };
        let count = doc.page_count();
        let chapter = current_chapter(doc, toc, page).map(|i| toc[i].title.clone());
        let bookmarked = page_marked(&self.marks, doc, page);
        MenuInfo {
            title: doc.metadata().title.clone(),
            page,
            pages: count.pages,
            complete: count.complete,
            estimate: footer::estimate(doc),
            // The footer's fraction, so the menu and the status bar show the same percent.
            percent: footer::bar_progress(doc, page),
            chapter,
            bookmarked,
        }
    }

    fn goto_info(&mut self, tab: usize) -> goto::GotoInfo {
        let page = self.page;
        self.load_toc();
        let toc = self.toc.as_deref().unwrap_or_default();
        let Some(doc) = self.doc.as_deref() else { return goto::GotoInfo::default() };
        let rows = toc
            .iter()
            .map(|e| goto::TocRow { title: e.title.clone(), level: e.level, page: e.page.or_else(|| doc.page_of_known(&e.position)) })
            .collect();
        let mut marks: Vec<(Option<usize>, Bookmark)> = self.marks.iter().map(|m| (m.page(doc), m.bookmark.clone())).collect();
        marks.sort_by_key(|(p, _)| p.unwrap_or(usize::MAX));
        let bookmarks = marks
            .iter()
            .map(|(p, b)| goto::BookmarkRow {
                title: b.chapter.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| b.label.clone()),
                subtitle: b.created.clone(),
                page: *p,
            })
            .collect();
        self.listed_marks = marks.into_iter().map(|(_, b)| b).collect();
        goto::GotoInfo { tab, toc: rows, current: current_chapter(doc, toc, page), bookmarks, percent: footer::bar_progress(doc, page) }
    }

    /// Adds a bookmark on the current page, or removes the ones on it. Returns true when
    /// the page is bookmarked afterwards.
    fn mark_page_toggle(&mut self) -> bool {
        let page = self.page;
        self.load_toc();
        let toc = self.toc.as_deref().unwrap_or_default();
        let Some(doc) = self.doc.as_deref() else { return false };
        let n = self.marks.len();
        self.marks.retain(|m| m.page(doc) != Some(page));
        let marked = if self.marks.len() == n {
            let Ok(pos) = doc.position_of_page(page) else { return false };
            let chapter = current_chapter(doc, toc, page).map(|i| toc[i].title.clone());
            // Parsed back from its text, as when the book is opened again.
            self.marks.push(Mark::new(Bookmark { position: pos.to_string(), label: format!("Page {}", page + 1), created: timestamp(), chapter, page: fixed_page(doc, page), note: None }));
            true
        } else {
            false
        };
        self.update_sidecar("bookmarks", |state| state.bookmarks = self.marks.iter().map(|m| m.bookmark.clone()).collect());
        log::info!("reader bookmark page {}: {}", page + 1, if marked { "added" } else { "removed" });
        marked
    }

    /// A row of the Go to panel was tapped: the panel closes and the reader goes to the
    /// row's place `pos` (nothing when the row is gone).
    fn go_to_row(&mut self, pos: Option<Position>) {
        if let Some(pos) = pos {
            self.close_overlay();
            self.go_to_position(&pos);
        }
    }

    /// Goes to `pos` (a TOC entry, bookmark or search hit), remembering the way back.
    fn go_to_position(&mut self, pos: &Position) {
        let Some(doc) = self.doc.as_deref() else { return };
        match doc.page_of(pos) {
            Ok(p) => self.navigate_to_page(p),
            Err(e) => log::warn!("reader: cannot go to {pos}: {e}"),
        }
    }

    /// The link under `pos` on the shown page (rectangles widened for fingers).
    fn link_at(&mut self, pos: Point) -> Option<Link> {
        let page = self.page;
        let doc = self.doc.as_deref_mut()?;
        if self.links.as_ref().is_none_or(|(p, _)| *p != page) {
            let links = doc.position_of_page(page).ok().and_then(|p| doc.links(&p).ok()).unwrap_or_default();
            self.links = Some((page, links));
        }
        let (_, links) = self.links.as_ref()?;
        links.iter().find(|l| l.rect.is_some_and(|r| near(&r, pos, 18))).cloned()
    }

    /// Follows a tapped link: a footnote opens a popup, another internal link jumps
    /// (remembering where from), an external link is only shown.
    fn follow_link(&mut self, link: Link, cx: &mut AppCtx) {
        log::info!("reader link {:?} footnote {}", link.target, link.footnote);
        let size = cx.screen.size();
        let anchor = link.rect.unwrap_or_default();
        let popup = |title: String, text: String, spans: Vec<(String, Style)>, buttons: Vec<(String, String)>| {
            let mut o = Overlay::over_page(overlay::OverlayKind::Popup, size, "close");
            // Height from the text length (about 17 px per character at the popup's
            // 34 px text), between 320 px and 60 % of the screen.
            let pw = (size.w - 48).min(1100);
            let per_line = ((pw as usize).saturating_sub(56) / 17).max(10);
            let lines = text.chars().count().div_ceil(per_line) as u32 + 1;
            // The title is a step larger than the text (ink_ui::Popup).
            let ph = (134 + lines * 46 + 130).clamp(320, size.h * 3 / 5);
            let psize = Size::new(pw, ph);
            let frame = ink_ui::Popup::frame(anchor, psize, size);
            o.buttons.extend(buttons.iter().enumerate().map(|(i, (_, a))| {
                // Same split as the popup's own button row.
                let body = Rect::new(frame.x, frame.y, frame.w - 8, frame.h - 8);
                let row = Rect::new(body.x + 16, body.bottom() - 96 - 12, body.w - 32, 96);
                (a.clone(), ink_ui::widget::split_cols(row, buttons.len() as u32, 12)[i])
            }));
            o.tree.add(None, Box::new(ink_ui::Popup { title, text, buttons, spans }), frame);
            o
        };
        match link.target {
            LinkTarget::Internal(target) => {
                let note = if link.footnote { self.doc.as_deref_mut().and_then(|d| d.note(&target).ok().flatten()) } else { None };
                match note {
                    Some(mut n) => {
                        let title = note_popup_title(&mut n, &link.text);
                        self.note_target = Some(n.position.clone());
                        let buttons = vec![("Go to note".to_string(), "note-go".to_string()), ("Close".to_string(), "close".to_string())];
                        let o = popup(title, note_excerpt(&n.text, 700), note_spans(&n, 700), buttons);
                        self.open_overlay(o, cx);
                    }
                    None => self.jump(&target, link.footnote),
                }
            }
            LinkTarget::External(url) => {
                let text = format!("{url} (the reader does not open web links)");
                let o = popup("Web link".to_string(), text, Vec::new(), vec![("Close".to_string(), "close".to_string())]);
                self.open_overlay(o, cx);
            }
        }
    }

    /// Follows an in-book link or footnote to `target`, remembering the current
    /// position for "Back to p. N". `pill`: draw the Back pill on the page (footnotes
    /// only); otherwise the way back is in the Aa sheet and the back gesture.
    fn jump(&mut self, target: &Position, pill: bool) {
        let Some(doc) = self.doc.as_deref() else { return };
        let to = match doc.page_of(target) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("reader: cannot follow link to {target}: {e}");
                return;
            }
        };
        self.remember_here(to, true);
        self.back_pill = pill;
        log::info!("reader: link from page {} to page {} (back pill {})", self.page + 1, to + 1, if pill { "shown" } else { "hidden" });
        self.show_page(to);
    }

    /// True when the Back pill is on the page (a footnote was followed).
    pub(super) fn pill_shown(&self) -> bool {
        self.back_pill && self.back.last().is_some_and(|r| r.link)
    }

    /// The 0-based page "Back to p. N" returns to: where the last followed in-book link
    /// or footnote was. `None` when the last jump was not a link (or the place is
    /// unknown).
    pub fn link_return_page(&self) -> Option<usize> {
        self.back.last().filter(|r| r.link).and_then(|_| self.return_page())
    }

    /// The 0-based page the menu's "Go back" returns to: where the last jump of any
    /// kind started.
    pub fn return_page(&self) -> Option<usize> {
        let doc = self.doc.as_deref()?;
        self.back.last().and_then(|r| doc.page_of(&r.position).ok())
    }

    /// Returns to where the last followed in-book link or footnote was (the "back"
    /// action, the Back key, the "Back to p. N" pill). False when the last jump was not
    /// a link: a back swipe after a search turns the page instead.
    pub fn go_back_from_link(&mut self) -> bool {
        if !self.back.last().is_some_and(|r| r.link) {
            self.back_pill = false;
            log::info!("reader: back: no link to return from");
            return false;
        }
        self.go_back()
    }

    /// Returns to where the last jump started (the menu's "Go back"). False when there
    /// is none.
    pub fn go_back(&mut self) -> bool {
        self.back_pill = false;
        let Some(r) = self.back.pop() else {
            log::info!("reader: go back: nowhere to return to");
            return false;
        };
        match self.doc.as_deref().map(|d| d.page_of(&r.position)) {
            Some(Ok(page)) => {
                log::info!("reader: go back to page {} ({})", page + 1, if r.link { "link" } else { "jump" });
                self.show_page(page);
            }
            Some(Err(e)) => log::warn!("reader: cannot go back to {}: {e}", r.position),
            None => {}
        }
        true
    }

    /// Saves the current selection as a highlight (with `note`, if any) and remembers
    /// its rectangles.
    fn add_highlight(&mut self, note: Option<String>) {
        // A selection continued on another page: its first page names the chapter.
        let pages = match self.sel.as_ref() {
            Some(s) if self.sel_pages.iter().any(|(p, _)| *p == s.page) => self.sel_pages.clone(),
            Some(s) => vec![(s.page, s.rects.clone())],
            None => return,
        };
        let page = pages.first().map_or(self.page, |(p, _)| *p);
        self.load_toc();
        let toc = self.toc.as_deref().unwrap_or_default();
        let (Some(doc), Some(sel)) = (self.doc.as_deref(), self.sel.as_ref()) else { return };
        let h = Highlight {
            start: sel.start.to_string(),
            end: sel.end.to_string(),
            text: sel.text.clone(),
            style: "lighten".into(),
            chapter: current_chapter(doc, toc, page).map(|i| toc[i].title.clone()),
            created: timestamp(),
            page: fixed_page(doc, page),
            note: note.filter(|n| !n.trim().is_empty()),
            ..Highlight::default()
        };
        let h_note = h.note.as_ref().map_or(0, |n| n.chars().count());
        let key = doc.layout().cache_key();
        self.session_hl.extend(pages.iter().map(|(p, rects)| SessionMark { layout: key.clone(), page: *p, rects: rects.clone(), start: sel.start.clone() }));
        self.saved_hl.push((sel.start.clone(), sel.end.clone()));
        if self.update_sidecar("highlight", |state| state.add_highlight(h)) {
            log::info!("reader highlight on page {} ({} pages): {} chars, note {}", page + 1, pages.len(), sel.text.chars().count(), h_note);
        }
    }

    /// The saved highlight under `pos` on the shown page: its range and rectangles. A
    /// document that cannot map a saved range (fixed layouts) has the rectangles kept
    /// for the highlights made this session.
    fn highlight_at(&mut self, pos: Point) -> Option<(Position, Position, Vec<Rect>)> {
        let near = |rects: &[Rect]| rects.iter().any(|r| near(r, pos, 8));
        let page = self.page;
        let doc = self.doc.as_deref_mut()?;
        let mapped = self.saved_hl.iter().rev().find_map(|(start, end)| {
            let rects = doc.range_rects(page, start, end);
            near(&rects).then(|| (start.clone(), end.clone(), rects))
        });
        if mapped.is_some() {
            return mapped;
        }
        if self.session_hl.is_empty() {
            return None;
        }
        let key = doc.layout().cache_key();
        let m = self.session_hl.iter().rev().find(|m| m.on(&key, page) && near(&m.rects))?;
        let (start, end) = self.saved_hl.iter().find(|(s, _)| *s == m.start)?;
        Some((start.clone(), end.clone(), m.rects.clone()))
    }

    /// Selects the saved highlight `start`..`end` (drawn at `rects` on the shown page)
    /// and opens its card, as after selecting the text by hand: plugins read it as the
    /// selection, and the card offers "Remove highlight".
    fn open_highlight(&mut self, start: Position, end: Position, rects: Vec<Rect>, cx: &mut AppCtx) {
        let (s, e) = (start.to_string(), end.to_string());
        let text = BookState::load(&self.path).ok().and_then(|st| st.highlights.into_iter().find(|h| h.start == s && h.end == e)).map(|h| h.text).unwrap_or_default();
        log::info!("reader: tap on a highlight on page {} ({} chars)", self.page + 1, text.chars().count());
        let sel = Selection { start, end, text, page: self.page, rects: rects.clone() };
        if let Some(d) = self.doc.as_deref_mut() {
            d.set_selection(sel.clone());
        }
        self.sel = Some(sel.clone());
        self.sel_pages = vec![(self.page, rects)];
        self.sel_highlight = true;
        let o = self.selection_overlay(&sel, None, None, cx);
        self.open_overlay(o, cx);
    }

    /// Removes the saved highlight `start`..`end` from the page and the book's sidecar.
    fn remove_highlight(&mut self, start: &Position, end: &Position) {
        self.saved_hl.retain(|(s, e)| !(s == start && e == end));
        self.session_hl.retain(|m| m.start != *start);
        let mut removed = false;
        if self.update_sidecar("the removed highlight", |state| removed = state.remove_highlight(&start.to_string(), &end.to_string())) {
            log::info!("reader: highlight removed ({})", if removed { "from the sidecar" } else { "not in the sidecar" });
        }
    }

    /// Selection by touch, by whole words: the first hold on text selects the word under
    /// the finger; moving the finger extends the selection word by word (never by
    /// characters); the lift opens the selection card, or, when the finger stayed on the
    /// one word and `gestures.long_press` is `dictionary`, looks the word up at once
    /// with the dictionary plugin's selection action. Returns true when the event was
    /// used. Called only when a long press selects ([`actions::long_press_selects`]).
    fn select_touch(&mut self, t: &ink_ui::Touch, cx: &mut AppCtx) -> bool {
        use ink_ui::TouchKind;
        if t.slot != 0 {
            return false;
        }
        let page = self.page;
        match (t.kind, self.selecting) {
            (TouchKind::Hold, None) if self.sel.is_none() => {
                let Some(doc) = self.doc.as_deref_mut() else { return false };
                // A hold away from any text (blank space, margins) is not a selection.
                let t0 = Instant::now();
                match selection::word_hit(doc, page, t.pos) {
                    Some(w) if w.sel.rects.iter().any(|r| near(r, t.pos, 30)) => {
                        log::info!(
                            "reader: long press at {},{} selects {:?} on page {} ({} ms)",
                            t.pos.x,
                            t.pos.y,
                            note_excerpt(&w.sel.text, 60),
                            page + 1,
                            t0.elapsed().as_millis()
                        );
                        self.selecting = Some(t.pos);
                        self.sel_end = selection::bounds(&w.sel.rects).map(|r| (r, t.pos));
                        self.sel_anchor_page = page;
                        self.sel_pages = vec![(page, w.sel.rects.clone())];
                        self.leave_edge();
                        self.sel_text_box = None;
                        self.sel = Some(w.sel.clone());
                        self.sel_word = Some(w);
                        self.sel_dirty = true;
                        true
                    }
                    _ => {
                        log::info!("reader: long press at {},{}: no word there", t.pos.x, t.pos.y);
                        doc.clear_selection();
                        false
                    }
                }
            }
            (TouchKind::Move | TouchKind::Hold, Some(_)) => {
                let p = t.pos;
                let size = cx.screen.size();
                // Off the text at the bottom or top: the page turns after a short rest
                // there and the selection continues on the next (previous) page. The
                // selection itself does not change in the zone.
                // (Only once the finger moved: a press on the last line is no turn.)
                let zone = if t.moved { selection::edge_zone(p, size, self.page_text_box(size)) } else { 0 };
                if zone != 0 {
                    if !self.sel_edge_lock && self.sel_edge.is_none_or(|(d, _)| d != zone) {
                        log::debug!("reader: selection drag rests in the {} turn zone", if zone > 0 { "next-page" } else { "previous-page" });
                        self.sel_edge = Some((zone, Instant::now()));
                    }
                    self.sel_edge_turn(cx);
                    return true;
                }
                self.leave_edge();
                if self.sel_end.is_some_and(|(r, q)| r.contains(p) || ((q.x - p.x).abs() < 24 && (q.y - p.y).abs() < 24)) {
                    return true;
                }
                let Some(doc) = self.doc.as_deref_mut() else { return false };
                if self.sel_word.is_none() {
                    return false;
                }
                // Half a page on: the point is on the upper or the lower page.
                let (page, q) = match self.bridge {
                    Some(b) => b.to_page(p),
                    None => (page, p),
                };
                let Some(end) = selection::word_hit(doc, page, q) else {
                    // Between words or lines: keep the selection as it is.
                    return true;
                };
                let rects = match self.bridge {
                    Some(b) => b.to_screen(&[(page, end.sel.rects.clone())]),
                    None => end.sel.rects.clone(),
                };
                self.sel_end = selection::bounds(&rects).map(|r| (r, p));
                self.extend_selection(&selection::SpanEnd::Word(end), size, page);
                true
            }
            (TouchKind::Up, Some(_)) => {
                self.end_drag();
                self.swallow_gestures = true;
                let Some(sel) = self.sel.clone() else {
                    self.end_bridge();
                    return true;
                };
                self.sel_settle = selection::bounds(&self.sel_screen_rects());
                let Some(doc) = self.doc.as_deref_mut() else { return true };
                let one_word = self.sel_pages.len() <= 1 && self.sel_word.as_ref().is_some_and(|w| w.sel.rects == sel.rects);
                let pages = match (self.sel_pages.first(), self.sel_pages.last()) {
                    (Some(a), Some(b)) if a.0 != b.0 => format!("pages {}-{}", a.0 + 1, b.0 + 1),
                    _ => format!("page {}", sel.page + 1),
                };
                log::info!("reader selection {pages}: {:?} ({})", note_excerpt(&sel.text, 60), if one_word { "one word" } else { "extended" });
                cx.plugins.selection(doc, &sel);
                // One word with long_press = dictionary: the lookup runs at once and its
                // card becomes a sibling of the selection card in one overlay (a tap on
                // the page closes both).
                let mut card = None;
                if one_word && cx.settings.gestures.action("long_press") == "dictionary" {
                    match cx.plugins.selection_actions().iter().find(|a| a.id == "lookup").map(|a| (a.plugin.clone(), a.id.clone())) {
                        Some((plugin, id)) => {
                            log::info!("reader: long press looks up {:?} with {plugin}", sel.text);
                            cx.plugins.trigger(&plugin, &id, Some(doc));
                            match cx.plugins.take_card_dialog(&plugin) {
                                Some((rid, c)) => card = Some((plugin, rid, c)),
                                None => log::info!("reader: {plugin} answered the lookup without a card; it shows in the plugin layer"),
                            }
                        }
                        None => log::info!("reader: no dictionary lookup action; selection card only"),
                    }
                }
                let o = self.selection_overlay(&sel, card, None, cx);
                self.open_overlay(o, cx);
                true
            }
            (TouchKind::Cancel, Some(_)) => {
                self.end_drag();
                self.close_overlay();
                self.sel_dirty = true;
                true
            }
            _ => false,
        }
    }

    /// The dragged end left the page-turn zones: no turn pending, and the next rest in a
    /// zone may turn again.
    fn leave_edge(&mut self) {
        self.sel_edge = None;
        self.sel_edge_lock = false;
    }

    /// The finger that dragged the selection lifted (or the touch was cancelled).
    fn end_drag(&mut self) {
        self.selecting = None;
        self.sel_end = None;
        self.leave_edge();
    }

    /// Extends the selection from the word it started on to `end` on the page shown
    /// (across pages when the drag turned the page; `select` and `set_selection` set the
    /// document's selection, which plugins read).
    /// `page`: the page `end` is on (the page shown, or in a bridge either of its pages).
    fn extend_selection(&mut self, end: &selection::SpanEnd, size: Size, page: usize) {
        let (Some(doc), Some(word)) = (self.doc.as_deref_mut(), self.sel_word.as_ref()) else { return };
        let Some(span) = selection::span_select(doc, size, self.sel_anchor_page, word, page, end) else { return };
        if self.sel.as_ref().is_none_or(|s| s.page != span.sel.page || s.rects != span.sel.rects || s.text != span.sel.text) {
            log::debug!("reader: selection extended to {:?} ({} pages)", note_excerpt(&span.sel.text, 60), span.pages.len());
            self.sel = Some(span.sel);
            self.sel_dirty = true;
        }
        self.sel_pages = span.pages;
    }

    /// Screen rectangles of the selection's part on screen (both pages of a bridge).
    fn sel_screen_rects(&self) -> Vec<Rect> {
        match (self.bridge, self.sel.as_ref()) {
            (Some(b), Some(_)) => b.to_screen(&self.sel_pages),
            (None, Some(s)) if s.page == self.page => s.rects.clone(),
            _ => Vec::new(),
        }
    }

    /// Leaves a bridge: shows the page the selection ended on, drawn whole.
    fn end_bridge(&mut self) {
        if let Some(b) = self.bridge {
            let page = self.sel.as_ref().map_or(b.page, |s| s.page);
            log::info!("reader: selection done, the half-page view goes back to page {}", page + 1);
            self.show_page(page);
        }
    }

    /// The first line's top and the last line's bottom of the page shown (cached).
    fn page_text_box(&mut self, size: Size) -> Option<(i32, i32)> {
        match self.sel_text_box {
            _ if self.bridge.is_some() => self.bridge.map(|b| b.text_box()),
            Some((p, b)) if p == self.page => b,
            _ => {
                let doc = self.doc.as_deref_mut()?;
                let b = selection::text_box(doc, self.page, size);
                // `text_box` cleared the document's selection: set it again.
                if let Some(s) = self.sel.clone() {
                    doc.set_selection(s);
                }
                self.sel_text_box = Some((self.page, b));
                b
            }
        }
    }

    /// The dragged selection end rested in a turn zone for [`selection::EDGE_DWELL`]:
    /// moves on half a page ([`selection::Bridge`]; a whole page turn when a page has
    /// no lines to cut) and continues the selection there: forward at the first word
    /// below what was shown (the next page's first word, or the word after the lower
    /// part of a bridge), back at the last word above it. That word sits in the middle
    /// of the screen with the text before it still in sight. Returns true when the view
    /// moved.
    fn sel_edge_turn(&mut self, cx: &mut AppCtx) -> bool {
        let Some((dir, since)) = self.sel_edge else { return false };
        if self.sel_edge_lock || self.selecting.is_none() || since.elapsed() < selection::EDGE_DWELL {
            return false;
        }
        self.sel_edge = None;
        self.sel_edge_lock = true;
        let size = cx.screen.size();
        let anchor = self.sel_anchor_page;
        let Some(doc) = self.doc.as_deref_mut() else { return false };
        let total = doc.page_count().pages;
        let fits = |p: usize| p < total && p.abs_diff(anchor) < selection::MAX_SPAN_PAGES;
        // The view (page, bridge) and where the selection continues (page, end).
        let (page, bridge, end_page, end) = match (self.bridge, dir > 0) {
            (None, true) if fits(self.page + 1) => {
                let next = self.page + 1;
                let b = selection::Bridge::new(doc, self.page, size);
                let end = selection::edge_word(doc, next, size, true);
                (if b.is_some() { self.page } else { next }, b, next, end)
            }
            (Some(b), true) => {
                let next = b.page + 1;
                let end = if b.cut2 > b.top2 { selection::line_word(doc, next, size, b.cut2 - 2, false) } else { selection::edge_word(doc, next, size, true) };
                (next, None, next, end)
            }
            (None, false) if self.page > 0 && fits(self.page - 1) => {
                let prev = self.page - 1;
                let b = selection::Bridge::new(doc, prev, size);
                let end = selection::edge_word(doc, prev, size, false);
                (prev, b, prev, end)
            }
            (Some(b), false) => (b.page, None, b.page, selection::line_word(doc, b.page, size, b.cut + 2, true)),
            _ => {
                log::info!("reader: selection drag at the {} edge: no page to continue on", if dir > 0 { "bottom" } else { "top" });
                return false;
            }
        };
        self.page = page;
        self.bridge = bridge;
        self.rerender = true;
        self.turned = true;
        self.dirty = Some(cx.refresh.page_turn());
        self.turn_wave = None;
        self.sel_text_box = None;
        // The new view is drawn whole, with its part of the selection inverted.
        self.sel_shown.clear();
        self.sel_end = None;
        self.extend_selection(&end, size, end_page);
        log::info!(
            "reader: selection continues on page {} ({}; from page {}): {} chars",
            end_page + 1,
            match bridge {
                Some(b) => format!("half-page view of pages {}-{}", b.page + 1, b.page + 2),
                None => format!("page {} whole", page + 1),
            },
            anchor + 1,
            self.sel.as_ref().map_or(0, |s| s.text.chars().count())
        );
        true
    }

    /// Result rows of the last search.
    fn hit_rows(&self) -> Option<Vec<search::HitRow>> {
        let doc = self.doc.as_deref()?;
        let hits = self.hits.as_ref()?;
        Some(
            hits.iter()
                .map(|h| search::HitRow {
                    excerpt: search::excerpt(&h.excerpt, h.matched.clone()),
                    page: h.page.or_else(|| doc.page_of(&h.position).ok()),
                })
                .collect(),
        )
    }

    /// The chapter title of each hit's page (empty when unknown). The TOC pages are
    /// looked up once, not once per hit.
    fn hit_chapters(&mut self, rows: &[search::HitRow]) -> Vec<String> {
        self.load_toc();
        let toc = self.toc.as_deref().unwrap_or_default();
        let Some(doc) = self.doc.as_deref() else { return Vec::new() };
        let starts: Vec<(usize, &str)> = toc.iter().filter_map(|e| Some((e.page.or_else(|| doc.page_of(&e.position).ok())?, e.title.trim()))).collect();
        rows.iter()
            .map(|r| {
                let Some(page) = r.page else { return String::new() };
                starts.iter().rev().find(|(p, _)| *p <= page).map(|(_, t)| t.to_string()).unwrap_or_default()
            })
            .collect()
    }

    /// Opens the search page (keyboard shown when asked or before the first search).
    fn open_search_page(&mut self, keyboard: bool, cx: &mut AppCtx) {
        let rows = self.hit_rows();
        let keyboard = keyboard || rows.is_none();
        let chapters = match rows.as_deref() {
            Some(rows) if !rows.is_empty() => self.hit_chapters(rows),
            _ => Vec::new(),
        };
        let mut o = search::build_with_chapters(&self.query, rows.as_deref(), &chapters, keyboard, cx.screen.size());
        // The enabled keyboard languages (`ui.keyboard_layouts`).
        if let Some(kb) = o.ids.get("keyboard").and_then(|id| o.tree.get_mut::<ink_ui::Keyboard>(*id)) {
            *kb = crate::screens::settings::keyboard_layouts::keyboard(cx.settings);
        }
        self.open_overlay(o, cx);
    }

    /// Searches the book for the field's text and shows the results.
    fn run_search(&mut self, cx: &mut AppCtx) {
        let q = self.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let Some(doc) = self.doc.as_deref_mut() else { return };
        let t = Instant::now();
        let hits = doc.search(&q, search::MAX_HITS).unwrap_or_else(|e| {
            log::warn!("reader search '{q}': {e}");
            Vec::new()
        });
        log::info!("reader search '{q}': {} hits in {:?}", hits.len(), t.elapsed());
        self.hits = Some(hits);
        self.open_search_page(false, cx);
    }

    /// Shows hit `i` with the hit bar.
    fn show_hit(&mut self, i: usize, cx: &mut AppCtx) {
        let Some(hits) = self.hits.as_ref() else { return };
        let Some(h) = hits.get(i) else { return };
        let (pos, total) = (h.position.clone(), hits.len());
        self.hit = i;
        // From the results list: a jump to remember. Stepping hit to hit keeps the way
        // back to where the search started.
        if self.overlay.as_ref().is_some_and(|o| o.kind == overlay::OverlayKind::Hits) {
            if let Some(Ok(p)) = self.doc.as_deref().map(|d| d.page_of(&pos)) {
                self.go_to_page(p);
            }
        } else {
            self.go_to_position(&pos);
        }
        self.open_overlay(search::hit_bar(i, total, cx.screen.size()), cx);
    }

    /// Saves the selection as a highlight with the note editor's text and closes it.
    fn save_note(&mut self) {
        let note = std::mem::take(&mut self.note_text);
        log::info!("reader: note saved ({} chars)", note.chars().count());
        self.add_highlight(Some(note));
        self.close_overlay();
        self.marks_changed();
    }

    /// Feeds the keys typed on the search (or note) keyboard into the field.
    fn search_keys(&mut self, cx: &mut AppCtx) {
        let Some(o) = self.overlay.as_mut() else { return };
        let note = o.kind == overlay::OverlayKind::Note;
        let (Some(&kid), Some(&fid)) = (o.ids.get("keyboard"), o.ids.get("field")) else { return };
        let actions = o.tree.get_mut::<ink_ui::Keyboard>(kid).map(|k| k.take_actions()).unwrap_or_default();
        for a in actions {
            let Some(field) = o.tree.get_mut::<ink_ui::TextInput>(fid) else { return };
            match field.apply(&a) {
                ink_ui::input::InputResult::Edited => {
                    if note {
                        self.note_text = field.text.clone();
                    } else {
                        self.query = field.text.clone();
                    }
                    o.tree.mark_dirty(fid, RefreshClass::Du);
                }
                ink_ui::input::InputResult::Submitted if note => {
                    self.note_text = field.text.clone();
                    self.save_note();
                    return;
                }
                ink_ui::input::InputResult::Closed if note => {
                    log::info!("reader: note cancelled");
                    self.close_overlay();
                    return;
                }
                ink_ui::input::InputResult::Submitted => {
                    self.query = field.text.clone();
                    self.run_search(cx);
                    return;
                }
                ink_ui::input::InputResult::Closed => {
                    self.open_search_page(false, cx);
                    return;
                }
                _ => {}
            }
        }
    }

    /// The selection overlay for `sel`: the page scrim, the selection card and, with
    /// `card` (`(plugin, request id, card)`), the plugin's card as a sibling of the
    /// selection card (the card at the bottom, the selection card kept above it; without a
    /// card, above `keep_above` when given: where a closed card was, so the selection
    /// card does not move). Sets [`ReaderScreen::dict_card`].
    fn selection_overlay(&mut self, sel: &Selection, card: Option<(String, u32, crate::plugin_host::CardDialog)>, keep_above: Option<i32>, cx: &mut AppCtx) -> Overlay {
        let size = cx.screen.size();
        // In a half-page view the card sits by the selection where it is shown.
        let shown;
        let sel = match self.bridge {
            Some(b) => {
                shown = Selection { rects: b.to_screen(&self.sel_pages), ..sel.clone() };
                &shown
            }
            None => sel,
        };
        let (extra, insightful) = selection_entries(cx);
        let card = card.map(|(plugin, id, mut c)| {
            c.prefix = PCARD.into();
            let r = c.frame(&mut PaintCx::new(cx.canvas, cx.fonts, cx.theme), size);
            (plugin, id, c, r)
        });
        let mut o = selection::popup(sel, &extra, insightful, self.sel_highlight, size, card.as_ref().map(|(.., r)| r.y).or(keep_above));
        self.dict_card = None;
        if let Some((plugin, id, c, r)) = card {
            log::info!("reader: {plugin} card {id} {:?} at {r:?}, sibling of the selection card (pages {:?})", c.title, c.pages);
            o.buttons.extend(c.action_rects(r));
            o.ids.insert("dict", o.tree.add(None, Box::new(c), r));
            self.dict_card = Some((plugin, id));
        }
        o
    }

    /// A button or swipe of the plugin card in the selection overlay (`pcard:<result>`):
    /// the plugin gets the answer. `next`/`prev` (a swipe on a paged card) put the
    /// plugin's new card in place of the old one (same rectangle: one partial update);
    /// any other answer closes the card and leaves the selection card.
    fn plugin_card_action(&mut self, result: &str, cx: &mut AppCtx) {
        let Some((plugin, id)) = self.dict_card.clone() else {
            log::warn!("reader: card action {result:?} with no card open");
            return;
        };
        let doc = self.doc.as_deref_mut().map(|d| d as &mut dyn Document);
        cx.plugins.answer(&plugin, id, result, doc);
        let Some(sel) = self.sel.clone() else { return };
        if matches!(result, "next" | "prev") {
            let Some((rid, mut c)) = cx.plugins.take_card_dialog(&plugin) else {
                log::info!("reader: {plugin} sent no card for {result}; the card stays");
                return;
            };
            let size = cx.screen.size();
            c.prefix = PCARD.into();
            let r = c.frame(&mut PaintCx::new(cx.canvas, cx.fonts, cx.theme), size);
            let Some(o) = self.overlay.as_mut() else { return };
            let wid = o.ids.get("dict").copied();
            let old = wid.and_then(|w| o.tree.bounds(w));
            if let (Some(w), true, true) = (wid, old == Some(r), rid == id) {
                log::info!("reader: {plugin} card {result}: {:?} page {:?} in place", c.title, c.pages);
                o.buttons.retain(|(a, _)| !a.starts_with(PCARD));
                o.buttons.extend(c.action_rects(r));
                if let Some(slot) = o.tree.get_mut::<crate::plugin_host::CardDialog>(w) {
                    *slot = c;
                }
                o.tree.mark_dirty(w, RefreshClass::Gl16);
            } else {
                // Another size or request: the overlay is built again with the new card.
                log::info!("reader: {plugin} card {result}: new card at {r:?} (was {old:?}), overlay rebuilt");
                self.reveal_selection_under(old);
                let o = self.selection_overlay(&sel, Some((plugin, rid, c)), None, cx);
                self.open_overlay(o, cx);
            }
            return;
        }
        // The card closes; the selection card stays (as when the card was a plugin layer).
        log::info!("reader: {plugin} card closed by {result:?}; selection card stays");
        self.dict_card = None;
        let old = self.overlay.as_ref().and_then(|o| o.ids.get("dict").and_then(|w| o.tree.bounds(*w)));
        self.reveal_selection_under(old);
        let o = self.selection_overlay(&sel, None, old.map(|r| r.y), cx);
        self.open_overlay(o, cx);
    }

    /// The card that covered `r` goes away: when it covered the inverted selection, the
    /// selection is drawn again (the page buffer under the card has no inversion).
    fn reveal_selection_under(&mut self, r: Option<Rect>) {
        if let (Some(r), Some(b)) = (r, selection::bounds(&self.sel_shown)) {
            if !r.intersect(&b).is_empty() {
                self.sel_dirty = true;
            }
        }
    }

    /// Sends the empty answer to plugin cards that went away without a button.
    fn flush_card_cancels(&mut self, cx: &mut AppCtx) {
        for (plugin, id) in std::mem::take(&mut self.dict_cancel) {
            log::info!("reader: {plugin} card {id} dismissed");
            let doc = self.doc.as_deref_mut().map(|d| d as &mut dyn Document);
            cx.plugins.answer(&plugin, id, "", doc);
        }
    }

    fn open_overlay(&mut self, o: Overlay, cx: &mut AppCtx) {
        // The plugin card goes with the overlay it was in.
        if !o.ids.contains_key("dict") {
            if let Some(d) = self.dict_card.take() {
                self.dict_cancel.push(d);
            }
        }
        // Leaving the hit bar for another overlay: the page loses its hit marks.
        if self.overlay.as_ref().is_some_and(|x| x.kind == overlay::OverlayKind::Hits) && o.kind != overlay::OverlayKind::Hits {
            self.rerender = true;
        }
        // Replacing an overlay: the page comes back where the old one was (from the
        // page buffer); the old rects and the new overlay are shown with GL16.
        if let Some(old) = self.overlay.as_ref() {
            self.reveal.extend(old.tree.painted_rects());
        }
        self.swap_same = self.overlay.as_ref().is_some_and(|old| old.kind == o.kind);
        log::info!("reader overlay {:?}", o.kind);
        let (rot, panel) = (cx.screen.rotation(), cx.screen.panel_size());
        for (a, r) in &o.buttons {
            let p = rot.to_panel(Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2), panel);
            log::info!("reader button {a} panel {} {}", p.x, p.y);
        }
        self.overlay = Some(o);
    }

    /// Lays the book out again for the current screen size and settings, keeping the
    /// reading position (Aa changes, rotation).
    fn relayout(&mut self, cx: &mut AppCtx, class: RefreshClass) -> anyhow::Result<()> {
        let Some(doc) = self.doc.as_deref_mut() else { return Ok(()) };
        let size = cx.screen.size();
        let layout = aa::book_layout(cx.settings, &self.book_settings, size.w, size.h);
        if &layout != doc.layout() {
            doc.set_current_page(self.page);
            doc.set_layout(&layout)?;
            self.page = doc.current_page();
            self.forget_page_numbers();
            self.rerender = true;
            self.dirty = Some(class);
            self.turned = true;
        }
        Ok(())
    }

    /// Status bar contents for the current page (without device values).
    fn footer_info(&mut self) -> Option<footer::FooterInfo> {
        // The layout or the pagination state changed (e.g. background pagination
        // finished): the cached TOC holds page numbers from before, so read it again.
        let stale = !self.chapter_map.as_ref().is_some_and(|m| self.doc.as_deref().is_some_and(|d| m.is_for(d)));
        if stale {
            self.toc = None;
        }
        self.load_toc();
        let toc = self.toc.as_deref().unwrap_or_default();
        let doc = self.doc.as_deref()?;
        if stale {
            self.chapter_map = Some(footer::ChapterMap::build(doc, toc));
        }
        let map = self.chapter_map.as_ref()?;
        self.counting_shown = !doc.page_count().complete;
        Some(footer::info(doc, toc, map, self.page, self.speed))
    }

    /// Closes the overlay; the page comes back from its buffer with one full update.
    fn close_overlay(&mut self) {
        // Full-screen panels leave ghosts after a gray update: flash when they close.
        let full = matches!(self.overlay.as_ref().map(|o| o.kind), Some(overlay::OverlayKind::Goto | overlay::OverlayKind::Search | overlay::OverlayKind::Note));
        // The page buffer has the search hits marked: draw the page again without them.
        if self.overlay.as_ref().is_some_and(|o| o.kind == overlay::OverlayKind::Hits) {
            self.rerender = true;
        }
        self.end_bridge();
        if self.sel.take().is_some() {
            self.sel_highlight = false;
            // The inverted selection is on the canvas only; put the page back there too.
            if let Some(b) = selection::bounds(&self.sel_shown) {
                self.reveal.push(b);
            }
            self.sel_shown.clear();
            self.sel_dirty = false;
            if let Some(d) = self.doc.as_deref_mut() {
                d.clear_selection();
            }
        }
        if let Some(d) = self.dict_card.take() {
            self.dict_cancel.push(d);
        }
        if let Some(o) = self.overlay.take() {
            self.swallow_gestures = true;
            if full {
                self.full_close = true;
            } else {
                self.reveal.extend(o.tree.painted_rects());
            }
        }
    }

    /// Acts on an overlay action.
    fn action(&mut self, a: &str, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        log::info!("reader action: {a}");
        self.actions.push(a.to_string());
        if let Some(t) = self.menu_action(a, cx)? {
            return Ok(t);
        }
        // `name:argument` (`toc:3`, `pct:42.5`, `pcard:next`), or a plain name.
        let (name, arg) = a.split_once(':').unwrap_or((a, ""));
        let index = arg.parse::<usize>().ok();
        match name {
            "close" => self.close_overlay(),
            actions::EXIT_CONFIRMED => return Ok(Transition::Exit),
            "back" => return Ok(Transition::Pop),
            "page" => {
                if let Some(n) = index {
                    self.close_overlay();
                    self.navigate_to_page(n.saturating_sub(1));
                }
            }
            "toc" => {
                self.load_toc();
                let pos = index.and_then(|i| self.toc.as_deref()?.get(i).map(|e| e.position.clone()));
                self.go_to_row(pos);
            }
            "bm" => {
                let pos = index.and_then(|i| self.listed_marks.get(i)).and_then(|b| b.position.parse::<Position>().ok());
                self.go_to_row(pos);
            }
            "pct" => {
                if let (Ok(n), Some(doc)) = (arg.parse::<f32>(), self.doc.as_deref_mut()) {
                    let page = percent_page(doc, n);
                    self.close_overlay();
                    self.navigate_to_page(page);
                }
            }
            // aa: the Aa sheet and its actions (reader/aa.rs).
            _ if name == "aa" || name.starts_with("aa-") => {
                if let Some(t) = self.aa_action(a, cx)? {
                    return Ok(t);
                }
            }
            "search" | "hits-list" => self.open_search_page(false, cx),
            "keyboard" => self.search_keys(cx),
            "focus" if arg == "query" => {
                if !self.overlay.as_ref().is_some_and(|o| o.ids.contains_key("keyboard")) {
                    self.open_search_page(true, cx);
                }
            }
            "search-go" => self.run_search(cx),
            "hit" => {
                if let Some(i) = index {
                    self.show_hit(i, cx);
                }
            }
            "hit-next" | "hit-prev" => {
                let n = self.hits.as_ref().map_or(0, Vec::len);
                if n > 0 {
                    let i = if a == "hit-next" { (self.hit + 1) % n } else { (self.hit + n - 1) % n };
                    self.show_hit(i, cx);
                }
            }
            "plugin-menu" | "plugin-sel" => {
                let i = index.unwrap_or(usize::MAX);
                let entry = if name == "plugin-sel" {
                    cx.plugins.selection_actions().get(i).map(|x| (x.plugin.clone(), x.id.clone()))
                } else {
                    plugin_menu(cx.plugins).get(i).map(|x| (x.plugin.clone(), x.id.clone()))
                };
                if let Some((plugin, id)) = entry {
                    log::info!("reader: plugin {plugin} action {id}");
                    // The plugin reads the selection: trigger before the popup closes.
                    let doc = self.doc.as_deref_mut().map(|d| d as &mut dyn Document);
                    cx.plugins.trigger(&plugin, &id, doc);
                }
                self.close_overlay();
            }
            "sel-cancel" => self.close_overlay(),
            "sel-highlight" => {
                self.add_highlight(None);
                self.close_overlay();
                self.marks_changed();
            }
            "sel-unhighlight" => {
                if let Some(sel) = self.sel.clone() {
                    self.remove_highlight(&sel.start, &sel.end);
                }
                self.close_overlay();
                self.marks_changed();
            }
            _ if a.starts_with(PCARD) => self.plugin_card_action(&a[PCARD.len()..], cx),
            "sel-note" => {
                if let Some(sel) = self.sel.as_ref() {
                    self.note_text.clear();
                    let o = selection::note_editor(&selection::quote(sel, 200), cx.screen.size());
                    self.open_overlay(o, cx);
                }
            }
            "note-save" => self.save_note(),
            "focus" => {}
            "sel-search" => {
                let text = self.sel.as_ref().map(|s| s.text.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
                self.close_overlay();
                self.query = text.chars().take(80).collect();
                self.run_search(cx);
            }
            "note-go" => {
                if let Some(t) = self.note_target.take() {
                    self.close_overlay();
                    self.jump(&t, true);
                }
            }
            "bookmark" => {
                self.mark_page_toggle();
                self.close_overlay();
                // The page shows (or loses) its dog-ear.
                self.marks_changed();
            }
            _ => log::info!("reader: '{a}' is not available yet"),
        }
        Ok(Transition::None)
    }

    fn handle_overlay(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        let Some(o) = self.overlay.as_mut() else { return Ok(Transition::None) };
        // The hit bar leaves the page usable: what it does not take goes to the page.
        let page_below = o.kind == overlay::OverlayKind::Hits;
        match ev {
            UiEvent::Quit => return Ok(Transition::Exit),
            UiEvent::Key(k) if k.pressed => match k.key {
                Key::Power => return Ok(Transition::Exit),
                Key::Back | Key::Menu => self.close_overlay(),
                _ if page_below => return self.handle_reading(ev, cx),
                _ => {}
            },
            UiEvent::Timer(_) => {
                o.tree.expire();
            }
            UiEvent::Gesture(_) | UiEvent::Touch(_) => match o.tree.dispatch(ev) {
                Some((_, EventResult::Action(a))) => return self.action(&a, cx),
                None if page_below => return self.handle_reading(ev, cx),
                _ => {}
            },
            UiEvent::Power(p) => log::info!("powerd: {p:?}"),
            _ => {}
        }
        Ok(Transition::None)
    }
}

/// Reader actions for gesture bindings (`reader/actions.rs` maps a trigger to one of
/// these) and other screens. Each logs what it does at info.
impl ReaderScreen {
    /// Turns to the next page (with the page-turn animation when it is on).
    pub fn next_page(&mut self, cx: &mut AppCtx) {
        log::info!("reader: next page from {}", self.page + 1);
        self.turn(1, cx);
    }

    /// Turns to the previous page.
    pub fn prev_page(&mut self, cx: &mut AppCtx) {
        log::info!("reader: previous page from {}", self.page + 1);
        self.turn(-1, cx);
    }

    /// Opens the reader menu (top and bottom bars).
    pub fn open_menu(&mut self, cx: &mut AppCtx) {
        log::info!("reader: open menu");
        let o = self.menu_overlay(&plugin_labels(cx), cx.screen.size());
        self.open_overlay(o, cx);
    }

    // `open_aa` (the reading settings sheet) is in aa.rs.

    /// Opens the Go to panel on its Contents tab.
    pub fn open_toc(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        self.action("gtab:0", cx).map(|_| ())
    }

    /// Opens the Go to panel (contents, bookmarks, highlights and the percent slider to
    /// go to any place).
    pub fn open_goto(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        self.action("goto", cx).map(|_| ())
    }

    /// Opens the Go to panel on its Bookmarks tab.
    pub fn open_bookmarks(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        self.action("gtab:1", cx).map(|_| ())
    }

    /// Adds a bookmark on the current page, or removes the ones on it, and redraws the
    /// page (dog-ear). Returns true when the page is bookmarked afterwards.
    pub fn toggle_bookmark(&mut self) -> bool {
        let marked = self.mark_page_toggle();
        self.marks_changed();
        self.raise_dirty(RefreshClass::Gl16);
        marked
    }

    /// Opens the search page (with the keyboard before the first search, else the last
    /// results).
    pub fn open_search(&mut self, cx: &mut AppCtx) {
        log::info!("reader: open search");
        self.open_search_page(false, cx);
    }

    /// Makes the text one step larger (`steps` > 0) or smaller (< 0): 4 px per step,
    /// within the Aa sheet's range. Saved for this book like an Aa change.
    pub fn font_step(&mut self, steps: i32, cx: &mut AppCtx) -> anyhow::Result<()> {
        let Some(doc) = self.doc.as_deref() else { return Ok(()) };
        let now = doc.layout().font_px;
        let px = (now + steps as f32 * 2.0 * aa::FONT_PX.2).clamp(aa::FONT_PX.0, aa::FONT_PX.1);
        log::info!("reader: font size {now} -> {px}");
        if px == now {
            return Ok(());
        }
        self.action(&format!("aa-size:{px}"), cx).map(|_| ())
    }

    /// Switches night mode on or off (saved in the settings).
    pub fn toggle_night(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        let on = !cx.screen.night_mode();
        log::info!("reader: night mode {}", if on { "on" } else { "off" });
        self.action(if on { "aa-night:on" } else { "aa-night:off" }, cx).map(|_| ())
    }

    /// Shows the screen again with a full GC16 refresh (cleans ghosting).
    pub fn full_refresh(&mut self) {
        log::info!("reader: full refresh");
        self.dirty = Some(RefreshClass::Gc16);
    }

    /// Hides or shows the status bars ([`ReaderScreen::status_bars_hidden`]) and redraws
    /// the page.
    pub fn toggle_status_bars(&mut self) {
        self.status_bars_hidden = !self.status_bars_hidden;
        publish_bars(Some(self.status_bars_hidden));
        log::info!("reader: status bars {}", if self.status_bars_hidden { "hidden" } else { "shown" });
        self.rerender = true;
        self.raise_dirty(RefreshClass::Gl16);
    }

    /// Zooms a fixed-layout page in (`steps` > 0) or out ([`Document::zoom_step`]): the
    /// pages are cut into more or fewer screens, so the page number changes and the page
    /// is drawn again with one update. False when nothing changed (reflowable book, or
    /// the zoom is at its limit). The zoom lasts until the book is closed.
    pub fn zoom_step(&mut self, steps: i32) -> bool {
        let Some(doc) = self.doc.as_deref_mut() else { return false };
        doc.set_current_page(self.page);
        let Some(zoom) = doc.zoom_step(steps) else {
            log::info!("reader: zoom {steps:+}: no change (reflowable book or at the zoom limit)");
            return false;
        };
        self.page = doc.current_page();
        log::info!("reader: zoom {steps:+} -> x{zoom}, now screen {}", self.page + 1);
        self.forget_page_numbers();
        self.session_hl.clear();
        self.rerender = true;
        self.dirty = Some(RefreshClass::Gl16);
        true
    }
}

impl AppScreen for ReaderScreen {
    fn name(&self) -> &'static str {
        "reader"
    }

    /// After a rotation: overlays are built for the old size, so they close; the book
    /// is laid out for the new size (integrator, settings request).
    fn on_resize(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        if self.doc.is_none() {
            return self.on_enter(cx);
        }
        self.close_overlay();
        self.flush_card_cancels(cx);
        self.page_buf = None;
        self.relayout(cx, RefreshClass::Gc16)?;
        self.rerender = true;
        self.dirty = Some(RefreshClass::Gc16);
        Ok(())
    }

    /// Back from a screen that covered only `exposed` (quick light): the page comes
    /// back from the page buffer in that rect (REAGL), not with a full GC16. Anything
    /// else that changed (bars, an overlay, a new layout) takes the full path.
    fn on_reveal(&mut self, cx: &mut AppCtx, exposed: Rect, hint: RefreshClass) -> anyhow::Result<()> {
        let bounds = cx.canvas.bounds();
        let buf_ok = self.page_buf.as_ref().is_some_and(|b| b.width == cx.canvas.width && b.height == cx.canvas.height);
        let toggle = BARS_TOGGLE.with(|c| c.get());
        let partial = exposed != bounds
            && hint != RefreshClass::Gc16
            && self.doc.is_some()
            && self.error.is_none()
            && self.overlay.is_none()
            && self.dirty.is_none()
            && !self.rerender
            && buf_ok
            && !toggle
            && !self.bars_changed(cx);
        if !partial {
            return self.on_enter(cx);
        }
        log::info!("reader: page back in {},{} {}x{} from the page buffer", exposed.x, exposed.y, exposed.w, exposed.h);
        self.reveal.push(exposed);
        Ok(())
    }

    fn on_enter(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        if self.doc.is_some() || self.error.is_some() {
            self.dirty = Some(RefreshClass::Gc16);
            // The text area is placed between the bars (footer::text_layout), and the
            // screen may have turned while a screen above (the control center) was on
            // top: only the top screen hears `on_resize`. `relayout` does nothing when
            // the layout still matches.
            self.relayout(cx, RefreshClass::Gc16)?;
            self.rerender |= self.bars_changed(cx);
            if BARS_TOGGLE.with(|c| c.replace(false)) {
                log::info!("reader: status bars toggled from the control center");
                self.toggle_status_bars();
            }
            if self.doc.is_some() {
                publish_bars(Some(self.status_bars_hidden));
            }
            return Ok(());
        }
        let mut trace = crate::perf::OpenTrace::start();
        // No "Opening" screen: its GC16 flash blocked for the panel update (about 290 ms
        // on the PW12) before the book was even read. The screen below stays on the panel
        // until the first page replaces it with its own GC16 flash.
        log::info!("reader: opening '{}'", self.path.display());
        let size = cx.screen.size();
        let state = BookState::load(&self.path).ok();
        self.book_settings = state.as_ref().map(|s| s.settings.clone()).unwrap_or_default();
        if self.book_settings.migrate_margins(&cx.settings.status_bars) && (self.book_settings.margin_top.is_some() || self.book_settings.margin_bottom.is_some()) {
            log::info!("reader: book margins now measured from the status bars: top {:?}, bottom {:?}", self.book_settings.margin_top, self.book_settings.margin_bottom);
        }
        let layout = aa::book_layout(cx.settings, &self.book_settings, size.w, size.h);
        // The font faces of the first page are parsed on another core while the book is
        // read (MD5, zip, spine, TOC); the first layout takes them parsed.
        ink_html::fonts::prewarm_faces(layout.font_family.as_deref());
        let cache = cx.paths.cache().join("pagemaps");
        // One partial MD5 for the page-map cache, the reading speed and the plugins.
        let md5 = ink_store::partial_md5(&self.path).ok();
        let saved = state.as_ref().and_then(|s| s.position.clone());
        let saved_pos = saved.as_deref().and_then(|s| s.parse::<Position>().ok());
        trace.step("state+md5");
        let doc = match open_traced(cx.providers, &self.path, &layout, Some(&cache), md5.as_deref(), saved_pos.as_ref(), &mut trace) {
            Ok(doc) => doc,
            Err(e) => {
                // The message stays on screen until the user taps: the reader does not
                // end the app (or leave the library) on its own.
                let text = format!("{e:#}");
                log::warn!("cannot open '{}': {text}", self.path.display());
                self.error = Some(text);
                self.dirty = Some(RefreshClass::Gc16);
                return Ok(());
            }
        };
        self.marks = state.as_ref().map(|s| s.bookmarks.iter().cloned().map(Mark::new).collect()).unwrap_or_default();
        self.saved_hl = state.as_ref().map(|s| saved_ranges(&s.highlights)).unwrap_or_default();
        self.page = saved_pos.as_ref().and_then(|p| doc.page_of(p).ok()).unwrap_or(0);
        log::info!("saved position {:?} -> page {}", saved, self.page + 1);
        trace.step("saved position");
        self.doc = Some(doc);
        // The "Book cover" screensaver shows this book from now on (made on a thread).
        crate::sleep_cover::prepare(cx.paths, &self.path, cx.screen.panel_size());
        self.speed = md5.as_deref().and_then(|md5| footer::speed_from_stats(&cx.paths.statistics(), md5)).unwrap_or_default();
        log::info!("reader: footer reading speed {:?}", self.speed);
        trace.step("reading speed");
        // Plugins (statistics database, sync) hear about the book after the first page
        // is on the panel (`paint_screen`), so the page does not wait for them.
        self.plugins_pending = true;
        self.open_trace = Some(trace);
        self.rerender = true;
        self.dirty = Some(RefreshClass::Gc16);
        Ok(())
    }

    fn on_exit(&mut self, cx: &mut AppCtx) {
        self.flush_position();
        publish_bars(None);
        BARS_TOGGLE.with(|c| c.set(false));
        // Plugins that never heard of the book (closed before its first paint) are not
        // told it closed.
        if std::mem::take(&mut self.plugins_pending) {
            return;
        }
        if let Some(doc) = self.doc.as_deref_mut() {
            cx.plugins.book_close(doc);
        }
    }

    fn handle(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        if self.error.is_some() {
            return Ok(match ev {
                UiEvent::Quit => Transition::Exit,
                UiEvent::Key(k) if k.pressed => Transition::Pop,
                UiEvent::Gesture(Gesture::Tap { .. } | Gesture::LongPress { .. } | Gesture::Swipe { .. }) => Transition::Pop,
                _ => Transition::None,
            });
        }
        // The position goes to the sidecar once the page rested, or before the device
        // sleeps (power events).
        if matches!(ev, UiEvent::Power(_)) || self.position_due().is_some_and(|d| Instant::now() >= d) {
            self.flush_position();
        }
        if matches!(ev, UiEvent::Timer(_)) && self.prefetch_at.is_some_and(|d| Instant::now() >= d) {
            self.prefetch_next();
        }
        // Plugin dialogs, menus, viewers and keyboards got the event first in the app
        // loop (plugin_host::hooks::app_handle).
        let t = if self.overlay.is_some() {
            let t = self.handle_overlay(ev, cx);
            self.flush_card_cancels(cx);
            self.aa_flush(cx)?; // aa: debounced relayout (due, or the sheet closed)
            t
        } else {
            self.aa_flush(cx)?; // aa: a relayout left pending when the sheet closed
            let t = self.handle_reading(ev, cx);
            self.flush_card_cancels(cx);
            t
        };
        // A screen pushed over the reader (settings, statistics, light) gets the timer
        // and power events: write the position now.
        if matches!(t, Ok(Transition::Push(_))) {
            self.flush_position();
        }
        t
    }

    fn paint(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        // The plugin overlay is drawn over this by the app loop (hooks::app_paint).
        self.paint_screen(cx)
    }

    /// The open book, for plugin calls made by the app loop (events, dialog answers,
    /// network results, suspend/resume).
    fn document(&mut self) -> Option<&mut dyn Document> {
        self.doc.as_deref_mut().map(|d| d as &mut dyn Document)
    }

    fn plugin_go_to(&mut self, position: &str) -> bool {
        match position.parse::<Position>() {
            Ok(pos) => self.go_to_position(&pos),
            Err(e) => log::warn!("reader: plugin position {position:?}: {e}"),
        }
        self.doc.is_some()
    }

    fn double_tap_mode(&self, settings: &ink_store::Settings) -> ink_ui::DoubleTapMode {
        actions::double_tap_mode(self.overlay.is_some(), settings)
    }

    fn timeout(&self) -> Option<std::time::Duration> {
        // The plugin timeout is merged by the app loop (hooks::app_timeout).
        let save = self.position_due().map(|d| d.saturating_duration_since(Instant::now()));
        // A selection drag resting in a page-turn zone.
        let edge = self.sel_edge.filter(|_| !self.sel_edge_lock && self.selecting.is_some()).map(|(_, since)| selection::EDGE_DWELL.saturating_sub(since.elapsed()));
        // Bars drawn while pagination counts: look again for the end of the count.
        let counting = (self.counting_shown && !self.status_bars_hidden && self.overlay.is_none()).then_some(COUNTING_POLL);
        // The next page drawn ahead once the shown one rested.
        let prefetch = self.prefetch_at.map(|d| d.saturating_duration_since(Instant::now()));
        let expiry = self.overlay.as_ref().and_then(|o| o.tree.next_expiry()).or_else(|| self.bars_timeout());
        // aa: debounce; statusbar: minute tick; position save; selection edge; page count; prefetch
        expiry.into_iter().chain(aa::relayout_timeout()).chain(save).chain(edge).chain(counting).chain(prefetch).min()
    }
}

impl ReaderScreen {
    /// True when the bars on screen show a count still in progress that is now done.
    fn counting_done(&self) -> bool {
        self.counting_shown && !self.status_bars_hidden && self.doc.as_deref().is_some_and(|d| d.page_count().complete)
    }

    /// Ends the open trace once the first page went to the panel (`open:` log line).
    fn finish_open_trace(&mut self) {
        let Some(mut trace) = self.open_trace.take() else { return };
        let Some(doc) = self.doc.as_deref() else { return };
        let count = doc.page_count();
        let state = if count.complete {
            "complete".to_string()
        } else {
            let est = doc.page_estimate().map(|e| format!(", {} estimated", footer::about(e))).unwrap_or_default();
            format!("{} pages known{est}, paginating", count.pages)
        };
        trace.finish(&doc.metadata().title, self.page, &state);
    }

    /// Events on the page with no overlay (or under the hit bar).
    fn handle_reading(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        let size = cx.screen.size();
        match ev {
            UiEvent::Touch(t) if t.kind == ink_ui::TouchKind::Down => self.swallow_gestures = false,
            UiEvent::Gesture(_) if self.swallow_gestures => return Ok(Transition::None),
            _ => {}
        }
        if let UiEvent::Touch(t) = ev {
            if self.slide_touch(t, cx) {
                return Ok(Transition::None);
            }
            if actions::long_press_selects(cx.settings) && self.select_touch(t, cx) {
                return Ok(Transition::None);
            }
        }
        // A selection drag resting in a page-turn zone (no touch events while it rests).
        if matches!(ev, UiEvent::Timer(_)) && self.selecting.is_some() {
            self.sel_edge_turn(cx);
        }
        match ev {
            UiEvent::Quit => return Ok(Transition::Exit),
            UiEvent::Key(k) if k.pressed => match k.key {
                Key::Power => return Ok(Transition::Exit),
                Key::PageNext => self.turn(1, cx),
                Key::PagePrev => self.turn(-1, cx),
                Key::Back => {
                    if !self.go_back_from_link() {
                        return Ok(Transition::Pop);
                    }
                }
                Key::Menu => {
                    let o = self.menu_overlay(&plugin_labels(cx), size);
                    self.open_overlay(o, cx);
                }
                _ => {}
            },
            UiEvent::Gesture(g) => match *g {
                // The Back pill, a saved highlight or a link under the finger.
                Gesture::Tap { pos } if self.tap_target(pos, cx) => {}
                // The hold already selected a word (TouchKind::Hold, just before): the long
                // press that comes with it while the finger is down is that selection.
                Gesture::LongPress { .. } if self.sel.is_some() => {}
                // Corner holds bound by a plugin go to the plugin.
                Gesture::LongPress { .. } if self.plugin_gesture(g, true, cx) => {}
                // Everything else: the user's gesture map (actions.rs); unmapped gestures
                // go to plugin bindings.
                _ => match self.gesture_action(g, cx)? {
                    Some(t) => return Ok(t),
                    None => {
                        self.plugin_gesture(g, false, cx);
                    }
                },
            },
            UiEvent::Power(p) => log::info!("powerd: {p:?}"),
            _ => {}
        }
        Ok(Transition::None)
    }

    /// A tap on something on the page: the "Back to p. N" pill returns, a saved
    /// highlight opens its selection card (Remove highlight, Note, Insightful, ...), a
    /// link is followed. False when the tap is on none of them (it goes to the gesture
    /// map). Each target is looked up once per tap.
    fn tap_target(&mut self, pos: Point, cx: &mut AppCtx) -> bool {
        if self.pill_shown() && back_rect(cx.screen.size()).contains(pos) {
            self.go_back_from_link();
        } else if let Some((start, end, rects)) = self.highlight_at(pos) {
            self.open_highlight(start, end, rects, cx);
        } else if let Some(link) = self.link_at(pos) {
            self.follow_link(link, cx);
        } else {
            return false;
        }
        true
    }

    /// Edge slides ([`light_gestures`]): a vertical drag from the left or right edge
    /// starts one, moves set the level, the lift writes the final one and swallows the
    /// swipe gesture that follows. A second finger ends it. Returns true when the event
    /// was used.
    fn slide_touch(&mut self, t: &ink_ui::Touch, cx: &mut AppCtx) -> bool {
        use ink_ui::TouchKind;
        let size = cx.screen.size();
        if let Some(slide) = self.slide.as_mut() {
            match t.kind {
                TouchKind::Move if t.slot == 0 => slide.moved(t.pos, size.h, &mut *cx.services.light),
                TouchKind::Up | TouchKind::Cancel | TouchKind::Down => {
                    if let Some(slide) = self.slide.take() {
                        let now = cx.services.clock.local_now();
                        slide.finish(&mut *cx.services.light, &now);
                    }
                    self.swallow_gestures = true;
                }
                _ => {}
            }
            return true;
        }
        if t.slot != 0 || t.kind != TouchKind::Move || !t.moved || self.selecting.is_some() || t.held >= ink_ui::LONG_PRESS {
            return false;
        }
        let Some(warmth) = light_gestures::slide_start(t.start, t.pos, size, &cx.settings.gestures) else { return false };
        let Some(mut slide) = light_gestures::Slide::new(warmth, t.start, &*cx.services.light) else { return false };
        slide.moved(t.pos, size.h, &mut *cx.services.light);
        self.slide = Some(slide);
        true
    }

    /// Offers a gesture the reader does not use to plugin gesture bindings
    /// (`hooks::gesture`). With `corners_only`, only corner holds are offered (a plain
    /// long press turns the page). Returns true when a plugin took it.
    fn plugin_gesture(&mut self, g: &Gesture, corners_only: bool, cx: &mut AppCtx) -> bool {
        if corners_only && !crate::plugin_host::gesture_name(g, cx.screen.size()).is_some_and(|n| n.starts_with("corner_hold:")) {
            return false;
        }
        let doc = self.doc.as_deref_mut().map(|d| d as &mut dyn Document);
        let used = crate::plugin_host::hooks::gesture(cx, g, doc);
        if used {
            log::info!("reader: gesture {g:?} used by a plugin");
        }
        used
    }

    fn paint_screen(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        // Plugin status texts changed: show the page again with the new texts.
        let status = status_texts(cx);
        let mut bar_damage = Vec::new();
        if self.dirty.is_none() && self.doc.is_some() && status != self.status_shown {
            // Only the bars' text rows change: redraw and show those rows, not the page.
            let rows = if self.status_bars_hidden {
                Some(Vec::new())
            } else if self.overlay.is_none() && !self.rerender && self.error.is_none() {
                self.redraw_bars(cx, &status, footer::Part::Text)
            } else {
                None
            };
            match rows {
                Some(rows) => {
                    self.status_shown = status.clone();
                    bar_damage.extend(rows.into_iter().map(|r| cx.refresh.damage(Change::Status, r)));
                }
                None => {
                    self.rerender = true;
                    self.dirty = Some(RefreshClass::Gl16);
                }
            }
        }
        if std::mem::take(&mut self.full_close) {
            // Full-screen panels leave ghosts: REAGL now, and ghost debt makes a later
            // page turn flash sooner (instead of a GC16 flash on every close).
            let c = cx.refresh.class_for(Change::FullOverlayClose);
            self.raise_dirty(c);
        }
        if self.dirty.is_none() && self.overlay.is_none() && !self.rerender && self.error.is_none() && self.counting_done() {
            // Pagination finished after the bars were drawn (a font size change): only
            // the bars again, with the counted total and the chapter ticks (the page
            // itself is not rendered again).
            let t = Instant::now();
            match self.redraw_bars(cx, &status, footer::Part::All) {
                Some(rows) => {
                    log::info!("reader: pagination done; status bars drawn again in {} ms", t.elapsed().as_millis());
                    bar_damage.extend(rows.into_iter().map(|r| cx.refresh.damage(Change::Status, r)));
                }
                None => {
                    self.rerender = true;
                    self.dirty = Some(RefreshClass::Gl16);
                }
            }
        }
        let swap_before = std::mem::take(&mut self.swap_same).then(|| cx.canvas.clone());
        let reveal = self.reveal_damage(cx);
        if self.dirty.is_none() {
            bar_damage.extend(self.bars_tick(cx)?);
        }
        let mark = std::mem::take(&mut self.mark_change);
        let settle = self.sel_settle.take();
        if let Some(class) = self.dirty.take() {
            // A jump counts as a page turn: REAGL, a GC16 every N turns (or on an image page).
            let class = if std::mem::take(&mut self.jumped) { class.max(cx.refresh.class_for(Change::Jump)) } else { class };
            // A highlight or bookmark change on the page on screen: keep what the panel
            // shows now and refresh only the pixels that change.
            let before = (mark && class == RefreshClass::Gl16).then(|| cx.canvas.clone());
            if self.error.is_some() {
                self.draw_error(cx);
                cx.screen.update(cx.canvas, cx.canvas.bounds(), class)?;
                return Ok(Vec::new());
            }
            if let Some(t) = self.open_trace.as_mut() {
                t.step("app loop to paint");
            }
            // A new page: the plugins hear of it before the bars are drawn, so their
            // status texts are in this one update (no second update of the bars).
            let will_render = self.rerender || self.page_buf.as_ref().is_none_or(|b| b.size() != cx.canvas.size());
            let mut status = status;
            let mut plugins_ms = 0;
            if will_render {
                if let Some(doc) = self.doc.as_deref_mut() {
                    let t_plugins = Instant::now();
                    doc.set_current_page(self.page);
                    if !self.plugins_pending {
                        cx.plugins.page_change(doc, self.page);
                    }
                    plugins_ms = t_plugins.elapsed().as_millis() as u64;
                }
                status = status_texts(cx);
            }
            // With the Back pill shown, the footer bar starts right of it.
            let bar_left = if !self.pill_shown() { 0 } else { back_rect(cx.screen.size()).right() + ink_ui::theme::space(2) };
            publish_bars(Some(self.status_bars_hidden)); // for the control center (a thread-local store)
            let finfo = if self.status_bars_hidden { None } else { self.bar_info(cx, bar_left, &status) };
            if let Some(t) = self.open_trace.as_mut() {
                t.step("status bars info");
            }
            let Some(doc) = self.doc.as_deref_mut() else { return Ok(Vec::new()) };
            let restored = match &self.page_buf {
                Some(buf) if !self.rerender && buf.size() == cx.canvas.size() => {
                    cx.canvas.pixels.copy_from_slice(&buf.pixels);
                    true
                }
                _ => false,
            };
            let mut save = false;
            if !restored {
                let t_draw = Instant::now();
                match self.bridge {
                    // Half a page on: both pages drawn whole with their highlights, then
                    // the two halves put together.
                    Some(b) => {
                        let mut upper = cx.canvas.clone();
                        let mut lower = cx.canvas.clone();
                        doc.render_page(b.page, &mut upper)?;
                        draw_highlights(doc, b.page, &mut upper, &self.saved_hl, &self.session_hl);
                        doc.render_page(b.page + 1, &mut lower)?;
                        draw_highlights(doc, b.page + 1, &mut lower, &self.saved_hl, &self.session_hl);
                        b.compose(&upper, &lower, cx.canvas);
                    }
                    None => {
                        doc.render_page(self.page, cx.canvas)?;
                        draw_highlights(doc, self.page, cx.canvas, &self.saved_hl, &self.session_hl);
                    }
                }
                if let Some(t) = self.open_trace.as_mut() {
                    t.step("render page");
                }
                if let Some(info) = &finfo {
                    footer::draw_bars(cx.canvas, cx.fonts, doc.layout(), &cx.settings.status_bars, info, footer::Part::All);
                }
                self.status_shown = status;
                // Search hits on this page while the hit bar is shown.
                if self.overlay.as_ref().is_some_and(|o| o.kind == overlay::OverlayKind::Hits) {
                    for h in self.hits.iter().flatten() {
                        if h.page.or_else(|| doc.page_of(&h.position).ok()) == Some(self.page) {
                            let rects = doc.hit_rects(self.page, h);
                            selection::frame(cx.canvas, &rects);
                        }
                    }
                }
                if page_marked(&self.marks, doc, self.page) {
                    draw_dog_ear(cx.canvas);
                }
                if self.back_pill && self.back.last().is_some_and(|r| r.link) {
                    let to = self.back.last().and_then(|r| doc.page_of(&r.position).ok());
                    draw_back(cx.canvas, cx.fonts, cx.theme, to);
                }
                log::debug!("reader: page {} drawn in {:.2} ms (plugins {plugins_ms} ms before)", self.page + 1, t_draw.elapsed().as_secs_f64() * 1000.0);
                match &mut self.page_buf {
                    Some(buf) if buf.size() == cx.canvas.size() => buf.pixels.copy_from_slice(&cx.canvas.pixels),
                    buf => *buf = Some(cx.canvas.clone()),
                }
                self.rerender = false;
                save = true;
                self.prefetch_at = Some(Instant::now() + PREFETCH_IDLE);
                if let Some(t) = self.open_trace.as_mut() {
                    t.step("decorations+plugins");
                }
            }
            // A selection drag turned the page: its part on this page is shown inverted
            // in the same update (the page buffer stays without it).
            if self.selecting.is_some() {
                let rects = self.sel_screen_rects();
                if !rects.is_empty() {
                    selection::invert(cx.canvas, &rects);
                    self.sel_shown = rects;
                    self.sel_dirty = false;
                }
            }
            if let Some(o) = self.overlay.as_mut() {
                o.tree.mark_all_dirty(class);
                o.tree.paint(&mut PaintCx::new(cx.canvas, cx.fonts, cx.theme));
            }
            let mut shown = Vec::new();
            let wave = self.turn_wave.take();
            if let Some(old) = before.as_ref() {
                let full = vec![cx.refresh.damage(Change::Mark, cx.canvas.bounds())];
                shown = ink_ui::refresh::changed_bands(old, cx.canvas, full);
                log::debug!("reader: mark change refreshes {}", crate::app::describe_updates(&shown));
            } else {
                // One update: the page with its status bars (no animation frames, no
                // separate bar update).
                match wave {
                    Some(w) => cx.screen.update_wave(cx.canvas, cx.canvas.bounds(), class, w)?,
                    None => cx.screen.update(cx.canvas, cx.canvas.bounds(), class)?,
                }
                log::debug!("reader: page {} shown in 1 update ({class:?}, wave {wave:?})", self.page + 1);
            }
            self.finish_open_trace();
            if std::mem::take(&mut self.plugins_pending) {
                if let Some(doc) = self.doc.as_deref_mut() {
                    let t = Instant::now();
                    cx.plugins.book_open(doc, &self.path);
                    cx.plugins.page_change(doc, self.page);
                    log::info!("reader: plugins told about the book after the first page ({} ms)", t.elapsed().as_millis());
                }
            }
            // After a page turn the sidecar write waits until the page rests
            // (`flush_position`); the first page and jumps are written now.
            if save {
                self.note_position();
                if !std::mem::take(&mut self.turned) {
                    self.flush_position();
                }
            }
            return Ok(shown);
        }
        if std::mem::take(&mut self.sel_dirty) {
            // Put the page back where the old selection was, invert the new one.
            let new = if self.sel.is_some() { self.sel_screen_rects() } else { Vec::new() };
            let mut all = self.sel_shown.clone();
            all.extend(new.iter().copied());
            if let (Some(area), Some(buf)) = (selection::bounds(&all), self.page_buf.as_ref()) {
                let area = area.intersect(&cx.canvas.bounds());
                if buf.size() == cx.canvas.size() && !area.is_empty() {
                    copy_rect(cx.canvas, buf, area);
                    selection::invert(cx.canvas, &new);
                    cx.screen.update(cx.canvas, area, RefreshClass::Du)?;
                }
            }
            self.sel_shown = new;
        }
        let mut damage = reveal;
        damage.extend(bar_damage);
        if let Some(r) = settle {
            damage.push(cx.refresh.damage(Change::SelectionLift, r.intersect(&cx.canvas.bounds())));
        }
        if let Some(o) = self.overlay.as_mut() {
            // The app loop coalesces and applies the UI flash policy (app::present).
            damage.extend(o.tree.paint(&mut PaintCx::new(cx.canvas, cx.fonts, cx.theme)));
        }
        if let Some(old) = swap_before {
            damage = ink_ui::refresh::changed_bands(&old, cx.canvas, damage);
            log::debug!("reader: same-kind overlay swap refreshes {}", crate::app::describe_updates(&damage));
        }
        if self.overlay.is_some() {
            // A large sheet (Aa) opening or switching tabs: REAGL, not a GC16 flash.
            let class = cx.refresh.class_for(Change::LargePanel);
            damage = ink_ui::refresh::no_flash(damage, cx.canvas.bounds(), class);
        }
        Ok(damage)
    }

    /// Puts the page back from `page_buf` where closed or replaced overlays were
    /// (`self.reveal`) and returns those rects: REAGL when no overlay is open any more
    /// (page text under it), GL16 when a new overlay replaced the old one. When the full
    /// screen is shown anyway (`dirty`), or the page must be drawn again (`rerender`,
    /// no buffer), nothing is restored here; the full path shows everything.
    fn reveal_damage(&mut self, cx: &mut AppCtx) -> Vec<Damage> {
        if self.reveal.is_empty() {
            return Vec::new();
        }
        let rects = std::mem::take(&mut self.reveal);
        let usable = !self.rerender && self.error.is_none() && self.page_buf.as_ref().is_some_and(|b| b.size() == cx.canvas.size());
        if self.dirty.is_some() {
            return Vec::new();
        }
        if !usable {
            self.dirty = Some(RefreshClass::Gl16);
            return Vec::new();
        }
        let class = if self.overlay.is_some() { RefreshClass::Gl16 } else { cx.refresh.class_for(Change::OverlayClose) };
        let Some(buf) = self.page_buf.as_ref() else { return Vec::new() };
        let bounds = cx.canvas.bounds();
        let mut out = Vec::new();
        for r in rects.iter().map(|r| r.intersect(&bounds)).filter(|r| !r.is_empty()) {
            copy_rect(cx.canvas, buf, r);
            out.push(Damage { rect: r, class });
        }
        log::debug!("reader: overlay rects restored from the page buffer: {}", crate::app::describe_updates(&out));
        out
    }
}
