//! Overlays over the page: a [`WidgetTree`] that only holds the overlay's widgets.
//!
//! The page itself is not a widget. Removing a widget from a tree clears its area to the
//! background, which would erase the page, so overlays never remove widgets: closing an
//! overlay drops its whole tree and the reader puts the page back from its buffer.

use ink_device::Rect;
use ink_ui::{Button, EventResult, Gesture, PaintCx, UiEvent, Widget, WidgetId, WidgetTree};
use std::collections::HashMap;

/// Which overlay is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayKind {
    /// Top and bottom bars.
    Menu,
    /// Contents, bookmarks, go to percent.
    Goto,
    /// Font and layout panel.
    Aa,
    /// Search field, keyboard and results.
    Search,
    /// Footnote or link popup.
    Popup,
    /// Selection actions.
    Selection,
    /// The bar that steps through search hits (the page stays usable).
    Hits,
    /// The note editor of a selection (field and keyboard).
    Note,
    /// The card shown past the last page (reader/end.rs).
    End,
}

/// An open overlay.
pub struct Overlay {
    /// Which one.
    pub kind: OverlayKind,
    /// Its widgets.
    pub tree: WidgetTree,
    /// Widgets the reader updates later, by name.
    pub ids: HashMap<&'static str, WidgetId>,
    /// Logical rectangle of every button by action (tests, logs).
    pub buttons: Vec<(String, Rect)>,
}

impl Overlay {
    /// An empty overlay.
    pub fn new(kind: OverlayKind) -> Self {
        Overlay { kind, tree: WidgetTree::new(), ids: HashMap::new(), buttons: Vec::new() }
    }

    /// Adds a button and records where it is.
    pub fn button(&mut self, parent: Option<WidgetId>, b: Button, r: Rect) -> WidgetId {
        self.buttons.push((b.action.clone(), r));
        self.tree.add(parent, Box::new(b), r)
    }

    /// Where the button with `action` is.
    pub fn button_rect(&self, action: &str) -> Option<Rect> {
        self.buttons.iter().find(|(a, _)| a == action).map(|(_, r)| *r)
    }
}

/// Where a [`Bar`] draws its rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Along the top edge (bottom bars).
    Top,
    /// Along the bottom edge (top bars).
    Bottom,
    /// No rule (full-screen panels).
    None,
}

/// A plain panel (white, from the tree) with an optional rule along one edge.
pub struct Bar {
    /// The rule.
    pub rule: Rule,
    /// A tap on the bar's empty space was just taken: the next damage report is empty.
    taken: bool,
}

impl Bar {
    /// A panel with `rule`.
    pub fn new(rule: Rule) -> Self {
        Bar { rule, taken: false }
    }
}

impl Widget for Bar {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.border_px;
        let y = match self.rule {
            Rule::Top => b.y,
            Rule::Bottom => b.bottom() - t,
            Rule::None => return,
        };
        cx.canvas.fill_rect(b.x, y, b.w as i32, t, cx.theme.fg);
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        // Taps on the bar's empty parts do not fall through to the scrim (which closes).
        // Touch notifications are ignored: a widget that takes one is repainted.
        match ev {
            UiEvent::Gesture(_) => {
                self.taken = true;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    /// A taken tap changes nothing on screen: no rectangle, so no refresh. (The first
    /// paint, when the overlay opens, is reported in full.)
    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        if std::mem::take(&mut self.taken) {
            Some(Vec::new())
        } else {
            None
        }
    }
}

/// An invisible full-screen layer under an overlay: reports `action` on a tap, swallows
/// other gestures, ignores touch notifications and never adds damage (the
/// [`ink_ui::Scrim`] refreshes the whole screen whenever it takes an event).
pub struct PageScrim {
    /// Reported on a tap.
    pub action: String,
}

impl Widget for PageScrim {
    fn paint(&mut self, _cx: &mut PaintCx, _b: Rect) {}

    fn opaque(&self) -> bool {
        false
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Tap { .. }) => EventResult::Action(self.action.clone()),
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        Some(Vec::new())
    }

    /// Paints nothing: opening an overlay over it refreshes only the overlay's own
    /// rects (the menu bars), not the full screen.
    fn adds_damage(&self) -> bool {
        false
    }
}
