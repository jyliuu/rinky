//! Retained widget tree.
//!
//! A [`WidgetTree`] owns widgets with absolute logical bounds, parent/child links and a
//! dirty flag carrying a [`RefreshClass`]. [`WidgetTree::paint`] repaints dirty widgets
//! (and their children) into the canvas and returns the [`Damage`] to show;
//! [`WidgetTree::dispatch`] hit-tests a gesture from the topmost widget down.
//!
//! - Touch capture: the widget that takes a [`TouchKind::Down`] receives that contact's
//!   `Move`, `Hold` and `Up` (or `Cancel`) too, wherever the finger goes.
//! - Timed layers ([`WidgetTree::add_timed`]): toasts remove themselves; screens return
//!   [`WidgetTree::next_expiry`] from their `timeout` and call [`WidgetTree::expire`].
//! - Partial damage: a widget changed by an event may report only the rectangles that
//!   changed ([`Widget::damage`]), so a key press refreshes one key, not the keyboard.
//!
//! Layout is explicit: whoever adds a widget gives its bounds (helpers [`split_rows`],
//! [`split_cols`]).

use crate::gesture::{Gesture, Touch, TouchKind};
use crate::icon::Icon;
use crate::refresh::Damage;
use crate::theme::Theme;
use crate::UiEvent;
use ink_device::{Canvas, Point, Rect, RefreshClass};
use ink_text::{Fonts, Style};
use std::any::Any;
use std::time::{Duration, Instant};

/// Index of a widget in its tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WidgetId(pub usize);

/// What a widget did with an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventResult {
    /// Not interested; try the widget below.
    Ignored,
    /// Consumed, nothing to report.
    Handled,
    /// Consumed; the screen should act on this action name (e.g. `"ok"`, `"key:a"`).
    Action(String),
}

/// Everything painting needs.
pub struct PaintCx<'a> {
    /// Target (logical screen size).
    pub canvas: &'a mut Canvas,
    /// Fonts.
    pub fonts: &'a mut Fonts,
    /// Colors and sizes.
    pub theme: &'a Theme,
}

thread_local! {
    /// The text clip of the widget being painted on this thread (see [`PaintCx::clip`]).
    static CLIP: std::cell::Cell<Option<Rect>> = const { std::cell::Cell::new(None) };
}

/// Restores the previous text clip when dropped (also when a paint panics).
#[must_use = "the clip is restored when the guard drops"]
pub struct ClipGuard {
    prev: Option<Rect>,
}

impl Drop for ClipGuard {
    fn drop(&mut self) {
        CLIP.with(|c| c.set(self.prev));
    }
}

impl<'a> PaintCx<'a> {
    /// A paint context.
    pub fn new(canvas: &'a mut Canvas, fonts: &'a mut Fonts, theme: &'a Theme) -> Self {
        PaintCx { canvas, fonts, theme }
    }

    /// The rectangle that text drawn through this context stays in: the bounds of the
    /// widget being painted. [`WidgetTree::paint`] sets it for every widget (the tree
    /// clears and refreshes only those bounds, so ink outside them would stay on the
    /// panel as garbage). `None` outside a tree paint: no limit.
    pub fn clip(&self) -> Option<Rect> {
        CLIP.with(|c| c.get())
    }

    /// Limits text drawing to `r` (inside the current clip) until the guard is dropped.
    /// Containers use it for children they paint themselves ([`crate::ScrollView`]).
    pub fn push_clip(&self, r: Rect) -> ClipGuard {
        let prev = self.clip();
        let new = match prev {
            Some(p) => r.intersect(&p),
            None => r,
        };
        CLIP.with(|c| c.set(Some(new)));
        ClipGuard { prev }
    }

    /// Paints `widget` into `r`, keeping everything it draws (text, icons, shapes) inside
    /// `r` and the clips already set: the tree clears and refreshes only `r`.
    pub(crate) fn paint_inside(&mut self, widget: &mut dyn Widget, r: Rect) {
        let _clip = self.push_clip(r);
        let outer = self.canvas.clip();
        self.canvas.set_clip(Some(outer.map_or(r, |o| o.intersect(&r))));
        widget.paint(self, r);
        self.canvas.set_clip(outer);
    }

    /// Draws a shaped run inside the current clip (and inside `extra` when given).
    pub(crate) fn put_run(&mut self, run: &ink_text::TextRun, x: f32, baseline: f32, value: u8, extra: Option<Rect>) -> f32 {
        let clip = match (self.clip(), extra) {
            (Some(a), Some(b)) => Some(a.intersect(&b)),
            (a, b) => a.or(b),
        };
        match clip {
            Some(c) => self.fonts.draw_run_clipped(self.canvas, run, x, baseline, value, c),
            None => self.fonts.draw_run_value(self.canvas, run, x, baseline, value),
        }
    }

    /// Shaped width of `text`.
    pub fn text_width(&mut self, style: Style, px: f32, text: &str) -> f32 {
        self.fonts.shaped_width(style, px, text)
    }

    /// Draws `text` centred in `r` at `px` (shaped: kerning, ligatures, fallback faces),
    /// cut with "…" when wider than `r`; inside the [`PaintCx::clip`].
    pub fn text_centered(&mut self, r: Rect, text: &str, px: f32, style: Style, value: u8) {
        let run = self.fonts.shape(style, px, text);
        let run = self.fit(run, text, style, r.w as f32);
        self.put_centered(r, &run, style, value);
    }

    /// Draws `run` (in `style`) centred in `r`, inside the [`PaintCx::clip`].
    fn put_centered(&mut self, r: Rect, run: &ink_text::TextRun, style: Style, value: u8) {
        let x = r.x as f32 + (r.w as f32 - run.width) / 2.0;
        let baseline = self.baseline_in(r, run.px, style);
        self.put_run(run, x, baseline, value, None);
    }

    /// `run` (`text` shaped in `style`), or the shaped [`PaintCx::ellipsize`]d text when
    /// `run` is wider than `max`.
    fn fit(&mut self, run: ink_text::TextRun, text: &str, style: Style, max: f32) -> ink_text::TextRun {
        if run.width <= max {
            return run;
        }
        self.cut_to_fit(text, &run, style, max).1
    }

    /// Like [`PaintCx::draw_text_end`], but also nothing is drawn outside `clip` (for
    /// parts of a widget, e.g. a dialog's body above its buttons).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_clipped(&mut self, style: Style, px: f32, x: f32, baseline: f32, text: &str, value: u8, clip: Rect) -> f32 {
        let run = self.fonts.shape(style, px, text);
        self.put_run(&run, x, baseline, value, Some(clip))
    }

    /// Like [`PaintCx::draw_text_clipped`], but `text` ends at `right` (right-aligned);
    /// returns its width.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_right(&mut self, style: Style, px: f32, right: f32, baseline: f32, text: &str, value: u8, clip: Rect) -> f32 {
        let run = self.fonts.shape(style, px, text);
        self.put_run(&run, right - run.width, baseline, value, Some(clip));
        run.width
    }

    /// Draws `text` with its left edge at `x` and baseline at `baseline` in gray `value`
    /// (shaped: kerning, ligatures, fallback faces).
    pub fn draw_text(&mut self, style: Style, px: f32, x: f32, baseline: f32, text: &str, value: u8) {
        self.draw_text_end(style, px, x, baseline, text, value);
    }

    /// Like [`PaintCx::draw_text`]; returns the pen position after the text. Inside the
    /// [`PaintCx::clip`].
    pub fn draw_text_end(&mut self, style: Style, px: f32, x: f32, baseline: f32, text: &str, value: u8) -> f32 {
        let run = self.fonts.shape(style, px, text);
        self.put_run(&run, x, baseline, value, None)
    }

    /// Baseline that centres one line of `px` text vertically in `r`.
    pub fn baseline_in(&self, r: Rect, px: f32, style: Style) -> f32 {
        let (asc, desc) = self.fonts.vmetrics(style, px);
        r.y as f32 + (r.h as f32 - (asc - desc)) / 2.0 + asc
    }

    /// Draws `text` left-aligned in `r` (vertically centred), cut with "…" when too wide;
    /// inside the [`PaintCx::clip`].
    pub fn text_left(&mut self, r: Rect, text: &str, px: f32, style: Style, value: u8) {
        let run = self.fonts.shape(style, px, text);
        self.put_left(r, run, text, style, value);
    }

    /// [`PaintCx::text_left`] of `run`, `text` already shaped in `style`.
    pub(crate) fn put_left(&mut self, r: Rect, run: ink_text::TextRun, text: &str, style: Style, value: u8) {
        let baseline = self.baseline_in(r, run.px, style);
        let run = self.fit(run, text, style, r.w as f32);
        self.put_run(&run, r.x as f32, baseline, value, None);
    }

    /// `text` shortened with "…" to fit `max` pixels, cut at a grapheme boundary.
    ///
    /// The cut is found by galloping then bisecting over grapheme boundaries from where
    /// the shaped `text` says it is, so it usually costs two shapings of prefixes about as
    /// long as the result (a 2000-char text cut to one line does not shape 2000 ever
    /// shorter copies).
    pub fn ellipsize(&mut self, text: &str, px: f32, style: Style, max: f32) -> String {
        let run = self.fonts.shape(style, px, text);
        if run.width <= max {
            return text.to_string();
        }
        self.cut_to_fit(text, &run, style, max).0
    }

    /// [`PaintCx::ellipsize`] of a `text` (shaped: `run`) known to be wider than `max`,
    /// with its shaped run.
    fn cut_to_fit(&mut self, text: &str, run: &ink_text::TextRun, style: Style, max: f32) -> (String, ink_text::TextRun) {
        use unicode_segmentation::UnicodeSegmentation;
        // Candidate cuts: ends of the proper prefixes, shortest first.
        let ends: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).filter(|i| *i > 0).collect();
        // The last candidate whose glyphs end left of `max` less about an ellipsis.
        let room = max - run.px * 0.75;
        let over = run.glyphs.iter().find(|g| g.x + g.advance > room).map_or(text.len(), |g| g.cluster);
        let guess = ends.partition_point(|e| *e <= over).saturating_sub(1);
        let cand = |k: usize| format!("{}…", text[..ends[k]].trim_end());
        // The candidate that fits last, shaped (see below: it is the cut).
        let mut fit = None;
        let mut fits = |k: usize| {
            let c = cand(k);
            let r = self.fonts.shape(style, run.px, &c);
            let ok = r.width <= max;
            if ok {
                fit = Some((c, r));
            }
            ok
        };
        // Widths grow with the prefix: find the last k that fits. `lo` fits, `hi` does not
        // (or is past the end); `lo` only moves to a candidate that fits.
        let n = ends.len();
        let (mut lo, mut hi, mut step) = (guess, n, 1usize);
        if n == 0 {
            return ("…".into(), self.fonts.shape(style, run.px, "…"));
        } else if fits(guess) {
            while lo + step < n {
                if fits(lo + step) {
                    lo += step;
                    step *= 2;
                } else {
                    hi = lo + step;
                    break;
                }
            }
        } else {
            hi = guess;
            lo = loop {
                let k = hi.saturating_sub(step);
                if fits(k) {
                    break k;
                } else if k == 0 {
                    return ("…".into(), self.fonts.shape(style, run.px, "…"));
                }
                (hi, step) = (k, step * 2);
            };
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        fit.expect("lo fits")
    }

    /// Draws an icon centred in `r` (a square of `side`, at most as large as `r`).
    pub fn icon(&mut self, icon: Icon, r: Rect, side: u32, value: u8) {
        let side = side.min(r.w).min(r.h);
        let ir = Rect::new(r.x + (r.w as i32 - side as i32) / 2, r.y + (r.h as i32 - side as i32) / 2, side, side);
        icon.draw(self.canvas, ir, value);
    }
}

/// Lets the tree hand out concrete widget types.
pub trait AsAny {
    /// `self` as `Any`.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Any> AsAny for T {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// A UI element.
pub trait Widget: AsAny {
    /// Paints into `bounds` (already cleared to the theme background by the tree).
    fn paint(&mut self, cx: &mut PaintCx, bounds: Rect);

    /// Handles an event that hit `bounds`.
    fn event(&mut self, _ev: &UiEvent, _bounds: Rect) -> EventResult {
        EventResult::Ignored
    }

    /// Class used when this widget changes (GL16 by default; DU for highlights/keys).
    fn refresh_class(&self) -> RefreshClass {
        RefreshClass::Gl16
    }

    /// When false the tree does not clear the background before painting.
    fn opaque(&self) -> bool {
        true
    }

    /// After a paint caused by this widget's own event handling: the rectangles that
    /// really changed (inside `bounds`), or `None` for all of `bounds`. Called after
    /// every paint; widgets that track changes reset their record here.
    fn damage(&mut self, _bounds: Rect) -> Option<Vec<Rect>> {
        None
    }

    /// False for widgets that never draw anything (invisible scrims under modal layers):
    /// the tree paints them but reports no damage for them, not on the first paint,
    /// not after they take an event, and not when they are removed. Their children
    /// still report damage as usual.
    fn adds_damage(&self) -> bool {
        true
    }
}

struct Node {
    widget: Box<dyn Widget>,
    bounds: Rect,
    parent: Option<WidgetId>,
    children: Vec<WidgetId>,
    dirty: Option<RefreshClass>,
    /// The next paint must show all of `bounds` (not just the widget's own damage).
    full: bool,
    expires: Option<Instant>,
}

impl Node {
    /// Marks the node for a repaint with at least `class`.
    fn mark(&mut self, class: RefreshClass) {
        self.dirty = Some(self.dirty.map_or(class, |c| c.max(class)));
    }
}

/// The tree. Ids are stable until the widget is removed.
#[derive(Default)]
pub struct WidgetTree {
    nodes: Vec<Option<Node>>,
    roots: Vec<WidgetId>,
    /// Areas to clear and show on the next paint (removed widgets, invalidated regions).
    exposed: Vec<(Rect, RefreshClass)>,
    /// Touch slot -> widget that took its Down.
    capture: Vec<(u8, WidgetId)>,
}

impl WidgetTree {
    /// An empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `widget` at `bounds` under `parent` (or as a new top-level layer), dirty.
    pub fn add(&mut self, parent: Option<WidgetId>, widget: Box<dyn Widget>, bounds: Rect) -> WidgetId {
        let class = widget.refresh_class();
        let id = WidgetId(self.nodes.len());
        self.nodes.push(Some(Node {
            widget,
            bounds,
            parent,
            children: Vec::new(),
            dirty: Some(class),
            full: true,
            expires: None,
        }));
        match parent.and_then(|p| self.node_mut(p)) {
            Some(p) => p.children.push(id),
            None => self.roots.push(id),
        }
        id
    }

    fn node(&self, id: WidgetId) -> Option<&Node> {
        self.nodes.get(id.0)?.as_ref()
    }

    fn node_mut(&mut self, id: WidgetId) -> Option<&mut Node> {
        self.nodes.get_mut(id.0)?.as_mut()
    }

    /// Adds a top-level layer that [`WidgetTree::expire`] removes after `ttl` (toasts).
    pub fn add_timed(&mut self, widget: Box<dyn Widget>, bounds: Rect, ttl: Duration) -> WidgetId {
        let id = self.add(None, widget, bounds);
        if let Some(n) = self.node_mut(id) {
            n.expires = Some(Instant::now() + ttl);
        }
        id
    }

    /// Time until the next timed layer expires.
    pub fn next_expiry(&self) -> Option<Duration> {
        let now = Instant::now();
        self.nodes.iter().flatten().filter_map(|n| n.expires).min().map(|t| t.saturating_duration_since(now))
    }

    /// Removes timed layers whose time is up; returns their ids.
    pub fn expire(&mut self) -> Vec<WidgetId> {
        let now = Instant::now();
        let due: Vec<WidgetId> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.as_ref().and_then(|n| n.expires).filter(|t| *t <= now).map(|_| WidgetId(i)))
            .collect();
        for id in &due {
            self.remove(*id);
        }
        due
    }

    /// True when the widget exists.
    pub fn contains(&self, id: WidgetId) -> bool {
        self.nodes.get(id.0).is_some_and(Option::is_some)
    }

    /// Removes a widget and its children. Its area is cleared and shown again (with the
    /// widgets underneath repainted) on the next [`WidgetTree::paint`].
    pub fn remove(&mut self, id: WidgetId) -> Option<Rect> {
        let node = self.nodes.get_mut(id.0)?.take()?;
        if node.widget.adds_damage() {
            self.exposed.push((node.bounds, RefreshClass::Gl16));
        }
        self.capture.retain(|(_, w)| *w != id);
        for c in &node.children {
            self.remove(*c);
        }
        match node.parent.and_then(|p| self.node_mut(p)) {
            Some(p) => p.children.retain(|c| *c != id),
            None => self.roots.retain(|c| *c != id),
        }
        Some(node.bounds)
    }

    /// The rectangles the tree draws into: the bounds of each widget that adds damage
    /// and has no ancestor that adds damage (a [`crate::Scrim`] layer itself is left
    /// out, its children are not). Used to restore what was under a tree that goes away.
    pub fn painted_rects(&self) -> Vec<Rect> {
        let mut out = Vec::new();
        let mut stack: Vec<WidgetId> = self.roots.clone();
        while let Some(id) = stack.pop() {
            let Some(n) = self.node(id) else { continue };
            if n.widget.adds_damage() {
                if !n.bounds.is_empty() {
                    out.push(n.bounds);
                }
            } else {
                stack.extend(n.children.iter().copied());
            }
        }
        out
    }

    /// Bounds of a widget.
    pub fn bounds(&self, id: WidgetId) -> Option<Rect> {
        self.node(id).map(|n| n.bounds)
    }

    /// Marks a widget for a full repaint with at least `class`.
    pub fn mark_dirty(&mut self, id: WidgetId, class: RefreshClass) {
        if let Some(n) = self.node_mut(id) {
            n.mark(class);
            n.full = true;
        }
    }

    /// Marks a widget for a repaint with at least `class` where only what its
    /// [`Widget::damage`] names is shown (a list whose one row changed), as after an event
    /// it took. [`WidgetTree::mark_dirty`] shows all of its bounds.
    pub fn mark_changed(&mut self, id: WidgetId, class: RefreshClass) {
        if let Some(n) = self.node_mut(id) {
            n.mark(class);
        }
    }

    /// Clears `r` to the background on the next paint, repaints the widgets that overlap
    /// it, and shows all of `r` with `class`. Screens call this with the whole screen when
    /// they appear, so nothing of the previous screen stays on the panel.
    pub fn invalidate(&mut self, r: Rect, class: RefreshClass) {
        self.exposed.push((r, class));
    }

    /// Marks every widget dirty (full repaint).
    pub fn mark_all_dirty(&mut self, class: RefreshClass) {
        for n in self.nodes.iter_mut().flatten() {
            n.dirty = Some(class);
            n.full = true;
        }
    }

    /// The concrete widget behind `id`.
    pub fn get_mut<T: Widget + 'static>(&mut self, id: WidgetId) -> Option<&mut T> {
        // Deref the box first: Box<dyn Widget> is itself Any.
        self.node_mut(id)?.widget.as_mut().as_any_mut().downcast_mut::<T>()
    }

    /// Takes the rectangles uncovered by [`WidgetTree::remove`] since the last paint, for
    /// a caller that puts back what was under them itself (a reader restores its page
    /// there); [`WidgetTree::paint`] would fill them with the background. Widgets over
    /// them are still painted again.
    pub fn take_exposed(&mut self) -> Vec<Rect> {
        let exposed = std::mem::take(&mut self.exposed);
        for (r, class) in &exposed {
            for n in self.nodes.iter_mut().flatten() {
                if !n.bounds.intersect(r).is_empty() {
                    n.mark(*class);
                    n.full = true;
                }
            }
        }
        exposed.into_iter().map(|(r, _)| r).collect()
    }

    /// Paints dirty widgets (parents before children, layers in order) and returns the
    /// merged damage ([`crate::refresh::merge`]: overlapping rectangles only). The app
    /// should coalesce a whole frame with [`crate::refresh::coalesce`].
    pub fn paint(&mut self, cx: &mut PaintCx) -> Vec<Damage> {
        let mut damage = Vec::new();
        for (r, class) in std::mem::take(&mut self.exposed) {
            cx.canvas.fill_rect(r.x, r.y, r.w as i32, r.h as i32, cx.theme.bg);
            for n in self.nodes.iter_mut().flatten() {
                if !n.bounds.intersect(&r).is_empty() {
                    n.mark(class);
                    n.full = true;
                }
            }
            damage.push(Damage { rect: r, class });
        }
        for i in 0..self.roots.len() {
            self.paint_node(self.roots[i], cx, None, &mut damage);
        }
        crate::refresh::merge(damage)
    }

    fn paint_node(&mut self, id: WidgetId, cx: &mut PaintCx, inherited: Option<RefreshClass>, damage: &mut Vec<Damage>) {
        let Some(node) = self.node_mut(id) else { return };
        // The widget took an event that changed nothing on it (empty own damage): its
        // children are painted again (it drew over them) with the same pixels, so only
        // children with a change of their own report damage.
        let mut quiet = false;
        let full = node.full || inherited.is_some();
        let class = match (node.dirty.take(), inherited) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if let Some(class) = class {
            let b = node.bounds;
            if node.widget.opaque() {
                cx.canvas.fill_rect(b.x, b.y, b.w as i32, b.h as i32, cx.theme.bg);
            }
            // A row cut by a list edge never leaves ink over the app bar or under the list.
            cx.paint_inside(node.widget.as_mut(), b);
            node.full = false;
            // Called even when unused: widgets reset their change record here.
            let own = node.widget.damage(b);
            quiet = !full && matches!(&own, Some(r) if r.is_empty());
            match own {
                _ if !node.widget.adds_damage() => {}
                Some(rects) if !full => {
                    for r in rects {
                        let r = r.intersect(&b);
                        if !r.is_empty() {
                            damage.push(Damage { rect: r, class });
                        }
                    }
                }
                _ => damage.push(Damage { rect: b, class }),
            }
        }
        // Children repaint (fully) with their parent, which may have cleared them. A
        // paint never adds or removes widgets, so the child list stays as it is.
        for i in 0..node.children.len() {
            let Some(c) = self.node(id).map(|n| n.children[i]) else { return };
            if quiet && !self.subtree_dirty(c) {
                self.paint_node(c, cx, class, &mut Vec::new());
            } else {
                self.paint_node(c, cx, class, damage);
            }
        }
    }

    /// True when `id` or a widget below it is marked dirty.
    fn subtree_dirty(&self, id: WidgetId) -> bool {
        let Some(n) = self.node(id) else { return false };
        n.dirty.is_some() || n.full || n.children.iter().any(|c| self.subtree_dirty(*c))
    }

    /// Sends `ev` to the right widget and marks it dirty when it reacts.
    ///
    /// - Gestures go to the topmost widget under the gesture position (layers added last
    ///   first, children before parents) until one does not ignore them.
    /// - A touch `Down` is hit-tested the same way and the widget that takes it captures
    ///   the contact: later `Move`/`Hold`/`Up` of that slot go to it directly.
    /// - Other events go to every widget from the top until one does not ignore them.
    pub fn dispatch(&mut self, ev: &UiEvent) -> Option<(WidgetId, EventResult)> {
        if let UiEvent::Touch(t) = ev {
            if t.kind != TouchKind::Down {
                let target = self.capture.iter().find(|(s, _)| *s == t.slot).map(|(_, w)| *w);
                if matches!(t.kind, TouchKind::Up | TouchKind::Cancel) {
                    self.capture.retain(|(s, _)| *s != t.slot);
                }
                let id = target?;
                return self.deliver(id, ev);
            }
            self.capture.retain(|(s, _)| *s != t.slot);
        }
        let pos = match ev {
            UiEvent::Gesture(g) => gesture_pos(g),
            UiEvent::Touch(t) => Some(t.pos),
            _ => None,
        };
        let mut order = Vec::new();
        for r in &self.roots {
            self.collect(*r, &mut order);
        }
        for id in order.into_iter().rev() {
            let Some(node) = self.node(id) else { continue };
            if let Some(p) = pos {
                if !node.bounds.contains(p) {
                    continue;
                }
            }
            if let Some(r) = self.deliver(id, ev) {
                if let UiEvent::Touch(t) = ev {
                    self.capture.push((t.slot, id));
                }
                return Some(r);
            }
        }
        None
    }

    /// Gives `ev` to one widget; `None` when it ignores it.
    fn deliver(&mut self, id: WidgetId, ev: &UiEvent) -> Option<(WidgetId, EventResult)> {
        let node = self.node_mut(id)?;
        match node.widget.event(ev, node.bounds) {
            EventResult::Ignored => None,
            r => {
                node.mark(node.widget.refresh_class());
                Some((id, r))
            }
        }
    }

    fn collect(&self, id: WidgetId, out: &mut Vec<WidgetId>) {
        if let Some(n) = self.node(id) {
            out.push(id);
            for c in &n.children {
                self.collect(*c, out);
            }
        }
    }

    /// Number of live widgets.
    pub fn len(&self) -> usize {
        self.nodes.iter().flatten().count()
    }

    /// True when the tree has no widgets.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Position of a gesture, for hit testing.
pub fn gesture_pos(g: &Gesture) -> Option<Point> {
    Some(g.pos())
}

/// Splits `r` into rows of the given heights from the top (the last row may be cut).
pub fn split_rows(r: Rect, heights: &[u32]) -> Vec<Rect> {
    let mut y = r.y;
    heights
        .iter()
        .map(|h| {
            let row = Rect::new(r.x, y, r.w, (*h).min((r.bottom() - y).max(0) as u32));
            y += *h as i32;
            row
        })
        .collect()
}

/// Splits `r` into `n` equal columns with `gap` pixels between them.
pub fn split_cols(r: Rect, n: u32, gap: u32) -> Vec<Rect> {
    let n = n.max(1);
    let w = (r.w.saturating_sub(gap * (n - 1))) / n;
    (0..n).map(|i| Rect::new(r.x + (i * (w + gap)) as i32, r.y, w, r.h)).collect()
}

/// A line of text.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// Text.
    pub text: String,
    /// Size in pixels.
    pub px: f32,
    /// Style.
    pub style: Style,
    /// Centre horizontally (else left-aligned with a small inset).
    pub centered: bool,
}

impl Label {
    /// A centred regular label.
    pub fn new(text: impl Into<String>, px: f32) -> Self {
        Label { text: text.into(), px, style: Style::Regular, centered: true }
    }

    /// A left-aligned label.
    pub fn left(text: impl Into<String>, px: f32) -> Self {
        Label { centered: false, ..Label::new(text, px) }
    }

    setters! {
        /// The same label in `style`.
        styled(style: Style) => style = style;
    }
}

impl Widget for Label {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        if self.centered {
            cx.text_centered(b, &self.text, self.px, self.style, fg);
        } else {
            let r = Rect::new(b.x + 8, b.y, b.w.saturating_sub(16), b.h);
            cx.text_left(r, &self.text, self.px, self.style, fg);
        }
    }
}

/// How a [`Button`] looks (`docs/DESIGN.md`, "Buttons").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    /// Black pill, white bold caption: the one main action of a screen or dialog.
    Filled,
    /// White pill with a black outline: secondary actions (the default).
    #[default]
    Outlined,
    /// Caption only (bold), no frame: low-emphasis actions in bars and dialogs, and
    /// icon-only buttons.
    Text,
}

/// A button that reports `action` when tapped: a pill (fully rounded) in one of three
/// [`ButtonKind`]s. It shows pressed (inverted, DU) while a finger rests on it (from
/// [`TouchKind::Press`], not from the touch-down: see [`crate::Touch::press_target`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    /// Caption.
    pub label: String,
    /// Action name reported on tap.
    pub action: String,
    /// Drawn inverted (pressed or primary): the same as [`ButtonKind::Filled`].
    pub inverted: bool,
    /// Icon left of the caption (or alone when the caption is empty).
    pub icon: Option<Icon>,
    /// Disabled buttons are gray and ignore taps.
    pub enabled: bool,
    /// A finger is on the button.
    pub pressed: bool,
    /// Look.
    pub kind: ButtonKind,
    /// The last event changed the look (pressed state).
    changed: bool,
}

impl Button {
    /// An outlined button.
    pub fn new(label: impl Into<String>, action: impl Into<String>) -> Self {
        Button { label: label.into(), action: action.into(), inverted: false, icon: None, enabled: true, pressed: false, kind: ButtonKind::Outlined, changed: false }
    }

    /// A filled (primary) button.
    pub fn filled(label: impl Into<String>, action: impl Into<String>) -> Self {
        Button { kind: ButtonKind::Filled, ..Button::new(label, action) }
    }

    /// A text (frameless) button.
    pub fn text(label: impl Into<String>, action: impl Into<String>) -> Self {
        Button { kind: ButtonKind::Text, ..Button::new(label, action) }
    }

    setters! {
        /// With an icon.
        with_icon(icon: Icon) => icon = Some(icon);
        /// Disabled.
        disabled() => enabled = false;
    }
}

/// Follows press feedback for a touch ([`Touch::press_target`]) on a widget whose part
/// `T` under the finger shows pressed (rows, cells, bar buttons). Returns what the widget
/// answers and whether `pressed` changed: `Handled` on a change, or for a touch-down it
/// takes (`take_down`: it captures the contact for its later `Press`, no repaint);
/// else `Ignored`. Nothing shows on touch-down: the touch may still become a swipe.
pub(crate) fn follow_press<T: Copy + PartialEq>(t: &Touch, pressed: &mut Option<T>, under: Option<T>, take_down: bool) -> (EventResult, bool) {
    let now = t.press_target(*pressed, under);
    if now != *pressed {
        *pressed = now;
        (EventResult::Handled, true)
    } else if t.kind == TouchKind::Down && take_down {
        (EventResult::Handled, false)
    } else {
        (EventResult::Ignored, false)
    }
}

/// The events of a widget in `b` that shows pressed as a whole (a button, a card, an
/// icon) and reports `action` on a tap: [`follow_press`] for its touches (it takes every
/// touch-down), the action for a tap; `None` for any other event. `changed` records a
/// change of `pressed`.
pub(crate) fn press_or_tap(ev: &UiEvent, b: Rect, pressed: &mut bool, changed: &mut bool, action: &str) -> Option<EventResult> {
    match ev {
        UiEvent::Touch(t) => {
            let mut p = pressed.then_some(());
            let (res, c) = follow_press(t, &mut p, b.contains(t.pos).then_some(()), true);
            *pressed = p.is_some();
            *changed |= c;
            Some(res)
        }
        UiEvent::Gesture(Gesture::Tap { .. }) => {
            *changed |= std::mem::take(pressed);
            Some(EventResult::Action(action.to_string()))
        }
        _ => None,
    }
}

/// What a widget answers to an event it has no use for: it takes gestures (nothing under
/// it reacts to them) and ignores everything else.
pub(crate) fn take_gestures(ev: &UiEvent) -> EventResult {
    if matches!(ev, UiEvent::Gesture(_)) {
        EventResult::Handled
    } else {
        EventResult::Ignored
    }
}

/// The refresh class of a widget with press feedback: DU while pressed (fast black and
/// white), GL16 otherwise (the released look gets its gray levels back).
pub(crate) fn press_class(pressed: bool) -> RefreshClass {
    if pressed {
        RefreshClass::Du
    } else {
        RefreshClass::Gl16
    }
}

/// [`Widget::damage`] of a widget whose events change only its look: all of its bounds
/// after a change, nothing after an event that changed nothing (a touch-down, a quick
/// tap). Resets `changed`.
pub(crate) fn look_damage(changed: &mut bool) -> Option<Vec<Rect>> {
    if std::mem::take(changed) {
        None
    } else {
        Some(Vec::new())
    }
}

/// Paints a button face into `b`: what [`Button`] draws, for screens that paint
/// directly. `pressed` inverts it (pure black and white, for a DU update).
pub fn paint_button(cx: &mut PaintCx, b: Rect, label: &str, icon: Option<Icon>, kind: ButtonKind, enabled: bool, pressed: bool) {
    let t = cx.theme;
    let (fg, bg) = (t.fg, t.bg);
    // Corners: a pill, but never rounder than a square icon button's circle.
    let rad = (b.h.min(b.w) as f32 / 2.0).max(0.0);
    let filled = (kind == ButtonKind::Filled) ^ pressed;
    let ink = if !enabled {
        t.muted
    } else if filled {
        bg
    } else {
        fg
    };
    if filled && enabled {
        crate::draw::fill_round_rect(cx.canvas, b, rad, fg);
    } else if !enabled && kind == ButtonKind::Filled {
        crate::draw::fill_round_rect(cx.canvas, b, rad, t.light);
    }
    if kind == ButtonKind::Outlined && !filled {
        crate::draw::stroke_round_rect(cx.canvas, b, rad, t.border_px as f32, if enabled { fg } else { t.subtle });
    }
    let style = if kind == ButtonKind::Outlined { t.label.style } else { Style::Bold };
    let px = t.label.px;
    // The caption is shaped once: it gives the width the button needs, and it is what
    // is drawn unless it has to be cut.
    let caption = cx.fonts.shape(style, px, label);
    // Keep the caption off the round ends: at least `space(2)` (less on small buttons)
    // between the caption and the outline; a caption that does not fit is ellipsized.
    let need = caption.width + if icon.is_some() { px * 1.5 } else { 0.0 };
    let min_pad = (rad * 0.5).clamp(4.0, crate::theme::space(2) as f32);
    let pad = ((b.w as f32 - need) / 2.0).clamp(min_pad, (rad * 0.5).max(min_pad));
    let inner = Rect::new(b.x + pad as i32, b.y, b.w.saturating_sub(2 * pad as u32), b.h);
    match (icon, label.is_empty()) {
        (Some(icon), true) => {
            let side = (b.h.min(b.w) as f32 * 0.5) as u32;
            cx.icon(icon, b, side, ink);
        }
        (Some(icon), false) => {
            // Icon and caption centred together; the caption is cut to fit.
            let side = ((px * 1.1) as u32).min(b.h * 4 / 5).min(b.w);
            let gap = px * 0.4;
            let room = (inner.w as f32 - side as f32 - gap).max(0.0);
            let run = cx.fit(caption, label, style, room);
            let total = side as f32 + gap + run.width;
            let x0 = (b.x as f32 + (b.w as f32 - total) / 2.0).max(inner.x as f32);
            let ir = Rect::new(x0 as i32, b.y + (b.h as i32 - side as i32) / 2, side, side);
            icon.draw(cx.canvas, ir, ink);
            let baseline = cx.baseline_in(b, px, style);
            cx.put_run(&run, x0 + side as f32 + gap, baseline, ink, Some(b));
        }
        (None, _) => {
            let run = cx.fit(caption, label, style, inner.w as f32);
            cx.put_centered(inner, &run, style, ink)
        }
    }
}

impl Widget for Button {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        // `inverted` makes any button filled.
        let kind = if self.inverted { ButtonKind::Filled } else { self.kind };
        paint_button(cx, b, &self.label, self.icon, kind, self.enabled, self.pressed);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        if !self.enabled {
            return match ev {
                UiEvent::Gesture(_) | UiEvent::Touch(_) => EventResult::Handled,
                _ => EventResult::Ignored,
            };
        }
        press_or_tap(ev, b, &mut self.pressed, &mut self.changed, &self.action).unwrap_or(EventResult::Ignored)
    }

    fn refresh_class(&self) -> RefreshClass {
        press_class(self.pressed)
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        look_damage(&mut self.changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gesture::Touch;

    fn cx<'a>(canvas: &'a mut Canvas, fonts: &'a mut Fonts, theme: &'a Theme) -> PaintCx<'a> {
        PaintCx::new(canvas, fonts, theme)
    }

    fn touch(slot: u8, kind: TouchKind, x: i32, y: i32) -> UiEvent {
        UiEvent::Touch(Touch { slot, kind, pos: Point::new(x, y), start: Point::new(x, y), held: Duration::ZERO, moved: false })
    }

    #[test]
    fn paint_dispatch_remove() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 300);
        let mut t = WidgetTree::new();
        let label = t.add(None, Box::new(Label::new("Hello", 30.0)), Rect::new(0, 0, 400, 100));
        let button = t.add(Some(label), Box::new(Button::new("OK", "ok")), Rect::new(150, 120, 100, 60));
        let dmg = t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg.len(), 2);
        assert!(canvas.pixels.iter().any(|v| *v < 64));
        // Nothing dirty: nothing painted.
        assert!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme)).is_empty());
        let tap = |x, y| UiEvent::Gesture(Gesture::Tap { pos: Point::new(x, y) });
        assert_eq!(t.dispatch(&tap(160, 130)), Some((button, EventResult::Action("ok".into()))));
        assert_eq!(t.dispatch(&tap(10, 10)), None);
        t.get_mut::<Label>(label).unwrap().text = "Bye".into();
        t.mark_dirty(label, RefreshClass::Gl16);
        let dmg = t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg.len(), 2, "children repaint with their parent");
        assert!(t.get_mut::<Button>(label).is_none());
        assert_eq!(t.remove(label), Some(Rect::new(0, 0, 400, 100)));
        assert!(t.is_empty());
    }

    #[test]
    fn exposed_rects_taken_by_the_caller_are_not_filled() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 300);
        let mut t = WidgetTree::new();
        let r = Rect::new(150, 120, 100, 60);
        let b = t.add(None, Box::new(Button::new("OK", "ok")), r);
        t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        t.remove(b);
        // The caller puts back what was under the button (here: black).
        assert_eq!(t.take_exposed(), vec![r]);
        canvas.fill_rect(r.x, r.y, r.w as i32, r.h as i32, 0);
        assert!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme)).is_empty());
        assert!(canvas.pixels[(r.y as usize + 10) * 400 + r.x as usize + 10] == 0);
        assert!(t.take_exposed().is_empty());
    }

    #[test]
    fn a_scrim_adds_no_damage() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 400);
        let mut t = WidgetTree::new();
        let scrim = t.add(None, Box::new(crate::Scrim::new("close")), Rect::new(0, 0, 400, 400));
        let ok = Rect::new(150, 150, 100, 60);
        t.add(None, Box::new(Button::new("OK", "ok")), ok);
        // First paint: only the button, not the full-screen scrim.
        let dmg = t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg, vec![Damage { rect: ok, class: RefreshClass::Gl16 }]);
        // Touching the scrim is swallowed and refreshes nothing.
        assert_eq!(t.dispatch(&touch(0, TouchKind::Down, 10, 10)).map(|r| r.0), Some(scrim));
        t.dispatch(&touch(0, TouchKind::Up, 10, 10));
        assert!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme)).is_empty());
        // Removing it refreshes nothing either.
        t.remove(scrim);
        assert!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme)).is_empty());
    }

    #[test]
    fn removing_a_layer_clears_and_shows_its_area() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(300, 300);
        let mut t = WidgetTree::new();
        t.add(None, Box::new(Label::new("under", 30.0)), Rect::new(0, 0, 300, 50));
        let top = t.add(None, Box::new(Button::new("top", "x")), Rect::new(50, 100, 200, 100));
        t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert_eq!(canvas.get(150, 101), Some(0), "button frame drawn");
        t.remove(top);
        let dmg = t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert!(dmg.iter().any(|d| d.rect.intersect(&Rect::new(50, 100, 200, 100)) == Rect::new(50, 100, 200, 100)));
        assert_eq!(canvas.get(150, 101), Some(255), "area cleared");
        t.invalidate(Rect::new(0, 0, 300, 300), RefreshClass::Gc16);
        let dmg = t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg, vec![Damage { rect: Rect::new(0, 0, 300, 300), class: RefreshClass::Gc16 }]);
        assert!(canvas.pixels[..300 * 50].iter().any(|v| *v < 64), "label repainted");
    }

    #[test]
    fn button_press_feedback_is_du_and_release_is_gl16() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 300);
        let mut t = WidgetTree::new();
        let b = t.add(None, Box::new(Button::new("OK", "ok")), Rect::new(100, 100, 200, 100));
        t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        // Touch-down: captured, but nothing drawn or refreshed (it may become a swipe).
        assert_eq!(t.dispatch(&touch(0, TouchKind::Down, 150, 150)), Some((b, EventResult::Handled)));
        assert_eq!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme)), vec![]);
        assert_eq!(canvas.get(200, 110), Some(255), "not pressed on touch-down");
        // Press (the finger rested): inverted, DU.
        assert_eq!(t.dispatch(&touch(0, TouchKind::Press, 150, 150)), Some((b, EventResult::Handled)));
        let dmg = t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg, vec![Damage { rect: Rect::new(100, 100, 200, 100), class: RefreshClass::Du }]);
        assert_eq!(canvas.get(200, 110), Some(0), "pressed: inverted");
        // The finger leaves the button area: the captured contact still reaches it.
        assert_eq!(t.dispatch(&touch(0, TouchKind::Up, 10, 10)), Some((b, EventResult::Handled)));
        let dmg = t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg[0].class, RefreshClass::Gl16);
        assert_eq!(canvas.get(200, 110), Some(255));
        // A Move with no captured contact goes nowhere.
        assert_eq!(t.dispatch(&touch(3, TouchKind::Move, 150, 150)), None);
        // Disabled buttons swallow taps without an action.
        t.get_mut::<Button>(b).unwrap().enabled = false;
        let tap = UiEvent::Gesture(Gesture::Tap { pos: Point::new(150, 150) });
        assert_eq!(t.dispatch(&tap), Some((b, EventResult::Handled)));
    }

    #[test]
    fn button_quick_tap_and_swipe_show_no_press() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 300);
        let mut t = WidgetTree::new();
        let b = t.add(None, Box::new(Button::new("OK", "ok")), Rect::new(100, 100, 200, 100));
        t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        // A quick tap: down, up, tap. The action, and no refresh of the button.
        t.dispatch(&touch(0, TouchKind::Down, 150, 150));
        t.dispatch(&touch(0, TouchKind::Up, 150, 150));
        let tap = UiEvent::Gesture(Gesture::Tap { pos: Point::new(150, 150) });
        assert_eq!(t.dispatch(&tap), Some((b, EventResult::Action("ok".into()))));
        assert_eq!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme)), vec![]);
        // Held (Press), then the finger leaves the tap slop: the feedback goes, in one
        // update of the button only.
        t.dispatch(&touch(0, TouchKind::Down, 150, 150));
        t.dispatch(&touch(0, TouchKind::Press, 150, 150));
        t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        let slide = UiEvent::Touch(Touch { moved: true, ..match touch(0, TouchKind::Move, 160, 150) {
            UiEvent::Touch(x) => x,
            _ => unreachable!(),
        } });
        assert_eq!(t.dispatch(&slide), Some((b, EventResult::Handled)));
        let dmg = t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg, vec![Damage { rect: Rect::new(100, 100, 200, 100), class: RefreshClass::Gl16 }]);
        assert_eq!(canvas.get(200, 110), Some(255), "released");
    }

    /// Draws a black square that sticks out of its bounds on every side.
    struct Spill;
    impl Widget for Spill {
        fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
            cx.canvas.fill_rect(b.x - 20, b.y - 20, b.w as i32 + 40, b.h as i32 + 40, 0);
            cx.icon(Icon::Back, Rect::new(b.x - 30, b.y - 30, 60, 60), 60, 0);
        }
    }

    #[test]
    fn widgets_cannot_draw_outside_their_bounds() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 300);
        let mut t = WidgetTree::new();
        let r = Rect::new(100, 100, 200, 100);
        t.add(None, Box::new(Spill), r);
        t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        for y in 0..300 {
            for x in 0..400 {
                let want = if r.contains(Point::new(x, y)) { 0 } else { 255 };
                assert_eq!(canvas.get(x, y), Some(want), "({x}, {y})");
            }
        }
        assert_eq!(canvas.clip(), None, "the clip is put back after the paint");
    }

    /// Reports one small rectangle of damage.
    struct Partial(u32);
    impl Widget for Partial {
        fn paint(&mut self, _cx: &mut PaintCx, _b: Rect) {}
        fn event(&mut self, _ev: &UiEvent, _b: Rect) -> EventResult {
            self.0 += 1;
            EventResult::Handled
        }
        fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
            Some(vec![Rect::new(b.x, b.y, 10, 10)])
        }
    }

    #[test]
    fn partial_damage_only_after_own_events() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 300);
        let mut t = WidgetTree::new();
        let id = t.add(None, Box::new(Partial(0)), Rect::new(0, 0, 200, 200));
        // First paint shows everything.
        assert_eq!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme))[0].rect, Rect::new(0, 0, 200, 200));
        t.dispatch(&touch(0, TouchKind::Down, 5, 5));
        assert_eq!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme))[0].rect, Rect::new(0, 0, 10, 10));
        t.mark_dirty(id, RefreshClass::Gl16);
        assert_eq!(t.paint(&mut cx(&mut canvas, &mut fonts, &theme))[0].rect, Rect::new(0, 0, 200, 200));
    }

    #[test]
    fn timed_layers_expire() {
        let mut t = WidgetTree::new();
        let id = t.add_timed(Box::new(Label::new("toast", 20.0)), Rect::new(0, 0, 10, 10), Duration::from_millis(20));
        assert!(t.next_expiry().unwrap() <= Duration::from_millis(20));
        assert!(t.expire().is_empty());
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(t.expire(), vec![id]);
        assert!(!t.contains(id));
        assert_eq!(t.next_expiry(), None);
    }

    #[test]
    fn rows_and_cols() {
        let r = split_rows(Rect::new(0, 10, 100, 50), &[20, 20, 20]);
        assert_eq!(r[2], Rect::new(0, 50, 100, 10));
        let c = split_cols(Rect::new(0, 0, 100, 10), 3, 5);
        assert_eq!(c, vec![Rect::new(0, 0, 30, 10), Rect::new(35, 0, 30, 10), Rect::new(70, 0, 30, 10)]);
    }

    /// Panics in paint, after checking the clip it was given.
    struct Boom(Rect);
    impl Widget for Boom {
        fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
            assert_eq!(cx.clip(), Some(b));
            self.0 = b;
            panic!("boom");
        }
    }

    #[test]
    fn text_clip_is_the_widget_bounds_and_is_restored() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 300);
        {
            let c = cx(&mut canvas, &mut fonts, &theme);
            assert_eq!(c.clip(), None);
            {
                let _a = c.push_clip(Rect::new(0, 0, 100, 100));
                let _b = c.push_clip(Rect::new(50, 50, 100, 100));
                assert_eq!(c.clip(), Some(Rect::new(50, 50, 50, 50)), "nested clips intersect");
            }
            assert_eq!(c.clip(), None);
        }
        // A widget that panics while painting does not leave its clip behind.
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut t = WidgetTree::new();
            t.add(None, Box::new(Boom(Rect::default())), Rect::new(10, 10, 20, 20));
            t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        }));
        assert!(res.is_err());
        assert_eq!(cx(&mut canvas, &mut fonts, &theme).clip(), None);
        // Text drawn through the context stays inside the widget bounds.
        let mut t = WidgetTree::new();
        let b = Rect::new(150, 100, 90, 24);
        t.add(None, Box::new(Label::left("A long caption that does not fit", 30.0)), b);
        t.add(None, Box::new(Label::new("Another caption much wider than its box", 40.0)), b);
        let mut canvas = Canvas::new(400, 300);
        t.paint(&mut cx(&mut canvas, &mut fonts, &theme));
        let outside = (0..300).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|(x, y)| !b.contains(Point::new(*x, *y)) && canvas.get(*x, *y) != Some(255)).count();
        assert_eq!(outside, 0);
        assert!(canvas.pixels.iter().any(|v| *v < 64), "something drawn inside");
    }

    #[test]
    fn text_helpers() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(400, 100);
        let mut c = cx(&mut canvas, &mut fonts, &theme);
        let s = c.ellipsize("A rather long caption that will not fit", 30.0, Style::Regular, 200.0);
        assert!(s.ends_with('…') && c.text_width(Style::Regular, 30.0, &s) <= 200.0, "{s}");
        assert_eq!(c.ellipsize("short", 30.0, Style::Regular, 200.0), "short");
        c.text_left(Rect::new(0, 0, 400, 100), "Left", 30.0, Style::Bold, 0);
        assert!(canvas.pixels.iter().any(|v| *v < 64));
    }
}
