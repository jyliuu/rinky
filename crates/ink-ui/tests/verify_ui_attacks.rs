//! Verifier attacks on `ink-ui` (wave A, cycle 1): hostile touch input and timing,
//! dropped multi-touch slots, rotation during a touch, keyboard repeat while rotating,
//! degenerate widget sizes and very long text.
//!
//! The six defects found in cycle 1 are fixed; their tests run with the others.

use ink_device::headless::{self, HeadlessOptions};
use ink_device::{Canvas, Input, InputEvent, Point, Rect, Rotation, Size, Timestamp, TouchContact, TouchFrame, TouchPhase};
use ink_text::Fonts;
use ink_ui::keyboard::KeyAction;
use ink_ui::{
    pump, AppBar, BarAction, BottomSheet, Button, Card, Chips, Cover, NavBar, NavItem, RingProgress, Segmented, Dialog, DoubleTapMode, EventResult, Gesture, GestureRecognizer, Icon, IconView, Keyboard,
    Label, List, ListItem, Menu, PaintCx, Popup, ProgressBar, Scrim, ScreenSpace, ScrollView, Slider, Tabs, TextBlock,
    TextInput, Theme, Toast, Toggle, Touch, TouchKind, UiEvent, Widget, WidgetTree,
};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

const PANEL: Size = Size::new(1272, 1696);

fn rec() -> GestureRecognizer {
    GestureRecognizer::new(ScreenSpace { panel: PANEL, rotation: Rotation::Portrait })
}

fn c(slot: u8, x: i32, y: i32, phase: TouchPhase) -> TouchContact {
    TouchContact { slot, x, y, phase }
}

fn fr(ms: u64, contacts: Vec<TouchContact>) -> TouchFrame {
    TouchFrame { time: Timestamp::from_millis(ms), contacts }
}

fn gestures(ev: &[UiEvent]) -> Vec<Gesture> {
    ev.iter()
        .filter_map(|e| match e {
            UiEvent::Gesture(g) => Some(*g),
            _ => None,
        })
        .collect()
}

fn taps(g: &[Gesture]) -> usize {
    g.iter().filter(|g| matches!(g, Gesture::Tap { .. })).count()
}

// ---- Rapid taps and same-frame taps ----

/// 20 taps per second (50 ms apart), alternating between two far-apart points, with
/// 20 ms contacts and with down+up in one SYN frame: every tap is recognised.
#[test]
fn twenty_taps_per_second_are_all_recognised() {
    for quick in [false, true] {
        let mut r = rec();
        let mut all = Vec::new();
        for i in 0..40u64 {
            let (x, y) = if i % 2 == 0 { (200, 300) } else { (1000, 1400) };
            let t = i * 50;
            if quick {
                all.extend(r.feed(&fr(t, vec![c(0, x, y, TouchPhase::DownUp)])));
            } else {
                all.extend(r.feed(&fr(t, vec![c(0, x, y, TouchPhase::Down)])));
                all.extend(r.feed(&fr(t + 20, vec![c(0, x, y, TouchPhase::Up)])));
            }
        }
        all.extend(gestures(&r.tick(Timestamp::from_millis(10_000))));
        assert_eq!(taps(&all), 40, "quick={quick}: {all:?}");
        assert!(all.iter().all(|g| matches!(g, Gesture::Tap { .. })), "far apart: no double taps: {all:?}");
        assert_eq!(r.active_contacts(), 0);
    }
}

/// 20 taps per second on one spot: After mode gives every tap plus a double tap per pair;
/// Delayed mode gives one double tap per pair and no lost or extra single taps.
#[test]
fn twenty_taps_per_second_on_one_spot_in_every_double_tap_mode() {
    for mode in [DoubleTapMode::After, DoubleTapMode::Delayed, DoubleTapMode::Off] {
        let mut r = rec();
        r.double_tap = mode;
        let mut all = Vec::new();
        for i in 0..40u64 {
            all.extend(r.feed(&fr(i * 50, vec![c(0, 600, 800, TouchPhase::DownUp)])));
        }
        all.extend(gestures(&r.tick(Timestamp::from_millis(10_000))));
        let doubles = all.iter().filter(|g| matches!(g, Gesture::DoubleTap { .. })).count();
        match mode {
            DoubleTapMode::After => assert_eq!((taps(&all), doubles), (40, 20), "{all:?}"),
            DoubleTapMode::Delayed => assert_eq!((taps(&all), doubles), (0, 20), "{all:?}"),
            DoubleTapMode::Off => assert_eq!((taps(&all), doubles), (40, 0), "{all:?}"),
        }
    }
}

/// Down and up of one finger in one frame is a tap in every rotation, at the rotated
/// position.
#[test]
fn same_frame_tap_in_every_rotation() {
    let p = Point::new(100, 200);
    for rot in [Rotation::Portrait, Rotation::Clockwise, Rotation::UpsideDown, Rotation::CounterClockwise] {
        let mut r = GestureRecognizer::new(ScreenSpace { panel: PANEL, rotation: rot });
        r.double_tap = DoubleTapMode::Off;
        let g = r.feed(&fr(0, vec![c(0, p.x, p.y, TouchPhase::DownUp)]));
        assert_eq!(g, vec![Gesture::Tap { pos: rot.from_panel(p, PANEL) }], "{rot:?}");
    }
}

/// A frame where one finger goes down+up while another finger rests: the resting finger
/// is paired (two fingers on the glass), so neither gives a single-finger gesture, and
/// nothing is left behind once both lifted.
#[test]
fn same_frame_tap_while_another_finger_rests_leaves_no_state() {
    let mut r = rec();
    r.feed(&fr(0, vec![c(0, 300, 300, TouchPhase::Down)]));
    r.feed(&fr(100, vec![c(1, 900, 900, TouchPhase::DownUp)]));
    r.feed(&fr(200, vec![c(0, 300, 300, TouchPhase::Up)]));
    assert_eq!(r.active_contacts(), 0);
    assert_eq!(r.next_deadline(), None);
    // A plain tap afterwards works.
    r.double_tap = DoubleTapMode::Off;
    assert_eq!(r.feed(&fr(1000, vec![c(0, 500, 500, TouchPhase::DownUp)])), vec![Gesture::Tap { pos: Point::new(500, 500) }]);
}

/// Two fingers whose touch-down and touch-up both arrive in one SYN frame (the PW12
/// chip's short taps, two fingers at once). The gesture contract says two fingers give `TwoFingerTap` or `Pinch` and "neither
/// finger gives a single-finger gesture". Before the fix each contact was handled alone
/// (down, up, down, up: two `Tap`s); now all downs of a frame come before its lifts.
#[test]
fn two_fingers_down_and_up_in_one_frame_are_a_two_finger_tap() {
    let mut r = rec();
    let g = r.feed(&fr(0, vec![c(0, 400, 800, TouchPhase::DownUp), c(1, 800, 800, TouchPhase::DownUp)]));
    assert_eq!(g, vec![Gesture::TwoFingerTap { pos: Point::new(600, 800) }]);
}

/// Contacts in slots up to 9 and a frame that lists the same slot twice do not panic
/// and do not leave contacts behind.
#[test]
fn many_slots_and_duplicate_slots_in_one_frame() {
    let mut r = rec();
    let downs: Vec<TouchContact> = (0..10).map(|s| c(s, 100 + s as i32 * 100, 500, TouchPhase::Down)).collect();
    r.feed(&fr(0, downs));
    assert_eq!(r.active_contacts(), 10);
    let ups: Vec<TouchContact> = (0..10).map(|s| c(s, 100 + s as i32 * 100, 500, TouchPhase::Up)).collect();
    r.feed(&fr(100, ups));
    assert_eq!(r.active_contacts(), 0);
    r.feed(&fr(200, vec![c(3, 10, 10, TouchPhase::Down), c(3, 20, 20, TouchPhase::Down)]));
    assert_eq!(r.active_contacts(), 1, "a slot is one contact");
    r.feed(&fr(300, vec![c(3, 20, 20, TouchPhase::Up), c(3, 20, 20, TouchPhase::Up)]));
    assert_eq!(r.active_contacts(), 0);
    // Coordinates far off the panel.
    r.feed(&fr(400, vec![c(0, i32::MAX / 2, i32::MIN / 2, TouchPhase::DownUp)]));
}

// ---- Dropped multi-touch slots ----

/// Two fingers touch; the second finger's release is lost (e.g. SYN_DROPPED on a busy
/// reader); the first lifts. Later single taps must still be taps. Before the fix the
/// stale second contact (already used by the finished two-finger gesture) paired with
/// every new finger, so no tap was recognised again; now a consumed contact never pairs.
#[test]
fn lost_release_of_a_second_finger_does_not_disable_taps() {
    let mut r = rec();
    r.double_tap = DoubleTapMode::Off;
    r.feed(&fr(0, vec![c(0, 400, 800, TouchPhase::Down), c(1, 800, 800, TouchPhase::Down)]));
    // Slot 1's release never arrives; slot 0 lifts.
    r.feed(&fr(100, vec![c(0, 400, 800, TouchPhase::Up)]));
    let mut got = Vec::new();
    for i in 0..5u64 {
        let t = 1000 + i * 1000;
        got.extend(r.feed(&fr(t, vec![c(0, 300, 300, TouchPhase::Down)])));
        got.extend(r.feed(&fr(t + 60, vec![c(0, 300, 300, TouchPhase::Up)])));
    }
    assert_eq!(taps(&got), 5, "taps after a lost release: {got:?}");
}

/// One finger's release is lost. The next touch in the same slot replaces the stale
/// contact, so taps work again from the next touch on.
#[test]
fn lost_release_of_the_only_finger_recovers_on_the_next_touch() {
    let mut r = rec();
    r.double_tap = DoubleTapMode::Off;
    r.feed(&fr(0, vec![c(0, 400, 800, TouchPhase::Down)]));
    // No release. Holds keep firing for the stale contact until the next touch.
    let holds = r.tick(Timestamp::from_millis(3000)).iter().filter(|e| matches!(e, UiEvent::Touch(t) if t.kind == TouchKind::Hold)).count();
    assert!(holds > 10, "{holds}");
    let g = r.feed(&fr(4000, vec![c(0, 300, 300, TouchPhase::DownUp)]));
    assert_eq!(g, vec![Gesture::Tap { pos: Point::new(300, 300) }]);
    assert_eq!(r.active_contacts(), 0);
    assert_eq!(r.next_deadline(), None);
}

/// A lost touch-down (Move and Up for a slot never seen) is ignored.
#[test]
fn lost_touch_down_is_ignored() {
    let mut r = rec();
    assert!(r.feed_events(&fr(0, vec![c(2, 400, 800, TouchPhase::Move)])).is_empty());
    assert!(r.feed_events(&fr(10, vec![c(2, 400, 800, TouchPhase::Up)])).is_empty());
    assert_eq!(r.active_contacts(), 0);
}

// ---- Rotation during a touch, keyboard repeat while rotating ----

/// Input that plays queued items; `None` is a timeout with no input.
struct Queue(VecDeque<Option<InputEvent>>);

impl Input for Queue {
    fn next_event(&mut self, _timeout: Option<Duration>) -> ink_device::Result<Option<InputEvent>> {
        Ok(self.0.pop_front().unwrap_or(Some(InputEvent::Quit)))
    }
}

fn touch_ev(ms: u64, slot: u8, x: i32, y: i32, phase: TouchPhase) -> Option<InputEvent> {
    Some(InputEvent::Touch(fr(ms, vec![c(slot, x, y, phase)])))
}

/// Holding backspace repeats through Hold notifications. The screen rotates while the
/// finger is still down (e.g. an orientation change from another source): the repeat
/// stops at once, the stale release adds nothing, and the next tap is mapped with the
/// new rotation and types one key.
#[test]
fn keyboard_repeat_stops_when_the_screen_rotates() {
    let (mut dev, _probe) = headless::open(HeadlessOptions::default());
    let space = ScreenSpace { panel: dev.screen.panel_size(), rotation: dev.screen.rotation() };
    let mut r = GestureRecognizer::new(space);
    let mut tree = WidgetTree::new();
    let kb_rect = Rect::new(0, 1000, 1272, 696);
    let kb = tree.add(None, Box::new(Keyboard::new()), kb_rect);
    let del = {
        let k = tree.get_mut::<Keyboard>(kb).unwrap();
        let area = k.keys_area(kb_rect);
        let (rect, _) = k.layout.key_rects(area).into_iter().find(|(_, s)| s.label == "del").unwrap();
        Point::new(rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2)
    };
    let mut input = Queue(VecDeque::from(vec![touch_ev(0, 0, del.x, del.y, TouchPhase::Down)]));
    let mut deletes = 0;
    let run = |r: &mut GestureRecognizer, input: &mut Queue, screen: &dyn ink_device::Screen, tree: &mut WidgetTree| {
        let ev = pump::next_events(r, input, screen, None).unwrap();
        let mut n = 0;
        for e in &ev {
            if let Some((_, EventResult::Action(_))) = tree.dispatch(e) {
                n += tree.get_mut::<Keyboard>(kb).unwrap().take_actions().iter().filter(|a| **a == KeyAction::Backspace).count();
            }
        }
        n
    };
    deletes += run(&mut r, &mut input, dev.screen.as_ref(), &mut tree);
    // Four repeats from the hold timer (the recogniser uses input time; tick directly).
    for e in r.tick(Timestamp::from_millis(900)) {
        if let Some((_, EventResult::Action(_))) = tree.dispatch(&e) {
            deletes += tree.get_mut::<Keyboard>(kb).unwrap().take_actions().len();
        }
    }
    assert_eq!(deletes, 4, "400, 550, 700, 850 ms");
    // Rotate while the finger is down; the next pump call sees the new rotation.
    dev.screen.set_rotation(Rotation::Clockwise).unwrap();
    input.0.push_back(None);
    deletes += run(&mut r, &mut input, dev.screen.as_ref(), &mut tree);
    assert_eq!(r.active_contacts(), 0, "the rotation dropped the touch");
    assert_eq!(r.next_deadline(), None, "no more hold timers: the repeat stopped");
    // The old finger lifts (panel coordinates of the old position).
    input.0.push_back(touch_ev(5000, 0, del.x, del.y, TouchPhase::Up));
    deletes += run(&mut r, &mut input, dev.screen.as_ref(), &mut tree);
    assert_eq!(deletes, 4, "no delete after the rotation");
}

/// A button takes a touch-down (pressed, inverted). The screen rotates before the finger
/// lifts. The button must not stay pressed after the finger is gone: the pump ends the
/// touch with `TouchKind::Cancel`.
#[test]
fn rotation_during_a_touch_does_not_leave_a_button_pressed() {
    let (mut dev, _probe) = headless::open(HeadlessOptions::default());
    let space = ScreenSpace { panel: dev.screen.panel_size(), rotation: dev.screen.rotation() };
    let mut r = GestureRecognizer::new(space);
    let mut tree = WidgetTree::new();
    let b = tree.add(None, Box::new(Button::new("OK", "ok")), Rect::new(500, 700, 300, 200));
    let mut input = Queue(VecDeque::from(vec![touch_ev(0, 0, 600, 800, TouchPhase::Down)]));
    for e in pump::next_events(&mut r, &mut input, dev.screen.as_ref(), None).unwrap() {
        tree.dispatch(&e);
    }
    // Pressed feedback comes with the Press timer (the finger rested 150 ms).
    for e in r.tick(Timestamp::from_millis(160)) {
        tree.dispatch(&e);
    }
    assert!(tree.get_mut::<Button>(b).unwrap().pressed);
    dev.screen.set_rotation(Rotation::UpsideDown).unwrap();
    input.0.push_back(touch_ev(200, 0, 600, 800, TouchPhase::Up));
    input.0.push_back(None);
    for _ in 0..2 {
        for e in pump::next_events(&mut r, &mut input, dev.screen.as_ref(), None).unwrap() {
            tree.dispatch(&e);
        }
    }
    assert!(!tree.get_mut::<Button>(b).unwrap().pressed, "button still drawn pressed after the finger lifted");
}

// ---- Degenerate sizes ----

fn all_widgets() -> Vec<(&'static str, Box<dyn Widget>)> {
    let items: Vec<ListItem> =
        (0..30).map(|i| ListItem::new(format!("Row {i}"), format!("r{i}")).subtitle("sub").detail("1 MB").icon(Icon::Bookmark).checked(true)).collect();
    let mut sv = ScrollView::new();
    sv.push(Box::new(Label::new("a", 30.0)), 50);
    sv.push(Box::new(Label::new("b", 30.0)), 5000);
    let mut ti = TextInput::new("placeholder", "in");
    ti.set_text("some text");
    ti.focused = true;
    vec![
        ("label", Box::new(Label::new("Hello", 40.0))),
        ("label-left", Box::new(Label::left("Hello world", 40.0))),
        ("button", Box::new(Button::new("OK", "ok"))),
        ("button-icon", Box::new(Button::new("", "x").with_icon(Icon::Close))),
        ("button-icon-label", Box::new(Button::new("Go", "x").with_icon(Icon::Search))),
        ("icon", Box::new(IconView::new(Icon::Settings, "gear"))),
        ("list", Box::new(List::new(items, 120))),
        ("list-empty", Box::new(List::new(Vec::new(), 120))),
        ("scroll", Box::new(sv)),
        ("scroll-empty", Box::new(ScrollView::new())),
        ("menu", Box::new(Menu::new(vec![("A".into(), "a".into()), ("B".into(), "b".into())]))),
        ("menu-empty", Box::new(Menu::new(Vec::new()))),
        ("tabs", Box::new(Tabs::new(&["One", "Two", "Three"], "t"))),
        ("tabs-empty", Box::new(Tabs::new(&[], "t"))),
        ("slider", Box::new(Slider::new("Size", 1.0, 10.0, 1.0, 5.0, "s"))),
        ("slider-flat", Box::new(Slider::new("Flat", 3.0, 3.0, 0.0, 3.0, "s"))),
        ("toggle", Box::new(Toggle::new("Night", true, "n"))),
        ("progress", Box::new({
            let mut p = ProgressBar::new(0.5);
            p.marks = vec![0.0, 0.5, 1.0];
            p.label = Some("50%".into());
            p
        })),
        ("progress-nan", Box::new(ProgressBar::new(f32::NAN))),
        ("dialog", Box::new(Dialog { title: "T".into(), body: "Body text".into(), buttons: vec![("OK".into(), "ok".into())] })),
        ("dialog-empty", Box::new(Dialog { title: String::new(), body: String::new(), buttons: Vec::new() })),
        ("toast", Box::new(Toast::new("toast"))),
        ("input", Box::new(ti)),
        ("popup", Box::new(Popup { title: "P".into(), text: "text".into(), buttons: vec![("Close".into(), "c".into())], spans: vec![] })),
        ("sheet", Box::new(BottomSheet { title: "Aa".into(), taken: false })),
        ("app-bar", Box::new(AppBar::new("Title").with_back().action(BarAction::icon(Icon::Search, "s")).action(BarAction::text("Done", "d")))),
        ("app-bar-empty", Box::new(AppBar::new(""))),
        ("nav-bar", Box::new(NavBar::new(vec![NavItem::new(Icon::Home, "Home", "h"), NavItem::new(Icon::Library, "Library", "l")]))),
        ("nav-bar-empty", Box::new(NavBar::new(Vec::new()))),
        ("segmented", Box::new(Segmented::new(&["A", "B", "C"], "seg"))),
        ("segmented-empty", Box::new(Segmented::new(&[], "seg"))),
        ("chips", Box::new(Chips::new(&["One", "Two"], "c").with_selected(1))),
        ("chips-empty", Box::new(Chips::new(&[], "c"))),
        ("progress-thin", Box::new(ProgressBar::thin(0.3).with_marks(vec![-1.0, 0.5, 2.0]).with_label("30%").with_caption("Chapter"))),
        ("ring", Box::new(RingProgress::new(0.75))),
        ("ring-nan", Box::new(RingProgress::new(f32::NAN))),
        ("card", Box::new(Card::new("Card", "card").subtitle("Sub").body("Body text").cover(Cover::placeholder("Book")).progress(0.4))),
        ("card-empty", Box::new(Card::new("", ""))),
        ("button-filled", Box::new(Button::filled("Read", "r"))),
        ("button-text", Box::new(Button::text("Cancel", "c").disabled())),
        ("scrim", Box::new(Scrim::new("dismiss"))),
        ("textblock", Box::new(TextBlock::new("Hyphenation of a paragraph", 36.0).hyphenate("en"))),
        ("textblock-empty", Box::new(TextBlock::new("", 36.0))),
    ]
}

fn touch(slot: u8, kind: TouchKind, p: Point) -> UiEvent {
    UiEvent::Touch(Touch { slot, kind, pos: p, start: p, held: Duration::from_millis(700), moved: false })
}

/// Runs paint and every kind of event on one widget at `bounds`; returns the panic
/// message if any step panics.
fn exercise(fonts: &mut Fonts, name: &str, w: Box<dyn Widget>, bounds: Rect) -> Option<String> {
    let res = catch_unwind(AssertUnwindSafe(|| {
        let theme = Theme::day();
        let fonts = &mut *fonts;
        let mut canvas = Canvas::new(300, 300);
        let mut tree = WidgetTree::new();
        tree.add(None, w, bounds);
        tree.paint(&mut PaintCx::new(&mut canvas, fonts, &theme));
        let p = Point::new(bounds.x + bounds.w as i32 / 2, bounds.y + bounds.h as i32 / 2);
        let evs = [
            touch(0, TouchKind::Down, p),
            touch(0, TouchKind::Hold, p),
            touch(0, TouchKind::Move, Point::new(p.x + 1, p.y)),
            touch(0, TouchKind::Up, p),
            UiEvent::Gesture(Gesture::Tap { pos: p }),
            UiEvent::Gesture(Gesture::LongPress { pos: p, held: Duration::from_secs(1) }),
            UiEvent::Gesture(Gesture::Swipe { from: p, to: Point::new(p.x, p.y - 500), dir: ink_ui::Direction::Up }),
            UiEvent::Gesture(Gesture::Swipe { from: p, to: Point::new(p.x, p.y + 500), dir: ink_ui::Direction::Down }),
        ];
        for e in &evs {
            tree.dispatch(e);
            tree.paint(&mut PaintCx::new(&mut canvas, fonts, &theme));
        }
    }));
    res.err().map(|e| {
        let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
        format!("{name} at {bounds:?}: {msg}")
    })
}

/// Every widget at 0x0, 1x1 and other degenerate bounds (including off-canvas) paints
/// and handles touches and gestures without panicking.
#[test]
fn widgets_survive_zero_and_one_pixel_bounds() {
    let sizes = [
        Rect::new(10, 10, 0, 0),
        Rect::new(10, 10, 1, 1),
        Rect::new(0, 0, 3, 200),
        Rect::new(0, 0, 200, 3),
        Rect::new(-500, -500, 20, 20),
        Rect::new(290, 290, 100, 100),
    ];
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failures = Vec::new();
    let mut fonts = Fonts::load();
    for b in sizes {
        for (name, w) in all_widgets() {
            if let Some(f) = exercise(&mut fonts, name, w, b) {
                failures.push(f);
            }
        }
    }
    std::panic::set_hook(quiet);
    assert!(failures.is_empty(), "{} panics:\n{}", failures.len(), failures.join("\n"));
}

/// A keyboard in a strip shorter than 30 px (or 20 x 20 px, also off-canvas) takes a
/// press and a hold without panicking. Before the fix `Keyboard::bubble` and the
/// alternates popup computed `self.strip(b) - 6` in u32 (overflow in debug builds, a
/// 4-billion-pixel rectangle in release builds).
#[test]
fn keyboard_in_a_short_strip_handles_press_and_hold() {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut fonts = Fonts::load();
    let off = exercise(&mut fonts, "keyboard", Box::new(Keyboard::new()), Rect::new(-500, -500, 20, 20));
    std::panic::set_hook(quiet);
    assert!(off.is_none(), "{off:?}");
    for h in [24u32, 40, 60] {
        let b = Rect::new(0, 0, 1272, h);
        let r = exercise(&mut fonts, "keyboard", Box::new(Keyboard::new()), b);
        let mut kb = Keyboard::new();
        let area = kb.keys_area(b);
        let rects: Vec<Rect> = kb.layout.key_rects(area).into_iter().map(|(r, _)| r).collect();
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let res = catch_unwind(AssertUnwindSafe(|| {
            for r in rects.iter().filter(|r| r.w > 0 && r.h > 0) {
                let p = Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
                kb.event(&touch(0, TouchKind::Down, p), b);
                kb.event(&touch(0, TouchKind::Hold, p), b);
                kb.event(&touch(0, TouchKind::Up, p), b);
            }
        }));
        std::panic::set_hook(quiet);
        assert!(r.is_none(), "{r:?}");
        assert!(res.is_ok(), "keyboard {h} px tall panicked on press/hold");
    }
}

// ---- Very long text ----

/// A left-aligned label with very long text (a highlight in a notes list, an OPDS
/// summary in a list row, a pasted paragraph) is cut with an ellipsis. Before the fix
/// `PaintCx::ellipsize` popped one char at a time and reshaped the rest each time
/// (quadratic: 32 s for 2200 chars in a debug build). The check is relative so it does
/// not depend on the build profile: cutting a text must cost at most 60 shapings of the
/// full text (galloping plus bisection over grapheme boundaries needs far fewer).
#[test]
fn long_label_text_ellipsizes_fast() {
    let theme = Theme::day();
    let mut fonts = Fonts::load();
    let mut canvas = Canvas::new(600, 100);
    let text = "All work and no play makes Jack a dull boy. ".repeat(50); // 2200 chars
    let t0 = Instant::now();
    for _ in 0..5 {
        fonts.shaped_width(ink_text::Style::Regular, 40.0, &text);
    }
    let one_shape = t0.elapsed() / 5;
    let t0 = Instant::now();
    let mut cx = PaintCx::new(&mut canvas, &mut fonts, &theme);
    let s = cx.ellipsize(&text, 40.0, ink_text::Style::Regular, 500.0);
    let took = t0.elapsed();
    assert!(s.ends_with('…'));
    assert!(took < one_shape * 60, "ellipsize of {} chars took {took:?}; one shaping takes {one_shape:?}", text.len());
}

/// Widgets never draw outside their bounds: the tree clears and refreshes only the
/// bounds (the damage rectangle), so ink outside them would stay on the canvas and reach
/// the panel later as garbage. Before the fix centred captions (`PaintCx::text_centered`)
/// had no ellipsis or clip, and a ScrollView child taller than the view was painted below
/// the view.
#[test]
fn long_captions_stay_inside_their_bounds() {
    let theme = Theme::day();
    let long = "Synchronise reading progress with the server now";
    let mut tall = ScrollView::new();
    tall.push(Box::new(TextBlock::new(long.repeat(20), 36.0)), 2000);
    let cases: Vec<(&str, Box<dyn Widget>)> = vec![
        ("scroll-view", Box::new(tall)),
        ("label", Box::new(Label::new(long, 40.0))),
        ("button", Box::new(Button::new(long, "b"))),
        ("toast", Box::new(Toast::new(long))),
        ("dialog", Box::new(Dialog { title: long.into(), body: "b".into(), buttons: vec![(long.into(), "ok".into())] })),
    ];
    let mut spills = Vec::new();
    for (name, w) in cases {
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1400, 700);
        let b = Rect::new(500, 100, 400, 500);
        let mut tree = WidgetTree::new();
        tree.add(None, w, b);
        tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        let outside = (0..700i32)
            .flat_map(|y| (0..1400i32).map(move |x| (x, y)))
            .filter(|(x, y)| !b.contains(Point::new(*x, *y)) && canvas.get(*x, *y).unwrap() != 255)
            .count();
        if outside > 0 {
            spills.push(format!("{name}: {outside} pixels outside {b:?}"));
        }
    }
    assert!(spills.is_empty(), "{spills:#?}");
}

/// Lists with very long titles, subtitles and details paint, page and hit-test.
#[test]
fn list_with_long_texts_and_many_rows() {
    let theme = Theme::day();
    let mut fonts = Fonts::load();
    let mut canvas = Canvas::new(1272, 1696);
    let long = "Lorem ipsum dolor sit amet ".repeat(2);
    let items: Vec<ListItem> = (0..10_000).map(|i| ListItem::new(format!("{i} {long}"), format!("r{i}")).subtitle(long.clone()).detail(long.clone())).collect();
    let mut tree = WidgetTree::new();
    let b = Rect::new(0, 0, 1272, 1696);
    let id = tree.add(None, Box::new(List::new(items, 120)), b);
    tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
    // Swipe to the next page many times, then tap a row.
    for _ in 0..5 {
        tree.dispatch(&UiEvent::Gesture(Gesture::Swipe { from: Point::new(600, 1200), to: Point::new(600, 200), dir: ink_ui::Direction::Up }));
        tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
    }
    assert_eq!(tree.get_mut::<List>(id).unwrap().page, 5);
    let r = tree.dispatch(&UiEvent::Gesture(Gesture::Tap { pos: Point::new(600, 60) }));
    let per = tree.get_mut::<List>(id).unwrap().per_page(b);
    assert_eq!(r.map(|r| r.1), Some(EventResult::Action(format!("r{}", 5 * per))));
}

/// A text input with a long text keeps the cursor visible and edits at the end.
#[test]
fn text_input_with_long_text() {
    let theme = Theme::day();
    let mut fonts = Fonts::load();
    let mut canvas = Canvas::new(1272, 200);
    let mut ti = TextInput::new("p", "in");
    ti.set_text(&"word ".repeat(400));
    ti.focused = true;
    let mut tree = WidgetTree::new();
    let id = tree.add(None, Box::new(ti), Rect::new(0, 0, 1272, 110));
    let t0 = Instant::now();
    for _ in 0..20 {
        let t = tree.get_mut::<TextInput>(id).unwrap();
        t.apply(&KeyAction::Char("x".into()));
        tree.mark_dirty(id, ink_device::RefreshClass::Du);
        tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
    }
    let t = tree.get_mut::<TextInput>(id).unwrap();
    assert!(t.text.ends_with("xxxxxxxxxxxxxxxxxxxx"));
    // Ink near the right edge: the end of the text (the cursor) is in view.
    let right_ink = (1000..1240).flat_map(|x| (20..90).map(move |y| (x, y))).filter(|(x, y)| canvas.get(*x, *y).unwrap() < 100).count();
    assert!(right_ink > 50, "{right_ink}");
    assert!(t0.elapsed() < Duration::from_secs(20));
}
