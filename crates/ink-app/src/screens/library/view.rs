//! Painting of the library screen. Every paint draws the whole view and records the
//! tap targets ([`Hit`]) the next events are matched against.

use super::{Hit, LibraryScreen, Overlay, View, ViewMode};
use ink_device::Rect;
use ink_store::BookEntry;
use ink_text::Style;
use ink_ui::draw::{fill_round_rect, stroke_round_rect};
use ink_ui::{paint_button, ButtonKind, PaintCx, Widget};

/// Header height (title, back, actions): the shared app bar's height, so its rule sits
/// where the other screens' rules do.
pub const HEADER_H: i32 = ink_ui::bars::APP_BAR_H as i32;
/// Tab bar height at the bottom.
pub const TAB_H: i32 = 130;
/// Toolbar under the header in the library view.
pub const TOOLBAR_H: i32 = 120;
/// Side margin.
pub const MARGIN: i32 = 32;
/// Grid columns (portrait).
pub const GRID_COLS: i32 = 3;
/// Grid rows.
pub const GRID_ROWS: i32 = 3;

/// Draws a pill button (`ink_ui::paint_button`); `primary` makes it filled.
pub fn button(p: &mut PaintCx, r: Rect, label: &str, primary: bool) {
    let kind = if primary { ButtonKind::Filled } else { ButtonKind::Outlined };
    paint_button(p, r, label, None, kind, true, false);
}

impl LibraryScreen {
    /// Paints everything into `p.canvas`.
    pub(super) fn draw(&mut self, p: &mut PaintCx) {
        let (w, h) = (p.canvas.width as i32, p.canvas.height as i32);
        p.canvas.clear(p.theme.bg);
        self.hits.clear();
        self.bar = None;
        let title = match &self.view {
            View::Home => "Home".to_string(),
            View::Books => self.query.collection.clone().unwrap_or_else(|| "Library".into()),
            View::Collections => "Collections".into(),
            View::Details(_) => "Book details".into(),
            View::Import => "KOReader import".into(),
            View::Files => "Files".into(),
        };
        let back = matches!(self.view, View::Details(_) | View::Files) || (self.view == View::Books && self.query.collection.is_some());
        // The library view's query (sort, filter, search over every book) runs once per
        // frame: the app bar's count and the rows both come from it.
        let shelf = (self.view == View::Books).then(|| self.page_books(h));
        self.draw_app_bar(p, w, &title, back, shelf.as_ref().map(|s| s.1));
        if self.job.is_some() {
            let fg = p.theme.fg;
            let r = Rect::new(MARGIN, HEADER_H + 120, (w - 2 * MARGIN) as u32, 80);
            p.text_left(r, "Please wait", p.theme.font_px * 1.2, Style::Bold, fg);
            self.draw_busy_line(p);
            return;
        }

        match self.view.clone() {
            View::Home => self.draw_home(p, w, h),
            View::Books => self.draw_books(p, w, h, shelf.unwrap_or_default()),
            View::Collections => self.draw_collections(p, w, h),
            View::Details(path) => self.draw_details(p, w, h, &path),
            View::Import => self.draw_import(p, w, h),
            View::Files => self.draw_files(p, w, h),
        }
        self.draw_nav(p, w, h);
        self.draw_overlay(p, w, h);
    }

    /// The progress line of a running rescan or import; returns its rectangle.
    pub(super) fn draw_busy_line(&mut self, p: &mut PaintCx) -> Rect {
        let w = p.canvas.width as i32;
        let r = Rect::new(MARGIN, HEADER_H + 220, (w - 2 * MARGIN) as u32, 70);
        p.canvas.fill_rect(r.x, r.y, r.w as i32, r.h as i32, p.theme.bg);
        let fg = p.theme.fg;
        p.text_left(r, &self.busy_text, p.theme.font_px, Style::Regular, fg);
        r
    }

    /// Books on the current screenful of the library view, the number of books that
    /// match, and the (clamped) screen index.
    pub(super) fn page_books(&self, h: i32) -> (Vec<BookEntry>, usize, usize) {
        let Some(lib) = self.lib.as_ref() else { return (Vec::new(), 0, 0) };
        let all = lib.query(&self.query);
        let per = self.per_page(h).max(1);
        let page = self.page.min(ink_ui::ScrollBar::pages_of(all.len(), per) - 1);
        let total = all.len();
        (all.into_iter().skip(page * per).take(per).cloned().collect(), total, page)
    }

    /// Books per screenful in the current mode.
    pub(super) fn per_page(&self, h: i32) -> usize {
        let content = h - HEADER_H - TOOLBAR_H - TAB_H;
        match self.mode {
            ViewMode::List => (content / super::browse::LIST_ROW_H).max(1) as usize,
            ViewMode::Grid => (GRID_COLS * GRID_ROWS) as usize,
        }
    }

    fn draw_import(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let (fg, muted) = (p.theme.fg, p.theme.muted);
        let tw = (w - 2 * MARGIN) as f32;
        let mut y = HEADER_H + 40;
        let intro = format!(
            "Takes reading positions, highlights, notes, bookmarks, status and ratings from KOReader ({}) into rinky, and KOReader's reading statistics. KOReader's files are only read, never changed.",
            self.ko.root.display()
        );
        for l in ink_ui::dialog::wrap_lines(p, &intro, p.theme.small_px, Style::Regular, tw) {
            p.text_left(Rect::new(MARGIN, y, tw as u32, 44), &l, p.theme.small_px, Style::Regular, fg);
            y += 44;
        }
        y += 30;
        let r = Rect::new(MARGIN, y, (w - 2 * MARGIN) as u32, 104);
        button(p, r, "Import from KOReader", true);
        self.hits.push((r, Hit::Import));
        y += 130;
        let half = ((w - 2 * MARGIN - 30) / 2) as u32;
        let r = Rect::new(MARGIN, y, half, 104);
        button(p, r, "Rescan library", false);
        self.hits.push((r, Hit::Rescan));
        let r = Rect::new(MARGIN + half as i32 + 30, y, half, 104);
        button(p, r, "Browse files", false);
        self.hits.push((r, Hit::Browse));
        y += 140;
        if let Some(lib) = self.lib.as_ref() {
            let mut info = vec![format!("Books in the library: {}", lib.index.books.len())];
            if lib.pending() > 0 {
                info.push(format!("Reading metadata: {} books left", lib.pending()));
            }
            for d in &lib.unreadable {
                info.push(format!("Cannot read {}", d.display()));
            }
            for l in info {
                p.text_left(Rect::new(MARGIN, y, tw as u32, 44), &l, p.theme.small_px, Style::Regular, muted);
                y += 46;
            }
        }
        if let Some(lines) = self.import_result.clone() {
            y += 20;
            p.text_left(Rect::new(MARGIN, y, tw as u32, 56), "Last import", p.theme.font_px, Style::Bold, fg);
            y += 64;
            for l in lines {
                if y + 46 > h - TAB_H {
                    break;
                }
                p.text_left(Rect::new(MARGIN, y, tw as u32, 44), &l, p.theme.small_px, Style::Regular, fg);
                y += 46;
            }
        }
    }

    fn draw_overlay(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let size = ink_device::Size::new(w as u32, h as u32);
        match &mut self.overlay {
            None => {}
            Some(Overlay::Menu { menu, rect, .. }) => {
                p.canvas.fill_rect(rect.x, rect.y, rect.w as i32, rect.h as i32, p.theme.bg);
                menu.paint(p, *rect);
            }
            Some(Overlay::Confirm { dialog, .. }) => {
                let f = ink_ui::Dialog::frame(size);
                dialog.paint(p, f);
            }
            Some(Overlay::Input { search, title, input, keyboard, .. }) => {
                let kb = super::keyboard_rect(size);
                p.canvas.fill_rect(kb.x, kb.y, kb.w as i32, kb.h as i32, p.theme.bg);
                keyboard.paint(p, kb);
                let _ = keyboard.damage(kb);
                let field = super::field_rect(size, *search);
                if !*search {
                    // Name panel above the keyboard.
                    let panel = super::name_panel(size);
                    fill_round_rect(p.canvas, panel, p.theme.radius, p.theme.bg);
                    stroke_round_rect(p.canvas, panel, p.theme.radius, p.theme.border_px as f32, p.theme.fg);
                    let fg = p.theme.fg;
                    p.text_left(Rect::new(panel.x + 24, panel.y + 10, panel.w - 48, 60), title, p.theme.font_px, Style::Bold, fg);
                    let (cancel, ok) = super::name_buttons(size);
                    button(p, cancel, "Cancel", false);
                    button(p, ok, "OK", true);
                }
                p.canvas.fill_rect(field.x, field.y, field.w as i32, field.h as i32, p.theme.bg);
                input.paint(p, field);
            }
        }
    }
}

/// `1.2 MB`, `340 KB`.
pub fn human_size(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", n.div_ceil(1024))
    }
}
