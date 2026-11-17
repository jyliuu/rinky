//! Paged lists for e-ink (no smooth scrolling: a swipe or the footer arrows turn a page).
//!
//! - [`List`]: rows of [`ListItem`]s (title, subtitle, detail, leading icon or book
//!   cover, reading progress, check mark) with a page footer. Tapping a row reports the
//!   item's action. Rows can be indented by level and carry a [`Fold`] chevron (a tree:
//!   tapping the chevron reports the fold's action instead). [`List::with_scroll_bar`]
//!   swaps the page footer for a [`ScrollBar`] at the right: a swipe still moves a
//!   screenful, a tap or a drag on the bar jumps there (the drag moves only the bar,
//!   DU; the lift shows the rows).
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
use crate::scrollbar::ScrollBar;
use crate::theme::space;
use crate::widget::{follow_press, look_damage, press_class, press_or_tap, take_gestures, EventResult, PaintCx, Widget};
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
    /// Nesting level: the row's content is indented [`INDENT`] per level (at most 4).
    pub level: u8,
    /// Fold chevron left of the title (a tree row with children).
    pub fold: Option<Fold>,
}

/// The chevron of a tree row with children: pointing down when open, right when closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fold {
    /// The children are shown.
    pub open: bool,
    /// Action reported on a tap on the chevron.
    pub action: String,
}

/// Indent per nesting level of a [`ListItem`].
pub const INDENT: i32 = 56;
/// Width of the fold chevron's slot (a touch target), kept on every row of a list with
/// any [`Fold`] so the titles of one level line up.
pub const FOLD_W: u32 = 96;
const _: () = assert!(FOLD_W >= 96, "the fold chevron is a touch target");

impl ListItem {
    /// A row with a title.
    pub fn new(title: impl Into<String>, action: impl Into<String>) -> Self {
        ListItem { title: title.into(), subtitle: None, detail: None, icon: None, action: action.into(), checked: None, cover: None, progress: None, level: 0, fold: None }
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
        /// At nesting level `l`.
        level(l: u8) => level = l;
    }

    /// With a fold chevron (`open`) reporting `action`.
    pub fn fold(mut self, open: bool, action: impl Into<String>) -> Self {
        self.fold = Some(Fold { open, action: action.into() });
        self
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
                let mut run = cx.fonts.shape(Style::Bold, px, &l);
                if run.width > inner.w as f32 {
                    run = cx.fonts.shape(Style::Bold, (px * inner.w as f32 / run.width).floor().max(10.0), &l);
                }
                cx.put_run(&run, inner.x as f32 + (inner.w as f32 - run.width).max(0.0) / 2.0, y, t.fg, Some(inner));
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
        res.unwrap_or_else(|| take_gestures(ev))
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
    /// A scroll bar at the right instead of the page footer.
    pub scroll_bar: bool,
    pressed: Option<usize>,
    changed: Vec<usize>,
    page_changed: bool,
    /// A finger on the scroll bar: the page under it (shown on the bar only).
    bar_drag: Option<usize>,
    /// The drag preview changed: repaint the bar only.
    bar_changed: bool,
}

impl List {
    /// A list with touch-sized rows.
    pub fn new(items: Vec<ListItem>, row_h: u32) -> Self {
        List { items, row_h, page: 0, selected: None, scroll_bar: false, pressed: None, changed: Vec::new(), page_changed: false, bar_drag: None, bar_changed: false }
    }

    /// With a scroll bar instead of the page footer.
    pub fn with_scroll_bar(mut self) -> Self {
        self.scroll_bar = true;
        self
    }

    /// Rows per page in `b`.
    pub fn per_page(&self, b: Rect) -> usize {
        let h = if self.scroll_bar { b.h } else { b.h.saturating_sub(FOOTER_H) };
        (h / self.row_h.max(1)).max(1) as usize
    }

    /// Index of the first row shown on `page`. With a scroll bar the last screen ends
    /// on the last row (it may repeat rows of the screen before) instead of half empty.
    fn first_of(&self, b: Rect, page: usize) -> usize {
        let per = self.per_page(b);
        let first = page.min(self.pages(b) - 1) * per;
        if self.scroll_bar {
            first.min(self.items.len().saturating_sub(per))
        } else {
            first
        }
    }

    /// Index of the first row shown in `b`.
    pub fn first(&self, b: Rect) -> usize {
        self.first_of(b, self.page)
    }

    /// The scroll bar of the screen shown (or previewed by a drag), when there is one and
    /// not everything fits.
    fn bar(&self, b: Rect) -> Option<ScrollBar> {
        let (total, per) = (self.items.len(), self.per_page(b));
        let bar = ScrollBar::new(total, self.first_of(b, self.bar_drag.unwrap_or(self.page)), per.min(total));
        (self.scroll_bar && bar.needed()).then_some(bar)
    }

    /// Number of pages in `b`.
    pub fn pages(&self, b: Rect) -> usize {
        self.items.len().div_ceil(self.per_page(b)).max(1)
    }

    fn row_rect(&self, b: Rect, slot: usize) -> Rect {
        let w = if self.bar(b).is_some() { ScrollBar::content(b).w } else { b.w };
        Rect::new(b.x, b.y + (slot as u32 * self.row_h) as i32, w, self.row_h)
    }

    /// The page whose items are under height `y` of the bar.
    fn bar_page(&self, b: Rect, y: i32) -> usize {
        let per = self.per_page(b);
        ScrollBar::paged(self.items.len(), per, 0).item_at(ScrollBar::track(b), y) / per
    }

    /// True when `p` is on the scroll bar's touch zone.
    fn on_bar(&self, b: Rect, p: ink_device::Point) -> bool {
        self.bar(b).is_some() && ScrollBar::hit_rect(b).contains(p)
    }

    /// Left edge of a row's content after its indent.
    fn content_x(&self, r: Rect, item: &ListItem) -> i32 {
        let pad = if self.has_folds() { 16 } else { 32 };
        r.x + pad + INDENT * item.level.min(4) as i32
    }

    fn has_folds(&self) -> bool {
        self.items.iter().any(|i| i.fold.is_some())
    }

    /// The touch target of item `i`'s fold chevron in `b`, when it has one and is on the
    /// page shown.
    pub fn fold_rect(&self, b: Rect, i: usize) -> Option<Rect> {
        let item = self.items.get(i)?;
        item.fold.as_ref()?;
        let first = self.first(b);
        if i < first || i >= first + self.per_page(b) {
            return None;
        }
        let r = self.row_rect(b, i - first);
        Some(Rect::new(self.content_x(r, item), r.y, FOLD_W, r.h))
    }

    fn footer(&self, b: Rect) -> Rect {
        Rect::new(b.x, b.bottom() - FOOTER_H as i32, b.w, FOOTER_H)
    }

    /// Item index under `y`, if any.
    fn item_at(&self, b: Rect, x: i32, y: i32) -> Option<usize> {
        let p = ink_device::Point::new(x, y);
        if !b.contains(p) || (!self.scroll_bar && y >= self.footer(b).y) || self.on_bar(b, p) {
            return None;
        }
        let slot = ((y - b.y) / self.row_h as i32) as usize;
        let i = self.first(b) + slot;
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
        let start = self.first(b);
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
            let mut x = self.content_x(r, item);
            if self.has_folds() {
                if let Some(f) = &item.fold {
                    let side = (r.h as f32 * 0.36) as u32;
                    let icon = if f.open { Icon::Down } else { Icon::Forward };
                    icon.draw(cx.canvas, Rect::new(x + (FOLD_W - side) as i32 / 2, r.y + (r.h - side) as i32 / 2, side, side), tfg);
                }
                x += FOLD_W as i32;
            }
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
        if let Some(bar) = self.bar(b) {
            if self.bar_drag.is_some() {
                bar.paint_drag(cx, ScrollBar::track(b));
            } else {
                bar.paint(cx, ScrollBar::track(b));
            }
            return;
        }
        let pages = self.pages(b);
        if pages > 1 && !self.scroll_bar {
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
            // A finger on the scroll bar: the bar follows it, the rows on the lift.
            UiEvent::Touch(t) if self.bar_drag.is_some() || (t.kind == TouchKind::Down && self.on_bar(b, t.pos)) => {
                match t.kind {
                    TouchKind::Up => {
                        let page = self.bar_drag.take().unwrap_or(self.page);
                        self.page = page;
                        self.page_changed = true;
                    }
                    TouchKind::Cancel => {
                        self.bar_drag = None;
                        self.page_changed = true;
                    }
                    _ => {
                        let page = self.bar_page(b, t.pos.y);
                        if self.bar_drag != Some(page) {
                            self.bar_drag = Some(page);
                            self.bar_changed = true;
                        }
                    }
                }
                EventResult::Handled
            }
            UiEvent::Touch(t) => {
                let before = self.pressed;
                let under = self.item_at(b, t.pos.x, t.pos.y);
                let (res, changed) = follow_press(t, &mut self.pressed, under, b.contains(t.pos));
                if changed {
                    self.changed.extend(before.into_iter().chain(self.pressed));
                }
                res
            }
            // Gestures of a touch that began on the bar: its lift moved the list already.
            UiEvent::Gesture(g) if self.on_bar(b, g.pos()) => EventResult::Handled,
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                if let Some(i) = self.item_at(b, pos.x, pos.y) {
                    if let (Some(f), Some(fr)) = (&self.items[i].fold, self.fold_rect(b, i)) {
                        if fr.contains(*pos) {
                            self.changed.extend(self.pressed.take());
                            return EventResult::Action(f.action.clone());
                        }
                    }
                    self.changed.extend(self.pressed.take());
                    self.changed.extend(self.selected);
                    self.changed.push(i);
                    self.selected = Some(i);
                    return EventResult::Action(self.items[i].action.clone());
                }
                let f = self.footer(b);
                if !self.scroll_bar && f.contains(*pos) {
                    return self.turn(b, if pos.x < f.x + f.w as i32 / 2 { -1 } else { 1 });
                }
                EventResult::Handled
            }
            UiEvent::Gesture(Gesture::Swipe { dir, .. }) => match dir {
                Direction::Up | Direction::Left => self.turn(b, 1),
                Direction::Down | Direction::Right => self.turn(b, -1),
            },
            _ => take_gestures(ev),
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        press_class(self.pressed.is_some() || self.bar_drag.is_some())
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        let changed = std::mem::take(&mut self.changed);
        if std::mem::take(&mut self.page_changed) {
            self.bar_changed = false;
            return None;
        }
        if std::mem::take(&mut self.bar_changed) {
            return Some(vec![ScrollBar::track(b)]);
        }
        let (first, per) = (self.first(b), self.per_page(b));
        Some(changed.into_iter().filter(|i| (first..first + per).contains(i)).map(|i| self.row_rect(b, i - first)).collect())
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
        match target.map(|(i, r)| self.children[i].0.event(ev, r)) {
            Some(EventResult::Ignored) | None => take_gestures(ev),
            Some(res) => res,
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
    fn fold_chevron_reports_its_own_action_and_indents_by_level() {
        let items = vec![
            ListItem::new("Part", "toc:0").fold(true, "fold:0"),
            ListItem::new("Chapter", "toc:1").level(1),
            ListItem::new("Leaf", "toc:2"),
        ];
        let b = Rect::new(0, 0, 800, 300 + FOOTER_H);
        let mut l = List::new(items, 100);
        let fr = l.fold_rect(b, 0).unwrap();
        assert!(fr.w >= 96 && fr.h == 100, "{fr:?}");
        assert_eq!(l.fold_rect(b, 1), None, "no chevron without children");
        assert_eq!(l.event(&tap(fr.x + 10, 50), b), EventResult::Action("fold:0".into()));
        assert_eq!(l.selected, None, "a fold tap does not select the row");
        assert_eq!(l.event(&tap(600, 50), b), EventResult::Action("toc:0".into()));
        // The chevron is drawn in its slot: open points down; the child is indented.
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(800, 400);
        let mut t = WidgetTree::new();
        t.add(None, Box::new(l), b);
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        let ink = |r: Rect| (r.x..r.right()).flat_map(|x| (r.y..r.bottom()).map(move |y| (x, y))).filter(|(x, y)| canvas.get(*x, *y) == Some(0)).count();
        assert!(ink(fr) > 20, "chevron drawn");
        let indent = |y0: i32| (0..800).find(|x| (y0 + 20..y0 + 80).any(|y| canvas.get(*x, y) == Some(0)));
        assert!(indent(100).unwrap() >= indent(200).unwrap() + INDENT, "child indented past the leaf");
    }

    #[test]
    fn scroll_bar_replaces_the_footer_and_jumps_on_a_drag() {
        let items: Vec<ListItem> = (0..25).map(|i| ListItem::new(format!("Row {i}"), format!("r:{i}"))).collect();
        let b = Rect::new(0, 0, 800, 1000);
        let mut l = List::new(items, 100).with_scroll_bar();
        assert_eq!((l.per_page(b), l.pages(b)), (10, 3), "no footer: the rows use the whole height");
        assert_eq!(l.event(&tap(100, 950), b), EventResult::Action("r:9".into()), "the last row is a row");
        let touch = |kind, y| UiEvent::Touch(Touch { slot: 0, kind, pos: Point::new(790, y), start: Point::new(790, 10), held: Duration::ZERO, moved: kind != TouchKind::Down });
        // Down on the bar, drag to its bottom: only the bar previews (DU); the lift shows the page.
        assert_eq!(l.event(&touch(TouchKind::Down, 10), b), EventResult::Handled);
        l.event(&touch(TouchKind::Move, 990), b);
        assert_eq!(l.page, 0, "rows wait for the lift");
        assert_eq!(l.refresh_class(), RefreshClass::Du);
        assert_eq!(l.damage(b), Some(vec![ScrollBar::track(b)]));
        l.event(&touch(TouchKind::Up, 990), b);
        assert_eq!(l.page, 2);
        // The last screen ends on the last row: rows 15..25, not 20..25 and blanks.
        assert_eq!(l.event(&tap(100, 50), b), EventResult::Action("r:15".into()));
        assert_eq!(l.damage(b), None, "the whole list repaints");
        // The tap gesture of that touch does not open a row; a swipe still moves a screen.
        assert_eq!(l.event(&tap(790, 990), b), EventResult::Handled);
        l.event(&swipe(Direction::Down), b);
        assert_eq!(l.page, 1);
        // The bar is drawn in the right column; rows stop short of it.
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(800, 1000);
        let mut t = WidgetTree::new();
        t.add(None, Box::new(l), b);
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        let thumb = ScrollBar::new(25, 10, 10).thumb(ScrollBar::track(b));
        assert_eq!(canvas.get(thumb.x + thumb.w as i32 / 2, thumb.y + thumb.h as i32 / 2), Some(0));
        // Everything fits: no bar, full-width rows.
        let short = List::new(vec![ListItem::new("One", "1")], 100).with_scroll_bar();
        assert!(short.bar(b).is_none());
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
