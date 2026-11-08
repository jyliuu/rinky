//! Dialog, toast, menu, popup, bottom sheet (the Kindle "Aa" panel style) and scrim.
//!
//! Each is a [`Widget`] meant to be added as a top-level layer of a [`crate::WidgetTree`].
//! Modal layers go on top of a [`Scrim`]: it takes every touch outside them and reports
//! its action (for example `dismiss`) on a tap, so nothing underneath reacts.

use crate::draw::{fill_round_rect, fill_top_round_rect, stroke_round_rect, stroke_top_round_rect};
use crate::gesture::{Direction, Gesture};
use crate::icon::Icon;
use crate::widget::{paint_button, split_rows, ButtonKind, EventResult, PaintCx, Widget};
use crate::UiEvent;
use ink_device::{Rect, RefreshClass, Size};
use ink_text::hyphen::NoHyphenation;
use ink_text::linebreak::wrap;
use ink_text::{Style, TextRun};
use std::time::Duration;

/// A modal message with buttons. Tapping a button reports its action; tapping outside
/// the buttons is consumed.
#[derive(Clone, Debug, PartialEq)]
pub struct Dialog {
    /// Title line.
    pub title: String,
    /// Body text (wrapped).
    pub body: String,
    /// `(label, action)` pairs, left to right.
    pub buttons: Vec<(String, String)>,
}

impl Dialog {
    /// Centred rectangle for a dialog on a screen of `screen` size.
    pub fn frame(screen: Size) -> Rect {
        let w = screen.w * 4 / 5;
        let h = screen.h / 3;
        Rect::new(((screen.w - w) / 2) as i32, ((screen.h - h) / 2) as i32, w, h)
    }

    fn button_rects(&self, b: Rect) -> Vec<Rect> {
        let n = self.buttons.len().max(1) as u32;
        let h = 110;
        let y = b.bottom() - h as i32 - 24;
        let w = b.w.saturating_sub(48) / n;
        (0..n).map(|i| Rect::new(b.x + 24 + (i * w) as i32, y, w.saturating_sub(12), h)).collect()
    }
}

/// Wraps `text` into lines no wider than `max` pixels (shaped widths).
pub fn wrap_lines(cx: &mut PaintCx, text: &str, px: f32, style: Style, max: f32) -> Vec<String> {
    let fonts = &mut *cx.fonts;
    let mut measure = |s: &str| fonts.shaped_width(style, px, s);
    wrap(text, max, &mut measure, &NoHyphenation, false)
        .into_iter()
        .map(|l| format!("{}{}", &text[l.range], if l.hyphen { "-" } else { "" }))
        .collect()
}

impl Widget for Dialog {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        fill_round_rect(cx.canvas, b, t.radius_lg, t.bg);
        stroke_round_rect(cx.canvas, b, t.radius_lg, t.border_px as f32, t.fg);
        let rows = split_rows(Rect::new(b.x + 32, b.y + 24, b.w.saturating_sub(64), b.h.saturating_sub(48)), &[80]);
        let px = t.font_px;
        // Title: heading role, left-aligned with the body.
        cx.text_left(rows[0], &self.title, t.heading.px, t.heading.style, t.fg);
        let mut y = rows[0].bottom() as f32 + px * 1.2;
        let fg = cx.theme.fg;
        // Body lines between the title and the buttons; lines that do not fit are left out.
        let buttons_top = self.button_rects(b).first().map_or(b.bottom() - 24, |r| r.y);
        let body = Rect::new(b.x + 32, rows[0].bottom(), b.w.saturating_sub(64), (buttons_top - 8 - rows[0].bottom()).max(0) as u32);
        let (_, desc) = cx.fonts.vmetrics(Style::Regular, px * 0.9);
        for line in wrap_lines(cx, &self.body, px * 0.9, Style::Regular, b.w.saturating_sub(64) as f32) {
            if y - desc > body.bottom() as f32 {
                break;
            }
            cx.draw_text_clipped(Style::Regular, px * 0.9, (b.x + 32) as f32, y, &line, fg, body);
            y += px * 1.3;
        }
        for (i, (r, (label, _))) in self.button_rects(b).into_iter().zip(&self.buttons).enumerate() {
            // The last button is the primary one (filled); the others are outlined.
            let primary = i + 1 == self.buttons.len() && self.buttons.len() > 1;
            let kind = if primary { ButtonKind::Filled } else { ButtonKind::Outlined };
            paint_button(cx, r, label, None, kind, true, false);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        buttons_event(ev, &self.button_rects(b), &self.buttons)
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        // Touches change nothing here (no press feedback): they refresh nothing.
        Some(Vec::new())
    }
}

/// What a modal box with `(label, action)` buttons at `rects` does with an event: a tap
/// on a button reports its action; every other touch and gesture is taken (nothing
/// under the box reacts).
fn buttons_event(ev: &UiEvent, rects: &[Rect], buttons: &[(String, String)]) -> EventResult {
    match ev {
        UiEvent::Gesture(Gesture::Tap { pos }) => match rects.iter().zip(buttons).find(|(r, _)| r.contains(*pos)) {
            Some((_, (_, action))) => EventResult::Action(action.clone()),
            None => EventResult::Handled,
        },
        UiEvent::Gesture(_) | UiEvent::Touch(_) => EventResult::Handled,
        _ => EventResult::Ignored,
    }
}

/// A short message near the bottom that disappears after `timeout`
/// (add it with [`crate::WidgetTree::add_timed`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    /// Text.
    pub text: String,
    /// How long it stays.
    pub timeout: Duration,
}

impl Toast {
    /// A toast shown for 2.5 s.
    pub fn new(text: impl Into<String>) -> Self {
        Toast { text: text.into(), timeout: Duration::from_millis(2500) }
    }

    /// Rectangle for a toast on a screen of `screen` size.
    pub fn frame(screen: Size) -> Rect {
        let w = screen.w * 3 / 4;
        Rect::new(((screen.w - w) / 2) as i32, screen.h.saturating_sub(300) as i32, w, 120)
    }
}

impl Widget for Toast {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        // A black pill with white text: readable after a DU update on any background.
        fill_round_rect(cx.canvas, b, b.h as f32 / 2.0, cx.theme.fg);
        let px = cx.theme.font_px * 0.9;
        let pad = (b.h / 3).min(b.w / 4);
        let inner = Rect::new(b.x + pad as i32, b.y, b.w.saturating_sub(2 * pad), b.h);
        cx.text_centered(inner, &self.text, px, Style::Regular, cx.theme.bg);
    }

    fn refresh_class(&self) -> RefreshClass {
        RefreshClass::Du
    }
}

/// A vertical list of items; tapping one reports its action.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    /// `(label, action)` pairs.
    pub items: Vec<(String, String)>,
    /// Row height.
    pub row_h: u32,
}

impl Menu {
    /// A menu with touch-sized rows.
    pub fn new(items: Vec<(String, String)>) -> Self {
        Menu { items, row_h: 110 }
    }

    /// Height needed for all rows.
    pub fn height(&self) -> u32 {
        self.row_h * self.items.len() as u32
    }
}

/// A menu label with a leading "✓ " marks the chosen entry. The check is drawn as
/// `Icon::Check` (the fonts may have no U+2713); returns whether it was there and the
/// label without it.
pub fn split_check(label: &str) -> (bool, &str) {
    match label.strip_prefix("\u{2713} ") {
        Some(rest) => (true, rest),
        None => (false, label),
    }
}

impl Widget for Menu {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        fill_round_rect(cx.canvas, b, t.radius_lg, t.bg);
        stroke_round_rect(cx.canvas, b, t.radius_lg, t.border_px as f32, t.fg);
        let rows = split_rows(b, &vec![self.row_h; self.items.len()]);
        // The check icon's column (icon side + gap), on every row when any is checked.
        let gutter = if self.items.iter().any(|(l, _)| split_check(l).0) { 48 + crate::theme::space(2) } else { 0 };
        for (i, (r, (label, _))) in rows.iter().zip(&self.items).enumerate() {
            let (checked, label) = split_check(label);
            if i > 0 {
                // Dividers: light and at least rule_px thick (thin gray lines ghost).
                cx.canvas.fill_rect(r.x + 32, r.y - t.rule_px / 2, r.w as i32 - 64, t.rule_px, t.light);
            }
            let (px, fg) = (cx.theme.font_px, cx.theme.fg);
            if checked {
                cx.icon(Icon::Check, Rect::new(r.x + 32, r.y, 48, r.h), 48, fg);
            }
            cx.text_left(Rect::new(r.x + 32 + gutter, r.y, r.w.saturating_sub(48 + gutter as u32), r.h), label, px, Style::Regular, fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        if let UiEvent::Gesture(Gesture::Tap { pos }) = ev {
            let i = ((pos.y - b.y).max(0) as u32 / self.row_h) as usize;
            if let Some((_, action)) = self.items.get(i) {
                return EventResult::Action(action.clone());
            }
        }
        EventResult::Handled
    }
}

/// A panel sliding up from the bottom edge (the stock "Aa" menu style). Content widgets
/// are added as its children; a downward swipe reports `close`.
#[derive(Clone, Debug, PartialEq)]
pub struct BottomSheet {
    /// Title shown on the sheet's top bar.
    pub title: String,
    /// The sheet took a gesture that changes nothing on it (set by `event`, cleared by
    /// `damage`).
    pub taken: bool,
}

impl BottomSheet {
    /// Rectangle for a sheet `height` pixels tall.
    pub fn frame(screen: Size, height: u32) -> Rect {
        let h = height.min(screen.h);
        Rect::new(0, (screen.h - h) as i32, screen.w, h)
    }

    /// The area below the title bar, for content.
    pub fn content(b: Rect) -> Rect {
        Rect::new(b.x + 24, b.y + 130, b.w.saturating_sub(48), b.h.saturating_sub(150))
    }
}

impl Widget for BottomSheet {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        // Rounded top corners and a black edge along the top and sides (the sheet
        // stands on the bottom edge of the screen).
        fill_top_round_rect(cx.canvas, b, t.radius_lg, t.bg);
        stroke_top_round_rect(cx.canvas, b, t.radius_lg, t.border_px, t.fg);
        // The side edges stop 8 px above the bottom: the panel's last 8 px stay empty.
        cx.canvas.fill_rect(b.x, b.bottom() - 8, b.w as i32, 8, t.bg);
        // Grab handle: a pill, mid gray, thick enough not to ghost.
        fill_round_rect(cx.canvas, Rect::new(b.x + b.w as i32 / 2 - 60, b.y + 20, 120, 12), 6.0, t.subtle);
        let r = Rect::new(b.x, b.y + 36, b.w, 80);
        cx.text_centered(r, &self.title, t.heading.px, t.heading.style, t.fg);
        cx.canvas.fill_rect(b.x + 24, b.y + 117, b.w as i32 - 48, t.rule_px, t.light);
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Down, .. }) => EventResult::Action("close".into()),
            // Taps on the sheet's empty parts do not fall through to what is under it.
            UiEvent::Gesture(_) => {
                self.taken = true;
                EventResult::Handled
            }
            // Touch notifications are left alone: a widget that takes one is painted
            // again, and for the sheet that is every control on it (the touch-down and
            // the lift of each tap on a control the sheet does not handle itself).
            _ => EventResult::Ignored,
        }
    }

    /// A tap on the sheet's empty parts changes nothing: no rectangle, so no
    /// refresh (the tree would otherwise repaint the whole sheet, which for a tall sheet
    /// becomes a full-screen flash). The first paint is reported in full.
    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        if std::mem::take(&mut self.taken) {
            Some(Vec::new())
        } else {
            None
        }
    }
}

/// A small box next to a point of interest (footnotes, dictionary, selection tools):
/// optional title, wrapped text, optional buttons. Put a [`Scrim`] under it so a tap
/// outside closes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Popup {
    /// Bold first line, a size above the body (empty: none).
    pub title: String,
    /// Body text.
    pub text: String,
    /// `(label, action)` buttons along the bottom.
    pub buttons: Vec<(String, String)>,
    /// Body text with inline styles (italic, bold); when not empty it is shown instead
    /// of `text`. White space between spans is as given.
    pub spans: Vec<(String, Style)>,
}

/// Wraps styled `spans` into lines no wider than `max` pixels: each line is a list of
/// `(x offset, text, style)` pieces. Breaks only at spaces; a word that changes style
/// inside (`foo<i>bar</i>`) stays whole; a word wider than `max` gets a line of its own.
pub fn wrap_spans(cx: &mut PaintCx, spans: &[(String, Style)], px: f32, max: f32) -> Vec<Vec<(f32, String, Style)>> {
    span_lines(cx, spans, px, max).into_iter().map(|l| l.into_iter().map(|(x, t, s, _)| (x, t, s)).collect()).collect()
}

/// [`wrap_spans`] with each piece's shaped run, which measured it and draws it.
fn span_lines(cx: &mut PaintCx, spans: &[(String, Style)], px: f32, max: f32) -> Vec<Vec<(f32, String, Style, TextRun)>> {
    // Words as pieces of one style each, and whether a space comes before the word.
    let mut words: Vec<(bool, Vec<(String, Style)>)> = Vec::new();
    let mut space = true;
    for (text, style) in spans {
        for (i, part) in text.split(' ').enumerate() {
            if i > 0 {
                space = true;
            }
            if part.is_empty() {
                continue;
            }
            match words.last_mut() {
                Some((_, w)) if !space => w.push((part.to_string(), *style)),
                _ => words.push((!words.is_empty(), vec![(part.to_string(), *style)])),
            }
            space = false;
        }
    }
    let fonts = &mut *cx.fonts;
    let space_w = fonts.shaped_width(Style::Regular, px, " ");
    let mut lines = Vec::new();
    let mut line = Vec::new();
    let mut x = 0.0f32;
    for (gap, pieces) in words {
        let runs: Vec<TextRun> = pieces.iter().map(|(t, s)| fonts.shape(*s, px, t)).collect();
        let w: f32 = runs.iter().map(|r| r.width).sum();
        let gap = if gap && !line.is_empty() { space_w } else { 0.0 };
        if !line.is_empty() && x + gap + w > max {
            lines.push(std::mem::take(&mut line));
            x = 0.0;
        } else {
            x += gap;
        }
        for ((t, s), run) in pieces.into_iter().zip(runs) {
            let pw = run.width;
            line.push((x, t, s, run));
            x += pw;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

impl Popup {
    /// Height of the button row.
    const BUTTON_H: u32 = 96;

    /// A rectangle `size` big next to `anchor`: below it when there is room, else above,
    /// moved sideways to stay on a screen of `screen` size.
    pub fn frame(anchor: Rect, size: Size, screen: Size) -> Rect {
        let w = size.w.min(screen.w);
        let h = size.h.min(screen.h);
        let x = (anchor.x + anchor.w as i32 / 2 - w as i32 / 2).clamp(0, (screen.w - w) as i32);
        let below = anchor.bottom() + 16;
        let y = if below + h as i32 <= screen.h as i32 {
            below
        } else {
            (anchor.y - 16 - h as i32).max(0)
        };
        Rect::new(x, y, w, h)
    }

    fn button_rects(&self, b: Rect) -> Vec<Rect> {
        let row = Rect::new(b.x + 16, b.bottom() - Popup::BUTTON_H as i32 - 12, b.w.saturating_sub(32), Popup::BUTTON_H);
        crate::widget::split_cols(row, self.buttons.len() as u32, 12)
    }
}

impl Widget for Popup {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        // A drop shadow to the bottom right, then the box.
        // A solid offset shadow (subtle gray, no blur: gradients do not survive the panel).
        let t = cx.theme;
        let shadow = Rect::new(b.x + 8, b.y + 8, b.w.saturating_sub(8), b.h.saturating_sub(8));
        fill_round_rect(cx.canvas, shadow, t.radius_lg, t.subtle);
        let body = Rect::new(b.x, b.y, b.w.saturating_sub(8), b.h.saturating_sub(8));
        fill_round_rect(cx.canvas, body, t.radius_lg, t.bg);
        stroke_round_rect(cx.canvas, body, t.radius_lg, t.border_px as f32, t.fg);
        let (fg, px) = (cx.theme.fg, cx.theme.font_px * 0.85);
        let inner = Rect::new(body.x + 8, body.y + 4, body.w.saturating_sub(16), body.h.saturating_sub(8));
        let (left, max) = ((body.x + 24) as f32, body.w.saturating_sub(48) as f32);
        let mut y = body.y as f32 + 24.0 + px;
        if !self.title.is_empty() {
            // A small heading: bold, the body role's size (a step above the popup's
            // text), with half a line of air under it.
            let tpx = cx.theme.body.px.max(px);
            y = body.y as f32 + 24.0 + tpx;
            let title = cx.ellipsize(&self.title, tpx, Style::Bold, max);
            cx.draw_text_clipped(Style::Bold, tpx, left, y, &title, fg, inner);
            y += tpx * 0.55 + px * 1.3;
        }
        let bottom = if self.buttons.is_empty() { body.bottom() } else { body.bottom() - Popup::BUTTON_H as i32 - 12 };
        if self.spans.is_empty() {
            for line in wrap_lines(cx, &self.text, px, Style::Regular, max) {
                if y > bottom as f32 - 8.0 {
                    break;
                }
                cx.draw_text_clipped(Style::Regular, px, left, y, &line, fg, inner);
                y += px * 1.3;
            }
        } else {
            for line in span_lines(cx, &self.spans, px, max) {
                if y > bottom as f32 - 8.0 {
                    break;
                }
                for (dx, _, _, run) in line {
                    cx.put_run(&run, left + dx, y, fg, Some(inner));
                }
                y += px * 1.3;
            }
        }
        // Popups hold tools of equal weight (Highlight, Search, Look up, Cancel): all
        // outlined.
        for (r, (label, _)) in self.button_rects(body).into_iter().zip(&self.buttons) {
            paint_button(cx, r, label, None, ButtonKind::Outlined, true, false);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let body = Rect::new(b.x, b.y, b.w.saturating_sub(8), b.h.saturating_sub(8));
        buttons_event(ev, &self.button_rects(body), &self.buttons)
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        // Touches change nothing here (no press feedback): they refresh nothing.
        Some(Vec::new())
    }
}

/// An invisible layer under modal widgets: it swallows every touch and gesture and
/// reports `action` on a tap (empty: none).
///
/// It draws nothing, so it adds no damage ([`Widget::adds_damage`]): adding it, touching
/// it or removing it refreshes nothing; only the modal widget on top is shown. Touches
/// stay swallowed (not `Ignored`), so a row under a dialog never shows a press.
#[derive(Clone, Debug, PartialEq)]
pub struct Scrim {
    /// Reported on a tap outside the modal widget.
    pub action: String,
}

impl Scrim {
    /// A scrim reporting `action` on taps.
    pub fn new(action: impl Into<String>) -> Self {
        Scrim { action: action.into() }
    }
}

impl Widget for Scrim {
    fn paint(&mut self, _cx: &mut PaintCx, _b: Rect) {}

    fn opaque(&self) -> bool {
        false
    }

    fn adds_damage(&self) -> bool {
        false
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Tap { .. }) if !self.action.is_empty() => EventResult::Action(self.action.clone()),
            UiEvent::Gesture(_) | UiEvent::Touch(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Button, Theme, WidgetTree};
    use ink_device::{Canvas, Point};
    use ink_text::Fonts;

    #[test]
    fn check_prefix_is_split_off_and_drawn_as_an_icon() {
        assert_eq!(split_check("\u{2713} Title"), (true, "Title"));
        assert_eq!(split_check("Title"), (false, "Title"));
        // A checked row paints dark pixels in the check gutter without any font glyph.
        let mut tree = WidgetTree::new();
        let b = Rect::new(0, 0, 600, 220);
        tree.add(None, Box::new(Menu::new(vec![("\u{2713} A".into(), "a".into()), ("B".into(), "b".into())])), b);
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(600, 220);
        canvas.clear(255);
        tree.paint(&mut crate::PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme });
        let dark = |r: Rect| (r.y..r.bottom()).any(|y| (r.x..r.right()).any(|x| canvas.get(x, y).unwrap_or(255) < 100));
        assert!(dark(Rect::new(32, 30, 48, 50)), "check icon in row 1");
        assert!(!dark(Rect::new(32, 140, 48, 50)), "no icon in row 2, the gutter stays empty");
    }

    #[test]
    fn tiny_frames_do_not_panic() {
        let d = Dialog { title: "t".into(), body: "b".into(), buttons: vec![("a".into(), "a".into())] };
        let mut tree = WidgetTree::new();
        let id = tree.add(None, Box::new(d), Rect::new(0, 0, 10, 10));
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(20, 20);
        tree.paint(&mut crate::PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme });
        let tap = UiEvent::Gesture(Gesture::Tap { pos: Point::new(1, 1) });
        assert_eq!(tree.dispatch(&tap).map(|r| r.0), Some(id));
        assert!(Toast::frame(Size::new(100, 100)).y >= 0);
    }

    #[test]
    fn dialog_menu_sheet_toast() {
        let screen = Size::new(1272, 1696);
        let mut tree = WidgetTree::new();
        let d = Dialog {
            title: "Delete book?".into(),
            body: "This removes the file from the device and cannot be undone.".into(),
            buttons: vec![("Cancel".into(), "cancel".into()), ("Delete".into(), "delete".into())],
        };
        let frame = Dialog::frame(screen);
        let rects = d.button_rects(frame);
        let id = tree.add(None, Box::new(d), frame);
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1272, 1696);
        tree.paint(&mut crate::PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme });
        let tap = |r: Rect| UiEvent::Gesture(Gesture::Tap { pos: Point::new(r.x + 5, r.y + 5) });
        assert_eq!(tree.dispatch(&tap(rects[1])), Some((id, EventResult::Action("delete".into()))));
        assert_eq!(tree.dispatch(&tap(Rect::new(frame.x + 5, frame.y + 5, 1, 1))), Some((id, EventResult::Handled)));

        let mut m = Menu::new(vec![("Go to".into(), "goto".into()), ("Search".into(), "search".into())]);
        let mb = Rect::new(0, 0, 600, m.height());
        assert_eq!(m.event(&tap(Rect::new(10, 115, 1, 1)), mb), EventResult::Action("search".into()));
        let mut s = BottomSheet { title: "Aa".into(), taken: false };
        let swipe = UiEvent::Gesture(Gesture::Swipe {
            from: Point::new(600, 1300),
            to: Point::new(600, 1600),
            dir: crate::Direction::Down,
        });
        assert_eq!(s.event(&swipe, BottomSheet::frame(screen, 700)), EventResult::Action("close".into()));
        // Touch notifications pass by (taking one would repaint every control on it).
        let p = Point::new(600, 1300);
        for kind in [crate::TouchKind::Down, crate::TouchKind::Up] {
            let touch = UiEvent::Touch(crate::Touch { slot: 0, kind, pos: p, start: p, held: Duration::ZERO, moved: false });
            assert_eq!(s.event(&touch, BottomSheet::frame(screen, 700)), EventResult::Ignored);
        }
        let t = Toast { text: "Saved".into(), timeout: Duration::from_secs(2) };
        assert_eq!(t.refresh_class(), RefreshClass::Du);
        assert!(Toast::frame(screen).bottom() < 1696);
        assert_eq!(Toast::new("x").timeout, Duration::from_millis(2500));
        let c = BottomSheet::content(BottomSheet::frame(screen, 700));
        assert!(c.y > 996 && c.bottom() <= 1696);
    }

    #[test]
    fn styled_popup_text_wraps_and_keeps_styles() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(800, 400);
        let mut cx = crate::PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme };
        let spans: Vec<(String, Style)> = vec![("The ".into(), Style::Regular), ("first".into(), Style::Italic), (" note, ".into(), Style::Regular), ("bold".into(), Style::Bold), ("ly done.".into(), Style::Regular)];
        let one = wrap_spans(&mut cx, &spans, 34.0, 10_000.0);
        assert_eq!(one.len(), 1);
        let pieces: Vec<(&str, Style)> = one[0].iter().map(|(_, t, s)| (t.as_str(), *s)).collect();
        assert_eq!(pieces, [("The", Style::Regular), ("first", Style::Italic), ("note,", Style::Regular), ("bold", Style::Bold), ("ly", Style::Regular), ("done.", Style::Regular)]);
        // "bold" + "ly" is one word: the second piece starts where the first ends.
        let w_bold = cx.fonts.shaped_width(Style::Bold, 34.0, "bold");
        assert!((one[0][4].0 - (one[0][3].0 + w_bold)).abs() < 0.01);
        assert!(one[0][1].0 > one[0][0].0, "a space before \"first\"");
        // Narrow: one word per line, never split inside "boldly".
        let narrow = wrap_spans(&mut cx, &spans, 34.0, 10.0);
        assert_eq!(narrow.len(), 5);
        assert_eq!(narrow[3].iter().map(|p| p.1.as_str()).collect::<String>(), "boldly");
        assert!(narrow.iter().all(|l| l[0].0 == 0.0));

        // Painted: italic and bold glyphs differ from the regular ones (pixels change).
        let paint = |spans: Vec<(String, Style)>| {
            let mut fonts = Fonts::load();
            let mut canvas = Canvas::new(800, 400);
            canvas.clear(255);
            let mut p = Popup { title: "Footnote 1".into(), text: "The first note".into(), spans, ..Default::default() };
            p.paint(&mut crate::PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme }, Rect::new(0, 0, 800, 400));
            canvas
        };
        let plain = paint(vec![]);
        let styled = paint(vec![("The ".into(), Style::Regular), ("first".into(), Style::Italic), (" note".into(), Style::Regular)]);
        let same_title = (0..90).all(|y| (0..800).all(|x| plain.get(x, y) == styled.get(x, y)));
        assert!(same_title, "the title is the same");
        assert!((90..200).any(|y| (0..800).any(|x| plain.get(x, y) != styled.get(x, y))), "the italic word differs");
    }

    #[test]
    fn popup_placement() {
        let screen = Size::new(1272, 1696);
        let size = Size::new(800, 400);
        // Room below.
        let r = Popup::frame(Rect::new(100, 200, 50, 40), size, screen);
        assert_eq!((r.x, r.y), (0, 256));
        // No room below: above, and kept on screen at the right edge.
        let r = Popup::frame(Rect::new(1200, 1500, 50, 40), size, screen);
        assert_eq!((r.x, r.bottom()), (472, 1484));
        let mut p = Popup { title: "Footnote 3".into(), text: "A short note.".into(), buttons: vec![("Go to".into(), "goto".into())], spans: vec![] };
        let b = Rect::new(0, 0, 800, 400);
        let br = p.button_rects(Rect::new(0, 0, 792, 392));
        assert_eq!(p.event(&UiEvent::Gesture(Gesture::Tap { pos: Point::new(br[0].x + 5, br[0].y + 5) }), b), EventResult::Action("goto".into()));
        assert_eq!(p.event(&UiEvent::Gesture(Gesture::Tap { pos: Point::new(5, 5) }), b), EventResult::Handled);
    }

    #[test]
    fn scrim_blocks_what_is_under_it() {
        let mut tree = WidgetTree::new();
        tree.add(None, Box::new(Button::new("under", "under")), Rect::new(0, 0, 400, 400));
        let scrim = tree.add(None, Box::new(Scrim::new("dismiss")), Rect::new(0, 0, 400, 400));
        let popup = tree.add(None, Box::new(Popup { title: String::new(), text: "Hi".into(), ..Default::default() }), Rect::new(100, 100, 200, 100));
        let tap = |x, y| UiEvent::Gesture(Gesture::Tap { pos: Point::new(x, y) });
        assert_eq!(tree.dispatch(&tap(10, 10)), Some((scrim, EventResult::Action("dismiss".into()))));
        assert_eq!(tree.dispatch(&tap(150, 150)), Some((popup, EventResult::Handled)));
        tree.remove(popup);
        tree.remove(scrim);
        assert_eq!(tree.dispatch(&tap(10, 10)).map(|r| r.1), Some(EventResult::Action("under".into())));
    }
}
