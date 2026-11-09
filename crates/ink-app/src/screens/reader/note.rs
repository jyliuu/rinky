//! The footnote panel: the note's text in a panel across the whole width of the screen
//! at its bottom, a thick rule along its top edge, as KOReader's `FootnoteWidget`
//! (`frontend/ui/widget/footnotewidget.lua`) shows it. Its height follows the text, up
//! to half the screen; a longer note is shown a page at a time (a tap on the text or a
//! swipe turns). It starts with the note's number in bold, as the book prints it before
//! the text, and a *Go to note* button at the right ends it; a tap outside closes it (the overlay's
//! scrim), so it has no Close button.
//!
//! The text is set in the interface family with its real bold and italic faces
//! ([`crate::fonts::with_emphasis_fonts`]), at about the book's text size.

use ink_device::{Canvas, Point, Rect, Size};
use ink_text::Style;
use ink_ui::{paint_button, ButtonKind, Direction, EventResult, Gesture, PaintCx, UiEvent, Widget};

/// Thickness of the rule along the panel's edge (KOReader: `Size.line.thick`).
const RULE: u32 = 4;
/// Space between the rule and the text.
const PAD_TOP: u32 = 28;
/// Space between the text and the button row.
const PAD_TEXT: u32 = 24;
/// Height of the button row.
const ROW_H: u32 = 80;
/// Space under the button row.
const PAD_BOTTOM: u32 = 16;
/// Width of each button.
const BUTTON_W: u32 = 320;

/// A wrapped line: `(x offset, text, style)` pieces; empty between paragraphs.
type Line = Vec<(f32, String, Style)>;

/// The footnote panel (see the module docs).
#[derive(Clone, Debug)]
pub struct NotePanel {
    /// The note's text, as given.
    pub spans: Vec<(String, Style)>,
    /// `(label, action)` of the two buttons.
    pub buttons: Vec<(String, String)>,
    /// Text size in pixels.
    pub px: f32,
    /// Left and right padding of the text.
    pub pad_x: f32,
    /// Drawn at the top of the screen (the rule along its bottom edge): the panel at
    /// the bottom would have covered the tapped link.
    pub at_top: bool,
    /// The lines, wrapped to the panel's width.
    lines: Vec<Line>,
    /// The lines of each page (ranges into `lines`).
    pub pages: Vec<std::ops::Range<usize>>,
    /// Page shown.
    pub page: usize,
    /// The page just changed: the next damage report is the whole panel.
    turned: bool,
}

impl NotePanel {
    /// Height of a text line.
    fn line_h(&self) -> f32 {
        (self.px * 1.35).round()
    }

    /// Height of the gap that separates paragraphs.
    fn gap_h(&self) -> f32 {
        (self.px * 0.4).round()
    }

    fn height_of(&self, line: &Line) -> f32 {
        if line.is_empty() {
            self.gap_h()
        } else {
            self.line_h()
        }
    }

    /// The panel for `spans` (paragraphs separated by `\n`) at `px` on a `screen`,
    /// text padded `pad_x` at the sides, and where it goes: at the bottom, full width,
    /// as high as its text (at most half the screen), or at the top when the bottom
    /// would cover `anchor` (the tapped link) and the top would not.
    pub fn new(spans: Vec<(String, Style)>, buttons: Vec<(String, String)>, px: f32, pad_x: f32, screen: Size, anchor: Rect) -> (NotePanel, Rect) {
        let width = (screen.w as f32 - 2.0 * pad_x).max(px * 4.0);
        let lines = crate::fonts::with_emphasis_fonts(|fonts| {
            let mut canvas = Canvas::new(1, 1);
            let theme = ink_ui::Theme::day();
            ink_ui::dialog::wrap_spans(&mut PaintCx { canvas: &mut canvas, fonts, theme: &theme }, &spans, px, width)
        });
        let mut p = NotePanel { spans, buttons, px, pad_x, at_top: false, lines, pages: Vec::new(), page: 0, turned: false };
        let chrome = RULE + PAD_TOP + PAD_TEXT + ROW_H + PAD_BOTTOM;
        let room = (screen.h / 2).saturating_sub(chrome) as f32;
        // Pages of lines: each at most `room` high, none starting with a paragraph gap.
        let (mut start, mut h, mut tallest) = (0, 0.0f32, 0.0f32);
        for (i, line) in p.lines.iter().enumerate() {
            if i == start && line.is_empty() {
                start += 1;
                continue;
            }
            let lh = p.height_of(line);
            if h + lh > room && i > start {
                p.pages.push(start..i);
                tallest = tallest.max(h);
                (start, h) = (if line.is_empty() { i + 1 } else { i }, if line.is_empty() { 0.0 } else { lh });
                continue;
            }
            h += lh;
        }
        if start < p.lines.len() || p.pages.is_empty() {
            p.pages.push(start..p.lines.len());
            tallest = tallest.max(h);
        }
        let height = (chrome + tallest.ceil() as u32).min(screen.h);
        let bottom_y = screen.h as i32 - height as i32;
        p.at_top = anchor.h > 0 && anchor.bottom() > bottom_y && anchor.y >= height as i32;
        let y = if p.at_top { 0 } else { bottom_y };
        (p, Rect::new(0, y, screen.w, height))
    }

    /// The text area inside the panel's bounds `b`.
    fn text_rect(&self, b: Rect) -> Rect {
        let top = if self.at_top { b.y } else { b.y + RULE as i32 };
        let row = self.row_rect(b);
        Rect::new(b.x, top, b.w, (row.y - top).max(0) as u32)
    }

    /// The button row inside `b`.
    fn row_rect(&self, b: Rect) -> Rect {
        let bottom = if self.at_top { b.bottom() - RULE as i32 } else { b.bottom() };
        Rect::new(b.x, bottom - (PAD_BOTTOM + ROW_H) as i32, b.w, ROW_H)
    }

    /// Where the buttons are inside the panel's bounds `b`, under the text's padding: a
    /// single button at the right, two at the left and the right.
    pub fn button_rects(&self, b: Rect) -> Vec<Rect> {
        let row = self.row_rect(b);
        let pad = self.pad_x.round() as i32;
        let w = BUTTON_W.min(b.w / 2 - pad as u32);
        let left = Rect::new(b.x + pad, row.y, w, ROW_H);
        let right = Rect::new(b.right() - pad - w as i32, row.y, w, ROW_H);
        match self.buttons.len() {
            0 => Vec::new(),
            1 => vec![right],
            _ => vec![left, right],
        }
    }

    /// Turns `by` pages (wrapping round); true when the page changed.
    fn turn(&mut self, by: isize) -> bool {
        let n = self.pages.len() as isize;
        if n < 2 {
            return false;
        }
        self.page = (self.page as isize + by).rem_euclid(n) as usize;
        self.turned = true;
        true
    }
}

impl Widget for NotePanel {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        let rule_y = if self.at_top { b.bottom() - RULE as i32 } else { b.y };
        cx.canvas.fill_rect(b.x, rule_y, b.w as i32, RULE as i32, fg);
        let text = self.text_rect(b);
        let lines = self.pages.get(self.page).cloned().unwrap_or(0..0);
        let (px, left) = (self.px, b.x as f32 + self.pad_x);
        let mut y = (text.y + PAD_TOP as i32) as f32;
        crate::fonts::with_emphasis_fonts(|fonts| {
            let mut tcx = PaintCx { canvas: &mut *cx.canvas, fonts, theme: cx.theme };
            for line in &self.lines[lines] {
                if !line.is_empty() {
                    // The baseline sits about 80 % down the line box.
                    let base = y + (self.line_h() + px * 0.6) / 2.0;
                    for (dx, t, s) in line {
                        tcx.draw_text_clipped(*s, px, left + dx, base, t, fg, text);
                    }
                }
                y += self.height_of(line);
            }
        });
        let rects = self.button_rects(b);
        for (r, (label, _)) in rects.iter().zip(&self.buttons) {
            paint_button(cx, *r, label, None, ButtonKind::Outlined, true, false);
        }
        if self.pages.len() > 1 {
            let row = self.row_rect(b);
            let mark = format!("{} / {}", self.page + 1, self.pages.len());
            let mpx = cx.theme.font_px * 0.75;
            let w = cx.text_width(Style::Regular, mpx, &mark);
            let base = row.y as f32 + (ROW_H as f32 + mpx * 0.7) / 2.0;
            cx.draw_text(Style::Regular, mpx, b.x as f32 + (b.w as f32 - w) / 2.0, base, &mark, fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let rects = self.button_rects(b);
        let in_text = |p: Point| self.text_rect(b).contains(p);
        match ev {
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                if let Some((_, (_, a))) = rects.iter().zip(&self.buttons).find(|(r, _)| r.contains(*pos)) {
                    return EventResult::Action(a.clone());
                }
                if in_text(*pos) {
                    self.turn(1);
                }
                EventResult::Handled
            }
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Left, .. }) => {
                self.turn(1);
                EventResult::Handled
            }
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Right, .. }) => {
                self.turn(-1);
                EventResult::Handled
            }
            UiEvent::Gesture(_) | UiEvent::Touch(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        // Touches change nothing but the page: only a turn refreshes the panel.
        if std::mem::take(&mut self.turned) {
            None
        } else {
            Some(Vec::new())
        }
    }
}
