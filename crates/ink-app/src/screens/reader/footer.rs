//! Status bars in the page margins (owner: the `statusbar` builder).
//!
//! The reader draws two KOReader-like bars from `settings.status_bars`
//! ([`ink_store::prefs::StatusBars`]): the top bar [`BAR_EDGE`] below the top screen
//! edge, the bottom bar [`BAR_EDGE`] above the bottom edge, both across the screen width
//! less [`BAR_INSET`] at each side. The text margins never move them: the text area is
//! placed between them ([`text_layout`]: the top and bottom margins are measured from
//! the inner edge of their bar). Each bar has three slots (left, centre, right), each a list of widgets
//! from [`STATUS_WIDGETS`] joined by the bar's separator, a text size, an on/off switch and
//! a progress bar ([`PROGRESS_BARS`]: none, thin, thick, or thin with a tick at every
//! chapter start). Text that does not fit is cut with "…"; the centre slot is centred on
//! the page and never overlaps the left and right slots. The top bar keeps clear of the
//! bookmark corner, the bottom bar of the Back pill.
//!
//! Plugin status texts (left, centre, right) are added to the bottom bar's left and right
//! slots and the top bar's centre slot (shown even when that bar is off).
//!
//! Refresh: the bars are drawn into the page before it is shown, so a page turn refreshes
//! them with the page. A bar that shows the clock, battery, Wi-Fi or light is redrawn on
//! its own once a minute ([`ReaderScreen::bars_tick`]): only the text rows are cleared and
//! drawn again and updated with GL16, no full-page refresh. The device values (battery,
//! Wi-Fi, light) are read through the device services only then (and when the book
//! opens): on the Kindle Wi-Fi and light are `lipc-get-prop` child processes, too slow for
//! every page turn. `ReaderScreen::status_bars_hidden` hides both bars.
//!
//! Chapters ([`ChapterMap`]): the chapter a page is in is the deepest TOC entry that
//! starts at or before it (the TOC is flat with levels, sorted by page). When the TOC is
//! flat or missing, spine items also count as chapter starts for "pages left in chapter"
//! ([`Document::spine_starts`]). Chapter ticks use every TOC entry, or only the top level when the
//! TOC has more than 60 entries.
//!
//! Time left: pages left times the seconds per page. Seconds per page come from ink-stats
//! for this book (read time / pages read, [`speed_from_stats`]) when there is enough data,
//! else from 250 words per minute and an estimate of the words on a page of the current
//! layout ([`words_per_page`]).
//!
//! The old `[reader] footer` / `header` styles are mapped onto the bars once by
//! `ink_store::Settings::migrate_status_bars`; the old per-book overrides
//! (`BookSettings::footer` / `header`) are kept in the sidecar but no longer drawn.

use super::{ReaderScreen, DOG_EAR};
use crate::router::AppCtx;
use ink_device::{Canvas, Rect};
use ink_doc::{Document, LayoutSettings, TocEntry};
use ink_store::prefs::{StatusBar, StatusBars};
use ink_text::{Fonts, Style};
use ink_ui::refresh::Change;
use ink_ui::{Damage, Icon, PaintCx, ProgressBar, Theme, Widget};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use ink_store::prefs::{PROGRESS_BARS, STATUS_WIDGETS};

/// Reading speed used when ink-stats has too little data for the book.
pub const DEFAULT_WPM: f32 = 250.0;
/// Characters per word (5 letters and a space) for [`words_per_page`].
const CHARS_PER_WORD: f32 = 6.0;
/// Average advance of a body-text character, in ems (Noto Serif, English text).
const CHAR_EM: f32 = 0.48;
/// Fewest pages and seconds of ink-stats data for a book before its speed is used.
const MIN_STATS_PAGES: u64 = 10;
const MIN_STATS_SECONDS: u64 = 120;
/// Text size of the page-number footer ([`draw_footer`]).
const TEXT_PX: f32 = 28.0;
const THIN_BOX_H: u32 = ink_store::prefs::BAR_THIN_BOX_H;
const THICK_BOX_H: u32 = ink_store::prefs::BAR_THICK_BOX_H;
const ROW_GAP: u32 = ink_store::prefs::BAR_ROW_GAP;
/// At most this many chapter ticks (more would merge into a dashed line); also the TOC
/// size above which only top-level entries get a tick.
const MAX_MARKS: usize = 60;
/// Smallest gap between chapter ticks, as a fraction of the bar (about 14 px).
const MIN_MARK_GAP: f32 = 0.012;
/// Smallest and largest bar text size.
pub const MIN_FONT_PX: f32 = ink_store::prefs::BAR_MIN_FONT_PX;
/// Largest bar text size.
pub const MAX_FONT_PX: f32 = ink_store::prefs::BAR_MAX_FONT_PX;
/// Distance of each bar from its screen edge (top bar from the top, bottom bar from the
/// bottom).
pub const BAR_EDGE: i32 = ink_store::prefs::BAR_EDGE_PX;
/// Distance of the bars from the left and right screen edges.
pub const BAR_INSET: i32 = 40;
/// Smallest text column width and text height the margins leave ([`text_layout`]).
pub const MIN_TEXT_PX: f32 = 300.0;
/// Widgets whose value changes without a page turn (redrawn once a minute).
const TIMED_WIDGETS: [&str; 6] = ["clock", "battery", "battery_icon", "battery_icon_percent", "wifi", "frontlight"];

/// How fast the reader reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Speed {
    /// Measured by ink-stats for this book.
    SecondsPerPage(f32),
    /// Words per minute (the default when there is no measurement).
    WordsPerMinute(f32),
}

impl Default for Speed {
    fn default() -> Self {
        Speed::WordsPerMinute(DEFAULT_WPM)
    }
}

impl Speed {
    /// Seconds to read one page of `layout`.
    pub fn seconds_per_page(self, layout: &LayoutSettings) -> f32 {
        match self {
            Speed::SecondsPerPage(s) => s,
            Speed::WordsPerMinute(wpm) => words_per_page(layout) / wpm.max(1.0) * 60.0,
        }
    }
}

/// Estimated words on a full page of `layout`: lines times characters per line over 6.
pub fn words_per_page(layout: &LayoutSettings) -> f32 {
    let px = layout.font_px.max(1.0);
    let text_w = (layout.width as f32 - layout.margin_left - layout.margin_right).max(px);
    let text_h = (layout.height as f32 - layout.margin_top - layout.margin_bottom).max(px);
    let lines = (text_h / (px * layout.line_spacing.max(1.0))).floor().max(1.0);
    let chars = (text_w / (px * CHAR_EM)).floor().max(1.0);
    lines * chars / CHARS_PER_WORD
}

/// Speed from a book's ink-stats totals: seconds per page when the book has at least 10
/// pages and 2 minutes of reading recorded.
pub fn speed_from_totals(read_time: u64, pages_read: u64) -> Option<Speed> {
    (pages_read >= MIN_STATS_PAGES && read_time >= MIN_STATS_SECONDS).then(|| Speed::SecondsPerPage((read_time as f32 / pages_read as f32).clamp(3.0, 1800.0)))
}

/// The book's speed from the statistics database `db` (book id `md5`, the partial MD5
/// the stats plugin records). A missing database is not created.
pub fn speed_from_stats(db: &std::path::Path, md5: &str) -> Option<Speed> {
    if !db.exists() {
        return None;
    }
    let stats = match ink_store::StatsDb::open(db) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("footer: cannot read reading speed from {}: {e}", db.display());
            return None;
        }
    };
    let t = stats.book_totals(md5).ok()??;
    speed_from_totals(t.read_time, t.pages_read)
}

/// One TOC entry for [`ChapterMap::from_entries`]: (first page, level, TOC index).
pub type TocStart = (usize, u8, usize);

/// Chapter starts of a document in its current layout ([`ChapterMap::is_for`]): built
/// once while pages are still being counted and once more when the count is complete.
///
/// While pages are counted, `page_of` would paginate every chapter on the spot, so the
/// map then uses only the pages the TOC already knows, and Kindle locations for the
/// ticks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChapterMap {
    /// (first page, TOC index) of every TOC entry whose page is known and of every spine
    /// item (TOC index `None`) when the TOC is flat or missing, by page.
    pub starts: Vec<(usize, Option<usize>)>,
    /// Fractions of the book where chapters start (bar ticks).
    pub marks: Vec<f32>,
    /// Layout key and pagination state the map was built for.
    key: (String, bool),
}

/// True when TOC entry `level` gets a bar tick: every entry when the TOC has at most 60
/// entries, else top-level entries only.
fn ticks_entry(levels: usize, top: u8, level: u8) -> bool {
    levels <= MAX_MARKS || level == top
}

/// Keeps tick fractions that are inside the bar and not closer than `MIN_MARK_GAP` to the
/// previous one (front matter, short scenes); none at all when more than 60 remain.
fn thin_marks(fracs: impl IntoIterator<Item = f32>) -> Vec<f32> {
    let mut marks: Vec<f32> = Vec::new();
    for f in fracs {
        if (MIN_MARK_GAP..1.0).contains(&f) && marks.last().is_none_or(|l| f - l >= MIN_MARK_GAP) {
            marks.push(f);
        }
    }
    if marks.len() > MAX_MARKS {
        marks.clear();
    }
    marks
}

impl ChapterMap {
    /// Reads the pages of the TOC entries of `doc` (and its spine items when the TOC is
    /// flat or missing).
    pub fn build(doc: &dyn Document, toc: &[TocEntry]) -> Self {
        let t = Instant::now();
        let count = doc.page_count();
        let page_of = |e: &TocEntry| if count.complete { e.page.or_else(|| doc.page_of(&e.position).ok()) } else { e.page };
        let entries: Vec<TocStart> = toc.iter().enumerate().filter_map(|(i, e)| Some((page_of(e)?, e.level, i))).collect();
        let flat = toc.iter().all(|e| e.level == toc[0].level);
        // Spine items (DocFragments) as chapter starts: from the page map, empty until
        // pagination ends.
        let spine = if flat && count.complete { doc.spine_starts() } else { Vec::new() };
        let mut map = if count.complete {
            Self::from_entries(&entries, toc.len(), &spine, count.pages)
        } else {
            // Ticks from Kindle locations while the page fraction still moves.
            let mut map = Self::from_entries(&entries, toc.len(), &[], 0);
            let top = toc.iter().map(|e| e.level).min().unwrap_or(0);
            let last = doc.location_count().filter(|l| l.0 > 1).map(|n| (n.0 - 1) as f32);
            let fraction = |e: &TocEntry| Some(doc.location(&e.position)?.0.saturating_sub(1) as f32 / last?);
            map.marks = if last.is_some() { thin_marks(toc.iter().filter(|e| ticks_entry(toc.len(), top, e.level)).filter_map(fraction)) } else { Vec::new() };
            map
        };
        map.key = Self::key_of(doc);
        log::info!(
            "footer: chapter map for {} TOC entries ({} with pages, {} spine starts, {} ticks, complete {}) in {} ms",
            toc.len(),
            entries.len(),
            spine.len(),
            map.marks.len(),
            count.complete,
            t.elapsed().as_millis()
        );
        map
    }

    /// The map for TOC `entries` (any order; `toc_len` entries in the whole TOC, some
    /// without a page) and `spine` starts in a book of `total` pages (0: no ticks).
    pub fn from_entries(entries: &[TocStart], toc_len: usize, spine: &[usize], total: usize) -> Self {
        let mut starts: Vec<(usize, Option<usize>)> = entries.iter().map(|&(p, _, i)| (p, Some(i))).collect();
        starts.extend(spine.iter().map(|&p| (p, None)));
        // By page; on one page the spine start first, then TOC order (parents before
        // their children), so the last entry at or before a page is the deepest.
        starts.sort_by_key(|&(p, i)| (p, i.map_or(0, |i| i + 1)));
        starts.dedup();
        let mut marks = Vec::new();
        if total > 0 {
            let top = entries.iter().map(|e| e.1).min().unwrap_or(0);
            let mut ticks: Vec<usize> = entries.iter().filter(|e| ticks_entry(toc_len, top, e.1)).map(|e| e.0).collect();
            if entries.is_empty() {
                ticks.extend_from_slice(spine);
            }
            ticks.sort_unstable();
            marks = thin_marks(ticks.into_iter().filter(|&p| p > 0).map(|p| p as f32 / total as f32));
        }
        ChapterMap { starts, marks, key: Default::default() }
    }

    fn key_of(doc: &dyn Document) -> (String, bool) {
        (doc.layout().cache_key(), doc.page_count().complete)
    }

    /// True when the map was built for the document's current layout and pagination
    /// state.
    pub fn is_for(&self, doc: &dyn Document) -> bool {
        self.key == Self::key_of(doc)
    }

    /// (TOC index of the deepest entry starting at or before `page`, first page of the
    /// next chapter start after `page`).
    pub fn chapter_of(&self, page: usize) -> (Option<usize>, Option<usize>) {
        let n = self.starts.partition_point(|(p, _)| *p <= page);
        let cur = self.starts[..n].iter().rev().find_map(|(_, i)| *i);
        let next = self.starts.get(n).map(|(p, _)| *p);
        (cur, next)
    }
}

/// Device values the bars show, read through the device services.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceStatus {
    /// `14:05` (empty: unknown).
    pub clock: String,
    /// Battery percent and charging.
    pub battery: Option<(u8, bool)>,
    /// Wi-Fi: `Some(true)` connected, `Some(false)` on but not connected or off.
    pub wifi: Option<bool>,
    /// Front light level.
    pub light: Option<u32>,
}

impl DeviceStatus {
    /// Reads the values `bars` show (the others stay `None`).
    pub fn read(cx: &AppCtx, bars: &StatusBars) -> Self {
        let t = Instant::now();
        let uses = |w: &str| uses_widget(bars, w);
        let s = &cx.services;
        let d = DeviceStatus {
            clock: if uses("clock") { s.clock.local_now().clock(false) } else { String::new() },
            battery: ["battery", "battery_icon", "battery_icon_percent"].into_iter().any(uses).then(|| s.battery.state().ok().map(|b| (b.percent, b.charging))).flatten(),
            wifi: uses("wifi").then(|| s.wifi.state().ok().map(|w| w.essid.is_some())).flatten(),
            light: uses("frontlight").then(|| s.light.brightness().ok()).flatten(),
        };
        log::info!("footer: device values {d:?} read in {} ms", t.elapsed().as_millis());
        d
    }
}

/// True when an enabled bar of `bars` has widget `name` in a slot.
pub fn uses_widget(bars: &StatusBars, name: &str) -> bool {
    [&bars.top, &bars.bottom].iter().any(|b| b.enabled && [&b.left, &b.center, &b.right].iter().any(|s| s.iter().any(|w| w == name)))
}

/// What the bars show for one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FooterInfo {
    /// 0-based page.
    pub page: usize,
    /// Pages in the book (so far, while pagination runs).
    pub total: usize,
    /// Estimated page count while pagination runs ([`Document::page_estimate`]); shown
    /// as [`about`] instead of `total`.
    pub total_estimate: Option<usize>,
    /// Fraction read, 0..=1 (the bar).
    pub progress: f32,
    /// Chapter start fractions (bar ticks).
    pub marks: Vec<f32>,
    /// Pages to the next chapter start (or the end of the book), counting this page;
    /// `None` while the end is not known yet (pages still being counted).
    pub pages_left_chapter: Option<usize>,
    /// Minutes to the end of the chapter, from the start of this page.
    pub minutes_left_chapter: Option<u32>,
    /// Minutes to the end of the book; `None` while pages are counted.
    pub minutes_left_book: Option<u32>,
    /// Book title.
    pub book_title: String,
    /// Authors, joined with ", ".
    pub author: String,
    /// Title of the chapter the page is in.
    pub chapter_title: Option<String>,
    /// The page has a bookmark.
    pub bookmarked: bool,
    /// Clock, battery, Wi-Fi, light.
    pub device: DeviceStatus,
    /// Plugin status texts: left, centre, right.
    pub status: [String; 3],
    /// The panel shows the page inverted (night mode): the bar track is drawn darker.
    pub night: bool,
    /// Leftmost x of the bottom bar (the reader's Back pill sits left of it); 0 = the
    /// margin.
    pub bar_left: i32,
}

/// Fraction read on `page`: the page fraction once all pages are counted (so chapter
/// ticks and the fill agree), else [`super::progress`] (Kindle locations).
pub fn bar_progress(doc: &dyn Document, page: usize) -> f32 {
    let c = doc.page_count();
    if c.complete {
        ((page + 1) as f32 / c.pages.max(1) as f32).clamp(0.0, 1.0)
    } else {
        super::progress(doc, page)
    }
}

/// The estimated page count while pagination runs, when it is above the pages counted
/// so far; `None` once the count is complete.
pub fn estimate(doc: &dyn Document) -> Option<usize> {
    let c = doc.page_count();
    if c.complete {
        None
    } else {
        doc.page_estimate().filter(|&e| e > c.pages)
    }
}

/// Minutes to read `pages` pages at `seconds_per_page`, rounded up, at least 1.
pub fn minutes_for(pages: usize, seconds_per_page: f32) -> u32 {
    ((pages as f32 * seconds_per_page.max(0.0)) / 60.0).ceil().max(1.0) as u32
}

/// The info for `page` of `doc` (`map` built for the doc's current layout).
pub fn info(doc: &dyn Document, toc: &[TocEntry], map: &ChapterMap, page: usize, speed: Speed) -> FooterInfo {
    let count = doc.page_count();
    let total = count.pages.max(1);
    let (cur, next) = map.chapter_of(page);
    let end = next.or(count.complete.then_some(total));
    let spp = speed.seconds_per_page(doc.layout());
    let pages_left = end.map(|end| end.saturating_sub(page).max(1));
    let meta = doc.metadata();
    FooterInfo {
        page,
        total: count.pages,
        total_estimate: estimate(doc),
        progress: bar_progress(doc, page),
        marks: map.marks.clone(),
        pages_left_chapter: pages_left,
        minutes_left_chapter: pages_left.map(|n| minutes_for(n, spp)),
        minutes_left_book: count.complete.then(|| minutes_for(total.saturating_sub(page).max(1), spp)),
        book_title: meta.title.trim().to_string(),
        author: meta.authors.join(", "),
        chapter_title: cur.and_then(|i| toc.get(i)).map(|e| e.title.trim().to_string()).filter(|t| !t.is_empty()),
        ..FooterInfo::default()
    }
}

/// `12 min`, `1 h 5 min`, `2 h`.
pub fn duration_text(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// `12 min left in chapter`, `1 h 5 min left in book`.
pub fn time_left_text(minutes: u32, in_chapter: bool) -> String {
    format!("{} left in {}", duration_text(minutes), if in_chapter { "chapter" } else { "book" })
}

/// An estimated page count while pagination runs: `about 340`. (Not `~340`: at
/// status-bar size the tilde read as a minus sign, "3 / -340"; the UI font has no `≈`.)
pub fn about(pages: usize) -> String {
    format!("about {pages}")
}

/// Percent shown in the bars: rounded down, 100 only at the end.
pub fn percent_text(progress: f32) -> String {
    let p = if progress.is_nan() { 0.0 } else { progress.clamp(0.0, 1.0) };
    format!("{}%", (p * 100.0 + 1e-3).floor() as u32)
}

/// One drawn piece of a slot.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    /// Text in a style.
    Text(String, Style),
    /// An icon (Wi-Fi, bookmark), optionally followed by text (the light level).
    Icon(Icon, String),
    /// A battery outline filled to the percent, with a bolt when charging, and the
    /// percent next to it when the bool after `charging` is set.
    Battery(u8, bool, bool),
}

/// The piece widget `name` shows for `info`; `None` when it has nothing to show (unknown
/// name, value not known yet, no bookmark on this page).
pub fn widget_piece(name: &str, info: &FooterInfo) -> Option<Piece> {
    let text = |s: String| (!s.is_empty()).then_some(Piece::Text(s, Style::Regular));
    match name {
        "clock" => text(info.device.clock.clone()),
        "battery" => info.device.battery.map(|(p, c)| Piece::Text(if c { format!("{p}% +") } else { format!("{p}%") }, Style::Regular)),
        "battery_icon" => info.device.battery.map(|(p, c)| Piece::Battery(p, c, false)),
        "battery_icon_percent" => info.device.battery.map(|(p, c)| Piece::Battery(p, c, true)),
        "wifi" => info.device.wifi.filter(|on| *on).map(|_| Piece::Icon(Icon::Wifi, String::new())),
        "frontlight" => info.device.light.map(|l| Piece::Icon(Icon::Light, l.to_string())),
        "page" => (info.total > 0).then(|| Piece::Text((info.page + 1).to_string(), Style::Regular)),
        "page_of_total" => (info.total > 0).then(|| {
            let total = info.total_estimate.map_or_else(|| info.total.to_string(), about);
            Piece::Text(format!("{} / {}", info.page + 1, total), Style::Regular)
        }),
        "percent" => Some(Piece::Text(percent_text(info.progress), Style::Bold)),
        "chapter_title" => info.chapter_title.clone().and_then(text),
        "book_title" => text(info.book_title.clone()),
        "author" => text(info.author.clone()),
        "pages_left_chapter" => info.pages_left_chapter.map(|n| Piece::Text(if n == 1 { "1 page left in chapter".into() } else { format!("{n} pages left in chapter") }, Style::Regular)),
        "time_left_chapter" => info.minutes_left_chapter.map(|m| Piece::Text(time_left_text(m, true), Style::Regular)),
        "time_left_book" => info.minutes_left_book.map(|m| Piece::Text(time_left_text(m, false), Style::Regular)),
        "bookmark" => info.bookmarked.then_some(Piece::Icon(Icon::Bookmark, String::new())),
        _ => None,
    }
}

/// Width of `piece` at `px`.
fn piece_width(cx: &mut PaintCx, piece: &Piece, px: f32) -> f32 {
    match piece {
        Piece::Text(t, st) => cx.text_width(*st, px, t),
        Piece::Icon(_, t) if t.is_empty() => px,
        Piece::Icon(_, t) => px * 1.15 + cx.text_width(Style::Regular, px, t),
        Piece::Battery(_, _, false) => (px * 1.5).round(),
        Piece::Battery(p, _, true) => (px * 1.5).round() + battery_gap(px) + cx.text_width(Style::Regular, px, &format!("{p}%")),
    }
}

/// Draws `piece` with its left edge at `x`; `row` is the text row.
fn draw_piece(cx: &mut PaintCx, piece: &Piece, px: f32, x: f32, row: Rect, baseline: f32) {
    let fg = cx.theme.fg;
    match piece {
        Piece::Text(t, st) => cx.draw_text(*st, px, x, baseline, t, fg),
        Piece::Icon(icon, t) => {
            let side = px.round() as u32;
            cx.icon(*icon, Rect::new(x.round() as i32, row.y + (row.h as i32 - side as i32) / 2, side, side), side, fg);
            if !t.is_empty() {
                cx.draw_text(Style::Regular, px, x + px * 1.15, baseline, t, fg);
            }
        }
        Piece::Battery(p, charging, text) => {
            let w = (px * 1.5).round() as u32;
            let h = (px * 0.7).round() as u32;
            // Centred on the digits (half their height above the baseline), not the row.
            let mid = baseline - ink_ui::icon::DIGIT_MID * px;
            let r = Rect::new(x.round() as i32, (mid - h as f32 / 2.0).round() as i32, w, h);
            ink_ui::icon::draw_battery(cx.canvas, r, *p, *charging, fg, cx.theme.bg);
            if *text {
                cx.draw_text(Style::Regular, px, x + w as f32 + battery_gap(px), baseline, &format!("{p}%"), fg);
            }
        }
    }
}

/// Space between the battery and its percent: about a narrow space, much less than a
/// separator.
fn battery_gap(px: f32) -> f32 {
    (px * 0.25).round()
}

/// Width of a slot's pieces with a separator of `sep_w` between each two.
fn slot_width(pieces: &[(Piece, f32)], sep_w: f32) -> f32 {
    pieces.iter().map(|p| p.1).sum::<f32>() + sep_w * pieces.len().saturating_sub(1) as f32
}

/// The pieces of a slot, with their widths, cut to `max` pixels: the widest text is
/// ellipsized first; pieces are dropped from the end when even that is not enough.
/// Returns the pieces and the total width (separators included).
fn fit_slot(cx: &mut PaintCx, mut pieces: Vec<(Piece, f32)>, sep_w: f32, px: f32, max: f32) -> (Vec<(Piece, f32)>, f32) {
    loop {
        let w = slot_width(&pieces, sep_w);
        if w <= max || pieces.is_empty() {
            return (pieces, w);
        }
        // The widest text piece gives up the excess, if it keeps at least two ems.
        let widest = pieces.iter().enumerate().filter(|(_, (p, _))| matches!(p, Piece::Text(..))).max_by(|a, b| a.1 .1.total_cmp(&b.1 .1)).map(|(i, _)| i);
        if let Some(i) = widest {
            let room = pieces[i].1 - (w - max);
            if room >= 2.0 * px {
                if let Piece::Text(t, st) = &pieces[i].0 {
                    let cut = cx.ellipsize(t, px, *st, room);
                    let cw = cx.text_width(*st, px, &cut);
                    pieces[i] = (Piece::Text(cut, *st), cw);
                    continue;
                }
            }
        }
        pieces.pop();
    }
}

/// Which bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// The top margin.
    Top,
    /// The bottom margin.
    Bottom,
}

/// Rows of one bar in its band.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BarRows {
    /// The text row (left, centre, right slots), when the bar has text and room for it.
    pub text: Option<Rect>,
    /// The progress bar's box, when the bar has one and room for it.
    pub progress: Option<Rect>,
    /// Text size.
    pub px: f32,
    /// The page centre the centre slot is centred on.
    pub centre: f32,
}

/// Pixels `edge`'s bar of `bars` takes from its screen edge to its inner edge
/// ([`StatusBar::room`]: [`BAR_EDGE`] plus the band), 0 when the bar is off or empty.
/// The reader's top and bottom text margins are measured from there (see
/// [`text_layout`]). Depends only on the bar settings, so hiding the bars or a plugin
/// text never lays the book out again.
pub fn bar_reserve(bars: &StatusBars, edge: Edge) -> f32 {
    match edge {
        Edge::Top => bars.top.room(),
        Edge::Bottom => bars.bottom.room(),
    }
}

/// The text layout for the stored margins in `l` (side, top, bottom) and the bars
/// `bars`: the top and bottom margins are measured from the inner edge of their bar
/// ([`bar_reserve`]; from the screen edge when the bar is off). The side margins are
/// cut so the text column keeps [`MIN_TEXT_PX`], and the top and bottom margins so
/// the text keeps [`MIN_TEXT_PX`] of height.
pub fn text_layout(mut l: LayoutSettings, bars: &StatusBars) -> LayoutSettings {
    let (w, h) = (l.width as f32, l.height as f32);
    let side = ((w - MIN_TEXT_PX) / 2.0).max(0.0);
    l.margin_left = l.margin_left.clamp(0.0, side);
    l.margin_right = l.margin_right.clamp(0.0, side);
    let top = bar_reserve(bars, Edge::Top) + l.margin_top.max(0.0);
    let bottom = bar_reserve(bars, Edge::Bottom) + l.margin_bottom.max(0.0);
    let room = (h - MIN_TEXT_PX).max(0.0);
    if top + bottom > room {
        // Cut both by the same share of the excess.
        let k = room / (top + bottom);
        (l.margin_top, l.margin_bottom) = ((top * k).floor(), (bottom * k).floor());
    } else {
        (l.margin_top, l.margin_bottom) = (top, bottom);
    }
    l
}

/// Where `edge`'s bar goes on a `layout` screen. The bars do not depend on the text
/// margins: each spans the screen width less [`BAR_INSET`] at both sides (the top bar
/// stops before the bookmark corner, the bottom bar starts right of `bar_left`) and sits
/// [`BAR_EDGE`] from its screen edge, as high as its rows ([`StatusBar::band_h`]). The bottom bar
/// has its progress bar above the text row, the top bar below it. `text` says whether the
/// bar has any slot text (widgets or plugin texts), `progress` whether it has a progress
/// bar.
pub fn bar_rows(layout: &LayoutSettings, edge: Edge, bar: &StatusBar, text: bool, progress: bool, bar_left: i32) -> BarRows {
    let w = layout.width as i32;
    let mut left = BAR_INSET;
    let mut right = w - BAR_INSET;
    let h = bar.band_h(text, progress);
    let y = match edge {
        Edge::Top => {
            // The bookmark mark is a triangle, widest at the screen top: at the bar's top
            // edge it reaches DOG_EAR - BAR_EDGE in from the right.
            right = right.min(w - (DOG_EAR - BAR_EDGE) - ink_ui::theme::space(1));
            BAR_EDGE
        }
        Edge::Bottom => {
            if bar_left > left && bar_left < right {
                left = bar_left;
            }
            layout.height as i32 - BAR_EDGE - h
        }
    };
    let centre = layout.width as f32 / 2.0;
    let px = bar.text_px();
    let mut rows = BarRows { text: None, progress: None, px, centre };
    if right - left < 64 || h == 0 {
        return rows;
    }
    let width = (right - left) as u32;
    let text_h = (px * 1.4).ceil() as i32;
    let box_h = if bar.progress_bar == "thick" { THICK_BOX_H } else { THIN_BOX_H } as i32;
    let (ty, by) = match (edge, text && progress) {
        (Edge::Top, true) => (y, y + text_h + ROW_GAP as i32),
        (Edge::Bottom, true) => (y + box_h + ROW_GAP as i32, y),
        _ => (y, y),
    };
    rows.text = text.then(|| Rect::new(left, ty, width, text_h as u32));
    rows.progress = progress.then(|| Rect::new(left, by, width, box_h as u32));
    rows
}

/// `edge`'s bar of `bars` and the plugin texts it shows in its left, centre and right
/// slots: the top bar the centre text, the bottom bar the left and right texts.
fn edge_bar<'a>(bars: &'a StatusBars, edge: Edge, info: &'a FooterInfo) -> (&'a StatusBar, [&'a str; 3]) {
    let s = &info.status;
    match edge {
        Edge::Top => (&bars.top, ["", &s[1], ""]),
        Edge::Bottom => (&bars.bottom, [&s[0], "", &s[2]]),
    }
}

/// The rows `edge`'s bar of `bars` takes for `info` (nothing when the bar is off and has
/// no plugin text).
pub fn rows_for(layout: &LayoutSettings, bars: &StatusBars, edge: Edge, info: &FooterInfo) -> BarRows {
    let (bar, plugin) = edge_bar(bars, edge, info);
    let plugin = plugin.iter().any(|t| !t.is_empty());
    let widgets = bar.enabled && [&bar.left, &bar.center, &bar.right].iter().any(|s| !s.is_empty());
    bar_rows(layout, edge, bar, widgets || plugin, bar.enabled && bar.has_progress(), info.bar_left)
}

/// A slot laid out in a row: its pieces with their widths, and its left x.
type PlacedSlot = (Vec<(Piece, f32)>, f32);

/// Lays out the text row of `bar` in `row` (centred on `centre`): `plugin` texts are added
/// to the left, centre and right slots; a disabled bar has only the plugin texts. Returns
/// the left, centre and right slots, cut to fit, and the separator width.
#[allow(clippy::too_many_arguments)]
fn plan_row(cx: &mut PaintCx, row: Rect, px: f32, centre: f32, bar: &StatusBar, info: &FooterInfo, plugin: [&str; 3]) -> ([PlacedSlot; 3], f32) {
    let sep = bar.separator.as_str();
    let sep_w = if sep.is_empty() { 0.0 } else { cx.text_width(Style::Regular, px, sep) };
    let slot = |cx: &mut PaintCx, names: &[String], extra: &str| -> Vec<(Piece, f32)> {
        let names = if bar.enabled { names } else { &[] };
        let plugin = (!extra.is_empty()).then(|| Piece::Text(extra.to_string(), Style::Regular));
        let mut v = Vec::new();
        for p in names.iter().filter_map(|n| widget_piece(n, info)).chain(plugin) {
            let w = piece_width(cx, &p, px);
            v.push((p, w));
        }
        v
    };
    let l = slot(cx, &bar.left, plugin[0]);
    let c = slot(cx, &bar.center, plugin[1]);
    let r = slot(cx, &bar.right, plugin[2]);
    let width = row.w as f32;
    let gap = px;
    let (wl, wr, wc) = (slot_width(&l, sep_w), slot_width(&r, sep_w), slot_width(&c, sep_w));
    // Side caps: with a centre slot, each side gets half of what the centre leaves (the
    // centre keeps at least a third); without, the sides share the row.
    let (cap_l, cap_r) = if c.is_empty() {
        if wl + wr + gap <= width {
            (wl, wr)
        } else {
            let half = (width - gap) / 2.0;
            let small = wl.min(wr).min(half);
            if wl <= wr {
                (small, width - gap - small)
            } else {
                (width - gap - small, small)
            }
        }
    } else {
        let side = ((width - wc.min(width / 3.0)) / 2.0 - gap).max(0.0);
        (side, side)
    };
    let (l, wl) = fit_slot(cx, l, sep_w, px, cap_l);
    let (r, wr) = fit_slot(cx, r, sep_w, px, cap_r);
    let left_end = row.x as f32 + wl;
    let right_start = row.right() as f32 - wr;
    let gap_l = if l.is_empty() { 0.0 } else { gap };
    let gap_r = if r.is_empty() { 0.0 } else { gap };
    let room_c = 2.0 * (centre - left_end - gap_l).min(right_start - gap_r - centre);
    let (c, wc) = if room_c >= px { fit_slot(cx, c, sep_w, px, room_c) } else { (Vec::new(), 0.0) };
    ([(l, row.x as f32), (c, centre - wc / 2.0), (r, right_start)], sep_w)
}

/// Draws the text row of `bar` in `row` (see [`plan_row`]).
fn draw_text_row(cx: &mut PaintCx, row: Rect, px: f32, centre: f32, bar: &StatusBar, info: &FooterInfo, plugin: [&str; 3]) {
    let (slots, sep_w) = plan_row(cx, row, px, centre, bar, info, plugin);
    let baseline = cx.baseline_in(row, px, Style::Regular);
    let fg = cx.theme.fg;
    for (pieces, mut x) in slots {
        for (i, (p, w)) in pieces.iter().enumerate() {
            if i > 0 && sep_w > 0.0 {
                cx.draw_text(Style::Regular, px, x, baseline, &bar.separator, fg);
                x += sep_w;
            }
            draw_piece(cx, p, px, x, row, baseline);
            x += w;
        }
    }
}

/// (left x, width) of the left, centre and right slots of `edge`'s text row, as
/// [`draw_bars`] places them (`None` when the bar has no text row).
pub fn slot_spans(canvas: &mut Canvas, fonts: &mut Fonts, layout: &LayoutSettings, bars: &StatusBars, edge: Edge, info: &FooterInfo) -> Option<[(f32, f32); 3]> {
    let rows = rows_for(layout, bars, edge, info);
    let row = rows.text?;
    let theme = Theme::day();
    let mut cx = PaintCx::new(canvas, fonts, &theme);
    let (bar, plugin) = edge_bar(bars, edge, info);
    let (slots, sep_w) = plan_row(&mut cx, row, rows.px, rows.centre, bar, info, plugin);
    Some(slots.map(|(p, x)| (x, slot_width(&p, sep_w))))
}

/// Draws the progress bar of `bar` in `b`.
fn draw_progress(cx: &mut PaintCx, b: Rect, bar: &StatusBar, info: &FooterInfo) {
    let mut pb = match bar.progress_bar.as_str() {
        "thick" => ProgressBar::new(info.progress),
        _ => ProgressBar::thin(info.progress),
    }
    .two_level();
    if bar.progress_bar == "chapters" {
        pb = pb.with_marks(info.marks.clone());
    }
    pb.paint(cx, b);
}

/// What [`draw_bars`] draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Text rows and progress bars.
    All,
    /// Only the text rows (the minute tick).
    Text,
}

/// Draws the status bars `bars` for `info` into the margins of `layout`; returns the
/// rectangles drawn (text rows and progress boxes). The margins must be blank (a freshly
/// rendered page, or rows cleared by the caller).
pub fn draw_bars(canvas: &mut Canvas, fonts: &mut Fonts, layout: &LayoutSettings, bars: &StatusBars, info: &FooterInfo, part: Part) -> Vec<Rect> {
    draw_bars_at(canvas, fonts, layout, (0, 0), bars, info, part)
}

/// [`draw_bars`] for a page whose top-left corner is at `origin` on the canvas (the
/// settings preview).
pub fn draw_bars_at(canvas: &mut Canvas, fonts: &mut Fonts, layout: &LayoutSettings, origin: (i32, i32), bars: &StatusBars, info: &FooterInfo, part: Part) -> Vec<Rect> {
    let theme = if info.night { Theme::night() } else { Theme::day() };
    let mut cx = PaintCx::new(canvas, fonts, &theme);
    let mut drawn = Vec::new();
    let shift = |r: Rect| Rect::new(r.x + origin.0, r.y + origin.1, r.w, r.h);
    for edge in [Edge::Top, Edge::Bottom] {
        let mut rows = rows_for(layout, bars, edge, info);
        rows.text = rows.text.map(shift);
        rows.progress = rows.progress.map(shift);
        rows.centre += origin.0 as f32;
        let (bar, plugin) = edge_bar(bars, edge, info);
        if let Some(r) = rows.text {
            draw_text_row(&mut cx, r, rows.px, rows.centre, bar, info, plugin);
            drawn.push(r);
        }
        if let (Some(r), Part::All) = (rows.progress, part) {
            draw_progress(&mut cx, r, bar, info);
            drawn.push(r);
        }
    }
    drawn
}

/// Draws the page-number footer (`3 / 208`, centred in the bottom margin) for page
/// `index` of `total`: what goldens and tools draw without settings.
pub fn draw_footer(canvas: &mut Canvas, fonts: &mut Fonts, layout: &LayoutSettings, index: usize, total: usize) {
    let h = layout.margin_bottom.max(0.0);
    let top = layout.height as f32 - h;
    let px = TEXT_PX.min(h * 0.45).max(12.0);
    let baseline = top + h / 2.0 + px / 3.0;
    let theme = Theme::day();
    let mut cx = PaintCx::new(canvas, fonts, &theme);
    let t = format!("{} / {}", index + 1, total);
    let w = cx.text_width(Style::Regular, px, &t);
    cx.draw_text(Style::Regular, px, layout.width as f32 / 2.0 - w / 2.0, baseline, &t, theme.muted);
}

/// Draws the default status bars ([`StatusBars::default`], no device values) for page
/// `page` of `doc` into its margins (`rinky render`, page-turn targets).
pub fn draw_default_bars(doc: &dyn Document, page: usize, canvas: &mut Canvas, fonts: &mut Fonts) {
    let toc = doc.toc();
    let map = ChapterMap::build(doc, &toc);
    let info = info(doc, &toc, &map, page, Speed::default());
    draw_bars(canvas, fonts, doc.layout(), &StatusBars::default(), &info, Part::All);
}

/// The bars' minute tick state of a reader.
#[derive(Clone, Debug, Default)]
pub struct BarsState {
    /// Device values shown (read at book open and on every tick).
    pub device: Option<DeviceStatus>,
    /// When the next minute starts (a tick is due), while the bars show timed widgets.
    pub next_tick: Option<Instant>,
    /// The bar settings the page was last drawn with.
    pub shown: Option<StatusBars>,
}

/// Time until the next wall-clock minute starts (at least 1 s).
fn until_next_minute() -> Duration {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    Duration::from_secs_f64((60.0 - secs % 60.0).max(1.0))
}

impl ReaderScreen {
    /// The bars' info for the current page (`None` without a document); device values
    /// come from the cache (read now when there is none, when the bar settings changed:
    /// the cache has only the values the old bars showed, e.g. no battery, or when the
    /// minute tick is due: a page turn after the minute starts shows the new clock in
    /// its own update, and no separate tick update of the bars follows it).
    pub(super) fn bar_info(&mut self, cx: &AppCtx, bar_left: i32, status: &[String; 3]) -> Option<FooterInfo> {
        let bars = &cx.settings.status_bars;
        let tick_due = self.bars.next_tick.is_some_and(|t| Instant::now() >= t);
        if self.bars.device.is_none() || self.bars.shown.as_ref() != Some(bars) || tick_due {
            self.bars.device = Some(DeviceStatus::read(cx, bars));
        }
        let mut info = self.footer_info()?;
        let doc = self.doc.as_deref()?;
        info.bookmarked = uses_widget(bars, "bookmark") && super::page_marked(&self.marks, doc, self.page);
        info.device = self.bars.device.clone().unwrap_or_default();
        info.status = status.clone();
        info.night = cx.theme.night;
        info.bar_left = bar_left;
        self.bars.next_tick = TIMED_WIDGETS.iter().any(|w| uses_widget(bars, w)).then(|| Instant::now() + until_next_minute());
        if self.bars.shown.as_ref() != Some(bars) {
            self.bars.shown = Some(bars.clone());
        }
        Some(info)
    }

    /// True when the bar settings changed since the page was drawn (back from the
    /// settings screen): the page must be drawn again, not restored from its buffer.
    pub(super) fn bars_changed(&self, cx: &AppCtx) -> bool {
        self.bars.shown.as_ref().is_some_and(|b| *b != cx.settings.status_bars)
    }

    /// How long the event loop may wait before the next minute tick of the bars.
    pub(super) fn bars_timeout(&self) -> Option<Duration> {
        if self.status_bars_hidden || self.overlay.is_some() || self.doc.is_none() {
            return None;
        }
        self.bars.next_tick.map(|t| t.saturating_duration_since(Instant::now()))
    }

    /// The minute tick: when due, reads the device values again and redraws the text rows
    /// of the bars (cleared first) into the canvas and the page buffer. Returns those rows
    /// as GL16 damage (the app loop shows them). Nothing while an overlay is open or the
    /// bars are hidden.
    pub(super) fn bars_tick(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        let due = self.bars.next_tick.is_some_and(|t| Instant::now() >= t);
        if !due || self.status_bars_hidden || self.overlay.is_some() || self.error.is_some() {
            return Ok(Vec::new());
        }
        if cx.services.power.sleep_state() != ink_device::sleep::SleepState::Awake {
            // The screensaver is up: do not draw over it; try again in a minute.
            self.bars.next_tick = Some(Instant::now() + until_next_minute());
            return Ok(Vec::new());
        }
        let t = Instant::now();
        // redraw_bars reads the device values: the tick is due (bar_info).
        let status = self.status_shown.clone();
        let Some(rows) = self.redraw_bars(cx, &status, Part::Text) else { return Ok(Vec::new()) };
        log::info!("footer: minute tick redrew {} bar rows in {} ms", rows.len(), t.elapsed().as_millis());
        Ok(rows.into_iter().map(|r| cx.refresh.damage(Change::Status, r)).collect())
    }

    /// Redraws the text rows of the bars, with plugin status texts `status`, into the
    /// canvas and the page buffer, and returns the rows (clipped to the screen); the
    /// progress bars too when `part` is [`Part::All`] (pagination finished: the chapter
    /// ticks and the fill change). `None` when that is not enough: no document or page
    /// buffer, or the new texts change which rows the bars take (the page must then be
    /// drawn again).
    pub(super) fn redraw_bars(&mut self, cx: &mut AppCtx, status: &[String; 3], part: Part) -> Option<Vec<Rect>> {
        if self.page_buf.as_ref().is_none_or(|b| b.size() != cx.canvas.size()) {
            return None;
        }
        let bar_left = if !self.pill_shown() { 0 } else { super::back_rect(cx.screen.size()).right() + ink_ui::theme::space(2) };
        let old_status = self.status_shown.clone();
        let info = self.bar_info(cx, bar_left, status)?;
        let doc = self.doc.as_deref()?;
        let layout = doc.layout();
        let bars = &cx.settings.status_bars;
        let old = FooterInfo { status: old_status, ..info.clone() };
        let edges = [Edge::Top, Edge::Bottom];
        if edges.iter().any(|&e| rows_for(layout, bars, e, &info) != rows_for(layout, bars, e, &old)) {
            return None;
        }
        let bounds = cx.canvas.bounds();
        let rows: Vec<Rect> = edges
            .iter()
            .flat_map(|&e| {
                let r = rows_for(layout, bars, e, &info);
                [r.text, r.progress.filter(|_| part == Part::All)]
            })
            .flatten()
            .map(|r| r.intersect(&bounds))
            .filter(|r| !r.is_empty())
            .collect();
        for r in &rows {
            cx.canvas.fill_rect(r.x, r.y, r.w as i32, r.h as i32, 0xFF);
        }
        draw_bars(cx.canvas, cx.fonts, layout, bars, &info, part);
        if let Some(buf) = self.page_buf.as_mut() {
            for r in &rows {
                super::copy_rect(buf, cx.canvas, *r);
            }
        }
        Some(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_and_time_texts() {
        let l = LayoutSettings::for_screen(1272, 1696);
        let wpp = words_per_page(&l);
        assert!((150.0..450.0).contains(&wpp), "{wpp} words per page");
        let spp = Speed::default().seconds_per_page(&l);
        assert!((wpp / DEFAULT_WPM * 60.0 - spp).abs() < 1e-3);
        assert_eq!(speed_from_totals(600, 5), None, "too few pages");
        assert_eq!(speed_from_totals(60, 50), None, "too little time");
        assert_eq!(speed_from_totals(1200, 20), Some(Speed::SecondsPerPage(60.0)));
        assert_eq!(minutes_for(10, 60.0), 10);
        assert_eq!(minutes_for(1, 5.0), 1);
        assert_eq!(time_left_text(12, true), "12 min left in chapter");
        assert_eq!(time_left_text(65, false), "1 h 5 min left in book");
        assert_eq!(time_left_text(120, true), "2 h left in chapter");
        assert_eq!(percent_text(0.999), "99%");
        assert_eq!(percent_text(1.0), "100%");
        assert_eq!(percent_text(f32::NAN), "0%");
    }

    #[test]
    fn missing_stats_database_is_not_created() {
        let p = std::env::temp_dir().join(format!("ink-footer-nostats-{}/statistics.sqlite3", std::process::id()));
        assert_eq!(speed_from_stats(&p, "abc"), None);
        assert!(!p.exists() && !p.parent().unwrap().exists());
    }

    /// The deepest entry at or before a page names the chapter; spine starts end it.
    #[test]
    fn chapter_lookup_uses_the_deepest_entry_and_spine_starts() {
        // Book One (p0) > Chapter 1 (p0) > Chapter 2 (p5); Book Two (p9).
        let entries = [(0, 0, 0), (0, 1, 1), (5, 1, 2), (9, 0, 3)];
        let map = ChapterMap::from_entries(&entries, 4, &[], 20);
        assert_eq!(map.chapter_of(0), (Some(1), Some(5)));
        assert_eq!(map.chapter_of(6), (Some(2), Some(9)));
        assert_eq!(map.chapter_of(12), (Some(3), None));
        assert_eq!(map.marks, vec![0.25, 0.45], "every level ticks in a short TOC");
        // A flat TOC (three books) with spine items every 4 pages.
        let flat = [(0, 0, 0), (8, 0, 1), (16, 0, 2)];
        let map = ChapterMap::from_entries(&flat, 3, &[0, 4, 8, 12, 16], 20);
        assert_eq!(map.chapter_of(5), (Some(0), Some(8)));
        assert_eq!(map.chapter_of(1), (Some(0), Some(4)), "the next spine item ends the chapter");
        // More than 60 entries: top-level ticks only.
        let many: Vec<TocStart> = (0..80).map(|i| (i * 10, u8::from(i % 8 != 0), i)).collect();
        let map = ChapterMap::from_entries(&many, 80, &[], 800);
        assert_eq!(map.marks.len(), 9);
        assert_eq!(ChapterMap { starts: vec![(3, Some(0))], ..ChapterMap::default() }.chapter_of(1), (None, Some(3)));
    }

    #[test]
    fn widget_texts() {
        let info = FooterInfo {
            page: 4,
            total: 10,
            progress: 0.5,
            pages_left_chapter: Some(1),
            device: DeviceStatus { clock: "09:05".into(), battery: Some((80, true)), wifi: Some(false), light: Some(12) },
            ..FooterInfo::default()
        };
        let t = |n: &str| match widget_piece(n, &info) {
            Some(Piece::Text(t, _)) => t,
            other => format!("{other:?}"),
        };
        assert_eq!((t("page_of_total"), t("percent"), t("battery"), t("clock")), ("5 / 10".into(), "50%".into(), "80% +".into(), "09:05".into()));
        // While pagination counts: the estimate in words, no tilde that reads as a minus.
        let counting = FooterInfo { total_estimate: Some(340), ..info.clone() };
        assert!(matches!(widget_piece("page_of_total", &counting), Some(Piece::Text(t, _)) if t == "5 / about 340"));
        assert_eq!(widget_piece("battery_icon_percent", &info), Some(Piece::Battery(80, true, true)));
        assert_eq!(widget_piece("battery_icon", &info), Some(Piece::Battery(80, true, false)));
        assert_eq!(t("pages_left_chapter"), "1 page left in chapter");
        assert_eq!(widget_piece("wifi", &info), None, "no icon while not connected");
        assert_eq!(widget_piece("bookmark", &info), None);
        assert_eq!(widget_piece("sparkles", &info), None);
    }
}
