//! Grouped settings rows: section headers, rows with a leading icon, the current value
//! on the right, a chevron, a switch or a check mark, Wi-Fi signal bars and a lock.
//!
//! [`SettingsList`] is one vertically scrolling list that jumps, as KOReader and the
//! Kindle do: a swipe up or down moves it by half the visible height (a long swipe,
//! [`LONG_SWIPE`] of the height or more, by a whole page) in one GL16 repaint of the
//! list. Nothing follows the finger while it moves. A scroll bar on the right edge
//! shows where the view is. Pressed rows invert (DU); only changed rows are refreshed.
//!
//! Geometry depends only on the kinds of rows, not on their texts, so tests find a row
//! by its action with [`SettingsList::locate`].
//!
//! These rows are the one row model of every settings page and settings popup: a page
//! ([`super::page`]) shows them under an app bar, a popup ([`super::popup`]) in a
//! rounded box. Row kinds: headers, notes, switches ([`Row::toggle`]), choices
//! ([`Row::choice`]), links to sub-pages ([`Row::nav`]), actions ([`Row::item`]),
//! ordered items ([`Trail::Order`]) and sliders ([`Row::slider`]).

use ink_device::{Point, Rect, RefreshClass};
use ink_text::Style;
use ink_ui::draw::{stroke_round_rect, Path};
use ink_ui::theme::space;
use ink_ui::{Direction, EventResult, Gesture, Icon, PaintCx, ScrollBar, Slider, Toggle, TouchKind, UiEvent, Widget};

/// Height of a section header.
pub const HEADER_H: u32 = 72;
/// Height of a one-line row.
pub const ROW_H: u32 = 120;
/// Height of a row with a second line.
pub const ROW2_H: u32 = 136;
/// Height of a note (text that is not tappable).
pub const NOTE_H: u32 = 96;
/// Height of a slider row (caption and value above the track, -/+ buttons).
pub const SLIDER_H: u32 = 200;
/// Side of a leading icon.
const LEAD: u32 = 52;
/// Width of the switch area on the right of a toggle row.
const SWITCH_W: i32 = 136;

/// What a row shows on its left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lead {
    /// Nothing; the title starts at the margin.
    None,
    /// An icon.
    Icon(Icon),
    /// Wi-Fi signal bars for a signal of 0..=5.
    Signal(u8),
}

/// What a row shows on its right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trail {
    /// Nothing.
    None,
    /// A chevron: the row opens a page.
    Chevron,
    /// A switch; a tap flips it and reports `action:on` or `action:off`.
    Toggle(bool),
    /// A check mark (one of several choices); the space is kept when unchecked.
    Check(bool),
    /// An icon (e.g. a trash can for "forget").
    Icon(Icon),
    /// An item of an ordered choice: "move up" and "move down" buttons and a switch.
    /// A tap on the up button reports `action:up`, on the down button `action:down`,
    /// anywhere else `action:on` / `action:off` (the switch flips).
    Order {
        /// Switch state.
        on: bool,
        /// The up button is active.
        up: bool,
        /// The down button is active.
        down: bool,
    },
}

/// Side of the up and down buttons of a [`Trail::Order`] row.
pub const ORDER_BTN: u32 = 96;

/// (up button, down button) of a [`Trail::Order`] row in `r`: left of the switch.
pub fn order_buttons(r: Rect) -> (Rect, Rect) {
    let right = r.right() - space(4) - SWITCH_W - space(3);
    let y = r.y + (r.h as i32 - ORDER_BTN as i32) / 2;
    let down = Rect::new(right - ORDER_BTN as i32, y, ORDER_BTN, ORDER_BTN);
    let up = Rect::new(down.x - space(2) - ORDER_BTN as i32, y, ORDER_BTN, ORDER_BTN);
    (up, down)
}

/// What kind of line a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A section header.
    Header,
    /// A tappable row (when it has an action).
    Item,
    /// A muted note.
    Note,
    /// A slider ([`Row::slider`]); its release reports `action:<value>`.
    Slider,
}

/// One line of a [`SettingsList`].
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Header, item or note.
    pub kind: Kind,
    /// Main text.
    pub title: String,
    /// Second line (muted caption).
    pub subtitle: Option<String>,
    /// Current value on the right (muted).
    pub value: Option<String>,
    /// Left side.
    pub lead: Lead,
    /// A lock before the right side (secured network).
    pub lock: bool,
    /// Right side.
    pub trail: Trail,
    /// Action reported on a tap (toggles add `:on` / `:off`); empty: not tappable.
    pub action: String,
    /// The slider of a [`Kind::Slider`] row.
    pub slider: Option<Slider>,
}

impl Row {
    fn base(kind: Kind, title: impl Into<String>, action: impl Into<String>) -> Self {
        Row {
            kind,
            title: title.into(),
            subtitle: None,
            value: None,
            lead: Lead::None,
            lock: false,
            trail: Trail::None,
            action: action.into(),
            slider: None,
        }
    }

    /// A section header.
    pub fn header(title: impl Into<String>) -> Self {
        Row::base(Kind::Header, title, "")
    }

    /// A muted line of text.
    pub fn note(text: impl Into<String>) -> Self {
        Row::base(Kind::Note, text, "")
    }

    /// A row with an icon that opens a page (chevron).
    pub fn nav(icon: Icon, title: impl Into<String>, action: impl Into<String>) -> Self {
        Row { lead: Lead::Icon(icon), trail: Trail::Chevron, ..Row::base(Kind::Item, title, action) }
    }

    /// A row with a switch on the right.
    pub fn toggle(title: impl Into<String>, on: bool, action: impl Into<String>) -> Self {
        Row { trail: Trail::Toggle(on), ..Row::base(Kind::Item, title, action) }
    }

    /// One of several choices (check mark when chosen).
    pub fn choice(title: impl Into<String>, checked: bool, action: impl Into<String>) -> Self {
        Row { trail: Trail::Check(checked), ..Row::base(Kind::Item, title, action) }
    }

    /// A plain tappable row.
    pub fn item(title: impl Into<String>, action: impl Into<String>) -> Self {
        Row::base(Kind::Item, title, action)
    }

    /// A slider from `min` to `max` in steps of `step`: the caption `title` and the value
    /// above the track, round minus and plus buttons. Dragging it repaints only the row
    /// (DU); the lift or a tap on minus or plus reports `action:<value>`.
    pub fn slider(title: impl Into<String>, min: f32, max: f32, step: f32, value: f32, action: impl Into<String>) -> Self {
        let (title, action) = (title.into(), action.into());
        let slider = Slider::new(title.clone(), min, max, step, value, action.clone());
        Row { slider: Some(slider), ..Row::base(Kind::Slider, title, action) }
    }

    /// With a leading icon.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.lead = Lead::Icon(icon);
        self
    }

    /// With a left side.
    pub fn lead(mut self, lead: Lead) -> Self {
        self.lead = lead;
        self
    }

    /// With a right side.
    pub fn trail(mut self, trail: Trail) -> Self {
        self.trail = trail;
        self
    }

    /// With a value on the right.
    pub fn value(mut self, v: impl Into<String>) -> Self {
        self.value = Some(v.into());
        self
    }

    /// With a second line.
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    /// With a lock (secured network).
    pub fn lock(mut self, on: bool) -> Self {
        self.lock = on;
        self
    }

    /// Height of the row.
    pub fn height(&self) -> u32 {
        match self.kind {
            Kind::Header => HEADER_H,
            Kind::Note => NOTE_H,
            Kind::Slider => SLIDER_H,
            Kind::Item if self.subtitle.is_some() => ROW2_H,
            Kind::Item => ROW_H,
        }
    }

    fn tappable(&self) -> bool {
        self.kind == Kind::Item && !self.action.is_empty()
    }
}

/// A swipe at least this fraction (percent) of the list height scrolls a whole page.
pub const LONG_SWIPE: i32 = 60;
/// Width of the scroll bar on the right edge (the shared [`ScrollBar`]'s).
pub const SCROLLBAR_W: u32 = ink_ui::scrollbar::SCROLLBAR_W;
/// Empty space below the last row when scrolled to the end.
const END_PAD: u32 = 16;

/// Grouped rows in one vertically scrolling list.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsList {
    /// The rows.
    pub rows: Vec<Row>,
    /// Scroll offset in pixels: the content y shown at the top of the area.
    pub scroll: i32,
    pressed: Option<usize>,
    changed: Vec<usize>,
    /// The scroll offset changed: the whole area is repainted.
    moved: bool,
    /// Jumped by a swipe or a page key: the next update of the area is DU.
    fast: bool,
    /// The slider row a finger drags (it took the touch-down).
    dragging: Option<usize>,
    /// The last touch went down on a slider: the swipe that ends it does not scroll.
    slid: bool,
}

impl SettingsList {
    /// A list of `rows`, scrolled to the top.
    pub fn new(rows: Vec<Row>) -> Self {
        SettingsList { rows, scroll: 0, pressed: None, changed: Vec::new(), moved: false, fast: false, dragging: None, slid: false }
    }

    /// Replaces row `i` (same kind keeps the geometry); only that row is repainted.
    /// False when there is no row `i`.
    pub fn replace_row(&mut self, i: usize, row: Row) -> bool {
        let Some(old) = self.rows.get_mut(i) else { return false };
        if *old != row {
            let moved = old.height() != row.height();
            *old = row;
            self.changed.push(i);
            self.moved |= moved;
        }
        true
    }

    /// The rectangle of a slider row's slider inside the row `r`.
    pub fn slider_rect(r: Rect) -> Rect {
        Rect::new(r.x + space(3), r.y, (r.w as i32 - 2 * space(3)).max(0) as u32, r.h)
    }

    /// Every row with its rectangle at scroll offset 0 (rows below the area included).
    pub fn layout(&self, b: Rect) -> Vec<(usize, Rect)> {
        let mut y = b.y;
        self.rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let r = Rect::new(b.x, y, b.w, row.height());
                y += row.height() as i32;
                (i, r)
            })
            .collect()
    }

    /// Height of all rows.
    pub fn content_height(&self) -> u32 {
        self.rows.iter().map(Row::height).sum()
    }

    /// Largest scroll offset in `b` (0 when everything fits).
    pub fn max_scroll(&self, b: Rect) -> i32 {
        let total = self.content_height();
        if total <= b.h {
            0
        } else {
            (total + END_PAD - b.h) as i32
        }
    }

    /// `y` clamped and moved to a row top: down to the top of the row that `y` falls
    /// in (`up`: to the next row top), so the first row shown is whole. The end of the
    /// list (the largest offset) stays as it is.
    pub fn snap(&self, b: Rect, y: i32, up: bool) -> i32 {
        let max = self.max_scroll(b);
        let y = y.clamp(0, max);
        if y >= max {
            return max;
        }
        let tops = self.layout(b).into_iter().map(|(_, r)| r.y - b.y);
        let snapped = if up { tops.filter(|t| *t >= y).min() } else { tops.filter(|t| *t <= y).max() };
        snapped.unwrap_or(y).clamp(0, max)
    }

    /// Sets the scroll offset (clamped, snapped down to a row top: [`SettingsList::snap`]);
    /// true when it changed.
    pub fn scroll_to(&mut self, b: Rect, y: i32) -> bool {
        let y = self.snap(b, y, false);
        if y != self.scroll {
            self.scroll = y;
            self.moved = true;
            true
        } else {
            false
        }
    }

    /// The scroll offset that shows the first row whose action is `action` (or
    /// `action:...` for a toggle row) whole, and its rectangle at that offset. Rows that
    /// are visible at offset 0 need offset 0.
    pub fn locate(&self, b: Rect, action: &str) -> Option<(i32, Rect)> {
        let (_, r) = self.layout(b).into_iter().find(|(i, _)| self.rows[*i].action == action)?;
        let need = (r.bottom() - b.bottom()).max(0).min(self.max_scroll(b));
        Some((need, Rect::new(r.x, r.y - need, r.w, r.h)))
    }

    /// Scrolls as little as needed to show the first row whose action is `action`
    /// whole (nothing when it is on screen or not in the list).
    pub fn reveal(&mut self, b: Rect, action: &str) {
        let Some((_, r)) = self.layout(b).into_iter().find(|(i, _)| self.rows[*i].action == action) else { return };
        let s = self.scroll.max(r.bottom() - b.bottom()).min(r.y - b.y);
        // Up to the next row top (the first row shown is whole); the row's own top is
        // one, so it stays on screen.
        self.scroll = self.snap(b, s, true);
    }

    /// Rows shown, with their rectangles on screen: only rows that fit whole. A row cut
    /// by the top or bottom edge is left blank (and cannot be tapped); on e-ink half a
    /// row is noise, and the next jump shows it whole.
    fn visible(&self, b: Rect) -> Vec<(usize, Rect)> {
        self.layout(b)
            .into_iter()
            .map(|(i, r)| (i, Rect::new(r.x, r.y - self.scroll, r.w, r.h)))
            .filter(|(_, r)| r.y >= b.y && r.bottom() <= b.bottom())
            .collect()
    }

    /// Row `i`'s rectangle on screen in the area `b`, when it is shown whole.
    pub fn shown_rect(&self, b: Rect, i: usize) -> Option<Rect> {
        self.visible(b).into_iter().find(|(k, _)| *k == i).map(|(_, r)| r)
    }

    /// The visible slider row under `p` and its slider's rectangle.
    fn slider_at(&self, b: Rect, p: Point) -> Option<(usize, Rect)> {
        if !b.contains(p) {
            return None;
        }
        self.visible(b)
            .into_iter()
            .find(|(i, r)| r.contains(p) && self.rows[*i].kind == Kind::Slider)
            .map(|(i, r)| (i, SettingsList::slider_rect(r)))
    }

    /// A touch for a slider row: the down that starts a drag on its track and every
    /// touch of that drag (`None`: not a slider touch). The row repaints alone, DU while
    /// it is dragged; the lift reports the slider's `action:<value>`.
    fn slider_touch(&mut self, ev: &UiEvent, t: &ink_ui::Touch, b: Rect) -> Option<EventResult> {
        if let Some(i) = self.dragging {
            let r = self.visible(b).into_iter().find(|(k, _)| *k == i).map(|(_, r)| SettingsList::slider_rect(r));
            if matches!(t.kind, TouchKind::Up | TouchKind::Cancel) {
                self.dragging = None;
            }
            let (Some(r), Some(s)) = (r, self.rows[i].slider.as_mut()) else {
                self.dragging = None;
                return Some(EventResult::Handled);
            };
            let out = s.event(ev, r);
            if out != EventResult::Ignored {
                self.changed.push(i);
            }
            return Some(match out {
                EventResult::Ignored => EventResult::Handled,
                o => o,
            });
        }
        if t.kind != TouchKind::Down {
            return None;
        }
        let (i, r) = self.slider_at(b, t.pos)?;
        let s = self.rows[i].slider.as_mut()?;
        match s.event(ev, r) {
            EventResult::Ignored => None,
            out => {
                self.dragging = Some(i);
                self.slid = true;
                self.changed.push(i);
                Some(out)
            }
        }
    }

    fn item_at(&self, b: Rect, p: Point) -> Option<usize> {
        if !b.contains(p) {
            return None;
        }
        self.visible(b).into_iter().find(|(i, r)| r.contains(p) && self.rows[*i].tappable()).map(|(i, _)| i)
    }

    /// The shared scroll bar for the list in `b`, in pixels: the whole content, the
    /// offset shown and the area's height.
    pub fn scroll_bar(&self, b: Rect) -> ScrollBar {
        ScrollBar::new((self.content_height() + END_PAD) as usize, self.scroll.max(0) as usize, b.h as usize)
    }

    /// Track and thumb of the scroll bar (`None` when everything fits). The track sits
    /// at the right edge, right of the switches (which end [`space`]`(4)` from it).
    pub fn scrollbar(&self, b: Rect) -> Option<(Rect, Rect)> {
        if self.max_scroll(b) == 0 {
            return None;
        }
        let track = Rect::new(b.right() - SCROLLBAR_W as i32 - 4, b.y + space(1), SCROLLBAR_W, b.h.saturating_sub(2 * space(1) as u32));
        Some((track, self.scroll_bar(b).thumb(track)))
    }

    fn paint_row(cx: &mut PaintCx, r: Rect, row: &Row, pressed: bool, divider: bool) {
        let t = cx.theme.clone();
        let (fg, bg) = if pressed { (t.bg, t.fg) } else { (t.fg, t.bg) };
        // Black and white only (no gray text, rules or tracks): a list jump is one DU
        // update, which shows two levels exactly.
        let muted = fg;
        let margin = space(4);
        if pressed {
            cx.canvas.fill_rect(r.x, r.y, r.w as i32, r.h as i32, bg);
        }
        match row.kind {
            Kind::Header => {
                // Bold caption near the bottom of the header, close to its rows.
                let hr = Rect::new(r.x + margin, r.y + space(2), (r.w as i32 - 2 * margin).max(0) as u32, r.h - space(2) as u32);
                cx.text_left(hr, &row.title, t.caption.px, Style::Bold, t.fg);
                return;
            }
            Kind::Note => {
                let nr = Rect::new(r.x + margin, r.y, (r.w as i32 - 2 * margin).max(0) as u32, r.h);
                cx.text_left(nr, &row.title, t.caption.px, Style::Regular, fg);
                return;
            }
            Kind::Slider => {
                if let Some(s) = &row.slider {
                    s.clone().paint(cx, SettingsList::slider_rect(r));
                }
                return;
            }
            Kind::Item => {}
        }
        let mut x = r.x + margin;
        let lead_r = Rect::new(x, r.y + (r.h as i32 - LEAD as i32) / 2, LEAD, LEAD);
        match row.lead {
            Lead::None => {}
            Lead::Icon(icon) => {
                icon.draw(cx.canvas, lead_r, fg);
                x = lead_r.right() + space(3);
            }
            Lead::Signal(s) => {
                paint_signal(cx, lead_r, s, fg);
                x = lead_r.right() + space(3);
            }
        }
        if divider && !pressed {
            // A thin black rule between rows of a section, from the text start, inside
            // the row: a press repaints only the row rect, so a rule half in the row
            // above would change pixels outside it.
            cx.canvas.fill_rect(x, r.y, r.right() - margin - x, 1, fg);
        }
        let mut right = r.right() - margin;
        // A square `side` px wide at the right end, centred in the row.
        let square = |right: i32, side: i32| Rect::new(right - side, r.y + (r.h as i32 - side) / 2, side as u32, side as u32);
        if let (Trail::Toggle(on) | Trail::Order { on, .. }, false) = (row.trail, pressed) {
            // The ink-ui switch, painted into the right end of the row.
            Toggle::new("", on, "").paint(cx, Rect::new(right - SWITCH_W, r.y, (SWITCH_W + 8) as u32, r.h));
        }
        match row.trail {
            Trail::None => {}
            Trail::Chevron => {
                let cr = square(right, 40);
                Icon::Forward.draw(cx.canvas, cr, muted);
                right = cr.x - space(2);
            }
            Trail::Toggle(_) => right -= SWITCH_W + space(3),
            Trail::Check(on) => {
                let cr = square(right, 48);
                if on {
                    Icon::Check.draw(cx.canvas, cr, fg);
                }
                right = cr.x - space(3);
            }
            Trail::Icon(icon) => {
                let cr = square(right, 48);
                icon.draw(cx.canvas, cr, fg);
                right = cr.x - space(3);
            }
            Trail::Order { up, down, .. } => {
                let (ur, dr) = order_buttons(r);
                for (b, icon, active) in [(ur, Icon::Up, up), (dr, Icon::Down, down)] {
                    // An inactive arrow: a thin outline only (no gray).
                    let px = if active { t.border_px as f32 } else { 1.0 };
                    stroke_round_rect(cx.canvas, b, b.w as f32 / 2.0, px, fg);
                    if active {
                        icon.draw(cx.canvas, ink_ui::draw::inset(b, 24), fg);
                    }
                }
                right = ur.x - space(3);
            }
        }
        if row.lock {
            let lr = square(right, 44);
            paint_lock(cx, lr, fg);
            right = lr.x - space(2);
        }
        let avail = (right - x).max(0) as f32;
        if let Some(v) = &row.value {
            // At most half of the space: the title keeps the rest.
            let px = t.label.px;
            let v = cx.ellipsize(v, px, Style::Regular, avail * 0.5);
            let w = cx.text_width(Style::Regular, px, &v);
            let baseline = cx.baseline_in(r, px, Style::Regular);
            cx.draw_text(Style::Regular, px, right as f32 - w, baseline, &v, muted);
            right -= w as i32 + space(3);
        }
        let tw = (right - x).max(0) as u32;
        match &row.subtitle {
            Some(sub) => {
                let top = Rect::new(x, r.y + space(1), tw, r.h / 2 + 4);
                let bottom = Rect::new(x, r.y + r.h as i32 / 2 + 4, tw, r.h / 2 - space(2) as u32);
                cx.text_left(top, &row.title, t.body.px, Style::Regular, fg);
                cx.text_left(bottom, sub, t.caption.px, Style::Regular, muted);
            }
            None => cx.text_left(Rect::new(x, r.y, tw, r.h), &row.title, t.body.px, Style::Regular, fg),
        }
    }
}

/// Four bars rising to the right; bars above the signal are outlined (never gray alone,
/// so they read in night mode and under DU).
pub fn paint_signal(cx: &mut PaintCx, r: Rect, signal: u8, value: u8) {
    let n = 4;
    let lit = ((signal.min(5) as f32) / 5.0 * n as f32).round() as i32;
    let gap = 5;
    let bw = (r.w as i32 - gap * (n - 1)) / n;
    for i in 0..n {
        let h = r.h as i32 * (i + 1) / n;
        let br = Rect::new(r.x + i * (bw + gap), r.bottom() - h, bw as u32, h as u32);
        if i < lit {
            cx.canvas.fill_rect(br.x, br.y, br.w as i32, br.h as i32, value);
        } else {
            stroke_round_rect(cx.canvas, br, 2.0, 3.0, value);
        }
    }
}

/// A padlock: a filled body under a round shackle.
pub fn paint_lock(cx: &mut PaintCx, r: Rect, value: u8) {
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    let body_h = h * 0.55;
    let body_y = y + h - body_h;
    Path::new().round_rect(x + w * 0.1, body_y, w * 0.8, body_h, 4.0).fill(cx.canvas, value);
    let rad = w * 0.26;
    let cxm = x + w / 2.0;
    let top = y + rad + 2.0;
    let stroke = 4.0;
    // The shackle: a half ring on top and its two legs down to the body.
    Path::new().arc(cxm, top, rad, std::f32::consts::PI, 2.0 * std::f32::consts::PI, stroke).fill(cx.canvas, value);
    Path::new().line(cxm - rad, top, cxm - rad, body_y + 2.0, stroke).fill(cx.canvas, value);
    Path::new().line(cxm + rad, top, cxm + rad, body_y + 2.0, stroke).fill(cx.canvas, value);
}

impl SettingsList {
    /// Scrolls by `dy` pixels (a swipe or a key): one DU repaint of the area. The rows
    /// are drawn in black and white (only text edges are gray), so DU shows them as they
    /// are and no settle update follows.
    pub fn scroll_by(&mut self, b: Rect, dy: i32) -> bool {
        let changed = self.scroll_to(b, self.scroll + dy);
        if changed {
            self.moved = true;
            self.fast = true;
            self.changed.extend(self.pressed.take());
            log::info!("settings list: scrolled to {} of {}", self.scroll, self.max_scroll(b));
        }
        changed
    }

    /// The scroll step of a swipe over `dist` pixels in an area of height `h`: half the
    /// height, a whole page for a long swipe.
    pub fn swipe_step(h: u32, dist: i32) -> i32 {
        if dist.abs() * 100 >= h as i32 * LONG_SWIPE {
            h as i32
        } else {
            h as i32 / 2
        }
    }
}

impl Widget for SettingsList {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let started = std::time::Instant::now();
        self.scroll = self.scroll.clamp(0, self.max_scroll(b));
        // Only whole rows are painted (see `visible`): icons and switches ignore the text
        // clip, and a cut row would draw them outside `b` and every update rect.
        let rows = self.visible(b);
        for (k, (i, r)) in rows.iter().enumerate() {
            let row = &self.rows[*i];
            // A divider above an item that follows another item of the same section.
            let divider = k > 0 && row.kind == Kind::Item && self.rows[rows[k - 1].0].kind == Kind::Item;
            SettingsList::paint_row(cx, *r, row, self.pressed == Some(*i), divider);
        }
        if let Some((track, _)) = self.scrollbar(b) {
            // The shared bar in black and white only (its DU look): a list jump is one
            // DU update, which shows no gray track.
            self.scroll_bar(b).paint_drag(cx, track);
        }
        log::debug!("settings list: frame at {} painted in {:?}", self.scroll, started.elapsed());
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Touch(t) => {
                if t.kind == TouchKind::Down && !b.contains(t.pos) {
                    return EventResult::Ignored;
                }
                if t.kind == TouchKind::Down {
                    self.slid = false;
                }
                if let Some(r) = self.slider_touch(ev, t, b) {
                    return r;
                }
                let under = self.item_at(b, t.pos);
                // No highlight on touch-down: the touch may still become a swipe that
                // scrolls the list. A row shows pressed from Press (held still) on, and
                // stops when the finger leaves it or the tap slop.
                let now = t.press_target(self.pressed, under);
                if now != self.pressed {
                    self.changed.extend(self.pressed);
                    self.changed.extend(now);
                    self.pressed = now;
                    EventResult::Handled
                } else if t.kind == TouchKind::Down {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                if let Some((i, sr)) = self.slider_at(b, *pos) {
                    // Minus and plus; a tap on the track was the drag's lift already.
                    let Some(s) = self.rows[i].slider.as_mut() else { return EventResult::Handled };
                    return match s.event(ev, sr) {
                        EventResult::Action(a) => {
                            self.changed.push(i);
                            EventResult::Action(a)
                        }
                        _ => EventResult::Handled,
                    };
                }
                if let Some(i) = self.item_at(b, *pos) {
                    self.changed.extend(self.pressed.take());
                    self.changed.push(i);
                    let (up, down) = order_buttons(self.visible(b).into_iter().find(|(k, _)| *k == i).map_or(b, |(_, r)| r));
                    let row = &mut self.rows[i];
                    return EventResult::Action(match &mut row.trail {
                        Trail::Order { up: can_up, .. } if up.contains(*pos) => {
                            if !*can_up {
                                return EventResult::Handled;
                            }
                            format!("{}:up", row.action)
                        }
                        Trail::Order { down: can_down, .. } if down.contains(*pos) => {
                            if !*can_down {
                                return EventResult::Handled;
                            }
                            format!("{}:down", row.action)
                        }
                        // A switch (an ordered item's too, outside its arrows) flips.
                        Trail::Toggle(on) | Trail::Order { on, .. } => {
                            *on = !*on;
                            format!("{}:{}", row.action, if *on { "on" } else { "off" })
                        }
                        _ => row.action.clone(),
                    });
                }
                EventResult::Handled
            }
            // The swipe that ends a slider drag does not scroll.
            UiEvent::Gesture(Gesture::Swipe { .. }) if std::mem::take(&mut self.slid) => EventResult::Handled,
            UiEvent::Gesture(Gesture::Swipe { from, to, dir }) => {
                let page = SettingsList::swipe_step(b.h, to.y - from.y);
                match dir {
                    Direction::Up => self.scroll_by(b, page),
                    Direction::Down => self.scroll_by(b, -page),
                    Direction::Left | Direction::Right => false,
                };
                EventResult::Handled
            }
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        if self.pressed.is_some() || self.fast || self.dragging.is_some() {
            // A jump is DU: fast, and exact for the black-and-white rows (a large GL16
            // would be promoted to a GC16 flash).
            RefreshClass::Du
        } else {
            // A released row: GL16.
            RefreshClass::Gl16
        }
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        self.fast = false;
        let changed = std::mem::take(&mut self.changed);
        // A new offset or a settle shows the whole area; an event that changed no row
        // (a touch-down) refreshes nothing.
        let moved = std::mem::take(&mut self.moved);
        if moved {
            return None;
        }
        // A touch that changed nothing (on a header, a note or empty space): nothing to
        // show. (It used to refresh the whole list, which is now a screen-change flash.)
        if changed.is_empty() {
            return Some(Vec::new());
        }
        let rows = self.visible(b);
        Some(rows.into_iter().filter(|(i, _)| changed.contains(i)).map(|(_, r)| r.intersect(&b)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(sections: &[usize]) -> Vec<Row> {
        let mut v = Vec::new();
        for (s, n) in sections.iter().enumerate() {
            v.push(Row::header(format!("S{s}")));
            for i in 0..*n {
                v.push(Row::nav(Icon::Info, format!("r{s}.{i}"), format!("a:{s}:{i}")));
            }
        }
        v
    }

    fn touch(kind: TouchKind, y: i32) -> UiEvent {
        UiEvent::Touch(ink_ui::Touch { slot: 0, kind, pos: Point::new(600, y), start: Point::new(600, 900), held: std::time::Duration::ZERO, moved: (y - 900).abs() >= 80 })
    }

    fn swipe(y0: i32, y1: i32) -> UiEvent {
        let dir = if y1 < y0 { Direction::Up } else { Direction::Down };
        UiEvent::Gesture(Gesture::Swipe { from: Point::new(600, y0), to: Point::new(600, y1), dir })
    }

    #[test]
    fn swipes_jump_half_a_page_without_following_the_finger() {
        let mut l = SettingsList::new(rows(&[30]));
        let b = Rect::new(0, 100, 1272, 1400);
        assert_eq!(l.max_scroll(b), (HEADER_H + 30 * ROW_H + END_PAD) as i32 - 1400);
        let (need, r) = l.locate(b, "a:0:19").unwrap();
        assert!(need > 0 && r.bottom() <= b.bottom());
        // Moves do not scroll: no frame follows the finger.
        l.event(&touch(TouchKind::Down, 900), b);
        l.event(&touch(TouchKind::Move, 700), b);
        l.event(&touch(TouchKind::Up, 600), b);
        assert_eq!(l.scroll, 0);
        // A short swipe up: half a page (700), down to a row top (header 72 + 5 rows),
        // so the first row shown is whole; one DU repaint of the whole list.
        assert_eq!(l.event(&swipe(900, 600), b), EventResult::Handled);
        assert_eq!((l.scroll, l.refresh_class()), (72 + 5 * ROW_H as i32, RefreshClass::Du));
        let shown = l.visible(b);
        assert_eq!(shown.first().map(|(_, r)| r.y), Some(b.y), "the top row is whole");
        assert!(shown.iter().all(|(_, r)| r.y >= b.y && r.bottom() <= b.bottom()), "only whole rows are shown");
        assert_eq!(l.damage(b), None, "a jump repaints the whole area");
        // A touch that changes nothing refreshes nothing (no full-list flash).
        assert_eq!(l.damage(b), Some(Vec::new()));
        // A long swipe down: a whole page (clamped at the top).
        l.event(&swipe(200, 1300), b);
        assert_eq!(l.scroll, 0);
        l.event(&swipe(1300, 200), b);
        assert_eq!(l.scroll, 72 + 11 * ROW_H as i32, "a page (1400), down to a row top");
        // The tap after a swipe acts.
        let tap = UiEvent::Gesture(Gesture::Tap { pos: Point::new(600, 600) });
        assert!(matches!(l.event(&tap, b), EventResult::Action(_)));
        // Clamped at both ends.
        assert!(!l.scroll_to(b, -50) || l.scroll == 0);
        l.scroll_to(b, 1_000_000);
        assert_eq!(l.scroll, l.max_scroll(b));
    }

    #[test]
    fn toggle_and_order_rows_report_their_parts() {
        let order = Row::item("Home", "nav:home").trail(Trail::Order { on: true, up: false, down: true });
        let mut l = SettingsList::new(vec![Row::header("H"), Row::toggle("Night", false, "night"), order]);
        let b = Rect::new(0, 0, 1272, 800);
        let (_, r) = l.locate(b, "night").unwrap();
        let tap = |x, y| UiEvent::Gesture(Gesture::Tap { pos: Point::new(x, y) });
        assert_eq!(l.event(&tap(r.x + 10, r.y + 10), b), EventResult::Action("night:on".into()));
        let (_, r) = l.locate(b, "nav:home").unwrap();
        let (up, down) = order_buttons(r);
        assert!(up.w >= 96 && down.x > up.right());
        assert_eq!(l.event(&tap(down.x + 40, down.y + 40), b), EventResult::Action("nav:home:down".into()));
        assert_eq!(l.event(&tap(up.x + 40, up.y + 40), b), EventResult::Handled, "up is inactive");
        assert_eq!(l.event(&tap(r.x + 40, r.y + 40), b), EventResult::Action("nav:home:off".into()));
        // Headers are not tappable.
        assert_eq!(l.event(&tap(10, 10), b), EventResult::Handled);
    }
}
