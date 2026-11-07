//! The Home tab of the library and the bottom navigation bar.
//!
//! - A hero card for the book being read ("Continue reading"): large cover, title,
//!   author, chapter (when the reader saved one), a thick progress bar with the
//!   percentage and "N min left in book" from the reading statistics. Without a book
//!   being read the card offers the newest unread book ("Start reading").
//! - A "Recently opened" shelf of covers with thin progress bars.
//! - A "Want to read" shelf of books never opened, newest first.
//! - With no books: "No books yet." and how to add some, with a rescan button.
//!
//! Sections that do not fit above the navigation bar are left out whole. The data the
//! card needs beyond the library index (chapter, time left) is read once per book and
//! position and kept in [`HomeCache`], never on every paint.

use super::browse::{authors_text, percent_text};
use super::view::{MARGIN, TAB_H};
use super::{Hit, LibraryScreen};
use ink_device::Rect;
use ink_store::{BookEntry, BookState, ReadingStatus};
use ink_text::Style;
use ink_ui::draw::stroke_round_rect;
use ink_ui::theme::space;
use ink_ui::{paint_button, paint_cover, ButtonKind, Cover, Icon, NavBar, NavItem, PaintCx, ProgressBar, Widget};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Top of the home content (under the header).
const TOP: i32 = super::view::HEADER_H + 24;
/// Height of the hero card.
pub(crate) const HERO_H: i32 = 480;
/// Gap between sections.
const GAP: i32 = 32;
/// Height of a section heading row.
const HEAD_H: i32 = 56;
/// Covers per shelf.
pub(crate) const SHELF_COLS: usize = 5;
/// Most rows of the "Want to read" shelf.
const MAX_ROWS: usize = 3;
/// Height of a shelf cell (cover, bar or author line, title).
pub(crate) const CELL_H: i32 = 350;
/// Cover height in a shelf cell.
const SHELF_COVER_H: i32 = 270;

/// Icon and short caption of a navigation bar item (`ink_store::prefs::NAV_ITEMS`).
pub(crate) fn nav_item(name: &str) -> (Icon, &'static str) {
    match name {
        "home" => (Icon::Home, "Home"),
        "library" => (Icon::Library, "Library"),
        "collections" => (Icon::Folder, "Collections"),
        "files" => (Icon::ListView, "Files"),
        "search" => (Icon::Search, "Search"),
        "stats" => (Icon::Chart, "Statistics"),
        "opds" => (Icon::Download, "Catalogs"),
        "settings" => (Icon::Settings, "Settings"),
        _ => (Icon::More, "?"),
    }
}

/// Rows at the bottom of the panel kept free of ink (the bezel covers them on some
/// units); the navigation bar still reacts there.
pub(crate) const BOTTOM_SAFE: i32 = 8;

/// The navigation bar's painted rectangle and its tap rectangle (the whole strip down
/// to the bottom edge) on a canvas `w` x `h`.
pub(crate) fn nav_rects(w: i32, h: i32) -> (Rect, Rect) {
    let hit = Rect::new(0, h - TAB_H, w as u32, TAB_H as u32);
    (Rect::new(0, hit.y, hit.w, (TAB_H - BOTTOM_SAFE) as u32), hit)
}

/// What the hero card shows beyond the index entry, for one book at one position.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HomeCache {
    /// Book, progress bits and last open the values below belong to.
    key: Option<(PathBuf, Option<u32>, Option<u64>)>,
    /// Chapter title saved by the reader (`chapter` in the sidecar), if any.
    pub chapter: Option<String>,
    /// Estimated reading time left in the book, seconds.
    pub left_secs: Option<u64>,
}

/// Seconds per page: the book's own pace when it has enough reading, else the pace
/// over all books; `None` without reading statistics.
pub(crate) fn secs_per_page(book: Option<&ink_store::stats::BookTotals>, all: &[ink_store::stats::BookTotals]) -> Option<f64> {
    if let Some(b) = book.filter(|b| b.pages_read >= 3 && b.read_time > 0) {
        return Some(b.read_time as f64 / b.pages_read as f64);
    }
    let (secs, pages) = all.iter().filter(|b| b.read_time > 0).fold((0u64, 0u64), |(s, p), b| (s + b.read_time, p + b.pages_read));
    (pages >= 3 && secs > 0).then(|| secs as f64 / pages as f64)
}

/// Reading time left for `pages` pages at `percent` done, at `spp` seconds per page.
pub(crate) fn time_left(percent: f32, pages: Option<u32>, spp: Option<f64>) -> Option<u64> {
    let pages = pages.filter(|p| *p > 0)? as f64;
    let spp = spp.filter(|s| s.is_finite() && *s > 0.0)?;
    let pc = if percent.is_nan() { 0.0 } else { percent.clamp(0.0, 1.0) as f64 };
    Some(((1.0 - pc) * pages * spp).round() as u64)
}

/// "12 min left in book", "3 h 05 min left in book", "Less than a minute left".
pub(crate) fn left_text(secs: u64) -> String {
    let min = secs.div_ceil(60);
    match min {
        0 => "Finished".into(),
        1 if secs < 60 => "Less than a minute left".into(),
        m if m < 60 => format!("{m} min left in book"),
        m => format!("{} h {:02} min left in book", m / 60, m % 60),
    }
}

/// The chapter title the reader saved in the sidecar (`"chapter"`), if any.
fn saved_chapter(path: &Path) -> Option<String> {
    let st = BookState::load(path).ok()?;
    st.extra.get("chapter").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

/// The hero book: the one being read, else the newest unread book that opens, else
/// the newest unread one, else the most recently opened one. `true` when it is being read.
pub(crate) fn hero_book(lib: &super::Library) -> Option<(BookEntry, bool)> {
    if let Some(b) = lib.current() {
        return Some((b.clone(), true));
    }
    let new = want_to_read(lib);
    new.iter()
        .find(|b| !b.unsupported)
        .or(new.first())
        .cloned()
        .or_else(|| lib.index.recent(1).first().map(|b| (*b).clone()))
        .or_else(|| lib.index.books.first().cloned())
        .map(|b| (b, false))
}

/// Books never opened, newest first.
pub(crate) fn want_to_read(lib: &super::Library) -> Vec<BookEntry> {
    let mut v: Vec<BookEntry> = lib.index.books.iter().filter(|b| b.last_opened.is_none() && b.status == ReadingStatus::New).cloned().collect();
    v.sort_by(|a, b| b.added.cmp(&a.added).then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase())));
    v
}

impl LibraryScreen {
    /// Fills [`HomeCache`] for `b` when the book or its position changed.
    fn home_cache_for(&mut self, b: &BookEntry) {
        let key = (b.path.clone(), b.percent.map(f32::to_bits), b.last_opened);
        if self.home.key.as_ref() == Some(&key) {
            return;
        }
        let chapter = saved_chapter(&b.path);
        let left_secs = self.paths.as_ref().filter(|p| p.statistics().exists()).and_then(|p| match ink_store::StatsDb::open(&p.statistics()) {
            Ok(db) => {
                let all = db.all_books().unwrap_or_default();
                let own = b.md5.as_deref().and_then(|m| all.iter().find(|t| t.md5 == m));
                let pages = b.pages.or_else(|| own.and_then(|t| t.pages));
                time_left(b.percent.unwrap_or(0.0), pages, secs_per_page(own, &all))
            }
            Err(e) => {
                log::warn!("library: home cannot read statistics: {e}");
                None
            }
        });
        self.home = HomeCache { key: Some(key), chapter, left_secs };
    }

    /// The Home tab between the header and the navigation bar.
    pub(super) fn draw_home(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let Some(lib) = self.lib.as_ref() else { return };
        if lib.index.books.is_empty() {
            self.draw_home_empty(p, w, h);
            return;
        }
        let hero = hero_book(lib);
        let hero_path = hero.as_ref().map(|(b, _)| b.path.clone());
        let recent: Vec<BookEntry> = lib.index.recent(SHELF_COLS + 1).into_iter().filter(|b| Some(&b.path) != hero_path.as_ref()).take(SHELF_COLS).cloned().collect();
        let new: Vec<BookEntry> = want_to_read(lib).into_iter().filter(|b| Some(&b.path) != hero_path.as_ref()).take(SHELF_COLS * MAX_ROWS).collect();
        let bottom = h - TAB_H - space(1);
        let mut y = TOP;
        if let Some((b, reading)) = hero {
            self.home_cache_for(&b);
            let r = Rect::new(MARGIN, y, (w - 2 * MARGIN) as u32, HERO_H as u32);
            self.draw_hero(p, r, &b, reading);
            y = r.bottom() + GAP;
        }
        // "Want to read" fills the rest of the page with more rows when there is room.
        for (title, books, progress) in [("Recently opened", recent, true), ("Want to read", new, false)] {
            if books.is_empty() || y + HEAD_H + CELL_H > bottom {
                continue;
            }
            let fg = p.theme.fg;
            p.text_left(Rect::new(MARGIN, y, (w - 2 * MARGIN) as u32, HEAD_H as u32), title, p.theme.heading.px * 0.85, Style::Bold, fg);
            y += HEAD_H;
            for (i, chunk) in books.chunks(SHELF_COLS).enumerate() {
                if i > 0 && y + CELL_H > bottom {
                    break;
                }
                let row = Rect::new(MARGIN, y, (w - 2 * MARGIN) as u32, CELL_H as u32);
                self.draw_shelf(p, row, chunk, progress);
                y = row.bottom() + GAP;
            }
        }
    }

    /// The cover of `b` for `paint_cover`, scaled from the thumbnail cache to `w` x `h`.
    fn cover_of(&mut self, b: &BookEntry, w: u32, h: u32) -> Cover {
        match b.cover.as_deref().and_then(|c| self.covers.get(c, w, h)) {
            Some(img) => Cover::image(b.title.clone(), Arc::new(img.clone())),
            None => Cover::placeholder(b.title.clone()),
        }
    }

    /// The "Continue reading" / "Start reading" card.
    fn draw_hero(&mut self, p: &mut PaintCx, r: Rect, b: &BookEntry, reading: bool) {
        let t = p.theme.clone();
        stroke_round_rect(p.canvas, r, t.radius_lg, t.border_px as f32, t.fg);
        let pad = space(3);
        let ch = r.h as i32 - 2 * pad;
        let cw = ch * 2 / 3;
        let cover_r = Rect::new(r.x + pad, r.y + pad, cw as u32, ch as u32);
        let cover = self.cover_of(b, cover_r.w, cover_r.h);
        paint_cover(p, cover_r, &cover);
        let x = cover_r.right() + space(4);
        let tw = (r.right() - pad - x).max(10) as u32;
        let mut y = r.y + pad;
        let kicker = match (reading, b.status) {
            (true, _) => "Continue reading",
            (false, ReadingStatus::New) => "Up next",
            (false, ReadingStatus::Complete) => "Read again",
            (false, _) => "Pick up again",
        };
        p.text_left(Rect::new(x, y, tw, 44), kicker, t.caption.px, Style::Bold, t.muted);
        y += 52;
        let lines = ink_ui::dialog::wrap_lines(p, &b.title, t.heading.px, Style::Bold, tw as f32);
        let lh = (t.heading.px * 1.25) as i32;
        let n = lines.len().min(3);
        for (i, l) in lines.iter().take(3).enumerate() {
            // The last shown line carries the rest of a long title, ellipsized.
            let text = if i + 1 == n && lines.len() > 3 { lines[i..].join(" ") } else { l.clone() };
            p.text_left(Rect::new(x, y, tw, lh as u32), &text, t.heading.px, Style::Bold, if b.unsupported { t.muted } else { t.fg });
            y += lh;
        }
        y += space(1);
        p.text_left(Rect::new(x, y, tw, 48), &authors_text(b), t.label.px, Style::Regular, t.muted);
        y += 56;
        if !reading && !b.unsupported {
            // The card is the target; the button says what a tap does.
            let bw = 380.min(tw as i32);
            paint_button(p, Rect::new(x, y + space(2), bw as u32, t.control_h()), if b.status == ReadingStatus::New { "Start reading" } else { "Open book" }, Some(Icon::Book), ButtonKind::Filled, true, false);
        }
        if reading {
            if let Some(c) = self.home.chapter.clone() {
                p.text_left(Rect::new(x, y, tw, 44), &c, t.caption.px, Style::Italic, t.fg);
            }
        }
        // Bottom block: thick progress with the percentage, then the time left.
        let bar_h = 56;
        let line_h = 44;
        let bar_y = r.bottom() - pad - line_h - space(1) - bar_h;
        let pc = b.percent.unwrap_or(if b.status == ReadingStatus::Complete { 1.0 } else { 0.0 });
        let fmt = b.format.to_uppercase();
        let below = if b.unsupported {
            format!("{fmt} · cannot be opened")
        } else if !reading && b.status == ReadingStatus::New && b.percent.is_none() {
            match b.pages {
                Some(n) => format!("Unread · {fmt} · {n} pages"),
                None => format!("Unread · {fmt}"),
            }
        } else {
            match self.home.left_secs.filter(|_| reading) {
                Some(s) => left_text(s),
                None => format!("{} · {fmt}", super::model::status_label(b.status)),
            }
        };
        // No bar for a book marked unread or at 0 % (the line below says "Unread").
        if super::browse::shows_progress(b) {
            let mut bar = ProgressBar::new(pc).with_label(percent_text(pc));
            bar.paint(p, Rect::new(x - 4, bar_y, tw + 4, bar_h as u32));
        }
        p.text_left(Rect::new(x, r.bottom() - pad - line_h, tw, line_h as u32), &below, t.caption.px, Style::Regular, t.muted);
        self.hits.push((r, Hit::Book(b.path.clone())));
    }

    /// One shelf of up to [`SHELF_COLS`] covers in `row`.
    fn draw_shelf(&mut self, p: &mut PaintCx, row: Rect, books: &[BookEntry], progress: bool) {
        let t = p.theme.clone();
        let gap = space(3);
        let cols = SHELF_COLS as i32;
        let cell_w = (row.w as i32 - gap * (cols - 1)) / cols;
        for (i, b) in books.iter().take(SHELF_COLS).enumerate() {
            let cell = Rect::new(row.x + i as i32 * (cell_w + gap), row.y, cell_w as u32, row.h);
            let cw = (SHELF_COVER_H * 2 / 3).min(cell_w);
            let cover_r = Rect::new(cell.x + (cell_w - cw) / 2, cell.y, cw as u32, SHELF_COVER_H as u32);
            let cover = self.cover_of(b, cover_r.w, cover_r.h);
            paint_cover(p, cover_r, &cover);
            let mut y = cover_r.bottom() + space(1);
            if progress {
                // The row keeps its height; a book marked unread or at 0 % has no bar.
                if super::browse::shows_progress(b) {
                    let pc = if b.status == ReadingStatus::Complete { 1.0 } else { b.percent.unwrap_or(0.0) };
                    let mut bar = ProgressBar::thin(pc).with_label(percent_text(pc));
                    bar.paint(p, Rect::new(cell.x, y, cell.w, 28));
                }
                y += 32;
            }
            let title_c = if b.unsupported { t.muted } else { t.fg };
            p.text_centered(Rect::new(cell.x, y, cell.w, 40), &b.title, t.caption.px, Style::Bold, title_c);
            y += 40;
            if !progress {
                p.text_centered(Rect::new(cell.x, y, cell.w, 36), &authors_text(b), t.caption.px, Style::Regular, t.muted);
            }
            self.hits.push((cell, Hit::Book(b.path.clone())));
        }
    }

    /// No books: what to do, the folders searched and a rescan button.
    fn draw_home_empty(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let t = p.theme.clone();
        let cw = (w - 2 * MARGIN) as u32;
        let mut y = TOP + 160;
        p.icon(Icon::Book, Rect::new(MARGIN, y, cw, 160), 160, t.fg);
        y += 160 + space(4);
        p.text_centered(Rect::new(MARGIN, y, cw, 64), "No books yet.", t.heading.px, Style::Bold, t.fg);
        y += 64 + space(2);
        for l in ink_ui::dialog::wrap_lines(p, "Copy books to the Books folder over USB.", t.body.px, Style::Regular, cw as f32) {
            p.text_centered(Rect::new(MARGIN, y, cw, 56), &l, t.body.px, Style::Regular, t.fg);
            y += 56;
        }
        y += space(2);
        if !self.dirs.is_empty() {
            let dirs = self.dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ");
            for l in ink_ui::dialog::wrap_lines(p, &format!("Folders: {dirs}"), t.caption.px, Style::Regular, cw as f32) {
                p.text_centered(Rect::new(MARGIN, y, cw, 40), &l, t.caption.px, Style::Regular, t.muted);
                y += 40;
            }
        }
        y += space(4);
        let bw = 360;
        let b = Rect::new((w - bw) / 2, y.min(h - TAB_H - 140), bw as u32, t.control_h());
        paint_button(p, b, "Look again", Some(Icon::Sync), ButtonKind::Outlined, true, false);
        self.hits.push((b, Hit::Rescan));
    }

    /// The bottom navigation bar (design-system [`NavBar`]) with the items of
    /// `ui.nav_items`; no item is selected on pages outside them (details). Nothing is
    /// drawn in the last [`BOTTOM_SAFE`] rows; each cell's tap rectangle spans the whole
    /// strip (icon, caption and the rows below them).
    pub(super) fn draw_nav(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let (r, hit) = nav_rects(w, h);
        p.canvas.fill_rect(hit.x, hit.y, hit.w as i32, hit.h as i32, p.theme.bg);
        let items = self
            .nav
            .iter()
            .map(|n| match ink_plugin::registry::parse_saved_id(n).and_then(|(p, id)| self.plugin_entries.iter().find(|e| e.plugin == p && e.id == id)) {
                Some(e) => NavItem::new(crate::plugin_host::icon_named(&e.icon).unwrap_or(Icon::More), e.label.clone(), n.as_str()),
                None => {
                    let (icon, label) = nav_item(n);
                    NavItem::new(icon, label, n.as_str())
                }
            })
            .collect();
        let mut nav = NavBar::new(items).with_selected(self.tab().unwrap_or(usize::MAX));
        nav.paint(p, r);
        for (i, c) in nav.cells(hit).into_iter().enumerate() {
            self.hits.push((c, Hit::Tab(i)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_store::stats::BookTotals;

    fn totals(md5: &str, secs: u64, pages: u64) -> BookTotals {
        BookTotals { md5: md5.into(), read_time: secs, pages_read: pages, ..Default::default() }
    }

    #[test]
    fn pace_prefers_the_book_then_all_books() {
        let all = vec![totals("a", 600, 10), totals("b", 3000, 30)];
        assert_eq!(secs_per_page(Some(&all[0]), &all), Some(60.0));
        // Too little reading of the book itself: the pace over all books.
        let thin = totals("c", 100, 2);
        assert_eq!(secs_per_page(Some(&thin), &all), Some(3600.0 / 40.0));
        assert_eq!(secs_per_page(None, &[]), None);
        assert_eq!(secs_per_page(None, &[totals("z", 0, 0)]), None);
    }

    #[test]
    fn time_left_needs_pages_and_pace() {
        assert_eq!(time_left(0.5, Some(200), Some(60.0)), Some(6000));
        assert_eq!(time_left(f32::NAN, Some(10), Some(60.0)), Some(600));
        assert_eq!(time_left(2.0, Some(10), Some(60.0)), Some(0));
        assert_eq!(time_left(0.5, None, Some(60.0)), None);
        assert_eq!(time_left(0.5, Some(0), Some(60.0)), None);
        assert_eq!(time_left(0.5, Some(10), None), None);
        assert_eq!(time_left(0.5, Some(10), Some(f64::INFINITY)), None);
    }

    #[test]
    fn left_text_rounds_up_to_minutes() {
        assert_eq!(left_text(0), "Finished");
        assert_eq!(left_text(30), "Less than a minute left");
        assert_eq!(left_text(60), "1 min left in book");
        assert_eq!(left_text(61), "2 min left in book");
        assert_eq!(left_text(59 * 60), "59 min left in book");
        assert_eq!(left_text(3 * 3600 + 5 * 60), "3 h 05 min left in book");
    }
}
