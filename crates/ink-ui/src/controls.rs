//! Small controls: [`Toggle`], [`Slider`], [`ProgressBar`] (thick or thin, chapter
//! marks), [`RingProgress`] (covers), [`Segmented`], [`Chips`], [`Tabs`], and
//! [`TextBlock`] (wrapped, optionally hyphenated text).
//!
//! Actions are strings: `"<action>:on"` / `"<action>:off"` for toggles,
//! `"<action>:<value>"` for sliders, `"<action>:<index>"` for tabs and segmented
//! controls, `"<action>:<index>:on"` / `":off"` for chips.

use crate::draw::{fill_round_rect, stroke_round_rect, Path};
use crate::gesture::{Gesture, TouchKind};
use crate::icon::Icon;
use crate::theme::space;
use crate::widget::{press_class, EventResult, PaintCx, Widget};
use crate::UiEvent;
use ink_device::{Point, Rect, RefreshClass};
use ink_text::hyphen::{hyphenator_for, Hyphenator};
use ink_text::linebreak::wrap;
use ink_text::Style;

/// An on/off switch with a label on its left.
#[derive(Clone, Debug, PartialEq)]
pub struct Toggle {
    /// Caption.
    pub label: String,
    /// State.
    pub on: bool,
    /// Action prefix.
    pub action: String,
    changed: bool,
}

impl Toggle {
    /// A toggle.
    pub fn new(label: impl Into<String>, on: bool, action: impl Into<String>) -> Self {
        Toggle { label: label.into(), on, action: action.into(), changed: false }
    }

    /// Rectangle of the switch inside `b`.
    pub fn switch_rect(b: Rect) -> Rect {
        let h = (b.h * 11 / 20).clamp(24, 64);
        let w = h * 2;
        Rect::new(b.right() - w as i32 - 8, b.y + (b.h as i32 - h as i32) / 2, w, h)
    }
}

impl Widget for Toggle {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let s = Toggle::switch_rect(b);
        let label_r = Rect::new(b.x + 8, b.y, (s.x - b.x - 24).max(0) as u32, b.h);
        let (fg, bg) = (cx.theme.fg, cx.theme.bg);
        let px = cx.theme.font_px;
        cx.text_left(label_r, &self.label, px, Style::Regular, fg);
        let r = s.h as f32 / 2.0;
        let knob_r = r - 6.0;
        if self.on {
            // Black track, white knob on the right with a check mark.
            fill_round_rect(cx.canvas, s, r, fg);
            let (kx, ky) = (s.right() as f32 - r, s.y as f32 + r);
            Path::new().circle(kx, ky, knob_r).fill(cx.canvas, bg);
            let side = (knob_r * 1.3) as u32;
            Icon::Check.draw(cx.canvas, Rect::new((kx - side as f32 / 2.0) as i32, (ky - side as f32 / 2.0) as i32, side, side), fg);
        } else {
            // White track with a black outline, black knob on the left: black and white
            // only, so a DU update (a list jump) shows it exactly.
            fill_round_rect(cx.canvas, s, r, bg);
            stroke_round_rect(cx.canvas, s, r, cx.theme.border_px as f32, fg);
            Path::new().circle(s.x as f32 + r, s.y as f32 + r, knob_r).fill(cx.canvas, fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Tap { .. }) => {
                self.on = !self.on;
                self.changed = true;
                EventResult::Action(format!("{}:{}", self.action, if self.on { "on" } else { "off" }))
            }
            _ => EventResult::Ignored,
        }
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        std::mem::take(&mut self.changed).then(|| vec![Toggle::switch_rect(b)])
    }
}

/// A horizontal slider with minus/plus buttons, a caption and the value.
#[derive(Clone, Debug, PartialEq)]
pub struct Slider {
    /// Caption (left, above the track).
    pub label: String,
    /// Smallest value.
    pub min: f32,
    /// Largest value.
    pub max: f32,
    /// Step (values snap to `min + k * step`).
    pub step: f32,
    /// Current value.
    pub value: f32,
    /// Action prefix.
    pub action: String,
    dragging: bool,
    /// Value when the drag started (restored when the touch is cancelled).
    before_drag: f32,
}

impl Slider {
    /// A slider.
    pub fn new(label: impl Into<String>, min: f32, max: f32, step: f32, value: f32, action: impl Into<String>) -> Self {
        let mut s = Slider { label: label.into(), min, max, step, value, action: action.into(), dragging: false, before_drag: value };
        s.value = s.snap(value);
        s
    }

    fn snap(&self, v: f32) -> f32 {
        let v = v.clamp(self.min, self.max);
        if self.step > 0.0 {
            (self.min + ((v - self.min) / self.step).round() * self.step).clamp(self.min, self.max)
        } else {
            v
        }
    }

    /// The value as text (no decimals for whole steps).
    pub fn value_text(&self) -> String {
        if self.step.fract() == 0.0 {
            format!("{}", self.value.round() as i64)
        } else {
            format!("{:.1}", self.value)
        }
    }

    /// (minus button, track, plus button) inside `b`.
    pub fn parts(b: Rect) -> (Rect, Rect, Rect) {
        let row_h = b.h / 2;
        let y = b.y + row_h as i32;
        let btn = row_h.min(110);
        let minus = Rect::new(b.x, y + (row_h as i32 - btn as i32) / 2, btn, btn);
        let plus = Rect::new(b.right() - btn as i32, minus.y, btn, btn);
        let track = Rect::new(minus.right() + 24, y, (plus.x - minus.right() - 48).max(1) as u32, row_h);
        (minus, track, plus)
    }

    fn at(&self, track: Rect, p: Point) -> f32 {
        let f = (p.x - track.x) as f32 / track.w.max(1) as f32;
        self.snap(self.min + f.clamp(0.0, 1.0) * (self.max - self.min))
    }

    fn report(&self) -> EventResult {
        EventResult::Action(format!("{}:{}", self.action, self.value_text()))
    }
}

/// Draws a round outlined icon button (a slider's step buttons).
pub fn paint_round_button(cx: &mut PaintCx, r: Rect, icon: Icon) {
    let fg = cx.theme.fg;
    stroke_round_rect(cx.canvas, r, r.w as f32 / 2.0, cx.theme.border_px as f32, fg);
    cx.icon(icon, r, r.w / 2, fg);
}

/// Height of a slider's track bar.
const TRACK_H: f32 = 12.0;

/// Draws a slider's bar across the middle of `track`, black up to the fraction `f`, the
/// rest in the theme's track gray, or white with a black outline when `du` (a drag shown
/// with DU has no gray). Returns the centre of the knob.
pub fn paint_slider_track(cx: &mut PaintCx, track: Rect, f: f32, du: bool) -> (f32, f32) {
    let (fg, r) = (cx.theme.fg, TRACK_H / 2.0);
    let ty = track.y as f32 + track.h as f32 / 2.0;
    let kx = track.x as f32 + f * track.w as f32;
    let bar = Rect::new(track.x, (ty - r) as i32, track.w, TRACK_H as u32);
    fill_round_rect(cx.canvas, bar, r, if du { cx.theme.bg } else { cx.theme.track() });
    if du {
        stroke_round_rect(cx.canvas, bar, r, 2.0, fg);
    }
    fill_round_rect(cx.canvas, Rect::new(bar.x, bar.y, (kx - track.x as f32).max(0.0) as u32, bar.h), r, fg);
    (kx, ty)
}

/// Draws a slider's knob centred at `at` on `track`: a ring, solid while `dragging`.
pub fn paint_slider_knob(cx: &mut PaintCx, track: Rect, (kx, ty): (f32, f32), dragging: bool) {
    let (fg, bg) = (cx.theme.fg, cx.theme.bg);
    let kr = (track.h as f32 * 0.28).min(26.0);
    Path::new().circle(kx, ty, kr).fill(cx.canvas, fg);
    Path::new().circle(kx, ty, kr - 5.0).fill(cx.canvas, if dragging { fg } else { bg });
}

impl Widget for Slider {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        let (px, small) = (cx.theme.font_px, cx.theme.small_px);
        let top = Rect::new(b.x + 8, b.y, b.w.saturating_sub(16), b.h / 2);
        cx.text_left(top, &self.label, small * 1.15, Style::Regular, fg);
        let baseline = cx.baseline_in(top, px, Style::Bold);
        cx.draw_text_right(Style::Bold, px, top.right() as f32, baseline, &self.value_text(), fg, b);
        let (minus, track, plus) = Slider::parts(b);
        paint_round_button(cx, minus, Icon::Minus);
        paint_round_button(cx, plus, Icon::Plus);
        let f = if self.max > self.min { (self.value - self.min) / (self.max - self.min) } else { 0.0 };
        let knob = paint_slider_track(cx, track, f, false);
        paint_slider_knob(cx, track, knob, self.dragging);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let (minus, track, plus) = Slider::parts(b);
        let hit_track = Rect::new(track.x - 30, track.y, track.w + 60, track.h);
        match ev {
            UiEvent::Touch(t) => match t.kind {
                TouchKind::Down if hit_track.contains(t.pos) => {
                    self.dragging = true;
                    self.before_drag = self.value;
                    self.value = self.at(track, t.pos);
                    EventResult::Handled
                }
                TouchKind::Move | TouchKind::Hold if self.dragging => {
                    let v = self.at(track, t.pos);
                    if v != self.value {
                        self.value = v;
                        EventResult::Handled
                    } else {
                        EventResult::Ignored
                    }
                }
                TouchKind::Up if self.dragging => {
                    self.dragging = false;
                    self.value = self.at(track, t.pos);
                    self.report()
                }
                // The drag ended without a lift (rotation): back to the value before it,
                // nothing reported.
                TouchKind::Cancel if self.dragging => {
                    self.dragging = false;
                    self.value = self.before_drag;
                    EventResult::Handled
                }
                _ => EventResult::Ignored,
            },
            UiEvent::Gesture(Gesture::Tap { pos }) if minus.contains(*pos) || plus.contains(*pos) => {
                let d = if minus.contains(*pos) { -1.0 } else { 1.0 };
                let step = if self.step > 0.0 { self.step } else { (self.max - self.min) / 10.0 };
                self.value = self.snap(self.value + d * step);
                self.report()
            }
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        press_class(self.dragging)
    }
}

/// Look of a [`ProgressBar`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressKind {
    /// A rounded bar with a black outline, 16-24 px tall (settings, downloads, the gallery).
    #[default]
    Thick,
    /// An 8 px track (reader footer, list rows, covers); chapter ticks stand out above
    /// and below it.
    Thin,
}

/// A progress bar with optional chapter marks, a caption above it (left) and a label on
/// its right (e.g. `42%`).
#[derive(Clone, Debug, PartialEq)]
pub struct ProgressBar {
    /// Fraction done, 0..=1.
    pub value: f32,
    /// Tick marks (fractions), e.g. chapter starts.
    pub marks: Vec<f32>,
    /// Caption right of the bar (e.g. `42%`).
    pub label: Option<String>,
    /// Text whose width is the least width of the label column (e.g. `100%`): bars of
    /// different rows end at the same x, and the label is right-aligned in the column.
    pub label_slot: Option<String>,
    /// Thick or thin.
    pub kind: ProgressKind,
    /// Caption above the bar, left (e.g. the chapter title); muted caption role.
    pub caption: Option<String>,
    /// Black and white only (no gray track, no anti-aliased ends): the reader's status
    /// bars, which change in the page-turn REAGL update. REAGL is a text waveform: gray
    /// pixels pass through dark while it runs, so a gray track flashes "full" on every
    /// turn. The unread part is a thin black line instead.
    pub two_level: bool,
}

impl ProgressBar {
    /// A thick bar at `value`.
    pub fn new(value: f32) -> Self {
        ProgressBar { value, marks: Vec::new(), label: None, label_slot: None, kind: ProgressKind::Thick, caption: None, two_level: false }
    }

    /// A thin bar at `value`.
    pub fn thin(value: f32) -> Self {
        ProgressBar { kind: ProgressKind::Thin, ..ProgressBar::new(value) }
    }

    setters! {
        /// Black and white only (see [`ProgressBar::two_level`]).
        two_level() => two_level = true;
        /// With chapter marks (fractions).
        with_marks(marks: Vec<f32>) => marks = marks;
        /// With a label on the right.
        with_label(label: impl Into<String>) => label = Some(label.into());
        /// Reserves the label column for `widest` (see [`ProgressBar::label_slot`]).
        with_label_slot(widest: impl Into<String>) => label_slot = Some(widest.into());
        /// With a caption above the bar.
        with_caption(caption: impl Into<String>) => caption = Some(caption.into());
    }

    /// Track thickness for `kind` in a box `h` tall.
    fn track_h(&self, h: u32) -> u32 {
        match self.kind {
            ProgressKind::Thick => (h / 3).clamp(8, 24),
            ProgressKind::Thin => 8.min(h.max(2)),
        }
    }
}

/// `v` clamped to 0..=1 (NaN counts as 0).
fn unit(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

impl Widget for ProgressBar {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        let small = t.caption.px;
        let text_w = match &self.label {
            Some(l) => cx.text_width(Style::Bold, small, l).ceil() as u32,
            None => 0,
        };
        let slot_w = match (&self.label, &self.label_slot) {
            (Some(_), Some(w)) => cx.text_width(Style::Bold, small, w).ceil() as u32,
            _ => 0,
        };
        let label_w = match &self.label {
            Some(_) => text_w.max(slot_w) + space(3) as u32,
            None => 0,
        };
        // The caption takes the top part of the box when there is one.
        let mut area = b;
        if let Some(c) = &self.caption {
            let cap_h = ((small * 1.4) as u32).min(b.h / 2);
            let r = Rect::new(b.x + 4, b.y, b.w.saturating_sub(8 + label_w), cap_h);
            cx.text_left(r, c, small, t.caption.style, t.muted);
            area = Rect::new(b.x, b.y + cap_h as i32, b.w, b.h - cap_h);
        }
        let h = self.track_h(area.h);
        let bar = Rect::new(area.x + 4, area.y + (area.h as i32 - h as i32) / 2, area.w.saturating_sub(8 + label_w), h);
        let done = (bar.w as f32 * unit(self.value)).round() as u32;
        if self.two_level {
            self.paint_two_level(cx, area, bar, h, done);
        } else {
            self.paint_gray(cx, area, bar, h, done);
        }
        if let Some(l) = &self.label {
            // Right-aligned in the column when a slot is reserved.
            let col_w = label_w.saturating_sub(space(2) as u32);
            let off = if self.label_slot.is_some() { col_w.saturating_sub(text_w + 4) } else { 0 };
            let r = Rect::new(bar.right() + space(2) + off as i32, area.y, col_w - off.min(col_w), area.h);
            cx.text_left(r, l, small, Style::Bold, t.fg);
        }
    }
}

impl ProgressBar {
    /// The gray track with a black read part (round ends).
    fn paint_gray(&self, cx: &mut PaintCx, area: Rect, bar: Rect, h: u32, done: u32) {
        let t = cx.theme;
        let rad = h as f32 / 2.0;
        fill_round_rect(cx.canvas, bar, rad, t.track());
        if done > 0 {
            fill_round_rect(cx.canvas, Rect::new(bar.x, bar.y, done.max(h).min(bar.w), h), rad, t.fg);
        }
        match self.kind {
            ProgressKind::Thick => {
                // Chapter starts: notches through the bar, white in the done part and
                // black in the rest (white on the light track would not show).
                let done_x = bar.x + done as i32;
                for m in &self.marks {
                    let x = bar.x + (bar.w as f32 * unit(*m)) as i32;
                    if x > bar.x + h as i32 / 2 && x < bar.right() - h as i32 / 2 {
                        cx.canvas.fill_rect(x - 2, bar.y, 4, h as i32, if x < done_x { t.bg } else { t.fg });
                    }
                }
                stroke_round_rect(cx.canvas, bar, rad, t.border_px as f32, t.fg);
            }
            ProgressKind::Thin => {
                // Chapter starts: black ticks reaching above and below the track.
                let reach = (area.h as i32 - h as i32) / 2;
                let tick = reach.clamp(0, 8);
                for m in &self.marks {
                    let x = bar.x + (bar.w as f32 * unit(*m)) as i32;
                    cx.canvas.fill_rect(x - 2, bar.y - tick, 4, h as i32 + 2 * tick, t.fg);
                }
            }
        }
    }

    /// Black and white only ([`ProgressBar::two_level`]): read part the full track height,
    /// unread part a 2 px line on the track's centre (thick bars: a 2 px outline), square
    /// ends; chapter ticks black, white where they cross the read part.
    fn paint_two_level(&self, cx: &mut PaintCx, area: Rect, bar: Rect, h: u32, done: u32) {
        let t = cx.theme;
        let line = 2.min(h as i32);
        match self.kind {
            ProgressKind::Thick => {
                let bw = t.border_px.max(2);
                cx.canvas.fill_rect(bar.x, bar.y, bar.w as i32, bw, t.fg);
                cx.canvas.fill_rect(bar.x, bar.bottom() - bw, bar.w as i32, bw, t.fg);
                cx.canvas.fill_rect(bar.x, bar.y, bw, h as i32, t.fg);
                cx.canvas.fill_rect(bar.right() - bw, bar.y, bw, h as i32, t.fg);
            }
            ProgressKind::Thin => cx.canvas.fill_rect(bar.x, bar.y + (h as i32 - line) / 2, bar.w as i32, line, t.fg),
        }
        cx.canvas.fill_rect(bar.x, bar.y, done.min(bar.w) as i32, h as i32, t.fg);
        let reach = if self.kind == ProgressKind::Thin { ((area.h as i32 - h as i32) / 2).clamp(0, 8) } else { 0 };
        let done_x = bar.x + done as i32;
        for m in &self.marks {
            let x = bar.x + (bar.w as f32 * unit(*m)) as i32;
            cx.canvas.fill_rect(x - 2, bar.y - reach, 4, h as i32 + 2 * reach, if x < done_x { t.bg } else { t.fg });
        }
    }
}

/// A ring that shows progress, for book covers and tiles: a solid paper disc (so it
/// reads over a cover picture), a light track, a black arc clockwise from 12 o'clock
/// and an optional label in the middle (e.g. `42%`).
#[derive(Clone, Debug, PartialEq)]
pub struct RingProgress {
    /// Fraction done, 0..=1.
    pub value: f32,
    /// Text in the middle (bold caption role); `None`: the arc only.
    pub label: Option<String>,
}

impl RingProgress {
    /// A ring at `value` labelled with the percentage.
    pub fn new(value: f32) -> Self {
        RingProgress { value, label: Some(format!("{}%", (unit(value) * 100.0).round() as u32)) }
    }

    setters! {
        /// Without the label.
        bare() => label = None;
    }

    /// Square badge rectangle `side` big in the bottom-right corner of `cover`, inset by
    /// one grid step.
    pub fn badge_rect(cover: Rect, side: u32) -> Rect {
        let side = side.min(cover.w).min(cover.h);
        let d = space(1).min((cover.w.min(cover.h) as i32 - side as i32) / 2).max(0);
        Rect::new(cover.right() - side as i32 - d, cover.bottom() - side as i32 - d, side, side)
    }
}

impl Widget for RingProgress {
    /// The disc is round: the corners of the bounds keep what is under them (a cover).
    fn opaque(&self) -> bool {
        false
    }

    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        let side = b.w.min(b.h) as f32;
        if side < 4.0 {
            return;
        }
        let (x, y) = crate::draw::center(b);
        let r = side / 2.0;
        let w = (side / 9.0).clamp(3.0, 14.0);
        // Backing disc with a black edge, so the ring reads on any cover.
        Path::new().circle(x, y, r).fill(cx.canvas, t.fg);
        Path::new().circle(x, y, r - 2.0).fill(cx.canvas, t.bg);
        let rr = r - 2.0 - w / 2.0 - 2.0;
        Path::new().ring(x, y, rr + w / 2.0, w).fill(cx.canvas, t.track());
        let v = unit(self.value);
        if v > 0.0 {
            let a0 = -std::f32::consts::FRAC_PI_2;
            Path::new().arc(x, y, rr, a0, a0 + v * 2.0 * std::f32::consts::PI, w).fill(cx.canvas, t.fg);
        }
        if let Some(l) = &self.label {
            let inner = (2.0 * (rr - w / 2.0) * 0.8) as u32;
            let px = (inner as f32 / 2.6).min(t.caption.px);
            let ir = Rect::new((x - inner as f32 / 2.0) as i32, (y - inner as f32 / 2.0) as i32, inner, inner);
            cx.text_centered(ir, l, px, Style::Bold, t.fg);
        }
    }
}

/// A row of mutually exclusive options in one pill (view modes, day/week/month); the
/// selected segment is black with white bold text. Reports `"<action>:<index>"`.
#[derive(Clone, Debug, PartialEq)]
pub struct Segmented {
    /// Option captions.
    pub options: Vec<String>,
    /// Selected index.
    pub selected: usize,
    /// Action prefix.
    pub action: String,
}

impl Segmented {
    /// A control with the first option selected.
    pub fn new(options: &[&str], action: impl Into<String>) -> Self {
        Segmented { options: options.iter().map(|s| s.to_string()).collect(), selected: 0, action: action.into() }
    }

    setters! {
        /// With `i` selected.
        with_selected(i: usize) => selected = i;
    }

    /// Segment rectangles inside `b`.
    pub fn cells(&self, b: Rect) -> Vec<Rect> {
        crate::widget::split_cols(b, self.options.len().max(1) as u32, 0).into_iter().take(self.options.len()).collect()
    }
}

impl Widget for Segmented {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        let rad = b.h as f32 / 2.0;
        let cells = self.cells(b);
        let n = cells.len();
        for (i, (r, label)) in cells.iter().zip(&self.options).enumerate() {
            let sel = i == self.selected;
            if sel {
                // Rounded on the outer ends only.
                let mut p = Path::new();
                p.round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, rad);
                if i > 0 {
                    p.rect(r.x as f32, r.y as f32, rad, r.h as f32);
                }
                if i + 1 < n {
                    p.rect(r.right() as f32 - rad, r.y as f32, rad, r.h as f32);
                }
                p.fill(cx.canvas, t.fg);
            } else if i > 0 && i - 1 != self.selected {
                cx.canvas.fill_rect(r.x - 1, r.y + space(2), t.border_px, r.h as i32 - 2 * space(2), t.fg);
            }
            let style = if sel { Style::Bold } else { t.label.style };
            let inner = Rect::new(r.x + space(1), r.y, r.w.saturating_sub(space(2) as u32), r.h);
            cx.text_centered(inner, label, t.label.px, style, if sel { t.bg } else { t.fg });
        }
        stroke_round_rect(cx.canvas, b, rad, t.border_px as f32, t.fg);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let UiEvent::Gesture(Gesture::Tap { pos }) = ev else {
            return match ev {
                UiEvent::Gesture(_) => EventResult::Handled,
                _ => EventResult::Ignored,
            };
        };
        select_tapped(&self.cells(b), *pos, &mut self.selected, &self.action)
    }
}

/// Selects the cell of `cells` under a tap at `pos`: reports `"<action>:<index>"` when
/// that is another cell than `selected`, else just takes the tap.
fn select_tapped(cells: &[Rect], pos: Point, selected: &mut usize, action: &str) -> EventResult {
    match cells.iter().position(|r| r.contains(pos)) {
        Some(i) if i != *selected => {
            *selected = i;
            EventResult::Action(format!("{action}:{i}"))
        }
        _ => EventResult::Handled,
    }
}

/// One chip of a [`Chips`] row.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    /// Caption.
    pub label: String,
    /// Selected (filled, with a check mark).
    pub selected: bool,
}

/// A wrapping row of pill-shaped filter chips. Outlined when off; black with a check
/// mark when on. Reports `"<action>:<index>:on"` / `":off"`. With `single`, selecting one
/// clears the others (and a tap on the selected chip keeps it).
#[derive(Clone, Debug, PartialEq)]
pub struct Chips {
    /// The chips.
    pub chips: Vec<Chip>,
    /// Action prefix.
    pub action: String,
    /// At most one selected.
    pub single: bool,
    /// Chip rectangles of the last paint (hit testing needs the measured widths).
    rects: Vec<Rect>,
}

impl Chips {
    /// Chips from captions, none selected.
    pub fn new(labels: &[&str], action: impl Into<String>) -> Self {
        Chips {
            chips: labels.iter().map(|l| Chip { label: l.to_string(), selected: false }).collect(),
            action: action.into(),
            single: false,
            rects: Vec::new(),
        }
    }

    setters! {
        /// Only one chip on at a time.
        single() => single = true;
    }

    /// With chip `i` selected.
    pub fn with_selected(mut self, i: usize) -> Self {
        if let Some(c) = self.chips.get_mut(i) {
            c.selected = true;
        }
        self
    }

    /// Height of one chip.
    pub const CHIP_H: u32 = 96;

    /// Chip rectangles from the last paint.
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }
}

impl Widget for Chips {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        let h = Chips::CHIP_H.min(b.h);
        let px = t.label.px * 0.95;
        let check = (px * 0.9) as u32;
        self.rects.clear();
        let (mut x, mut y) = (b.x, b.y);
        for c in &self.chips {
            // A few pixels of slack: the measured width is rounded down.
            let tw = cx.text_width(Style::Bold, px, &c.label).ceil() as u32 + 4;
            let w = (tw + 2 * space(3) as u32 + if c.selected { check + space(1) as u32 } else { 0 }).min(b.w);
            if x > b.x && x + w as i32 > b.right() {
                x = b.x;
                y += h as i32 + space(2);
            }
            let r = Rect::new(x, y, w, h);
            self.rects.push(r);
            x += w as i32 + space(2);
            if r.bottom() > b.bottom() {
                continue;
            }
            let rad = h as f32 / 2.0;
            if c.selected {
                fill_round_rect(cx.canvas, r, rad, t.fg);
                let ir = Rect::new(r.x + space(3), r.y + (h as i32 - check as i32) / 2, check, check);
                Icon::Check.draw(cx.canvas, ir, t.bg);
                let lr = Rect::new(ir.right() + space(1), r.y, (r.right() - space(3) - ir.right() - space(1)).max(0) as u32, h);
                cx.text_left(lr, &c.label, px, Style::Bold, t.bg);
            } else {
                stroke_round_rect(cx.canvas, r, rad, t.border_px as f32, t.fg);
                let lr = Rect::new(r.x + space(3), r.y, r.w.saturating_sub(2 * space(3) as u32), h);
                cx.text_left(lr, &c.label, px, Style::Regular, t.fg);
            }
        }
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        let UiEvent::Gesture(Gesture::Tap { pos }) = ev else {
            return match ev {
                UiEvent::Gesture(_) => EventResult::Handled,
                _ => EventResult::Ignored,
            };
        };
        let Some(i) = self.rects.iter().position(|r| r.contains(*pos)) else { return EventResult::Handled };
        if self.single {
            if self.chips[i].selected {
                return EventResult::Handled;
            }
            for c in self.chips.iter_mut() {
                c.selected = false;
            }
        }
        let c = &mut self.chips[i];
        c.selected = !c.selected;
        EventResult::Action(format!("{}:{i}:{}", self.action, if c.selected { "on" } else { "off" }))
    }
}

/// A row of tabs; the selected one is bold and underlined.
#[derive(Clone, Debug, PartialEq)]
pub struct Tabs {
    /// Titles.
    pub titles: Vec<String>,
    /// Selected index.
    pub selected: usize,
    /// Action prefix.
    pub action: String,
}

impl Tabs {
    /// Tabs with the first selected.
    pub fn new(titles: &[&str], action: impl Into<String>) -> Self {
        Tabs { titles: titles.iter().map(|s| s.to_string()).collect(), selected: 0, action: action.into() }
    }

    /// Cell rectangles.
    pub fn cells(&self, b: Rect) -> Vec<Rect> {
        crate::widget::split_cols(b, self.titles.len() as u32, 0)
    }
}

impl Widget for Tabs {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        let px = cx.theme.font_px * 0.95;
        let rule = cx.theme.rule_px;
        cx.canvas.fill_rect(b.x, b.bottom() - rule, b.w as i32, rule, fg);
        for (i, (r, t)) in self.cells(b).into_iter().zip(&self.titles).enumerate() {
            let style = if i == self.selected { Style::Bold } else { Style::Regular };
            let text_r = Rect::new(r.x, r.y, r.w, r.h.saturating_sub(10));
            cx.text_centered(text_r, t, px, style, if i == self.selected { fg } else { cx.theme.muted });
            if i == self.selected {
                cx.canvas.fill_rect(r.x + 12, r.bottom() - 8, r.w as i32 - 24, 8, fg);
            }
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let UiEvent::Gesture(Gesture::Tap { pos }) = ev else { return EventResult::Ignored };
        select_tapped(&self.cells(b), *pos, &mut self.selected, &self.action)
    }
}

/// Text alignment of a [`TextBlock`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    /// Ragged right.
    #[default]
    Left,
    /// Centred lines.
    Center,
    /// Both edges straight (the last line of a paragraph stays left).
    Justify,
}

/// Wrapped text: shaped runs, optional hyphenation by language, alignment.
pub struct TextBlock {
    /// Text; `\n` starts a new paragraph.
    pub text: String,
    /// Size.
    pub px: f32,
    /// Style.
    pub style: Style,
    /// Alignment.
    pub align: Align,
    /// Line height as a multiple of `px`.
    pub line_height: f32,
    /// Total-fit line breaking instead of first fit.
    pub optimal: bool,
    language: Option<String>,
    hyphenator: Option<Box<dyn Hyphenator + Send + Sync>>,
}

impl std::fmt::Debug for TextBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextBlock").field("text", &self.text).field("px", &self.px).field("language", &self.language).finish()
    }
}

impl TextBlock {
    /// Left-aligned text without hyphenation.
    pub fn new(text: impl Into<String>, px: f32) -> Self {
        TextBlock {
            text: text.into(),
            px,
            style: Style::Regular,
            align: Align::Left,
            line_height: 1.35,
            optimal: false,
            language: None,
            hyphenator: None,
        }
    }

    /// Hyphenates with the patterns of `language` (BCP 47).
    pub fn hyphenate(mut self, language: &str) -> Self {
        self.language = Some(language.to_string());
        self.hyphenator = Some(hyphenator_for(language));
        self
    }

    setters! {
        /// With an alignment.
        aligned(align: Align) => align = align;
    }

    /// Lines as (text, ends with a hyphen) for a width (for tests and measuring).
    pub fn lines(&self, cx: &mut PaintCx, width: f32) -> Vec<(String, bool)> {
        self.layout(cx.fonts, width).into_iter().map(|(t, h, _)| (t, h)).collect()
    }

    /// Height of the wrapped text at `width` (for stacking blocks).
    pub fn height(&self, fonts: &mut ink_text::Fonts, width: f32) -> u32 {
        let n = self.layout(fonts, width).len().max(1) as f32;
        (n * self.px * self.line_height + self.px * 0.4).ceil() as u32
    }

    /// Lines as (text, ends with a hyphen, last line of its paragraph).
    fn layout(&self, fonts: &mut ink_text::Fonts, width: f32) -> Vec<(String, bool, bool)> {
        let none = ink_text::NoHyphenation;
        let h: &dyn Hyphenator = match &self.hyphenator {
            Some(h) => h.as_ref(),
            None => &none,
        };
        let (style, px) = (self.style, self.px);
        let mut measure = |s: &str| fonts.shaped_width(style, px, s);
        let lines = wrap(&self.text, width, &mut measure, h, self.optimal);
        let mut out = Vec::with_capacity(lines.len());
        for (i, l) in lines.iter().enumerate() {
            let last = match lines.get(i + 1) {
                Some(next) => self.text[l.range.end..next.range.start.max(l.range.end)].contains('\n'),
                None => true,
            };
            out.push((self.text[l.range.clone()].to_string(), l.hyphen, last));
        }
        out
    }
}

impl Widget for TextBlock {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        // Glyphs may overhang their origin by a few pixels (italics, j); keep them inside
        // the bounds, which is all the tree clears and refreshes.
        let clip = b;
        let b = Rect::new(b.x + 4, b.y, b.w.saturating_sub(8), b.h);
        let lines = self.layout(cx.fonts, b.w as f32);
        let (asc, desc) = cx.fonts.vmetrics(self.style, self.px);
        let mut baseline = b.y as f32 + asc;
        // Lines whose descent would cross the bottom are not drawn; a word wider than the
        // block is cut at its edge.
        for (text, hyphen, last_of_para) in &lines {
            if baseline - desc > b.bottom() as f32 {
                break;
            }
            let shown = if *hyphen { format!("{}-", text.trim_end_matches('\u{ad}')) } else { text.replace('\u{ad}', "") };
            let words: Vec<&str> = match self.align {
                Align::Justify if !*last_of_para => shown.split(' ').filter(|w| !w.is_empty()).collect(),
                _ => Vec::new(),
            };
            if words.len() >= 2 {
                let ws: Vec<f32> = words.iter().map(|w| cx.text_width(self.style, self.px, w)).collect();
                let gap = (b.w as f32 - ws.iter().sum::<f32>()) / (words.len() - 1) as f32;
                let mut x = b.x as f32;
                for (w, ww) in words.iter().zip(&ws) {
                    cx.draw_text_clipped(self.style, self.px, x, baseline, w, fg, clip);
                    x += ww + gap;
                }
            } else {
                // Left, centred, or the last line of a justified paragraph (left).
                let x = match self.align {
                    Align::Center => b.x as f32 + (b.w as f32 - cx.text_width(self.style, self.px, &shown)) / 2.0,
                    _ => b.x as f32,
                };
                cx.draw_text_clipped(self.style, self.px, x, baseline, &shown, fg, clip);
            }
            baseline += self.px * self.line_height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::Touch;
    use crate::theme::Theme;
    use crate::widget::WidgetTree;
    use ink_device::Canvas;
    use ink_text::Fonts;
    use std::time::Duration;

    fn tap(x: i32, y: i32) -> UiEvent {
        UiEvent::Gesture(Gesture::Tap { pos: Point::new(x, y) })
    }

    fn touch(kind: TouchKind, x: i32, y: i32) -> UiEvent {
        UiEvent::Touch(Touch { slot: 0, kind, pos: Point::new(x, y), start: Point::new(x, y), held: Duration::ZERO, moved: false })
    }

    fn paint_one(w: Box<dyn Widget>, b: Rect) -> (WidgetTree, Canvas, Fonts, crate::widget::WidgetId) {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1272, 400);
        let mut t = WidgetTree::new();
        let id = t.add(None, w, b);
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        (t, canvas, fonts, id)
    }

    #[test]
    fn toggle_flips_and_refreshes_only_the_switch() {
        let b = Rect::new(0, 0, 800, 100);
        let (mut t, mut canvas, mut fonts, id) = paint_one(Box::new(Toggle::new("Night mode", false, "night")), b);
        let s = Toggle::switch_rect(b);
        assert_eq!(canvas.get(s.x + s.w as i32 / 2, s.y + 8), Some(0xFF), "off: white track (black and white only)");
        assert_eq!(t.dispatch(&tap(400, 50)), Some((id, EventResult::Action("night:on".into()))));
        let theme = Theme::day();
        let dmg = t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg, vec![crate::Damage { rect: s, class: RefreshClass::Gl16 }]);
        assert_eq!(canvas.get(s.x + s.w as i32 / 2, s.y + 8), Some(0), "on: black track");
        assert_eq!(t.dispatch(&tap(400, 50)), Some((id, EventResult::Action("night:off".into()))));
    }

    #[test]
    fn slider_drags_steps_and_reports() {
        let b = Rect::new(0, 0, 1000, 200);
        let mut s = Slider::new("Font size", 10.0, 30.0, 2.0, 21.0, "size");
        assert_eq!(s.value, 22.0, "snapped");
        let (minus, track, plus) = Slider::parts(b);
        assert_eq!(s.event(&tap(plus.x + 5, plus.y + 5), b), EventResult::Action("size:24".into()));
        assert_eq!(s.event(&tap(minus.x + 5, minus.y + 5), b), EventResult::Action("size:22".into()));
        assert_eq!(s.event(&touch(TouchKind::Down, track.x, track.y + 10), b), EventResult::Handled);
        assert_eq!(s.value, 10.0);
        assert_eq!(s.refresh_class(), RefreshClass::Du, "fast while dragging");
        assert_eq!(s.event(&touch(TouchKind::Move, track.right(), track.y + 10), b), EventResult::Handled);
        assert_eq!(s.event(&touch(TouchKind::Up, track.x + track.w as i32 / 2, track.y), b), EventResult::Action("size:20".into()));
        assert_eq!(s.refresh_class(), RefreshClass::Gl16);
        // Touches outside the track are not taken.
        assert_eq!(s.event(&touch(TouchKind::Down, 500, 10), b), EventResult::Ignored);
        // A drag that is cancelled (the screen rotated) goes back and reports nothing.
        s.event(&touch(TouchKind::Down, track.x, track.y + 10), b);
        s.event(&touch(TouchKind::Move, track.right(), track.y + 10), b);
        assert_eq!(s.value, 30.0);
        assert_eq!(s.event(&touch(TouchKind::Cancel, track.right(), track.y + 10), b), EventResult::Handled);
        assert_eq!((s.value, s.refresh_class()), (20.0, RefreshClass::Gl16));
        assert_eq!(s.event(&touch(TouchKind::Cancel, track.right(), track.y + 10), b), EventResult::Ignored);
        let f = Slider::new("Spacing", 1.0, 2.0, 0.1, 1.25, "sp");
        assert_eq!(f.value_text(), "1.3");
        paint_one(Box::new(f), b);
    }

    #[test]
    fn progress_and_tabs() {
        let mut p = ProgressBar::new(0.42);
        p.marks = vec![0.1, 0.5];
        p.label = Some("42%".into());
        let b = Rect::new(0, 0, 1000, 60);
        let (_, canvas, _, _) = paint_one(Box::new(p), b);
        let dark_left = (0..400).filter(|x| canvas.get(*x, 30).unwrap() < 64).count();
        let dark_right = (500..800).filter(|x| canvas.get(*x, 30).unwrap() < 64).count();
        assert!(dark_left > 300 && dark_right < 10, "{dark_left} {dark_right}");
        let mut tabs = Tabs::new(&["Controls", "Lists", "Input"], "tab");
        let b = Rect::new(0, 0, 900, 100);
        assert_eq!(tabs.event(&tap(450, 50), b), EventResult::Action("tab:1".into()));
        assert_eq!(tabs.event(&tap(450, 50), b), EventResult::Handled, "already selected");
        assert_eq!(tabs.event(&touch(TouchKind::Down, 1, 1), b), EventResult::Ignored);
        paint_one(Box::new(tabs), b);
    }

    #[test]
    fn thin_progress_ticks_and_ring() {
        let b = Rect::new(0, 0, 1000, 60);
        let bar = ProgressBar::thin(0.5).with_marks(vec![0.75]).with_label("50%");
        let (_, canvas, _, _) = paint_one(Box::new(bar), b);
        let theme = Theme::day();
        let y = 30;
        assert_eq!(canvas.get(100, y), Some(0), "done part black");
        let track = (500..700).filter(|x| canvas.get(*x, y) == Some(theme.light)).count();
        assert!(track > 100, "light track: {track}");
        // The tick at 75 % reaches above the 8 px track.
        let tick_x = (600..900).find(|x| canvas.get(*x, y - 8) == Some(0));
        assert!(tick_x.is_some(), "tick above the track");
        assert_eq!(canvas.get(300, y - 12), Some(255), "nothing else above the track");
        // Ring: backing disc, arc on the right half at 25 %, light track elsewhere.
        let (_, c, _, _) = paint_one(Box::new(RingProgress::new(0.25).bare()), Rect::new(0, 0, 120, 120));
        let ring_px = |x: i32, y: i32| c.get(x, y).unwrap();
        assert_eq!(ring_px(60, 60), 255, "paper centre");
        assert!(ring_px(60 + 46, 60 - 20) < 64, "arc in the first quarter");
        assert_eq!(ring_px(60 - 47, 60), theme.light, "track in the third quarter");
        assert_eq!(RingProgress::badge_rect(Rect::new(0, 0, 300, 450), 100), Rect::new(192, 342, 100, 100));
    }

    #[test]
    fn segmented_and_chips_select() {
        let b = Rect::new(0, 0, 900, 96);
        let mut seg = Segmented::new(&["List", "Grid", "Covers"], "view");
        let cells = seg.cells(b);
        assert_eq!(seg.event(&tap(cells[2].x + 10, 40), b), EventResult::Action("view:2".into()));
        assert_eq!(seg.event(&tap(cells[2].x + 10, 40), b), EventResult::Handled, "already selected");
        let (_, canvas, _, _) = paint_one(Box::new(seg.clone()), b);
        assert_eq!(canvas.get(cells[2].x + 40, 20), Some(0), "selected segment filled");
        assert_eq!(canvas.get(cells[0].x + 60, 20), Some(255), "others white");

        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1000, 400);
        let mut t = WidgetTree::new();
        let cb = Rect::new(0, 0, 1000, 400);
        let id = t.add(None, Box::new(Chips::new(&["Unread", "Reading", "Finished"], "st").single().with_selected(0)), cb);
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        let rects = t.get_mut::<Chips>(id).unwrap().rects().to_vec();
        assert_eq!(rects.len(), 3);
        assert!(rects.iter().all(|r| r.h >= 96));
        let r1 = rects[1];
        assert_eq!(t.dispatch(&tap(r1.x + 20, r1.y + 40)), Some((id, EventResult::Action("st:1:on".into()))));
        let chips = t.get_mut::<Chips>(id).unwrap();
        assert_eq!(chips.chips.iter().map(|c| c.selected).collect::<Vec<_>>(), vec![false, true, false], "single");
        assert_eq!(chips.event(&tap(r1.x + 20, r1.y + 40), cb), EventResult::Handled, "single: stays on");
        let mut multi = Chips::new(&["EPUB", "PDF"], "f");
        multi.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), cb);
        let r = multi.rects()[0];
        assert_eq!(multi.event(&tap(r.x + 5, r.y + 5), cb), EventResult::Action("f:0:on".into()));
        assert_eq!(multi.event(&tap(r.x + 5, r.y + 5), cb), EventResult::Action("f:0:off".into()));
    }

    #[test]
    fn text_block_wraps_and_hyphenates() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(600, 400);
        let mut cx = PaintCx::new(&mut canvas, &mut fonts, &theme);
        let text = "Hyphenation makes justified text on narrow e-ink screens look considerably better.";
        let plain = TextBlock::new(text, 36.0);
        let lines = plain.lines(&mut cx, 300.0);
        assert!(lines.len() >= 4 && lines.iter().all(|(_, h)| !h), "{lines:?}");
        let hy = TextBlock::new(text, 36.0).hyphenate("en-US");
        let hlines = hy.lines(&mut cx, 300.0);
        assert!(hlines.iter().any(|(_, h)| *h), "{hlines:?}");
        let h = hy.height(cx.fonts, 300.0);
        assert!((h as f32 - hlines.len() as f32 * 36.0 * 1.35).abs() < 30.0, "{h}");
        let mut tb = TextBlock::new(text, 36.0).hyphenate("en").aligned(Align::Justify);
        tb.paint(&mut cx, Rect::new(10, 10, 300, 380));
        assert!(canvas.pixels.iter().filter(|v| **v < 100).count() > 2000);
    }
}
