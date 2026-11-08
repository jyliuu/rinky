//! The settings popup: the one popup every settings popup is built from.
//!
//! A [`SettingsPopup`] is a rounded box with a heading-role title, a Close button (X) at
//! the right end of the title row, an optional note line, and a [`SettingsList`] of the
//! same [`Row`]s a settings page shows (headers, notes, switches, choices, links,
//! actions, sliders). The control center shows its long-press options in it (Wi-Fi,
//! Rotate, Full refresh, Status bars and every plugin tile), and the plugin overlay
//! shows plugin menus in it ([`crate::plugin_host::MenuBox`]). A [`PopupCatcher`] under
//! it turns a tap outside the box into the close action and keeps every other touch
//! from the screen below.
//!
//! On e-ink the popup never flashes: it appears with one GL16 update of its own
//! rectangle (it is smaller than [`ink_ui::refresh::FLASH_PERCENT`] of the screen, so it
//! is never promoted to a GC16), a tapped switch repaints only its row, a list jump inside
//! it is one DU of the list ([`SettingsList`]), and closing it repaints only the
//! rectangle it covered (GL16).
//!
//! Plugin menus are turned into rows by [`menu_rows`] (data, no custom drawing): an item
//! `"\u{2713} label"` is a chosen choice, `"label\ton"` / `"label\toff"` a switch,
//! `"label\tdetail"` a row with the detail as its value, the action `back` a row with a
//! back arrow, and an item with an empty action a note. Rows report `menu:<index>`
//! (switches `menu:<index>:on|off`); [`menu_pick`] gives back the item's action.

use super::rows::{Row, SettingsList};
use ink_device::{Rect, RefreshClass};
use ink_text::Style;
use ink_ui::draw::{fill_round_rect, stroke_round_rect};
use ink_ui::{EventResult, Gesture, Icon, PaintCx, UiEvent, Widget};

/// Height of the title row (and side of the Close button at its right end).
pub const TITLE_H: u32 = 100;
/// Height of the note line under the title.
pub const NOTE_H: u32 = 64;
/// Space under the last row.
pub const PAD: u32 = 16;
/// Widest popup.
pub const MAX_W: u32 = 1000;
/// Default action of the Close button and of a tap outside the popup.
pub const CLOSE: &str = "popup:close";
/// Action prefix of rows made by [`menu_rows`].
pub const MENU_ROW: &str = "menu";

/// Where [`SettingsPopup::frame`] puts the popup in its area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// At the top of the area (the control center: under its header).
    Top,
    /// In the middle of the area (plugin menus over a screen).
    Center,
}

/// A popup of settings rows (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsPopup {
    /// Title (heading role, bold).
    pub title: String,
    /// Line under the title (empty: none).
    pub note: String,
    /// The rows.
    pub list: SettingsList,
    /// Reported by the Close button.
    pub close_action: String,
    /// The whole popup changed (new, new rows, new title): the next paint shows all of it.
    fresh: bool,
}

impl SettingsPopup {
    /// A popup titled `title` with `rows`.
    pub fn new(title: impl Into<String>, rows: Vec<Row>) -> Self {
        SettingsPopup { title: title.into(), note: String::new(), list: SettingsList::new(rows), close_action: CLOSE.into(), fresh: true }
    }

    /// With a note line under the title.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }

    /// With `action` for the Close button (and the [`PopupCatcher`] under it).
    pub fn with_close(mut self, action: impl Into<String>) -> Self {
        self.close_action = action.into();
        self
    }

    /// Height of the title row and the note.
    pub fn head_h(note: bool) -> u32 {
        TITLE_H + if note { NOTE_H } else { 0 }
    }

    /// Height that shows every row.
    pub fn height(&self) -> u32 {
        Self::head_h(!self.note.is_empty()) + self.list.content_height() + PAD
    }

    /// How many one-line rows fit in a popup at most `h` tall.
    pub fn rows_fit(h: u32, note: bool) -> usize {
        (h.saturating_sub(Self::head_h(note) + PAD) / super::rows::ROW_H) as usize
    }

    /// The popup's rectangle in `area`: at most [`MAX_W`] wide (32 px from the sides),
    /// as tall as its rows (at most `area`; the list scrolls when they do not fit),
    /// centred across the area, at its top or in its middle.
    pub fn frame(&self, area: Rect, anchor: Anchor) -> Rect {
        let w = area.w.saturating_sub(64).min(MAX_W);
        let h = self.height().min(area.h);
        let x = area.x + (area.w - w) as i32 / 2;
        let y = match anchor {
            Anchor::Top => area.y,
            Anchor::Center => area.y + (area.h - h) as i32 / 2,
        };
        Rect::new(x, y, w, h)
    }

    /// The list's rectangle in the popup `b`: under the title (and note), inside the frame.
    pub fn list_rect(&self, b: Rect) -> Rect {
        let head = Self::head_h(!self.note.is_empty());
        let inset = 4;
        Rect::new(b.x + inset, b.y + head as i32, b.w.saturating_sub(2 * inset as u32), b.h.saturating_sub(head + PAD))
    }

    /// The Close button: a [`TITLE_H`] square at the right end of the title row.
    pub fn close_rect(b: Rect) -> Rect {
        Rect::new(b.right() - TITLE_H as i32, b.y, TITLE_H, TITLE_H)
    }

    /// The on-screen rectangle of the first row with `action` in the popup `b` (after
    /// scrolling it into view), for tests and hit-testing.
    pub fn locate(&mut self, b: Rect, action: &str) -> Option<Rect> {
        let area = self.list_rect(b);
        let (need, _) = self.list.locate(area, action)?;
        if need != self.list.scroll {
            self.list.scroll_to(area, need);
        }
        self.list.layout(area).into_iter().find(|(i, _)| self.list.rows[*i].action == action).map(|(_, r)| Rect::new(r.x, r.y - self.list.scroll, r.w, r.h))
    }

    /// A new note line: the whole popup repaints when it changed.
    pub fn set_note(&mut self, note: String) {
        if self.note != note {
            self.note = note;
            self.fresh = true;
        }
    }

    /// Row `i` changed: only that row repaints.
    pub fn set_row(&mut self, i: usize, row: Row) -> bool {
        self.list.replace_row(i, row)
    }
}

impl Widget for SettingsPopup {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        fill_round_rect(cx.canvas, b, t.radius_lg, t.bg);
        stroke_round_rect(cx.canvas, b, t.radius_lg, t.border_px as f32, t.fg);
        let pad = ink_ui::theme::space(4);
        // The title leaves room for the Close button.
        let title_w = (b.w as i32 - pad - TITLE_H as i32).max(1) as u32;
        let title = cx.ellipsize(&self.title, t.heading.px, t.heading.style, title_w as f32);
        cx.text_left(Rect::new(b.x + pad, b.y, title_w, TITLE_H), &title, t.heading.px, t.heading.style, t.fg);
        cx.icon(Icon::Close, Self::close_rect(b), 48, t.fg);
        // A black rule under the title row (black and white: no gray to ghost).
        cx.canvas.fill_rect(b.x + t.border_px, b.y + TITLE_H as i32 - t.rule_px, b.w as i32 - 2 * t.border_px, t.rule_px, t.fg);
        if !self.note.is_empty() {
            let r = Rect::new(b.x + pad, b.y + TITLE_H as i32, (b.w as i32 - 2 * pad).max(1) as u32, NOTE_H);
            let note = cx.ellipsize(&self.note, t.caption.px, Style::Regular, r.w as f32);
            cx.text_left(r, &note, t.caption.px, Style::Regular, t.fg);
        }
        let area = self.list_rect(b);
        self.list.paint(cx, area);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        if let UiEvent::Gesture(Gesture::Tap { pos }) = ev {
            if Self::close_rect(b).contains(*pos) {
                return EventResult::Action(self.close_action.clone());
            }
        }
        let area = self.list_rect(b);
        match self.list.event(ev, area) {
            EventResult::Ignored => match ev {
                // The popup is modal: the rest of it takes touches and gestures.
                UiEvent::Gesture(_) | UiEvent::Touch(_) => EventResult::Handled,
                _ => EventResult::Ignored,
            },
            other => other,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        if self.fresh {
            RefreshClass::Gl16
        } else {
            self.list.refresh_class()
        }
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        let area = self.list_rect(b);
        let list = self.list.damage(area);
        if std::mem::take(&mut self.fresh) {
            return None;
        }
        Some(list.unwrap_or_else(|| vec![area]))
    }
}

/// The invisible layer under an open popup: a tap outside `popup` reports `action`;
/// every other touch and gesture stops here, so the screen under it does not react.
/// Draws nothing and refreshes nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct PopupCatcher {
    /// The popup's box.
    pub popup: Rect,
    /// Reported by a tap outside it.
    pub action: String,
}

impl Widget for PopupCatcher {
    fn paint(&mut self, _cx: &mut PaintCx, _b: Rect) {}

    fn opaque(&self) -> bool {
        false
    }

    fn adds_damage(&self) -> bool {
        false
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Tap { pos }) if !self.popup.contains(*pos) => EventResult::Action(self.action.clone()),
            UiEvent::Gesture(_) | UiEvent::Touch(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }
}

// -- Plugin menus as rows -----------------------------------------------------------------------

/// True for a detail that is a switch state.
fn switch_state(detail: &str) -> Option<bool> {
    match detail.trim().to_ascii_lowercase().as_str() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// The row of menu item `i` (`label`, `action`); `choices`: some item of the menu is
/// checked, so the plain items are choices too (their check space is kept).
pub fn menu_row(i: usize, label: &str, action: &str, choices: bool) -> Row {
    let (checked, rest) = ink_ui::dialog::split_check(label);
    let (main, detail) = match rest.split_once('\t') {
        Some((m, d)) => (m.to_string(), Some(d.to_string())),
        None => (rest.to_string(), None),
    };
    if action.is_empty() {
        return Row::note(main);
    }
    let act = format!("{MENU_ROW}:{i}");
    if action == "back" {
        return Row::item(main.trim_start_matches(['<', ' ']).to_string(), act).icon(Icon::Back);
    }
    if let Some(on) = detail.as_deref().and_then(switch_state) {
        return Row::toggle(main, on, act);
    }
    let row = if choices || checked { Row::choice(main, checked, act) } else { Row::item(main, act) };
    match detail {
        Some(d) if !d.is_empty() => row.value(d),
        _ => row,
    }
}

/// The rows of a plugin menu's `(label, action)` items (see the module docs).
pub fn menu_rows(items: &[(String, String)]) -> Vec<Row> {
    let choices = items.iter().any(|(l, _)| ink_ui::dialog::split_check(l).0);
    items.iter().enumerate().map(|(i, (l, a))| menu_row(i, l, a, choices)).collect()
}

/// The index of the item a [`menu_rows`] row action (`menu:<i>`, `menu:<i>:on|off`)
/// stands for.
pub fn menu_index(row_action: &str) -> Option<usize> {
    let rest = row_action.strip_prefix(MENU_ROW)?.strip_prefix(':')?;
    rest.split(':').next()?.parse().ok()
}

/// The plugin action of a [`menu_rows`] row action.
pub fn menu_pick(row_action: &str, items: &[(String, String)]) -> Option<String> {
    items.get(menu_index(row_action)?).map(|(_, a)| a.clone())
}

#[cfg(test)]
mod tests {
    use super::super::rows::{Kind, Trail};
    use super::*;
    use ink_device::Point;

    fn items(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(l, a)| (l.to_string(), a.to_string())).collect()
    }

    #[test]
    fn menu_items_become_rows_of_the_settings_kinds() {
        let m = items(&[
            ("On · 192.168.15.244", "status"),
            ("Turn on when rinky starts\toff", "autostart"),
            ("Server\tauto", "server"),
            ("< Back", "back"),
            ("Settings are saved at once", ""),
        ]);
        let rows = menu_rows(&m);
        assert_eq!(rows[0].trail, Trail::None);
        assert_eq!(rows[1].trail, Trail::Toggle(false));
        assert_eq!(rows[1].title, "Turn on when rinky starts");
        assert_eq!(rows[2].value.as_deref(), Some("auto"));
        assert_eq!((rows[3].title.as_str(), rows[3].lead), ("Back", super::super::rows::Lead::Icon(Icon::Back)));
        assert_eq!(rows[4].kind, Kind::Note);
        assert_eq!(menu_pick("menu:1:on", &m).as_deref(), Some("autostart"));
        assert_eq!(menu_pick("menu:2", &m).as_deref(), Some("server"));
        assert_eq!(menu_pick("menu:9", &m), None);
        assert_eq!(menu_pick("other:1", &m), None);
        // A menu with a check: every plain item is a choice (the mark's space is kept).
        let m = items(&[("\u{2713} gpt-5", "m:0"), ("gpt-5-mini", "m:1")]);
        let rows = menu_rows(&m);
        assert_eq!((rows[0].trail, rows[1].trail), (Trail::Check(true), Trail::Check(false)));
        assert_eq!(rows[0].title, "gpt-5");
    }

    #[test]
    fn popup_fits_its_area_reports_rows_and_closes() {
        let rows = vec![Row::header("H"), Row::toggle("Wi-Fi", true, "wifi"), Row::choice("Office", false, "net:0")];
        let mut p = SettingsPopup::new("Wi-Fi networks", rows).with_note("2 found");
        let area = Rect::new(0, 120, 1272, 1000);
        let b = p.frame(area, Anchor::Top);
        assert_eq!((b.y, b.h), (120, TITLE_H + NOTE_H + 72 + 2 * 120 + PAD));
        assert!(b.w <= MAX_W && b.x > 0);
        let c = p.frame(area, Anchor::Center);
        assert_eq!(c.y, 120 + (1000 - c.h as i32) / 2);
        let r = p.locate(b, "wifi").unwrap();
        let tap = |pos: Point| UiEvent::Gesture(Gesture::Tap { pos });
        assert_eq!(p.event(&tap(Point::new(r.x + 20, r.y + 20)), b), EventResult::Action("wifi:off".into()));
        let close = SettingsPopup::close_rect(b);
        assert_eq!(p.event(&tap(Point::new(close.x + 40, close.y + 40)), b), EventResult::Action(CLOSE.into()));
        // A tap on the title is taken (modal) and reports nothing.
        assert_eq!(p.event(&tap(Point::new(b.x + 40, b.y + 40)), b), EventResult::Handled);
        let mut catcher = PopupCatcher { popup: b, action: "x".into() };
        assert_eq!(catcher.event(&tap(Point::new(5, 5)), Rect::new(0, 0, 1272, 1696)), EventResult::Action("x".into()));
        assert_eq!(catcher.event(&tap(Point::new(b.x + 5, b.y + 5)), Rect::new(0, 0, 1272, 1696)), EventResult::Handled);
        // Too many rows for the area: as tall as the area, the list scrolls.
        let many: Vec<Row> = (0..20).map(|i| Row::item(format!("r{i}"), format!("a:{i}"))).collect();
        let p = SettingsPopup::new("Many", many);
        assert_eq!(p.frame(area, Anchor::Top).h, 1000);
        assert_eq!(SettingsPopup::rows_fit(1000, false), ((1000 - TITLE_H - PAD) / 120) as usize);
    }
}
