//! Verifier attacks on `ink-ui` (wave A, cycle 2), aimed at the cycle-2 changes: the
//! `TouchKind::Cancel` path (rotation, lost releases), the new frame order, the text clip
//! and `PaintCx::ellipsize`, and what really reaches the panel.
//!
//! Most checks use a simulated e-ink panel: after each `WidgetTree::paint`, only the
//! returned damage rectangles are copied from the canvas to the panel (as `App::run`
//! sends them to the screen). Anything a widget changes outside its damage, or an
//! overlay that a repaint below it erases, shows up as a panel difference.

use ink_device::{Canvas, Point, Rect, Rotation, Size, Timestamp, TouchContact, TouchFrame, TouchPhase};
use ink_text::{Fonts, Style};
use ink_ui::keyboard::KeyAction;
use ink_ui::{
    AppBar, BarAction, BottomSheet, Button, Card, Chips, Cover, Damage, Dialog, DoubleTapMode, NavBar, NavItem, RingProgress, Segmented, EventResult, GestureRecognizer, Icon, IconView, Keyboard, Label,
    List, ListItem, Menu, PaintCx, Popup, ProgressBar, ScreenSpace, ScrollView, Slider, Tabs, TextBlock, TextInput, Theme,
    Toast, Toggle, Touch, TouchKind, UiEvent, Widget, WidgetId, WidgetTree,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

const SCREEN: Size = Size::new(1272, 1696);

/// A widget tree, its canvas and a simulated panel that only receives damage.
struct Rig {
    tree: WidgetTree,
    canvas: Canvas,
    panel: Canvas,
    /// Taken from [`FONT_POOL`] and given back on drop: a fresh `Fonts` has a cold glyph
    /// cache, and rasterising the same glyphs again for every rig took most of the time.
    fonts: Option<Fonts>,
    theme: Theme,
}

thread_local! {
    static FONT_POOL: std::cell::RefCell<Vec<Fonts>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(f) = self.fonts.take() {
            FONT_POOL.with(|p| p.borrow_mut().push(f));
        }
    }
}

impl Rig {
    fn new() -> Self {
        Rig {
            tree: WidgetTree::new(),
            canvas: Canvas::new(SCREEN.w as usize, SCREEN.h as usize),
            panel: Canvas::new(SCREEN.w as usize, SCREEN.h as usize),
            fonts: Some(FONT_POOL.with(|p| p.borrow_mut().pop()).unwrap_or_else(Fonts::load)),
            theme: Theme::day(),
        }
    }

    /// Paints the tree and copies the damage to the panel.
    fn paint(&mut self) -> Vec<Damage> {
        let damage = self.tree.paint(&mut PaintCx::new(&mut self.canvas, self.fonts.as_mut().expect("fonts"), &self.theme));
        for d in &damage {
            // Row by row (the panel has no clip; a pixel loop is slow in a debug build).
            let r = d.rect.intersect(&self.canvas.bounds());
            let w = self.canvas.width;
            for y in r.y..r.bottom() {
                let row = y as usize * w;
                let (x0, x1) = (row + r.x as usize, row + r.right() as usize);
                self.panel.pixels[x0..x1].copy_from_slice(&self.canvas.pixels[x0..x1]);
            }
        }
        damage
    }

    fn send(&mut self, ev: &UiEvent) -> Option<(WidgetId, EventResult)> {
        self.tree.dispatch(ev)
    }
}

/// Pixels that differ between `a` and `b` (inside `within` when given).
fn differ(a: &Canvas, b: &Canvas, within: Option<Rect>) -> usize {
    let r = within.unwrap_or_else(|| a.bounds()).intersect(&a.bounds());
    if (a.width, a.height) != (b.width, b.height) {
        return (r.y..r.bottom()).flat_map(|y| (r.x..r.right()).map(move |x| (x, y))).filter(|(x, y)| a.get(*x, *y) != b.get(*x, *y)).count();
    }
    // Row slices: the same count, without a get() per pixel.
    (r.y..r.bottom())
        .map(|y| {
            let row = y as usize * a.width;
            let (x0, x1) = (row + r.x as usize, row + r.right() as usize);
            a.pixels[x0..x1].iter().zip(&b.pixels[x0..x1]).filter(|(p, q)| p != q).count()
        })
        .sum()
}

fn touch(slot: u8, kind: TouchKind, p: Point, held_ms: u64) -> UiEvent {
    UiEvent::Touch(Touch { slot, kind, pos: p, start: p, held: Duration::from_millis(held_ms), moved: false })
}

fn center(r: Rect) -> Point {
    Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)
}

fn fr(ms: u64, contacts: Vec<TouchContact>) -> TouchFrame {
    TouchFrame { time: Timestamp::from_millis(ms), contacts }
}

fn c(slot: u8, p: Point, phase: TouchPhase) -> TouchContact {
    TouchContact { slot, x: p.x, y: p.y, phase }
}

const KB: Rect = Rect::new(0, 1000, 1272, 696);

/// Centre of the first key of the English keyboard at `KB` that matches `pick`.
fn key_point(pick: impl Fn(&ink_ui::keyboard::KeySpec) -> bool) -> Point {
    let kb = Keyboard::new();
    let area = kb.keys_area(KB);
    let (r, _) = kb.layout.key_rects(area).into_iter().find(|(_, s)| pick(s)).expect("key");
    center(r)
}

type Make = fn() -> Box<dyn Widget>;

/// Widgets that show press feedback (or take touches), their bounds and a press point.
fn pressable() -> Vec<(&'static str, Make, Rect, Point)> {
    let button = Rect::new(400, 400, 400, 120);
    let icon = Rect::new(500, 500, 150, 150);
    let list = Rect::new(0, 200, 1272, 800);
    let slider = Rect::new(100, 300, 1000, 150);
    let (_, track, _) = Slider::parts(slider);
    let scroll = Rect::new(100, 200, 800, 600);
    let alt_key = key_point(|s| !s.alternates.is_empty());
    let letter = key_point(|s| s.label == "q");
    let del = key_point(|s| s.label == "del");
    vec![
        ("button", || Box::new(Button::new("OK", "ok")), button, center(button)),
        ("button-icon-label", || Box::new(Button::new("Search", "s").with_icon(Icon::Search)), button, center(button)),
        ("icon-view", || Box::new(IconView::new(Icon::Settings, "gear")), icon, center(icon)),
        (
            "list-row",
            || Box::new(List::new((0..10).map(|i| ListItem::new(format!("Row {i}"), format!("r{i}")).detail("1 MB")).collect(), 120)),
            list,
            Point::new(600, 260),
        ),
        ("slider-drag", || Box::new(Slider::new("Size", 1.0, 10.0, 1.0, 5.0, "s")), slider, Point::new(track.x + track.w as i32 / 5, center(track).y)),
        (
            "scroll-view-button",
            || {
                let mut sv = ScrollView::new();
                sv.push(Box::new(Button::new("A", "a")), 120);
                sv.push(Box::new(Button::new("B", "b")), 120);
                Box::new(sv)
            },
            scroll,
            Point::new(scroll.x + 300, scroll.y + 60),
        ),
        ("keyboard-letter", || Box::new(Keyboard::new()), KB, letter),
        ("keyboard-alternates", || Box::new(Keyboard::new()), KB, alt_key),
        ("keyboard-del", || Box::new(Keyboard::new()), KB, del),
        ("menu", || Box::new(Menu::new(vec![("Open".into(), "o".into()), ("Delete".into(), "d".into())])), Rect::new(300, 300, 600, 220), Point::new(600, 350)),
        ("tabs", || Box::new(Tabs::new(&["One", "Two", "Three"], "t")), Rect::new(0, 100, 1272, 100), Point::new(600, 150)),
        ("toggle", || Box::new(Toggle::new("Night", false, "n")), Rect::new(0, 300, 1272, 100), Point::new(1150, 350)),
    ]
}

/// The panel after `steps` (each followed by a paint) on a fresh widget, compared with a
/// clean paint of an untouched widget. Returns a description of each difference.
fn panel_after(name: &str, make: Make, b: Rect, steps: &mut dyn FnMut(&mut Rig)) -> Option<String> {
    let mut clean = Rig::new();
    clean.tree.add(None, make(), b);
    clean.paint();
    let mut rig = Rig::new();
    rig.tree.add(None, make(), b);
    rig.paint();
    steps(&mut rig);
    let on_panel = differ(&rig.panel, &clean.panel, None);
    let on_canvas = differ(&rig.canvas, &clean.canvas, None);
    (on_panel > 0 || on_canvas > 0).then(|| format!("{name}: {on_panel} panel pixels and {on_canvas} canvas pixels differ from a clean paint"))
}

/// A contact that ends with `Cancel` (rotation, lost release) must leave the panel as a
/// clean paint of the untouched widget: no pressed button, key, row or preview bubble
/// left behind, no dragged slider value, and every pixel that changed back reached the
/// panel through the damage.
#[test]
fn cancel_leaves_the_panel_like_a_clean_paint() {
    let mut bad = Vec::new();
    for (name, make, b, p) in pressable() {
        let r = panel_after(name, make, b, &mut |rig| {
            for (kind, held) in [(TouchKind::Down, 0), (TouchKind::Move, 0), (TouchKind::Hold, 600), (TouchKind::Hold, 750), (TouchKind::Cancel, 800)] {
                let q = if kind == TouchKind::Move { Point::new(p.x + 3, p.y) } else { p };
                rig.send(&touch(0, kind, q, held));
                rig.paint();
            }
        });
        bad.extend(r);
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// The same through the recogniser and a lost release: slot 0 goes down on the widget,
/// its release is lost, and the next touch lands in slot 0 far away (on nothing). The
/// recogniser ends the old contact with `Cancel`; nothing repaints the whole screen on
/// this path (unlike a rotation).
#[test]
fn lost_release_leaves_the_panel_like_a_clean_paint() {
    let far = Point::new(1265, 5);
    let mut bad = Vec::new();
    for (name, make, b, p) in pressable() {
        assert!(!b.contains(far));
        let r = panel_after(name, make, b, &mut |rig| {
            let mut rec = GestureRecognizer::new(ScreenSpace { panel: SCREEN, rotation: Rotation::Portrait });
            rec.double_tap = DoubleTapMode::Off;
            for f in [fr(0, vec![c(0, p, TouchPhase::Down)]), fr(700, vec![c(0, far, TouchPhase::Down)]), fr(760, vec![c(0, far, TouchPhase::Up)])] {
                for e in rec.feed_events(&f) {
                    rig.send(&e);
                }
                rig.paint();
            }
        });
        bad.extend(r);
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// Two thumbs on the keyboard: cancelling slot 1 (its release was lost) leaves slot 0's
/// press alone, and slot 0's lift types its key.
#[test]
fn cancel_of_one_thumb_keeps_the_other_thumbs_key() {
    let mut kb = Keyboard::new();
    let q = key_point(|s| s.label == "q");
    let p = key_point(|s| s.label == "p");
    kb.event(&touch(0, TouchKind::Down, q, 0), KB);
    kb.event(&touch(1, TouchKind::Down, p, 0), KB);
    kb.event(&touch(1, TouchKind::Cancel, p, 50), KB);
    kb.event(&touch(0, TouchKind::Up, q, 80), KB);
    assert_eq!(kb.take_actions(), vec![KeyAction::Char("q".into())]);
}

// ---- Hold timers after a gap in input time ----

/// Backspace is held; the next input frame carries a time one hour later (the release
/// after the device slept with the finger resting, or after the input clock (evdev
/// timestamps, CLOCK_REALTIME by default) was set forward while the key was held). The
/// recogniser replays every missed 150 ms repeat before the lift, so one frame deletes
/// about 24 000 characters. Missed repeats should be dropped, not replayed (a held key
/// repeats while time passes on the panel, not afterwards).
#[test]
#[ignore = "verifier: hold timers replay every missed repeat after an input-time gap (24000 deletes from one frame)"]
fn held_key_after_a_long_input_gap_does_not_burst() {
    let mut rec = GestureRecognizer::new(ScreenSpace { panel: SCREEN, rotation: Rotation::Portrait });
    let mut tree = WidgetTree::new();
    let kb = tree.add(None, Box::new(Keyboard::new()), KB);
    let del = key_point(|s| s.label == "del");
    for e in rec.feed_events(&fr(0, vec![c(0, del, TouchPhase::Down)])) {
        tree.dispatch(&e);
    }
    let t0 = Instant::now();
    let ev = rec.feed_events(&fr(3_600_000, vec![c(0, del, TouchPhase::Up)]));
    let holds = ev.iter().filter(|e| matches!(e, UiEvent::Touch(t) if t.kind == TouchKind::Hold)).count();
    let mut deletes = 0;
    for e in &ev {
        if let Some((_, EventResult::Action(_))) = tree.dispatch(e) {
            deletes += tree.get_mut::<Keyboard>(kb).unwrap().take_actions().iter().filter(|a| **a == KeyAction::Backspace).count();
        }
    }
    assert!(holds <= 10 && deletes <= 10, "one frame after a 1 h gap: {holds} Hold notifications, {deletes} Backspace actions ({:?})", t0.elapsed());
}

// ---- Overlays and repaints underneath ----

/// A layer above a widget (toast, dialog, popup) stays on the panel when the widget below
/// repaints. `WidgetTree::paint_node` clears and repaints only the dirty widget; a layer
/// above it that is not dirty is not repainted, so the widget's background fill and
/// content overwrite the overlay in the canvas, and the damage sends that to the panel.
#[test]
#[ignore = "verifier: repainting a widget erases the overlay layers above it (toast, dialog) on the panel"]
fn a_layer_above_survives_a_repaint_of_the_widget_below() {
    let mut bad = Vec::new();
    // 1. A status label under a toast changes its text (like the demo's status line).
    {
        let mut rig = Rig::new();
        let label = rig.tree.add(None, Box::new(Label::left("Status: idle", 40.0)), Rect::new(0, 1380, 1272, 200));
        let frame = Toast::frame(SCREEN);
        rig.tree.add_timed(Box::new(Toast::new("Book added to the library")), frame, Duration::from_secs(60));
        rig.paint();
        let before = rig.panel.clone();
        rig.tree.get_mut::<Label>(label).unwrap().text = "Status: syncing".into();
        rig.tree.mark_dirty(label, ink_device::RefreshClass::Gl16);
        rig.paint();
        let n = differ(&rig.panel, &before, Some(frame));
        if n > 0 {
            bad.push(format!("label under a toast: {n} toast pixels changed on the panel"));
        }
    }
    // 2. A list row under a toast is pressed and released (finger outside the toast).
    {
        let mut rig = Rig::new();
        let items = (0..20).map(|i| ListItem::new(format!("Book {i}"), format!("b{i}"))).collect();
        rig.tree.add(None, Box::new(List::new(items, 120)), Rect::new(0, 0, 1272, 1696));
        let frame = Toast::frame(SCREEN);
        rig.tree.add_timed(Box::new(Toast::new("Download finished")), frame, Duration::from_secs(60));
        rig.paint();
        let before = rig.panel.clone();
        let p = Point::new(40, frame.y + 20);
        assert!(!frame.contains(p));
        rig.send(&touch(0, TouchKind::Down, p, 0));
        rig.paint();
        rig.send(&touch(0, TouchKind::Up, p, 80));
        rig.paint();
        let n = differ(&rig.panel, &before, Some(frame));
        if n > 0 {
            bad.push(format!("list row pressed under a toast: {n} toast pixels changed on the panel"));
        }
    }
    // 3. A progress bar under an open dialog advances (a download in the background).
    {
        let mut rig = Rig::new();
        let frame = Dialog::frame(SCREEN);
        let bar = rig.tree.add(None, Box::new(ProgressBar::new(0.2)), Rect::new(0, frame.y + 100, 1272, 60));
        rig.tree.add(None, Box::new(Dialog { title: "Delete book?".into(), body: "This cannot be undone.".into(), buttons: vec![("Cancel".into(), "c".into())] }), frame);
        rig.paint();
        let before = rig.panel.clone();
        rig.tree.get_mut::<ProgressBar>(bar).unwrap().value = 0.6;
        rig.tree.mark_dirty(bar, ink_device::RefreshClass::Du);
        rig.paint();
        let n = differ(&rig.panel, &before, Some(frame));
        if n > 0 {
            bad.push(format!("progress bar under a dialog: {n} dialog pixels changed on the panel"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

// ---- Shapes and text inside the bounds at normal sizes ----

fn long_widgets() -> Vec<(&'static str, Box<dyn Widget>)> {
    let long = "Synchronise reading progress and highlights with the server now";
    let mut ti = TextInput::new(long, "in");
    ti.set_text(&long.repeat(3));
    ti.focused = true;
    let mut sv = ScrollView::new();
    sv.push(Box::new(Label::new(long, 40.0)), 60);
    sv.push(Box::new(Button::new(long, "b")), 120);
    let mut bar = ProgressBar::new(0.4);
    bar.marks = vec![0.0, 0.1, 0.5, 0.99, 1.0];
    bar.label = Some(long.into());
    vec![
        ("label", Box::new(Label::new(long, 40.0))),
        ("label-left", Box::new(Label::left(long, 40.0))),
        ("button", Box::new(Button::new(long, "b"))),
        ("button-icon", Box::new(Button::new(long, "b").with_icon(Icon::Bookmark))),
        ("icon-view", Box::new(IconView::new(Icon::Wifi, "w"))),
        (
            "list",
            Box::new(List::new(
                (0..5).map(|i| ListItem::new(format!("{i} {long}"), "r").subtitle(long).detail(long).icon(Icon::Bookmark).checked(true)).collect(),
                120,
            )),
        ),
        ("menu", Box::new(Menu::new(vec![(long.into(), "a".into()), (long.into(), "b".into())]))),
        ("tabs", Box::new(Tabs::new(&[long, long, long], "t"))),
        ("slider", Box::new(Slider::new(long, 0.0, 1e9, 1.0, 123456789.0, "s"))),
        ("toggle", Box::new(Toggle::new(long, true, "t"))),
        ("progress", Box::new(bar)),
        ("dialog", Box::new(Dialog { title: long.into(), body: long.repeat(10), buttons: vec![(long.into(), "a".into()), (long.into(), "b".into()), (long.into(), "c".into())] })),
        ("toast", Box::new(Toast::new(long))),
        ("input", Box::new(ti)),
        ("popup", Box::new(Popup { title: long.into(), text: long.repeat(10), buttons: vec![(long.into(), "c".into())], spans: vec![] })),
        ("sheet", Box::new(BottomSheet { title: long.into(), taken: false })),
        ("scroll-view", Box::new(sv)),
        ("textblock", Box::new(TextBlock::new(long.repeat(10), 36.0).hyphenate("en"))),
        ("app-bar", Box::new(AppBar::new(long).with_subtitle(long).with_back().action(BarAction::icon(Icon::Search, "s")).action(BarAction::text(long, "d")))),
        ("nav-bar", Box::new(NavBar::new(vec![NavItem::new(Icon::Home, long, "h"), NavItem::new(Icon::Library, long, "l"), NavItem::new(Icon::Chart, long, "c")]).with_selected(1))),
        ("segmented", Box::new(Segmented::new(&[long, long, long], "seg").with_selected(2))),
        ("chips", Box::new(Chips::new(&[long, long, long, long], "c").with_selected(0))),
        ("progress-thin", Box::new(ProgressBar::thin(0.6).with_marks(vec![0.0, 0.2, 0.9, 1.0]).with_label(long).with_caption(long))),
        ("ring", Box::new(RingProgress::new(1.0))),
        ("card", Box::new(Card::new(long, "card").subtitle(long).body(long.repeat(10)).cover(Cover::placeholder(long)).progress(0.4))),
        ("list-covers", Box::new(List::new(
            (0..4).map(|i| ListItem::new(format!("{i} {long}"), "r").subtitle(long).cover(Cover::placeholder(long)).progress(0.5)).collect(),
            160,
        ))),
        ("button-filled", Box::new(Button::filled(long, "r").with_icon(Icon::Book))),
        ("keyboard", Box::new(Keyboard::new())),
    ]
}

/// Natural size of each widget in `long_widgets` (the frames the gallery and the widget
/// helpers give them).
fn natural(name: &str) -> Size {
    let r = |r: Rect| Size::new(r.w, r.h);
    match name {
        "label" | "label-left" => Size::new(600, 60),
        "button" | "button-icon" => Size::new(400, 120),
        "icon-view" => Size::new(150, 150),
        "list" => Size::new(1272, 800),
        "menu" => Size::new(600, 220),
        "tabs" | "toggle" => Size::new(1272, 100),
        "slider" => Size::new(1000, 150),
        "progress" => Size::new(1272, 60),
        "dialog" => r(Dialog::frame(SCREEN)),
        "toast" => r(Toast::frame(SCREEN)),
        "input" => Size::new(1000, 110),
        "popup" => Size::new(900, 420),
        "sheet" => r(BottomSheet::frame(SCREEN, 763)),
        "scroll-view" => Size::new(800, 600),
        "textblock" => Size::new(600, 400),
        "keyboard" => Size::new(1272, 696),
        "app-bar" => Size::new(1272, 136),
        "nav-bar" => Size::new(1272, 144),
        "segmented" => Size::new(900, 96),
        "chips" => Size::new(1000, 300),
        "progress-thin" => Size::new(1000, 90),
        "ring" => Size::new(120, 120),
        "card" => Size::new(1200, 360),
        "list-covers" => Size::new(1272, 800),
        "button-filled" => Size::new(400, 104),
        _ => panic!("{name}"),
    }
}

/// Every widget, with long texts, at its natural size: nothing is drawn outside the
/// bounds (text is cut by the clip; shapes are computed from the bounds). Undersized
/// bounds are a different matter (see the cycle-2 report).
#[test]
fn widgets_with_long_texts_stay_inside_natural_bounds() {
    let theme = Theme::day();
    let mut fonts = Fonts::load();
    let mut spills = Vec::new();
    for (name, widget) in long_widgets() {
        let n = natural(name);
        let b = Rect::new(100, 100, n.w, n.h);
        let mut canvas = Canvas::new(1500, 1000);
        let mut tree = WidgetTree::new();
        tree.add(None, widget, b);
        tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        let outside = (0..1000i32)
            .flat_map(|y| (0..1500i32).map(move |x| (x, y)))
            .filter(|(x, y)| !b.contains(Point::new(*x, *y)) && canvas.get(*x, *y) != Some(255))
            .count();
        if outside > 0 {
            spills.push(format!("{name} at {}x{}: {outside} pixels outside", n.w, n.h));
        }
    }
    assert!(spills.is_empty(), "{spills:#?}");
}

/// Cancel (and a Cancel for a slot that never went down) on every widget at 0x0, 1x1,
/// thin and off-canvas bounds: no panic.
#[test]
fn cancel_on_degenerate_bounds_does_not_panic() {
    let sizes = [Rect::new(10, 10, 0, 0), Rect::new(10, 10, 1, 1), Rect::new(0, 0, 3, 200), Rect::new(0, 0, 200, 3), Rect::new(-500, -500, 20, 20)];
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut fonts = Fonts::load();
    let theme = Theme::day();
    let mut failures = Vec::new();
    for b in sizes {
        for (name, w) in long_widgets() {
            let res = catch_unwind(AssertUnwindSafe(|| {
                let mut canvas = Canvas::new(300, 300);
                let mut tree = WidgetTree::new();
                tree.add(None, w, b);
                let p = center(b);
                for e in [touch(7, TouchKind::Cancel, p, 0), touch(0, TouchKind::Down, p, 0), touch(0, TouchKind::Hold, p, 600), touch(0, TouchKind::Cancel, p, 700)] {
                    tree.dispatch(&e);
                    tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
                }
            }));
            if res.is_err() {
                failures.push(format!("{name} at {b:?}"));
            }
        }
    }
    std::panic::set_hook(quiet);
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---- Rotation through the pump while dragging a slider ----

struct Queue(std::collections::VecDeque<Option<ink_device::InputEvent>>);

impl ink_device::Input for Queue {
    fn next_event(&mut self, _timeout: Option<Duration>) -> ink_device::Result<Option<ink_device::InputEvent>> {
        Ok(self.0.pop_front().unwrap_or(Some(ink_device::InputEvent::Quit)))
    }
}

/// A slider is dragged; the screen rotates before the finger lifts. The slider goes back
/// to its value before the drag, reports nothing, and the stale lift adds nothing.
#[test]
fn rotation_while_dragging_a_slider_restores_its_value() {
    let (mut dev, _probe) = ink_device::headless::open(ink_device::headless::HeadlessOptions::default());
    let mut rec = GestureRecognizer::new(ScreenSpace { panel: dev.screen.panel_size(), rotation: dev.screen.rotation() });
    let mut tree = WidgetTree::new();
    let b = Rect::new(100, 300, 1000, 150);
    let id = tree.add(None, Box::new(Slider::new("Size", 1.0, 10.0, 1.0, 5.0, "s")), b);
    let (_, track, _) = Slider::parts(b);
    let y = center(track).y;
    let ev = |ms, x, phase| Some(ink_device::InputEvent::Touch(fr(ms, vec![TouchContact { slot: 0, x, y, phase }])));
    let mut input = Queue(vec![ev(0, track.x + 10, TouchPhase::Down), ev(50, track.right() - 10, TouchPhase::Move)].into());
    let mut actions = Vec::new();
    let mut pump = |rec: &mut GestureRecognizer, input: &mut Queue, screen: &dyn ink_device::Screen, tree: &mut WidgetTree| {
        for e in ink_ui::pump::next_events(rec, input, screen, None).unwrap() {
            if let Some((_, EventResult::Action(a))) = tree.dispatch(&e) {
                actions.push(a);
            }
        }
    };
    pump(&mut rec, &mut input, dev.screen.as_ref(), &mut tree);
    pump(&mut rec, &mut input, dev.screen.as_ref(), &mut tree);
    assert!(tree.get_mut::<Slider>(id).unwrap().value > 8.0, "dragged to the right end");
    dev.screen.set_rotation(Rotation::CounterClockwise).unwrap();
    input.0.push_back(ev(200, track.right() - 10, TouchPhase::Up));
    pump(&mut rec, &mut input, dev.screen.as_ref(), &mut tree);
    pump(&mut rec, &mut input, dev.screen.as_ref(), &mut tree);
    let s = tree.get_mut::<Slider>(id).unwrap();
    assert_eq!((s.value, s.refresh_class()), (5.0, ink_device::RefreshClass::Gl16), "value restored, not dragging");
    assert!(actions.is_empty(), "{actions:?}");
}

// ---- Ellipsis with hostile text ----

/// `PaintCx::ellipsize` with hostile texts and widths: the result is the text itself, or
/// a prefix cut at a grapheme boundary plus "…"; it fits whenever "…" alone fits; and a
/// text that is one huge grapheme (a base letter with 3000 combining marks) is handled in
/// about the time of a few shapings.
#[test]
fn ellipsize_keeps_its_contract_for_hostile_text() {
    let theme = Theme::day();
    let mut fonts = Fonts::load();
    let mut canvas = Canvas::new(10, 10);
    let mut cx = PaintCx::new(&mut canvas, &mut fonts, &theme);
    let zalgo = format!("Z{}", "\u{301}\u{316}\u{334}".repeat(1000));
    let texts = [
        "".to_string(),
        "…".to_string(),
        " ".repeat(50),
        "plain words that are far too long for the box".to_string(),
        "Supercalifragilisticexpialidocious".repeat(20),
        "👨\u{200d}👩\u{200d}👧\u{200d}👦".repeat(30),
        "🇩🇰🇩🇪🇫🇷🇬🇧".repeat(20),
        "שלום עולם 123 مرحبا بالعالم ".repeat(10),
        "日本語の長いタイトルとサブタイトル".repeat(10),
        "e\u{301}".repeat(200),
        "\u{200b}".repeat(500) + "end",
        "tab\tnew\nline\r\nmixed ".repeat(20),
        zalgo.clone(),
    ];
    let mut bad = Vec::new();
    let ell_w = cx.text_width(Style::Regular, 40.0, "…");
    for t in &texts {
        let bounds: Vec<usize> = t.grapheme_indices(true).map(|(i, _)| i).chain([t.len()]).collect();
        for max in [-1.0f32, 0.0, 1.0, 5.0, 30.0, 100.0, 300.0, 2000.0, f32::NAN, f32::INFINITY] {
            let s = cx.ellipsize(t, 40.0, Style::Regular, max);
            if s == *t {
                continue;
            }
            let Some(stem) = s.strip_suffix('…') else {
                bad.push(format!("{:?} at {max}: {s:?} has no ellipsis", &t[..t.len().min(20)]));
                continue;
            };
            if !t.starts_with(stem) {
                bad.push(format!("{:?} at {max}: {s:?} is not a prefix plus an ellipsis", &t[..t.len().min(20)]));
            } else if !bounds.contains(&stem.len()) {
                bad.push(format!("{:?} at {max}: cut inside a grapheme at byte {}", &t[..t.len().min(20)], stem.len()));
            }
            let w = cx.text_width(Style::Regular, 40.0, &s);
            if ell_w <= max && w > max {
                bad.push(format!("{:?} at {max}: {s:?} is {w} px wide", &t[..t.len().min(20)]));
            }
        }
    }
    // One huge grapheme: a few shapings, not one per mark.
    let t0 = Instant::now();
    cx.text_width(Style::Regular, 40.0, &zalgo);
    let one = t0.elapsed();
    let t0 = Instant::now();
    let s = cx.ellipsize(&zalgo, 40.0, Style::Regular, 10.0);
    let took = t0.elapsed();
    assert_eq!(s, "…");
    assert!(took < one * 10 + Duration::from_millis(50), "{took:?} vs one shaping {one:?}");
    assert!(bad.is_empty(), "{bad:#?}");
}

/// Text drawn through `PaintCx` at hostile positions (NaN, ±1e9, far off the canvas) and
/// sizes (0, negative, NaN) inside and outside a tree paint: no panic, and nothing drawn
/// outside the widget bounds in the tree paint. (Not covered, reported as minor: in debug
/// builds positions beyond ±2^31 px overflow `i32` in `Fonts::draw_run_clipped`
/// (fonts.rs:552) and an infinite size overflows in `raster::rasterize` (raster.rs:239).)
#[test]
fn text_at_hostile_positions_and_sizes_does_not_panic() {
    struct Hostile;
    impl Widget for Hostile {
        fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
            for v in [f32::NAN, 1e9, -1e9, 5000.0, -5000.0] {
                cx.draw_text(Style::Bold, 40.0, v, 100.0, "Hostile", 0);
                cx.draw_text(Style::Italic, 40.0, 100.0, v, "Hostile", 0);
                cx.draw_text_clipped(Style::Regular, 40.0, v, v, "Hostile", 0, b);
            }
            for px in [0.0f32, -3.0, f32::NAN] {
                cx.draw_text_end(Style::Regular, px, 60.0, 100.0, "Hostile", 0);
                cx.text_left(b, "Hostile", px, Style::Regular, 0);
                cx.text_centered(b, "Hostile", px, Style::Regular, 0);
            }
        }
    }
    let theme = Theme::day();
    let mut fonts = Fonts::load();
    let mut canvas = Canvas::new(400, 300);
    let b = Rect::new(50, 50, 200, 100);
    let mut tree = WidgetTree::new();
    tree.add(None, Box::new(Hostile), b);
    tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
    let outside = (0..300).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|(x, y)| !b.contains(Point::new(*x, *y)) && canvas.get(*x, *y) != Some(255)).count();
    assert_eq!(outside, 0);
    // Outside a tree paint (no clip).
    Hostile.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), Rect::new(0, 0, 400, 300));
}
