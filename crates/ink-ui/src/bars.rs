//! Screen chrome: the top [`AppBar`] (back, title, actions) and the bottom [`NavBar`]
//! (icons with labels). Both work in a [`crate::WidgetTree`] and painted directly with
//! [`Widget::paint`] by screens that draw themselves; their hit rectangles are public so
//! such screens can hit-test without the tree.

use crate::draw::{fill_round_rect, stroke_round_rect, Path};
use crate::gesture::Gesture;
use crate::icon::Icon;
use crate::theme::space;
use crate::widget::{follow_press, press_class, EventResult, PaintCx, Widget};
use crate::UiEvent;
use ink_device::{Point, Rect, RefreshClass};
use ink_text::Style;

/// Recommended height of an [`AppBar`].
pub const APP_BAR_H: u32 = 136;
/// Recommended height of a [`NavBar`].
pub const NAV_BAR_H: u32 = 144;

/// A trailing action of an [`AppBar`]: an icon button, or a text button when `icon` is
/// `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct BarAction {
    /// Icon (icon-only button).
    pub icon: Option<Icon>,
    /// Caption of a text button (also the name for an icon button).
    pub label: String,
    /// Action reported on tap.
    pub action: String,
}

impl BarAction {
    /// An icon button.
    pub fn icon(icon: Icon, action: impl Into<String>) -> Self {
        let action = action.into();
        BarAction { icon: Some(icon), label: action.clone(), action }
    }

    /// A text button.
    pub fn text(label: impl Into<String>, action: impl Into<String>) -> Self {
        BarAction { icon: None, label: label.into(), action: action.into() }
    }
}

/// The top bar of a screen: optional back button, title (and subtitle), trailing
/// actions, a black rule along the bottom. A tap on back reports `back`.
#[derive(Clone, Debug, PartialEq)]
pub struct AppBar {
    /// Title (title role on top-level screens, heading role with a back button).
    pub title: String,
    /// Second line under the title (caption role, muted).
    pub subtitle: Option<String>,
    /// Show the back button (leading).
    pub back: bool,
    /// Trailing actions, left to right.
    pub actions: Vec<BarAction>,
    /// Draw the bottom rule.
    pub rule: bool,
    pressed: Option<usize>,
    changed: bool,
}

/// Index of the back button in [`AppBar`] hit lists.
const BACK: usize = usize::MAX;

impl AppBar {
    /// A bar with a title.
    pub fn new(title: impl Into<String>) -> Self {
        AppBar { title: title.into(), subtitle: None, back: false, actions: Vec::new(), rule: true, pressed: None, changed: false }
    }

    /// With a back button.
    pub fn with_back(mut self) -> Self {
        self.back = true;
        self
    }

    /// With a subtitle.
    pub fn with_subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    /// With a trailing action.
    pub fn action(mut self, a: BarAction) -> Self {
        self.actions.push(a);
        self
    }

    fn cell(b: Rect) -> u32 {
        b.h.saturating_sub(2 * space(2) as u32).clamp(1, 112).max(b.h.min(96))
    }

    /// Rectangle of the back button inside `b`.
    pub fn back_rect(b: Rect) -> Rect {
        let c = AppBar::cell(b);
        Rect::new(b.x + space(1), b.y + (b.h as i32 - c as i32) / 2, c, c)
    }

    /// Width of a text action (estimated from the character count, so hit tests need no
    /// fonts).
    fn text_w(label: &str) -> u32 {
        (label.chars().count() as u32 * 20 + 2 * space(4) as u32).max(160)
    }

    /// Rectangles of the trailing actions inside `b`, in `actions` order.
    pub fn action_rects(&self, b: Rect) -> Vec<Rect> {
        let c = AppBar::cell(b);
        let mut x = b.right() - space(1);
        let mut out = vec![Rect::default(); self.actions.len()];
        for (i, a) in self.actions.iter().enumerate().rev() {
            let w = if a.icon.is_some() { c } else { AppBar::text_w(&a.label).min(b.w / 3) };
            // Actions that do not fit get an empty rectangle (not drawn, never hit).
            let w = if x - (w as i32) < b.x { 0 } else { w };
            x -= w as i32;
            out[i] = Rect::new(x, b.y + (b.h as i32 - c as i32) / 2, w, c);
            x -= space(1);
        }
        out
    }

    fn hits(&self, b: Rect) -> Vec<(usize, Rect)> {
        let mut v: Vec<(usize, Rect)> = self.action_rects(b).into_iter().enumerate().collect();
        if self.back {
            v.push((BACK, AppBar::back_rect(b)));
        }
        v
    }

    fn hit(&self, b: Rect, p: Point) -> Option<usize> {
        self.hits(b).into_iter().find(|(_, r)| r.contains(p)).map(|(i, _)| i)
    }
}

/// Draws an icon button face: a black disc with a white icon when `pressed`, else just
/// the icon.
fn icon_face(cx: &mut PaintCx, r: Rect, icon: Icon, pressed: bool) {
    let (fg, bg) = (cx.theme.fg, cx.theme.bg);
    if pressed {
        let (x, y) = crate::draw::center(r);
        Path::new().circle(x, y, r.w.min(r.h) as f32 / 2.0).fill(cx.canvas, fg);
    }
    cx.icon(icon, r, r.w.min(r.h) / 2, if pressed { bg } else { fg });
}

impl Widget for AppBar {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        let mut x = b.x + space(4);
        if self.back {
            let r = AppBar::back_rect(b);
            icon_face(cx, r, Icon::Back, self.pressed == Some(BACK));
            x = r.right() + space(1);
        }
        let rects = self.action_rects(b);
        for (i, (a, r)) in self.actions.iter().zip(&rects).enumerate() {
            if r.w == 0 {
                continue;
            }
            let pressed = self.pressed == Some(i);
            match a.icon {
                Some(icon) => icon_face(cx, *r, icon, pressed),
                None => {
                    let rr = Rect::new(r.x, r.y + space(1), r.w, r.h.saturating_sub(2 * space(1) as u32));
                    let rad = rr.h as f32 / 2.0;
                    if pressed {
                        fill_round_rect(cx.canvas, rr, rad, t.fg);
                    } else {
                        stroke_round_rect(cx.canvas, rr, rad, t.border_px as f32, t.fg);
                    }
                    cx.text_centered(rr, &a.label, t.label.px, Style::Bold, if pressed { t.bg } else { t.fg });
                }
            }
        }
        let right = rects.iter().filter(|r| r.w > 0).map(|r| r.x - space(2)).min().unwrap_or(b.right() - space(4));
        let role = if self.back { t.heading } else { t.title };
        let tw = (right - x).max(0) as u32;
        let rule = if self.rule { t.rule_px } else { 0 };
        let inner = Rect::new(x, b.y, tw, (b.h as i32 - rule).max(0) as u32);
        match &self.subtitle {
            None => cx.text_left(inner, &self.title, role.px, role.style, t.fg),
            Some(sub) => {
                let top_h = (role.px * 1.25) as u32;
                let sub_h = (t.caption.px * 1.3) as u32;
                let y = inner.y + (inner.h as i32 - (top_h + sub_h) as i32) / 2;
                cx.text_left(Rect::new(x, y, tw, top_h), &self.title, role.px, role.style, t.fg);
                cx.text_left(Rect::new(x, y + top_h as i32, tw, sub_h), sub, t.caption.px, t.caption.style, t.muted);
            }
        }
        if self.rule {
            cx.canvas.fill_rect(b.x, b.bottom() - t.rule_px, b.w as i32, t.rule_px, t.fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Touch(t) => {
                let under = self.hit(b, t.pos);
                let (res, changed) = follow_press(t, &mut self.pressed, under, under.is_some());
                self.changed |= changed;
                res
            }
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                self.changed |= self.pressed.take().is_some();
                match self.hit(b, *pos) {
                    Some(BACK) => EventResult::Action("back".into()),
                    Some(i) => EventResult::Action(self.actions[i].action.clone()),
                    None => EventResult::Handled,
                }
            }
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        press_class(self.pressed.is_some())
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        // Press feedback changes only the buttons; an event that changed nothing (a
        // touch-down, a quick tap) refreshes nothing.
        Some(if std::mem::take(&mut self.changed) { self.hits(b).into_iter().map(|(_, r)| r).collect() } else { Vec::new() })
    }
}

/// One destination of a [`NavBar`].
#[derive(Clone, Debug, PartialEq)]
pub struct NavItem {
    /// Icon.
    pub icon: Icon,
    /// Caption under the icon.
    pub label: String,
    /// Action reported on tap.
    pub action: String,
}

impl NavItem {
    /// A destination.
    pub fn new(icon: Icon, label: impl Into<String>, action: impl Into<String>) -> Self {
        NavItem { icon, label: label.into(), action: action.into() }
    }
}

/// Bottom navigation: a black rule on top, equal cells with an icon and a caption. The
/// selected destination has its icon on a black pill and a bold caption. A tap reports
/// the item's action and selects it.
#[derive(Clone, Debug, PartialEq)]
pub struct NavBar {
    /// Destinations, left to right.
    pub items: Vec<NavItem>,
    /// Selected index.
    pub selected: usize,
    pressed: Option<usize>,
    changed: Vec<usize>,
}

impl NavBar {
    /// A bar with the first item selected.
    pub fn new(items: Vec<NavItem>) -> Self {
        NavBar { items, selected: 0, pressed: None, changed: Vec::new() }
    }

    /// With `i` selected.
    pub fn with_selected(mut self, i: usize) -> Self {
        self.selected = i;
        self
    }

    /// Cell rectangles inside `b`.
    pub fn cells(&self, b: Rect) -> Vec<Rect> {
        let n = self.items.len().max(1) as u32;
        let w = b.w / n;
        (0..self.items.len() as u32).map(|i| Rect::new(b.x + (i * w) as i32, b.y, if i + 1 == n { b.w - i * w } else { w }, b.h)).collect()
    }

    fn hit(&self, b: Rect, p: Point) -> Option<usize> {
        if !b.contains(p) {
            return None;
        }
        self.cells(b).iter().position(|r| r.contains(p))
    }
}

impl Widget for NavBar {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        cx.canvas.fill_rect(b.x, b.y, b.w as i32, t.rule_px, t.fg);
        let cap_h = (t.caption.px * 1.35) as i32;
        for (i, (r, item)) in self.cells(b).into_iter().zip(&self.items).enumerate() {
            let on = i == self.selected || self.pressed == Some(i);
            let top = r.y + t.rule_px + space(1);
            let avail = (r.bottom() - space(1) - cap_h - top).max(0);
            let pill_h = avail.min(72);
            let pill_w = (pill_h * 2).min(r.w as i32 - space(2));
            let pill = Rect::new(r.x + (r.w as i32 - pill_w) / 2, top + (avail - pill_h) / 2, pill_w.max(0) as u32, pill_h as u32);
            if on {
                fill_round_rect(cx.canvas, pill, pill.h as f32 / 2.0, t.fg);
            }
            let side = (pill_h as u32 * 2 / 3).max(1);
            cx.icon(item.icon, pill, side, if on { t.bg } else { t.fg });
            let cap = Rect::new(r.x + space(1), pill.bottom() + space(1) / 2, r.w.saturating_sub(space(2) as u32), cap_h as u32);
            let style = if i == self.selected { Style::Bold } else { Style::Regular };
            cx.text_centered(cap, &item.label, t.caption.px, style, t.fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Touch(t) => {
                let before = self.pressed;
                let under = self.hit(b, t.pos);
                let (res, changed) = follow_press(t, &mut self.pressed, under, under.is_some());
                if changed {
                    self.changed.extend(before.into_iter().chain(self.pressed));
                }
                res
            }
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                self.changed.extend(self.pressed.take());
                match self.hit(b, *pos) {
                    Some(i) => {
                        self.changed.push(self.selected);
                        self.changed.push(i);
                        self.selected = i;
                        EventResult::Action(self.items[i].action.clone())
                    }
                    None => EventResult::Handled,
                }
            }
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        press_class(self.pressed.is_some())
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        let mut changed = std::mem::take(&mut self.changed);
        changed.sort_unstable();
        changed.dedup();
        let cells = self.cells(b);
        Some(changed.into_iter().filter_map(|i| cells.get(i).copied()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::{Touch, TouchKind};
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

    #[test]
    fn app_bar_hits_back_and_actions() {
        let b = Rect::new(0, 0, 1272, APP_BAR_H);
        let mut bar = AppBar::new("Library").with_back().action(BarAction::icon(Icon::Search, "search")).action(BarAction::text("Done", "done"));
        let back = AppBar::back_rect(b);
        assert!(back.w >= 96 && back.h >= 96, "{back:?}");
        let rs = bar.action_rects(b);
        assert!(rs[0].right() < rs[1].x && rs[1].right() <= b.right());
        assert_eq!(bar.event(&tap(back.x + 5, back.y + 5), b), EventResult::Action("back".into()));
        assert_eq!(bar.event(&tap(rs[0].x + 5, rs[0].y + 5), b), EventResult::Action("search".into()));
        assert_eq!(bar.event(&tap(rs[1].x + 5, rs[1].y + 5), b), EventResult::Action("done".into()));
        assert_eq!(bar.event(&tap(600, 60), b), EventResult::Handled);
        // Touch-down: captured, nothing to refresh (it may become a swipe).
        assert_eq!(bar.event(&touch(TouchKind::Down, back.x + 5, back.y + 5), b), EventResult::Handled);
        assert_eq!(bar.damage(b), Some(vec![]));
        // Press feedback (after the rest time) is DU and damages only the buttons.
        assert_eq!(bar.event(&touch(TouchKind::Press, back.x + 5, back.y + 5), b), EventResult::Handled);
        assert_eq!(bar.refresh_class(), RefreshClass::Du);
        assert!(bar.damage(b).unwrap().contains(&back));
        assert_eq!(bar.event(&touch(TouchKind::Up, back.x + 5, back.y + 5), b), EventResult::Handled);
        assert_eq!(bar.refresh_class(), RefreshClass::Gl16);
    }

    #[test]
    fn nav_bar_selects_and_paints_the_pill() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1272, 400);
        let mut t = WidgetTree::new();
        let items = vec![
            NavItem::new(Icon::Home, "Home", "home"),
            NavItem::new(Icon::Library, "Library", "library"),
            NavItem::new(Icon::Chart, "Stats", "stats"),
        ];
        let b = Rect::new(0, 200, 1272, NAV_BAR_H);
        let id = t.add(None, Box::new(NavBar::new(items)), b);
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        assert_eq!(canvas.get(10, 201), Some(0), "rule on top");
        let cells = t.get_mut::<NavBar>(id).unwrap().cells(b);
        assert_eq!(cells.len(), 3);
        let c1 = cells[1];
        assert_eq!(t.dispatch(&tap(c1.x + 30, c1.y + 60)), Some((id, EventResult::Action("library".into()))));
        let dmg = t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg.len(), 2, "old and new cell: {dmg:?}");
        // The selected pill is black right of the icon.
        let pill_y = c1.y + theme.rule_px + space(1) + 36;
        let dark = (c1.x..c1.right()).filter(|x| canvas.get(*x, pill_y) == Some(0)).count();
        assert!(dark > 60, "{dark}");
        assert_eq!(t.get_mut::<NavBar>(id).unwrap().selected, 1);
    }
}
