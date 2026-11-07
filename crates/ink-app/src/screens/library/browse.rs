//! Painting of the browse views (library list and grid, collections, file browser) and
//! the app bar of every view. The browse views show a screenful of rows at a time with
//! a [`ScrollBar`] on the right (no pager footer). Built from the ink-ui design system:
//! `AppBar`, chips, `ProgressBar`, `paint_cover`, `paint_button` and the palette tokens.
//!
//! Rows, cells and covers here are separate from the Home tab's helpers in `view.rs`.

use super::model::sort_label;
use super::view::{MARGIN, TAB_H, TOOLBAR_H};
use super::{files, Hit, LibraryScreen, Overlay, StatusFilter, ViewMode};
use ink_device::Rect;
use ink_store::{BookEntry, ReadingStatus};
use ink_text::Style;
use ink_ui::draw::{fill_round_rect, stroke_round_rect};
use ink_ui::theme::space;
use ink_ui::{paint_button, paint_cover, AppBar, BarAction, ButtonKind, Cover, Icon, PaintCx, ProgressBar, ScrollBar, Widget};

/// Height of a list row with a cover thumbnail.
pub const LIST_ROW_H: i32 = 164;
/// Height of a collection row.
pub const COLL_ROW_H: i32 = 164;
/// Height of a file browser row.
pub const FILE_ROW_H: i32 = 128;
/// Height of a chip (touch target).
pub const CHIP_H: u32 = 96;
/// Chip captions of the status filters, in [`StatusFilter::ALL`] order (the saved
/// preferences keep using [`StatusFilter::label`]).
pub const STATUS_CHIPS: [&str; 4] = ["All", "Unread", "Reading", "Finished"];

/// What the badge on a cover says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// Never opened.
    New,
    /// Read to the end.
    Finished,
    /// DRM or an unsupported format.
    Locked,
}

impl Badge {
    pub(super) fn label(self) -> &'static str {
        match self {
            Badge::New => "Unread",
            Badge::Finished => "Finished",
            Badge::Locked => "Locked",
        }
    }
}

/// Whether the library shows a progress bar for `b` (every view: list, grid, Home,
/// details): a finished book, or one with at least 1 % read (as the label rounds). Never
/// for a book marked unread (whatever position it kept), one at 0 %, or a locked one.
pub fn shows_progress(b: &BookEntry) -> bool {
    if b.unsupported || b.status == ReadingStatus::New {
        return false;
    }
    b.status == ReadingStatus::Complete || b.percent.is_some_and(|v| (v.clamp(0.0, 1.0) * 100.0).round() >= 1.0)
}

/// Progress (0..=1) of a book that is being read, or the badge it gets instead: a book
/// marked unread or at 0 % gets the "Unread" badge, no bar ([`shows_progress`]).
pub fn book_state(b: &BookEntry) -> Result<f32, Badge> {
    if b.unsupported {
        return Err(Badge::Locked);
    }
    match (b.status, b.percent) {
        (ReadingStatus::Complete, _) => Err(Badge::Finished),
        (_, Some(pc)) if shows_progress(b) => Ok(pc),
        _ => Err(Badge::New),
    }
}

/// `42%`.
pub fn percent_text(v: f32) -> String {
    format!("{}%", (v.clamp(0.0, 1.0) * 100.0).round() as u32)
}

/// Width of a chip with `label` (and a leading icon or check mark).
pub fn chip_w(p: &mut PaintCx, label: &str, lead: bool) -> u32 {
    let px = p.theme.label.px * 0.95;
    let tw = p.text_width(Style::Bold, px, label).ceil() as u32 + 4;
    tw + 2 * space(3) as u32 + if lead { (px * 0.9) as u32 + space(1) as u32 } else { 0 }
}

/// A filter chip as `ink_ui::Chips` draws it: outlined when off, black with a check mark
/// (or `icon`) and bold white text when on. A leading `icon` shows in both states.
pub fn paint_chip(p: &mut PaintCx, r: Rect, label: &str, icon: Option<Icon>, on: bool) {
    let t = p.theme.clone();
    let px = t.label.px * 0.95;
    let rad = r.h as f32 / 2.0;
    let (ink, style) = if on { (t.bg, Style::Bold) } else { (t.fg, Style::Regular) };
    if on {
        fill_round_rect(p.canvas, r, rad, t.fg);
    } else {
        stroke_round_rect(p.canvas, r, rad, t.border_px as f32, t.fg);
    }
    let mut x = r.x + space(3);
    let lead = icon.or(if on { Some(Icon::Check) } else { None });
    if let Some(i) = lead {
        let side = (px * 0.9) as u32;
        let ir = Rect::new(x, r.y + (r.h as i32 - side as i32) / 2, side, side);
        i.draw(p.canvas, ir, ink);
        x = ir.right() + space(1);
    }
    let lr = Rect::new(x, r.y, (r.right() - space(3) - x).max(0) as u32, r.h);
    p.text_left(lr, label, px, style, ink);
}

/// A black pill with a bold white caption (and a paper ring, so it reads on a cover).
/// `r` is the pill; the ring is drawn just outside it.
pub fn paint_badge(p: &mut PaintCx, r: Rect, badge: Badge) {
    let t = p.theme.clone();
    let rad = r.h as f32 / 2.0;
    let ring = Rect::new(r.x - 3, r.y - 3, r.w + 6, r.h + 6);
    fill_round_rect(p.canvas, ring, ring.h as f32 / 2.0, t.bg);
    fill_round_rect(p.canvas, r, rad, t.fg);
    let px = (r.h as f32 * 0.62).min(t.caption.px);
    let mut x = r.x + r.h as i32 / 3;
    let icon = match badge {
        Badge::Finished => Some(Icon::Check),
        Badge::Locked => Some(Icon::Close),
        Badge::New => None,
    };
    if let Some(i) = icon {
        let side = (px * 0.9) as u32;
        let ir = Rect::new(x, r.y + (r.h as i32 - side as i32) / 2, side, side);
        i.draw(p.canvas, ir, t.bg);
        x = ir.right() + 6;
    }
    let lr = Rect::new(x, r.y, (r.right() - r.h as i32 / 3 - x).max(0) as u32, r.h);
    p.text_left(lr, badge.label(), px, Style::Bold, t.bg);
}

/// Width of a badge pill `h` tall.
pub fn badge_w(p: &mut PaintCx, badge: Badge, h: u32) -> u32 {
    let px = (h as f32 * 0.62).min(p.theme.caption.px);
    let icon = if badge == Badge::New { 0 } else { (px * 0.9) as u32 + 6 };
    p.text_width(Style::Bold, px, badge.label()).ceil() as u32 + 2 * (h / 3) + icon + 4
}

/// The 2:3 cover box inside `r`, bottom-centred.
pub fn cover_box(r: Rect) -> Rect {
    let w = r.w.min(r.h * 2 / 3).max(1);
    let h = r.h.min(w * 3 / 2).max(1);
    Rect::new(r.x + (r.w - w) as i32 / 2, r.bottom() - h as i32, w, h)
}

/// Text of the muted line under a title: authors.
pub fn authors_text(b: &BookEntry) -> String {
    if b.authors.is_empty() {
        "Unknown author".to_string()
    } else {
        b.authors.join(", ")
    }
}

impl LibraryScreen {
    /// Paints the cover of `b` into the 2:3 box `r`: the picture fitted and bottom-aligned
    /// with a black frame, or `ink_ui`'s placeholder. Returns the framed rectangle.
    pub(super) fn paint_book_cover(&mut self, p: &mut PaintCx, r: Rect, b: &BookEntry) -> Rect {
        let t = p.theme.clone();
        let bp = t.border_px;
        if let Some(path) = b.cover.as_deref() {
            let inner_w = (r.w as i32 - 2 * bp).max(1) as u32;
            let inner_h = (r.h as i32 - 2 * bp).max(1) as u32;
            if let Some(img) = self.covers.get(path, inner_w, inner_h) {
                let (iw, ih) = (img.width as u32, img.height as u32);
                let x = r.x + (r.w as i32 - iw as i32) / 2;
                let y = r.bottom() - bp - ih as i32;
                p.canvas.blit(img, x, y);
                let f = Rect::new(x - bp, y - bp, iw + 2 * bp as u32, ih + 2 * bp as u32);
                stroke_round_rect(p.canvas, f, 4.0, bp as f32, t.fg);
                return f;
            }
        }
        paint_cover(p, r, &Cover::placeholder(b.title.clone()))
    }

    /// The app bar of every view: back chevron, title, the search field (library
    /// view), and Stats, Settings (each only when the navigation bar lacks it) and the
    /// overflow menu (no exit: only Settings has "Exit to Kindle") as icon buttons.
    /// While the search keyboard is
    /// open: the field (painted by the overlay) and a filled "Done".
    /// `count`: the books the library view lists (`None` on the other views).
    pub(super) fn draw_app_bar(&mut self, p: &mut PaintCx, w: i32, title: &str, back: bool, count: Option<usize>) {
        let bar = Rect::new(0, 0, w as u32, super::view::HEADER_H as u32);
        let searching = matches!(self.overlay, Some(Overlay::Input { search: true, .. }));
        if searching {
            let t = p.theme.clone();
            let done = Rect::new(w - MARGIN - 200, (bar.h as i32 - 104) / 2, 200, 104);
            paint_button(p, done, "Done", None, ButtonKind::Filled, true, false);
            self.hits.push((done, Hit::Back));
            p.canvas.fill_rect(0, bar.bottom() - t.rule_px, w, t.rule_px, t.fg);
            return;
        }
        let books = count.is_some();
        let mut ab = AppBar::new("");
        if back {
            ab = ab.with_back();
        }
        if books {
            let icon = if self.mode == ViewMode::List { Icon::Grid } else { Icon::ListView };
            ab = ab.action(BarAction::icon(icon, "mode"));
        }
        // Stats and Settings icons only when the navigation bar does not already have
        // them (the overflow menu keeps both as labelled entries).
        let in_nav = |id: &str| self.nav.iter().any(|n| n == id);
        let (stats, gear) = (!in_nav("stats"), !in_nav("settings"));
        if stats {
            ab = ab.action(BarAction::icon(Icon::Chart, "stats"));
        }
        if gear {
            ab = ab.action(BarAction::icon(Icon::Settings, "settings"));
        }
        ab = ab.action(BarAction::icon(Icon::More, "more"));
        let rects = ab.action_rects(bar);
        let right = rects.iter().filter(|r| r.w > 0).map(|r| r.x).min().unwrap_or(bar.right()) - space(2);
        let role = if back { p.theme.heading } else { p.theme.title };
        let left = if back { AppBar::back_rect(bar).right() + space(1) } else { bar.x + space(4) };
        // Title: at most 40 % of the room when the search field shares the bar.
        let room = (right - left).max(0) as f32;
        let max_title = if books { room * 0.4 } else { room };
        let shown = p.ellipsize(title, role.px, role.style, max_title);
        let tw = p.text_width(role.style, role.px, &shown).ceil() as i32;
        ab.title = shown;
        if let Some(n) = count {
            ab = ab.with_subtitle(format!("{n} book{}", if n == 1 { "" } else { "s" }));
        }
        ab.paint(p, bar);
        if back {
            self.hits.push((AppBar::back_rect(bar), Hit::Back));
        }
        let mut hits = Vec::new();
        if books {
            hits.push(Hit::ToggleMode);
        }
        if stats {
            hits.push(Hit::Stats);
        }
        if gear {
            hits.push(Hit::Settings);
        }
        hits.push(Hit::More);
        for (r, h) in rects.into_iter().zip(hits) {
            if r.w > 0 {
                self.hits.push((r, h));
            }
        }
        if books {
            let x = left + tw + space(3);
            if right - x >= 200 {
                self.draw_search_field(p, Rect::new(x, (bar.h as i32 - 96) / 2, (right - x) as u32, 96));
            } else {
                // No room (long collection name): the field shrinks to its icon.
                let r = Rect::new(right - 96, (bar.h as i32 - 96) / 2, 96, 96);
                paint_button(p, r, "", Some(Icon::Search), ButtonKind::Outlined, true, false);
                self.hits.push((r, Hit::Search));
            }
        }
    }

    /// The search field in the app bar: a pill with a search icon and the query (or a
    /// muted placeholder); with a query, a clear button at its right end.
    fn draw_search_field(&mut self, p: &mut PaintCx, r: Rect) {
        let t = p.theme.clone();
        let rad = r.h as f32 / 2.0;
        stroke_round_rect(p.canvas, r, rad, t.border_px as f32, t.fg);
        let side = 44;
        let ir = Rect::new(r.x + space(3), r.y + (r.h as i32 - side) / 2, side as u32, side as u32);
        Icon::Search.draw(p.canvas, ir, t.fg);
        let tx = ir.right() + space(2);
        self.hits.push((r, Hit::Search));
        if self.query.search.is_empty() {
            p.text_left(Rect::new(tx, r.y, (r.right() - space(3) - tx).max(0) as u32, r.h), "Search", t.label.px, Style::Italic, t.muted);
        } else {
            let clear = Rect::new(r.right() - r.h as i32, r.y, r.h, r.h);
            p.text_left(Rect::new(tx, r.y, (clear.x - tx).max(0) as u32, r.h), &self.query.search, t.label.px, Style::Bold, t.fg);
            p.icon(Icon::Close, clear, 40, t.fg);
            self.hits.push((clear, Hit::ClearSearch));
        }
    }

    /// Top and bottom of the content area of the browse views.
    pub(super) fn content_span(h: i32) -> (i32, i32) {
        (super::view::HEADER_H + TOOLBAR_H, h - TAB_H)
    }

    /// The scrolling region of the browse views: rows or the grid, and the scroll bar in
    /// the column at its right edge ([`ScrollBar::content`] is the part for the rows).
    /// A scroll step refreshes only this rectangle (full width).
    pub(super) fn scroll_region(w: i32, h: i32) -> Rect {
        let (top, bottom) = Self::content_span(h);
        Rect::new(MARGIN, top, (w - MARGIN).max(0) as u32, (bottom - top).max(0) as u32)
    }

    /// Width of the rows (left of the scroll bar's column).
    fn rows_w(w: i32, h: i32) -> u32 {
        ScrollBar::content(Self::scroll_region(w, h)).w
    }

    /// Draws the scroll bar of the current view (nothing when everything fits) and keeps
    /// it for the touch handling.
    fn draw_scroll_bar(&mut self, p: &mut PaintCx, w: i32, h: i32, total: usize, per: usize, page: usize) {
        let region = Self::scroll_region(w, h);
        let bar = ScrollBar::paged(total, per, page);
        bar.paint(p, ScrollBar::track(region));
        self.bar = bar.needed().then_some(super::BarGeom { region, per: per.max(1), bar });
    }

    /// The chip row of the library view: sort, the four status filters, format.
    fn draw_filter_chips(&mut self, p: &mut PaintCx, w: i32) {
        let y = super::view::HEADER_H + (TOOLBAR_H - CHIP_H as i32) / 2;
        let mut chips: Vec<(String, Option<Icon>, bool, Hit)> = Vec::new();
        chips.push((sort_label(self.query.sort).to_string(), Some(Icon::Sort), false, Hit::Sort));
        let fmt = self.query.format.is_some();
        for (i, (f, label)) in StatusFilter::ALL.iter().zip(STATUS_CHIPS).enumerate() {
            chips.push((label.to_string(), None, !fmt && self.query.status == *f, Hit::StatusFilter(i)));
        }
        let fmt_label = self.query.format.as_ref().map_or("Format".to_string(), |f| f.to_uppercase());
        chips.push((fmt_label, Some(Icon::Filter), fmt, Hit::Filter));
        let widths: Vec<u32> = chips.iter().map(|(l, i, on, _)| chip_w(p, l, i.is_some() || *on)).collect();
        let total: u32 = widths.iter().sum();
        let room = (w - 2 * MARGIN) as u32;
        // Spread the chips over the row; squeeze the gaps when the captions are long.
        let n = chips.len() as u32;
        let gap = if total + (n - 1) * space(2) as u32 <= room { space(2) as u32 } else { room.saturating_sub(total) / (n - 1) };
        let mut x = MARGIN;
        for ((label, icon, on, hit), cw) in chips.into_iter().zip(widths) {
            let r = Rect::new(x, y, cw, CHIP_H);
            paint_chip(p, r, &label, icon, on);
            self.hits.push((r, hit));
            x += (cw + gap) as i32;
        }
    }

    /// The library view: chips, then rows or the cover grid, then the pager.
    /// `shelf`: [`LibraryScreen::page_books`].
    pub(super) fn draw_books(&mut self, p: &mut PaintCx, w: i32, h: i32, shelf: (Vec<BookEntry>, usize, usize)) {
        self.draw_filter_chips(p, w);
        let (books, total, page) = shelf;
        let (top, bottom) = Self::content_span(h);
        let rw = Self::rows_w(w, h);
        if books.is_empty() {
            let muted = p.theme.muted;
            let msg = if self.lib.as_ref().is_some_and(|l| l.index.books.is_empty()) { "No books" } else { "No books match" };
            p.text_centered(Rect::new(MARGIN, top + 100, (w - 2 * MARGIN) as u32, 80), msg, p.theme.font_px, Style::Italic, muted);
        }
        match self.mode {
            ViewMode::List => {
                for (i, b) in books.iter().enumerate() {
                    let r = Rect::new(MARGIN, top + i as i32 * LIST_ROW_H, rw, LIST_ROW_H as u32);
                    self.draw_book_row(p, r, b);
                }
            }
            ViewMode::Grid => {
                let cw = rw as i32 / super::view::GRID_COLS;
                let ch = (bottom - top) / super::view::GRID_ROWS;
                for (i, b) in books.iter().enumerate() {
                    let (c, r) = (i as i32 % super::view::GRID_COLS, i as i32 / super::view::GRID_COLS);
                    self.draw_book_cell(p, Rect::new(MARGIN + c * cw, top + r * ch, cw as u32, ch as u32), b);
                }
            }
        }
        let per = self.per_page(h);
        self.draw_scroll_bar(p, w, h, total, per, page);
    }

    /// One list row: cover thumbnail, title, author, and a progress bar or a badge; the
    /// format on the right; a light divider.
    fn draw_book_row(&mut self, p: &mut PaintCx, r: Rect, b: &BookEntry) {
        let t = p.theme.clone();
        let pad = space(2);
        let cover = cover_box(Rect::new(r.x, r.y + pad, 96, (r.h as i32 - 2 * pad) as u32));
        self.paint_book_cover(p, cover, b);
        let tx = r.x + 96 + space(3);
        let fmt = b.format.to_uppercase();
        let fw = p.text_width(Style::Regular, t.caption.px, &fmt).ceil() as i32;
        let tw = (r.right() - tx).max(10) as u32;
        let title_fg = if b.unsupported { t.muted } else { t.fg };
        p.text_left(Rect::new(tx, r.y + pad, tw, 56), &b.title, t.body.px, Style::Bold, title_fg);
        p.text_left(Rect::new(tx, r.y + pad + 56, (tw as i32 - fw - space(3)).max(10) as u32, 40), &authors_text(b), t.caption.px, Style::Regular, t.muted);
        p.text_left(Rect::new(r.right() - fw, r.y + pad + 56, fw as u32 + 2, 40), &fmt, t.caption.px, Style::Regular, t.muted);
        let line = Rect::new(tx, r.y + pad + 100, tw.min(560), 40);
        match book_state(b) {
            Ok(v) => ProgressBar::thin(v).with_label(percent_text(v)).with_label_slot("100%").paint(p, Rect::new(line.x - 4, line.y, line.w, line.h)),
            Err(badge) => {
                let bh = 40;
                let bw = badge_w(p, badge, bh);
                paint_badge(p, Rect::new(line.x, line.y, bw, bh), badge);
            }
        }
        p.canvas.fill_rect(r.x, r.bottom() - t.rule_px, r.w as i32, t.rule_px, t.light);
        self.hits.push((r, Hit::Book(b.path.clone())));
    }

    /// One grid cell: the cover with an "Unread" / "Finished" badge in its bottom-right corner,
    /// a progress bar under it while the book is being read, and the title.
    fn draw_book_cell(&mut self, p: &mut PaintCx, r: Rect, b: &BookEntry) {
        let t = p.theme.clone();
        let title_h = 44;
        let bar_h = 40;
        let area = Rect::new(r.x + space(2), r.y + space(2), r.w - 2 * space(2) as u32, (r.h as i32 - space(2) - bar_h - title_h - space(1)).max(20) as u32);
        let cover = self.paint_book_cover(p, cover_box(area), b);
        let state = book_state(b);
        if let Err(badge) = state {
            let bh = 44;
            let bw = badge_w(p, badge, bh).min(cover.w);
            let br = Rect::new(cover.right() - bw as i32 - space(1), cover.bottom() - bh as i32 - space(1), bw, bh);
            paint_badge(p, br, badge);
        }
        let y = cover.bottom() + space(1);
        if let Ok(v) = state {
            ProgressBar::thin(v).with_label(percent_text(v)).paint(p, Rect::new(area.x, y, area.w, bar_h as u32));
        }
        let fg = if b.unsupported { t.muted } else { t.fg };
        p.text_centered(Rect::new(r.x + space(1), y + bar_h, r.w - 2 * space(1) as u32, title_h as u32), &b.title, t.caption.px, Style::Regular, fg);
        self.hits.push((r, Hit::Book(b.path.clone())));
    }

    /// Collection rows on one page.
    pub(super) fn coll_per_page(h: i32) -> usize {
        let (top, bottom) = Self::content_span(h);
        ((bottom - top) / COLL_ROW_H).max(1) as usize
    }

    /// The Collections tab: a "New collection" button, then one row per collection with
    /// the cover of its first book, the name, the book count and a chevron.
    pub(super) fn draw_collections(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let Some(lib) = self.lib.as_ref() else { return };
        let rows: Vec<(String, usize, Option<BookEntry>)> = lib
            .index
            .collections
            .iter()
            .map(|c| {
                let books = lib.index.collection_books(&c.name);
                (c.name.clone(), books.len(), books.first().map(|b| (*b).clone()))
            })
            .collect();
        let t = p.theme.clone();
        let bh = t.control_h();
        let r = Rect::new(MARGIN, super::view::HEADER_H + (TOOLBAR_H - bh as i32) / 2, 420, bh);
        paint_button(p, r, "New collection", Some(Icon::Plus), ButtonKind::Outlined, true, false);
        self.hits.push((r, Hit::NewCollection));
        let (top, _) = Self::content_span(h);
        let per = Self::coll_per_page(h);
        let total = rows.len();
        let page = self.coll_page.min(ScrollBar::pages_of(total, per) - 1);
        let tw = Self::rows_w(w, h);
        if rows.is_empty() {
            p.text_centered(Rect::new(MARGIN, top + 100, tw, 80), "No collections", t.body.px, Style::Italic, t.muted);
            let lines = ink_ui::dialog::wrap_lines(p, "Create one here, or hold a book in the Library and choose Add to collection.", t.caption.px, Style::Regular, tw as f32);
            for (i, l) in lines.iter().enumerate() {
                p.text_centered(Rect::new(MARGIN, top + 190 + i as i32 * 44, tw, 44), l, t.caption.px, Style::Regular, t.muted);
            }
        }
        for (i, (name, n, first)) in rows.into_iter().skip(page * per).take(per).enumerate() {
            let rr = Rect::new(MARGIN, top + i as i32 * COLL_ROW_H, tw, COLL_ROW_H as u32);
            let lead = Rect::new(rr.x, rr.y + space(2), 96, (COLL_ROW_H - 2 * space(2)) as u32);
            match &first {
                Some(b) => {
                    self.paint_book_cover(p, cover_box(lead), b);
                }
                None => {
                    let r = cover_box(lead);
                    stroke_round_rect(p.canvas, r, 8.0, t.border_px as f32, t.fg);
                    p.icon(Icon::Folder, r, 48, t.fg);
                }
            }
            let tx = lead.right() + space(3);
            let chev = Rect::new(rr.right() - 64, rr.y, 64, rr.h);
            let cw = (chev.x - tx - space(2)).max(10) as u32;
            p.text_left(Rect::new(tx, rr.y + 28, cw, 60), &name, t.body.px, Style::Bold, t.fg);
            p.text_left(Rect::new(tx, rr.y + 90, cw, 44), &format!("{n} book{}", if n == 1 { "" } else { "s" }), t.caption.px, Style::Regular, t.muted);
            p.icon(Icon::Forward, chev, 44, t.fg);
            p.canvas.fill_rect(rr.x, rr.bottom() - t.rule_px, rr.w as i32, t.rule_px, t.light);
            self.hits.push((rr, Hit::Collection(name)));
        }
        self.draw_scroll_bar(p, w, h, total, per, page);
    }

    /// Rows on one file browser page and the number of pages (the "up" row counts).
    pub(super) fn files_per_page(h: i32) -> usize {
        let (top, bottom) = Self::content_span(h);
        ((bottom - top) / FILE_ROW_H).max(1) as usize
    }

    /// Rows of the file browser (the "up" row counts).
    pub(super) fn files_rows(&self) -> usize {
        self.files.len() + usize::from(self.files_dir.parent().is_some())
    }

    /// The file browser: the folder path, then rows with a folder, book or "up" icon.
    pub(super) fn draw_files(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let t = p.theme.clone();
        let tw = (w - 2 * MARGIN) as u32;
        // The folder path, cut at the start so the current folder stays visible.
        let dir = self.files_dir.display().to_string();
        let ir = Rect::new(MARGIN, super::view::HEADER_H + (TOOLBAR_H - 48) / 2, 48, 48);
        Icon::Folder.draw(p.canvas, ir, t.fg);
        let room = (tw as i32 - 48 - space(2)) as f32;
        let mut shown = dir.clone();
        let mut chars: Vec<char> = dir.chars().collect();
        while p.text_width(Style::Regular, t.caption.px, &shown) > room && chars.len() > 1 {
            chars.remove(0);
            shown = format!("…{}", chars.iter().collect::<String>());
        }
        p.text_left(Rect::new(ir.right() + space(2), super::view::HEADER_H, room as u32, TOOLBAR_H as u32), &shown, t.caption.px, Style::Regular, t.muted);
        let (top, _) = Self::content_span(h);
        let per = Self::files_per_page(h);
        let total = self.files_rows();
        let page = self.files_page.min(ScrollBar::pages_of(total, per) - 1);
        let rw = Self::rows_w(w, h);
        let mut rows: Vec<(String, String, Icon, Hit)> = Vec::new();
        if self.files_dir.parent().is_some() {
            rows.push(("Up".to_string(), "Parent folder".to_string(), Icon::Up, Hit::FileUp));
        }
        for e in &self.files {
            let (sub, icon) = if e.dir { ("Folder".to_string(), Icon::Folder) } else { (files::size_text(e.size), Icon::Book) };
            rows.push((e.name.clone(), sub, icon, Hit::File(e.path.clone())));
        }
        for (i, (name, sub, icon, hit)) in rows.into_iter().skip(page * per).take(per).enumerate() {
            let rr = Rect::new(MARGIN, top + i as i32 * FILE_ROW_H, rw, FILE_ROW_H as u32);
            let tile = Rect::new(rr.x, rr.y + (FILE_ROW_H - 88) / 2, 88, 88);
            let book = icon == Icon::Book;
            if book {
                stroke_round_rect(p.canvas, tile, t.radius, t.border_px as f32, t.fg);
            } else {
                fill_round_rect(p.canvas, tile, t.radius, t.fg);
            }
            p.icon(icon, tile, 48, if book { t.fg } else { t.bg });
            let tx = tile.right() + space(3);
            let folder = icon != Icon::Book;
            let chev = Rect::new(rr.right() - 64, rr.y, 64, rr.h);
            let cw = (chev.x - tx - space(2)).max(10) as u32;
            p.text_left(Rect::new(tx, rr.y + 14, cw, 56), &name, t.body.px, if folder { Style::Bold } else { Style::Regular }, t.fg);
            p.text_left(Rect::new(tx, rr.y + 70, cw, 44), &sub, t.caption.px, Style::Regular, t.muted);
            if icon == Icon::Folder {
                p.icon(Icon::Forward, chev, 44, t.fg);
            }
            p.canvas.fill_rect(rr.x, rr.bottom() - t.rule_px, rr.w as i32, t.rule_px, t.light);
            self.hits.push((rr, hit));
        }
        let note = match (&self.files_error, self.files.is_empty()) {
            (Some(e), _) => Some(format!("Cannot read this folder: {e}")),
            (None, true) => Some("No folders or books here".to_string()),
            _ => None,
        };
        if let Some(n) = note {
            let y = top + if self.files_dir.parent().is_some() { FILE_ROW_H + 40 } else { 40 };
            p.text_left(Rect::new(MARGIN + 10, y, tw - 20, 60), &n, t.caption.px, Style::Italic, t.muted);
        }
        self.draw_scroll_bar(p, w, h, total, per, page);
    }
}
