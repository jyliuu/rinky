//! Paged lists for e-ink (no smooth scrolling: a swipe or the footer arrows turn a page).
//!
//! - [`List`]: rows of [`ListItem`]s (title, subtitle, detail, leading icon or book
//!   cover, reading progress, check mark) with a page footer. Tapping a row reports the
//!   item's action.
//! - [`Card`]: a rounded panel (outlined or filled) with an optional cover, title,
//!   subtitle, body and progress; tappable. Other widgets can be its children.
//! - [`Cover`] and [`paint_cover`]: a book cover thumbnail, or a placeholder with the
//!   title when there is no picture.
//! - [`ScrollView`]: any widgets stacked vertically with their heights; a swipe up shows
//!   the next page of children, a swipe down the previous one; a scroll bar shows where
//!   the page is. Events reach the child under the finger.

use crate::draw::{fill_round_rect, stroke_round_rect, Path};
use crate::gesture::{Direction, Gesture, TouchKind};
use crate::icon::Icon;
use crate::theme::space;
use crate::widget::{follow_press, look_damage, press_class, press_or_tap, EventResult, PaintCx, Widget};
use crate::UiEvent;
use ink_device::{Canvas, Rect, RefreshClass};
use ink_text::Style;

/// One row of a [`List`].
#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    /// Main text.
    pub title: String,
    /// Second line, smaller and gray.
    pub subtitle: Option<String>,
    /// Text on the right (size, date, count).
    pub detail: Option<String>,
    /// Icon on the left.
    pub icon: Option<Icon>,
    /// Action reported on tap.
    pub action: String,
    /// Check mark on the right (`Some(false)` reserves the space).
    pub checked: Option<bool>,
    /// Book cover on the left (instead of the icon).
    pub cover: Option<Cover>,
    /// Reading progress (0..=1): a thin bar with the percentage under the subtitle.
    pub progress: Option<f32>,
}

impl ListItem {
    /// A row with a title.
    pub fn new(title: impl Into<String>, action: impl Into<String>) -> Self {
        ListItem { title: title.into(), subtitle: None, detail: None, icon: None, action: action.into(), checked: None, cover: None, progress: None }
    }

    setters! {
        /// With a subtitle.
        subtitle(s: impl Into<String>) => subtitle = Some(s.into());
        /// With a detail text.
        detail(s: impl Into<String>) => detail = Some(s.into());
        /// With an icon.
        icon(i: Icon) => icon = Some(i);
        /// With a check mark state.
        checked(c: bool) => checked = Some(c);
        /// With a book cover on the left.
        cover(c: Cover) => cover = Some(c);
        /// With reading progress (0..=1).
        progress(p: f32) => progress = Some(p);
    }
}

/// A book cover: a grayscale picture, or (without one) a placeholder showing the title.
#[derive(Clone, Debug, PartialEq)]
pub struct Cover {
    /// Title for the placeholder.
    pub title: String,
    /// The picture (any size; scaled to fit).
    pub image: Option<std::sync::Arc<Canvas>>,
}

impl Cover {
    /// A placeholder cover with `title`.
    pub fn placeholder(title: impl Into<String>) -> Self {
        Cover { title: title.into(), image: None }
    }

    /// A cover picture.
    pub fn image(title: impl Into<String>, image: std::sync::Arc<Canvas>) -> Self {
        Cover { title: title.into(), image: Some(image) }
    }
}

/// Paints `cover` into `r`: the picture scaled to fit with a black frame, or a light
/// placeholder with a black frame and the title in bold. Returns the frame rectangle.
pub fn paint_cover(cx: &mut PaintCx, r: Rect, cover: &Cover) -> Rect {
    let t = cx.theme;
    if r.w < 8 || r.h < 8 {
        return r;
    }
    match &cover.image {
        Some(img) => {
            let inner = crate::draw::inset(r, t.border_px);
            let got = crate::draw::blit_fit(cx.canvas, img, inner);
            let f = Rect::new(got.x - t.border_px, got.y - t.border_px, got.w + 2 * t.border_px as u32, got.h + 2 * t.border_px as u32);
            stroke_round_rect(cx.canvas, f, 4.0, t.border_px as f32, t.fg);
            f
        }
        None => {
            fill_round_rect(cx.canvas, r, 8.0, t.light);
            stroke_round_rect(cx.canvas, r, 8.0, t.border_px as f32, t.fg);
            let px = (r.w as f32 / 6.0).clamp(14.0, t.caption.px);
            let inner = crate::draw::inset(r, (r.w / 10).max(4) as i32);
            let lines = crate::dialog::wrap_lines(cx, &cover.title, px, Style::Bold, inner.w as f32);
            let mut y = inner.y as f32 + px;
            for l in lines {
                if y > inner.bottom() as f32 {
                    break;
                }
                // A word wider than the cover (no break point) is set smaller to fit.
                let mut lpx = px;
                let mut w = cx.text_width(Style::Bold, lpx, &l);
                if w > inner.w as f32 {
                    lpx = (px * inner.w as f32 / w).floor().max(10.0);
                    w = cx.text_width(Style::Bold, lpx, &l);
                }
                cx.draw_text_clipped(Style::Bold, lpx, inner.x as f32 + (inner.w as f32 - w).max(0.0) / 2.0, y, &l, t.fg, inner);
                y += px * 1.2;
            }
            r
        }
    }
}

/// Draws a thin progress bar with its percentage label into `r` (list rows, cards).
fn paint_row_progress(cx: &mut PaintCx, r: Rect, value: f32) {
    let label = format!("{}%", (value.clamp(0.0, 1.0) * 100.0).round() as u32);
    let mut bar = crate::controls::ProgressBar::thin(value).with_label(label).with_label_slot("100%");
    bar.paint(cx, r);
}

/// How a [`Card`] looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardKind {
    /// White with a black outline (the default; reads in day and night mode).
    #[default]
    Outlined,
    /// Light gray fill, no outline (grouping on busy screens).
    Filled,
}

/// A rounded panel: optional cover on the left, title (heading role), subtitle (muted),
/// body text, and a thin progress bar at the bottom. A tap reports `action` (empty:
/// taps are only consumed). Children added under it in the tree paint on top.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    /// Title.
    pub title: String,
    /// Second line (muted).
    pub subtitle: Option<String>,
    /// Wrapped body text.
    pub body: Option<String>,
    /// Cover on the left.
    pub cover: Option<Cover>,
    /// Progress (0..=1) along the bottom.
    pub progress: Option<f32>,
    /// Action reported on tap.
    pub action: String,
    /// Look.
    pub kind: CardKind,
    pressed: bool,
    /// The last event changed the look (pressed state).
    changed: bool,
}

impl Card {
    /// An outlined card with a title.
    pub fn new(title: impl Into<String>, action: impl Into<String>) -> Self {
        Card { title: title.into(), subtitle: None, body: None, cover: None, progress: None, action: action.into(), kind: CardKind::Outlined, pressed: false, changed: false }
    }

    setters! {
        /// With a subtitle.
        subtitle(s: impl Into<String>) => subtitle = Some(s.into());
        /// With body text.
        body(s: impl Into<String>) => body = Some(s.into());
        /// With a cover.
        cover(c: Cover) => cover = Some(c);
        /// With progress.
        progress(p: f32) => progress = Some(p);
        /// With a look.
        kind(k: CardKind) => kind = k;
    }

    /// The cover rectangle inside `b` (2:3, the card's height minus padding).
    pub fn cover_rect(b: Rect) -> Rect {
        let pad = space(3);
        let h = b.h.saturating_sub(2 * pad as u32);
        let w = (h * 2 / 3).min(b.w / 3);
        Rect::new(b.x + pad, b.y + pad, w, h)
    }
}

impl Widget for Card {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        let rad = t.radius_lg;
        match (self.kind, self.pressed) {
            (_, true) => fill_round_rect(cx.canvas, b, rad, t.fg),
            (CardKind::Outlined, false) => stroke_round_rect(cx.canvas, b, rad, t.border_px as f32, t.fg),
            (CardKind::Filled, false) => fill_round_rect(cx.canvas, b, rad, t.light),
        }
        if self.pressed {
            // DU feedback: black and white only; the content comes back on release.
            return;
        }
        let pad = space(3);
        let mut x = b.x + pad;
        if let Some(c) = &self.cover {
            let cr = Card::cover_rect(b);
            paint_cover(cx, cr, c);
            x = cr.right() + pad;
        }
        let w = (b.right() - pad - x).max(0) as u32;
        let mut y = b.y + pad;
        let th = (t.heading.px * 1.3) as u32;
        cx.text_left(Rect::new(x, y, w, th), &self.title, t.heading.px, t.heading.style, t.fg);
        y += th as i32;
        if let Some(sub) = &self.subtitle {
            let sh = (t.caption.px * 1.5) as u32;
            cx.text_left(Rect::new(x, y, w, sh), sub, t.caption.px * 1.1, t.caption.style, t.muted);
            y += sh as i32;
        }
        let bar_h = if self.progress.is_some() { 56 } else { 0 };
        if let Some(body) = &self.body {
            let px = t.caption.px * 1.1;
            let clip = Rect::new(x, y, w, (b.bottom() - pad - bar_h - y).max(0) as u32);
            let mut baseline = y as f32 + px * 1.2;
            for l in crate::dialog::wrap_lines(cx, body, px, Style::Regular, w as f32) {
                if baseline > clip.bottom() as f32 {
                    break;
                }
                cx.draw_text_clipped(Style::Regular, px, x as f32, baseline, &l, t.fg, clip);
                baseline += px * 1.35;
            }
        }
        if let Some(p) = self.progress {
            paint_row_progress(cx, Rect::new(x - 4, b.bottom() - pad - bar_h, w + 4, bar_h as u32), p);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        // Without an action taps are only consumed.
        let res = if self.action.is_empty() { None } else { press_or_tap(ev, b, &mut self.pressed, &mut self.changed, &self.action) };
        match res {
            Some(res) => res,
            None if matches!(ev, UiEvent::Gesture(_)) => EventResult::Handled,
            None => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        press_class(self.pressed)
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        look_damage(&mut self.changed)
    }
}

/// Height of the page footer of a [`List`] (its arrows are touch targets: at least 96).
pub const FOOTER_H: u32 = 96;
const _: () = assert!(FOOTER_H >= 96, "the footer arrows are touch targets");

/// A paged list.
#[derive(Clone, Debug, PartialEq)]
pub struct List {
    /// Rows.
    pub items: Vec<ListItem>,
    /// Row height.
    pub row_h: u32,
    /// Current page.
    pub page: usize,
    /// Highlighted row (the last tapped).
    pub selected: Option<usize>,
    pressed: Option<usize>,
    changed: Vec<usize>,
    page_changed: bool,
}

impl List {
    /// A list with touch-sized rows.
    pub fn new(items: Vec<ListItem>, row_h: u32) -> Self {
        List { items, row_h, page: 0, selected: None, pressed: None, changed: Vec::new(), page_changed: false }
    }

    /// Rows per page in `b`.
    pub fn per_page(&self, b: Rect) -> usize {
        (b.h.saturating_sub(FOOTER_H) / self.row_h.max(1)).max(1) as usize
    }

    /// Number of pages in `b`.
    pub fn pages(&self, b: Rect) -> usize {
        self.items.len().div_ceil(self.per_page(b)).max(1)
    }

    fn row_rect(&self, b: Rect, slot: usize) -> Rect {
        Rect::new(b.x, b.y + (slot as u32 * self.row_h) as i32, b.w, self.row_h)
    }

    fn footer(&self, b: Rect) -> Rect {
        Rect::new(b.x, b.bottom() - FOOTER_H as i32, b.w, FOOTER_H)
    }

    /// Item index under `y`, if any.
    fn item_at(&self, b: Rect, x: i32, y: i32) -> Option<usize> {
        if !b.contains(ink_device::Point::new(x, y)) || y >= self.footer(b).y {
            return None;
        }
        let slot = ((y - b.y) / self.row_h as i32) as usize;
        let i = self.page * self.per_page(b) + slot;
        (slot < self.per_page(b) && i < self.items.len()).then_some(i)
    }

    fn turn(&mut self, b: Rect, d: i32) -> EventResult {
        let p = (self.page as i32 + d).clamp(0, self.pages(b) as i32 - 1) as usize;
        if p != self.page {
            self.page = p;
            self.page_changed = true;
        }
        EventResult::Handled
    }
}

impl Widget for List {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let (fg, bg, muted) = (cx.theme.fg, cx.theme.bg, cx.theme.muted);
        let (px, small) = (cx.theme.font_px, cx.theme.small_px);
        let per = self.per_page(b);
        let start = self.page * per;
        for (slot, i) in (start..(start + per).min(self.items.len())).enumerate() {
            let r = self.row_rect(b, slot);
            let item = &self.items[i];
            let pressed = self.pressed == Some(i);
            let (tfg, tbg) = if pressed { (bg, fg) } else { (fg, bg) };
            if pressed {
                cx.canvas.fill_rect(r.x, r.y, r.w as i32, r.h as i32, tbg);
            } else if self.selected == Some(i) {
                cx.canvas.fill_rect(r.x, r.y, 10, r.h as i32, fg);
            }
            if slot > 0 && !pressed {
                // Light dividers, rule_px thick (thin gray lines ghost).
                let rule = cx.theme.rule_px;
                cx.canvas.fill_rect(r.x + 24, r.y - rule / 2, r.w as i32 - 48, rule, cx.theme.light);
            }
            let mut x = r.x + 32;
            if let (Some(cover), false) = (&item.cover, pressed) {
                let h = r.h.saturating_sub(24);
                let cr = Rect::new(x, r.y + 12, h * 2 / 3, h);
                paint_cover(cx, cr, cover);
                x += cr.w as i32 + 28;
            } else if item.cover.is_some() {
                x += (r.h.saturating_sub(24) * 2 / 3) as i32 + 28;
            } else if let Some(icon) = item.icon {
                let side = (r.h as f32 * 0.45) as u32;
                icon.draw(cx.canvas, Rect::new(x, r.y + (r.h - side) as i32 / 2, side, side), tfg);
                x += side as i32 + 28;
            }
            let mut right = r.right() - 32;
            if let Some(c) = item.checked {
                let side = (r.h as f32 * 0.4) as u32;
                let cr = Rect::new(right - side as i32, r.y + (r.h - side) as i32 / 2, side, side);
                if c {
                    Icon::Check.draw(cx.canvas, cr, tfg);
                }
                right = cr.x - 20;
            }
            if let Some(d) = &item.detail {
                let baseline = cx.baseline_in(r, small, Style::Regular);
                let w = cx.draw_text_right(Style::Regular, small, right as f32, baseline, d, if pressed { tfg } else { muted }, r);
                right -= w as i32 + 24;
            }
            let tw = (right - x).max(0) as u32;
            if let (Some(p), false) = (item.progress, pressed) {
                // Title, subtitle, then the thin bar along the bottom of the row.
                let third = r.h / 3;
                cx.text_left(Rect::new(x, r.y + 6, tw, third), &item.title, px, Style::Regular, tfg);
                if let Some(sub) = &item.subtitle {
                    cx.text_left(Rect::new(x, r.y + third as i32, tw, third), sub, small, Style::Italic, muted);
                }
                paint_row_progress(cx, Rect::new(x - 4, r.y + 2 * third as i32, tw.min(720), third.saturating_sub(6)), p);
                continue;
            }
            match &item.subtitle {
                Some(sub) => {
                    let top = Rect::new(x, r.y + 8, tw, r.h / 2);
                    let bottom = Rect::new(x, r.y + r.h as i32 / 2, tw, r.h / 2 - 8);
                    cx.text_left(top, &item.title, px, Style::Regular, tfg);
                    cx.text_left(bottom, sub, small, Style::Italic, if pressed { tfg } else { muted });
                }
                None => cx.text_left(Rect::new(x, r.y, tw, r.h), &item.title, px, Style::Regular, tfg),
            }
        }
        let pages = self.pages(b);
        if pages > 1 {
            let f = self.footer(b);
            cx.canvas.fill_rect(f.x, f.y, f.w as i32, cx.theme.rule_px, fg);
            let side = FOOTER_H / 2;
            let left = Rect::new(f.x + 24, f.y + (f.h - side) as i32 / 2, side, side);
            let right = Rect::new(f.right() - 24 - side as i32, left.y, side, side);
            Icon::Back.draw(cx.canvas, left, if self.page > 0 { fg } else { cx.theme.subtle });
            Icon::Forward.draw(cx.canvas, right, if self.page + 1 < pages { fg } else { cx.theme.subtle });
            let label = format!("{} / {}", self.page + 1, pages);
            cx.text_centered(f, &label, small, Style::Regular, fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Touch(t) => {
                let before = self.pressed;
                let under = self.item_at(b, t.pos.x, t.pos.y);
                let (res, changed) = follow_press(t, &mut self.pressed, under, b.contains(t.pos));
                if changed {
                    self.changed.extend(before.into_iter().chain(self.pressed));
                }
                res
            }
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                if let Some(i) = self.item_at(b, pos.x, pos.y) {
                    self.changed.extend(self.pressed.take());
                    self.changed.extend(self.selected);
                    self.changed.push(i);
                    self.selected = Some(i);
                    return EventResult::Action(self.items[i].action.clone());
                }
                let f = self.footer(b);
                if f.contains(*pos) {
                    return self.turn(b, if pos.x < f.x + f.w as i32 / 2 { -1 } else { 1 });
                }
                EventResult::Handled
            }
            UiEvent::Gesture(Gesture::Swipe { dir, .. }) => match dir {
                Direction::Up | Direction::Left => self.turn(b, 1),
                Direction::Down | Direction::Right => self.turn(b, -1),
            },
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        press_class(self.pressed.is_some())
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        let changed = std::mem::take(&mut self.changed);
        if std::mem::take(&mut self.page_changed) {
            return None;
        }
        let per = self.per_page(b);
        Some(
            changed
                .into_iter()
                .filter(|i| *i / per == self.page)
                .map(|i| self.row_rect(b, i % per))
                .collect(),
        )
    }
}

/// Children stacked vertically and shown a page at a time.
pub struct ScrollView {
    children: Vec<(Box<dyn Widget>, u32)>,
    /// Index of the first child shown.
    pub first: usize,
    /// Space between children.
    pub gap: u32,
    layout: Vec<(usize, Rect)>,
    touch_child: Option<usize>,
    /// The page changed since the last paint (everything is new).
    scrolled: bool,
}

/// Width of the scroll bar ([`crate::scrollbar::ScrollBar`]).
pub use crate::scrollbar::SCROLLBAR_W;

impl Default for ScrollView {
    fn default() -> Self {
        Self::new()
    }
}

impl ScrollView {
    /// An empty view.
    pub fn new() -> Self {
        ScrollView { children: Vec::new(), first: 0, gap: 12, layout: Vec::new(), touch_child: None, scrolled: false }
    }

    /// Appends a child `height` pixels tall.
    pub fn push(&mut self, child: Box<dyn Widget>, height: u32) {
        self.children.push((child, height));
    }

    /// Number of children.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// True without children.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// The concrete child at `i`.
    pub fn child_mut<T: Widget + 'static>(&mut self, i: usize) -> Option<&mut T> {
        self.children.get_mut(i)?.0.as_mut().as_any_mut().downcast_mut::<T>()
    }

    /// Children from `first` that fit in `b` (at least one). A first child taller than
    /// the view gets the view's height: children are painted and hit-tested only inside
    /// the view (the tree clears and refreshes only the view's bounds).
    fn page_from(&self, b: Rect, first: usize) -> Vec<(usize, Rect)> {
        let mut out = Vec::new();
        let mut y = b.y;
        let w = b.w.saturating_sub(SCROLLBAR_W + 12);
        for (i, (_, h)) in self.children.iter().enumerate().skip(first) {
            if !out.is_empty() && y + *h as i32 > b.bottom() {
                break;
            }
            out.push((i, Rect::new(b.x, y, w, (*h).min((b.bottom() - y).max(0) as u32))));
            y += (*h + self.gap) as i32;
        }
        out
    }

    /// Index of the first child of the page before the one starting at `first`.
    fn prev_start(&self, b: Rect, first: usize) -> usize {
        let mut start = first;
        let mut used = 0u32;
        while start > 0 {
            let h = self.children[start - 1].1 + self.gap;
            if used > 0 && used + h > b.h + self.gap {
                break;
            }
            used += h;
            start -= 1;
        }
        start
    }

    /// Shows the page after the current one; false at the end.
    pub fn next_page(&mut self, b: Rect) -> bool {
        let page = self.page_from(b, self.first);
        match page.last() {
            Some((last, _)) if last + 1 < self.children.len() => {
                self.first = last + 1;
                true
            }
            _ => false,
        }
    }

    /// Shows the page before the current one; false at the start.
    pub fn prev_page(&mut self, b: Rect) -> bool {
        if self.first == 0 {
            return false;
        }
        self.first = self.prev_start(b, self.first);
        true
    }

    fn child_at(&self, p: ink_device::Point) -> Option<(usize, Rect)> {
        self.layout.iter().copied().find(|(_, r)| r.contains(p))
    }
}

impl Widget for ScrollView {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        self.layout = self.page_from(b, self.first);
        for &(i, r) in &self.layout {
            cx.canvas.fill_rect(r.x, r.y, r.w as i32, r.h as i32, cx.theme.bg);
            // The child's text and drawing stay inside its slot.
            cx.paint_inside(self.children[i].0.as_mut(), r);
        }
        // Scroll bar: track plus a thumb for the visible part.
        let total: u32 = self.children.iter().map(|(_, h)| h + self.gap).sum::<u32>().max(1);
        let before: u32 = self.children.iter().take(self.first).map(|(_, h)| h + self.gap).sum();
        let shown: u32 = self.layout.iter().map(|(i, _)| self.children[*i].1 + self.gap).sum();
        if shown < total {
            let track = Rect::new(b.right() - SCROLLBAR_W as i32, b.y, SCROLLBAR_W, b.h);
            crate::scrollbar::ScrollBar::new(total as usize, before as usize, shown as usize).paint(cx, track);
            // A small arrow under the content when more follows.
            if self.layout.last().is_some_and(|(i, _)| i + 1 < self.children.len()) {
                let (cx_, cy) = (b.x as f32 + b.w as f32 / 2.0, b.bottom() as f32 - 10.0);
                Path::new().polygon(&[(cx_ - 14.0, cy - 10.0), (cx_ + 14.0, cy - 10.0), (cx_, cy + 4.0)]).fill(cx.canvas, cx.theme.muted);
            }
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Up, .. }) => {
                self.scrolled |= self.next_page(b);
                return EventResult::Handled;
            }
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Down, .. }) => {
                self.scrolled |= self.prev_page(b);
                return EventResult::Handled;
            }
            _ => {}
        }
        let target = match ev {
            UiEvent::Touch(t) if t.kind != TouchKind::Down => self.touch_child.and_then(|c| self.layout.iter().copied().find(|(i, _)| *i == c)),
            UiEvent::Touch(t) => self.child_at(t.pos),
            UiEvent::Gesture(g) => self.child_at(g.pos()),
            _ => None,
        };
        if let UiEvent::Touch(t) = ev {
            self.touch_child = match t.kind {
                TouchKind::Down => target.map(|(i, _)| i),
                TouchKind::Up | TouchKind::Cancel => None,
                _ => self.touch_child,
            };
        }
        match target {
            Some((i, r)) => match self.children[i].0.event(ev, r) {
                EventResult::Ignored if matches!(ev, UiEvent::Gesture(_)) => EventResult::Handled,
                res => res,
            },
            None if matches!(ev, UiEvent::Gesture(_)) => EventResult::Handled,
            None => EventResult::Ignored,
        }
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        // After a page change everything is new; otherwise only what the children
        // report (nothing for a touch-down).
        let scrolled = std::mem::take(&mut self.scrolled);
        let mut out = Vec::new();
        for &(i, r) in &self.layout {
            match self.children[i].0.damage(r) {
                Some(rects) => out.extend(rects),
                None => out.push(r),
            }
        }
        (!scrolled).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::Touch;
    use crate::theme::Theme;
    use crate::widget::{Button, WidgetTree};
    use ink_device::{Canvas, Point};
    use ink_text::Fonts;
    use std::time::Duration;

    fn tap(x: i32, y: i32) -> UiEvent {
        UiEvent::Gesture(Gesture::Tap { pos: Point::new(x, y) })
    }

    fn swipe(dir: Direction) -> UiEvent {
        UiEvent::Gesture(Gesture::Swipe { from: Point::new(300, 500), to: Point::new(300, 100), dir })
    }

    #[test]
    fn covers_cards_and_row_progress() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1272, 900);
        // A picture cover keeps its aspect inside the frame.
        let mut pic = Canvas::new(60, 90);
        pic.fill_rect(0, 0, 60, 90, 0x44);
        let mut cx = PaintCx::new(&mut canvas, &mut fonts, &theme);
        let f = paint_cover(&mut cx, Rect::new(10, 10, 200, 200), &Cover::image("x", std::sync::Arc::new(pic)));
        assert!(f.h > f.w && f.h <= 200 && f.w < 150, "{f:?}");
        assert_eq!(canvas.get(f.x + f.w as i32 / 2, f.y + f.h as i32 / 2), Some(0x44));
        assert_eq!(canvas.get(f.x + 1, f.y + 20), Some(0), "black frame");
        // A card with a placeholder cover and progress; a tap reports its action.
        let mut tree = WidgetTree::new();
        let b = Rect::new(0, 300, 1272, 360);
        let card = Card::new("Romeo and Juliet", "open").subtitle("William Shakespeare").cover(Cover::placeholder("Romeo and Juliet")).progress(0.42);
        let id = tree.add(None, Box::new(card), b);
        tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        let cr = Card::cover_rect(b);
        assert_eq!(canvas.get(cr.x + cr.w as i32 / 2, cr.bottom() - 10), Some(theme.light), "placeholder fill");
        assert_eq!(canvas.get(b.x + 1, b.y + b.h as i32 / 2), Some(0), "outline");
        let tap = UiEvent::Gesture(Gesture::Tap { pos: Point::new(800, 400) });
        assert_eq!(tree.dispatch(&tap), Some((id, EventResult::Action("open".into()))));
        // Rows with covers and progress paint a thin bar in the row.
        let items = vec![ListItem::new("Book", "b").subtitle("Author").cover(Cover::placeholder("Book")).progress(0.5)];
        let mut c2 = Canvas::new(1272, 400);
        let mut t2 = WidgetTree::new();
        t2.add(None, Box::new(List::new(items, 180)), Rect::new(0, 0, 1272, 400));
        t2.paint(&mut PaintCx::new(&mut c2, &mut fonts, &theme));
        let bar_row = (120..180).find(|y| (200..400).filter(|x| c2.get(*x, *y) == Some(0)).count() > 150);
        assert!(bar_row.is_some(), "progress bar in the row's last third");
    }

    #[test]
    fn list_pages_and_taps() {
        let items: Vec<ListItem> = (0..25).map(|i| ListItem::new(format!("Book {i}"), format!("open:{i}")).detail("2 MB")).collect();
        let b = Rect::new(0, 0, 800, 1000 + FOOTER_H); // 10 rows of 100 + footer
        let mut l = List::new(items, 100);
        assert_eq!((l.per_page(b), l.pages(b)), (10, 3));
        assert_eq!(l.event(&tap(100, 250), b), EventResult::Action("open:2".into()));
        assert_eq!(l.event(&swipe(Direction::Up), b), EventResult::Handled);
        assert_eq!(l.page, 1);
        assert_eq!(l.event(&tap(100, 50), b), EventResult::Action("open:10".into()));
        // Footer arrows.
        l.event(&tap(700, 1050), b);
        assert_eq!(l.page, 2);
        l.event(&tap(700, 1050), b);
        assert_eq!(l.page, 2, "last page");
        assert_eq!(l.event(&tap(100, 650), b), EventResult::Handled, "no row 26");
        l.event(&swipe(Direction::Down), b);
        assert_eq!(l.page, 1);
    }

    #[test]
    fn list_press_feedback_refreshes_one_row() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(800, 1200);
        let mut t = WidgetTree::new();
        let items = vec![
            ListItem::new("One", "1").subtitle("first").icon(Icon::Bookmark).checked(true),
            ListItem::new("Two", "2"),
        ];
        let id = t.add(None, Box::new(List::new(items, 120)), Rect::new(0, 0, 800, 1000));
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        let down = UiEvent::Touch(Touch { slot: 0, kind: TouchKind::Down, pos: Point::new(300, 150), start: Point::new(300, 150), held: Duration::ZERO, moved: false });
        assert_eq!(t.dispatch(&down), Some((id, EventResult::Handled)));
        // Touch-down shows nothing: the touch may still become a swipe.
        assert_eq!(t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme)), vec![]);
        assert_eq!(canvas.get(700, 130), Some(255), "row 2 not inverted on touch-down");
        let press = UiEvent::Touch(Touch { kind: TouchKind::Press, held: Duration::from_millis(150), ..match down {
            UiEvent::Touch(x) => x,
            _ => unreachable!(),
        } });
        assert_eq!(t.dispatch(&press), Some((id, EventResult::Handled)));
        let dmg = t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg, vec![crate::Damage { rect: Rect::new(0, 120, 800, 120), class: RefreshClass::Du }]);
        assert_eq!(canvas.get(700, 130), Some(0), "row 2 inverted");
    }

    #[test]
    fn scroll_view_pages_children_and_forwards_taps() {
        let mut v = ScrollView::new();
        for i in 0..10 {
            v.push(Box::new(Button::new(format!("B{i}"), format!("b{i}"))), 100);
        }
        let b = Rect::new(0, 0, 600, 450); // 4 children per page (100 + 12 gap)
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(600, 450);
        v.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), b);
        assert_eq!(v.event(&tap(100, 50), b), EventResult::Action("b0".into()));
        assert_eq!(v.event(&tap(100, 120 + 50), b), EventResult::Action("b1".into()));
        v.event(&swipe(Direction::Up), b);
        assert_eq!(v.first, 4);
        v.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), b);
        assert_eq!(v.event(&tap(100, 50), b), EventResult::Action("b4".into()));
        v.event(&swipe(Direction::Down), b);
        assert_eq!(v.first, 0);
        assert!(!v.prev_page(b));
        while v.next_page(b) {}
        assert_eq!(v.first, 8);
        assert_eq!(v.len(), 10);
        assert!(v.child_mut::<Button>(3).is_some());
        // The scroll bar is drawn on the right.
        assert!(canvas.pixels.chunks(600).any(|row| row[595] < 64));
    }
}
