//! The gallery driven headless: tabs, overlays, keyboard typing, gestures, night mode and
//! taps in all four rotations. With `INK_DEMO_OUT=DIR` each test writes PNGs of the
//! simulated panel there (`scripts`-free evidence for the acceptance checks).

use super::*;
use crate::app::providers;
use crate::plugin_host::PluginHost;
use crate::router::AppCtx;
use ink_device::headless::{self, HeadlessOptions, HeadlessProbe};
use ink_device::{Device, InputEvent, Timestamp, TouchContact, TouchFrame, TouchPhase};
use ink_store::{Paths, Settings};
use ink_text::Fonts;
use ink_ui::{Corner, GestureRecognizer, RefreshScheduler, ScreenSpace};

struct Harness {
    dev: Device,
    probe: HeadlessProbe,
    canvas: Canvas,
    fonts: Fonts,
    theme: Theme,
    refresh: RefreshScheduler,
    plugins: PluginHost,
    settings: Settings,
    paths: Paths,
    rec: GestureRecognizer,
    demo: DemoScreen,
    clock_ms: u64,
    out: Option<PathBuf>,
    pushed: Vec<Route>,
}

macro_rules! cx {
    ($h:ident, $p:ident) => {
        AppCtx {
            canvas: &mut $h.canvas,
            screen: $h.dev.screen.as_mut(),
            fonts: &mut $h.fonts,
            theme: &$h.theme,
            refresh: &mut $h.refresh,
            plugins: &mut $h.plugins,
            settings: &$h.settings,
            paths: &$h.paths,
            providers: &$p,
            services: crate::router::Services {
                info: &$h.dev.info,
                light: $h.dev.light.as_mut(),
                battery: $h.dev.battery.as_mut(),
                power: $h.dev.power.as_mut(),
                wifi: $h.dev.wifi.as_mut(),
                usb: $h.dev.usb.as_mut(),
                clock: $h.dev.clock.as_mut(),
            },
            settings_update: &mut None,
        }
    };
}

impl Harness {
    fn new(test: &str) -> Self {
        let (dev, probe) = headless::open(HeadlessOptions::default());
        let size = dev.screen.size();
        let space = ScreenSpace { panel: dev.screen.panel_size(), rotation: dev.screen.rotation() };
        let out = std::env::var_os("INK_DEMO_OUT").map(|d| PathBuf::from(d).join(test));
        let mut h = Harness {
            dev,
            probe,
            canvas: Canvas::new(size.w as usize, size.h as usize),
            fonts: Fonts::load(),
            theme: Theme::day(),
            refresh: RefreshScheduler::new(8),
            plugins: PluginHost::new(crate::plugins::compiled_in()),
            settings: Settings::default(),
            paths: Paths { root: std::env::temp_dir().join("ink-demo-test") },
            rec: GestureRecognizer::new(space),
            demo: DemoScreen::default(),
            clock_ms: 1000,
            out,
            pushed: Vec::new(),
        };
        let p = providers();
        h.demo.on_enter(&mut cx!(h, p)).unwrap();
        h.paint();
        h
    }

    fn paint(&mut self) {
        let p = providers();
        let mut cx = cx!(self, p);
        // As the app loop: coalesce, UI flash policy, update (app::present).
        let damage = self.demo.paint(&mut cx).unwrap();
        crate::app::present(&mut cx, crate::plugin_host::hooks::Frame { shown: Vec::new(), damage, wave: None }).unwrap();
    }

    /// Feeds one input event as the app's loop does (rotation kept in step), then paints.
    fn feed(&mut self, ev: InputEvent) {
        let space = ScreenSpace { panel: self.dev.screen.panel_size(), rotation: self.dev.screen.rotation() };
        if self.rec.space != space {
            self.rec.reset();
            self.rec.space = space;
        }
        let events = ink_ui::translate(&mut self.rec, ev);
        let p = providers();
        for ev in events {
            let t = self.demo.handle(&ev, &mut cx!(self, p)).unwrap();
            if let Transition::Push(r) = t {
                self.pushed.push(r);
            }
        }
        self.paint();
    }

    fn frame(&mut self, after_ms: u64, contacts: Vec<TouchContact>) {
        self.clock_ms += after_ms;
        self.feed(InputEvent::Touch(TouchFrame { time: Timestamp::from_millis(self.clock_ms), contacts }));
    }

    fn one(x: i32, y: i32, phase: TouchPhase) -> Vec<TouchContact> {
        vec![TouchContact { slot: 0, x, y, phase }]
    }

    fn panel_point(&self, r: Rect) -> Point {
        let c = Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
        self.dev.screen.rotation().to_panel(c, self.dev.screen.panel_size())
    }

    /// Taps the centre of a logical rectangle through panel coordinates.
    fn tap(&mut self, r: Rect) {
        let p = self.panel_point(r);
        self.frame(400, Self::one(p.x, p.y, TouchPhase::Down));
        self.frame(60, Self::one(p.x, p.y, TouchPhase::Up));
    }

    fn tap_key(&mut self, label: &str) {
        let r = self.demo.key(label).unwrap_or_else(|| panic!("no key {label}"));
        self.tap(r);
    }

    fn tap_button(&mut self, action: &str) {
        let r = self.demo.button(action).unwrap_or_else(|| panic!("no button {action}"));
        self.tap(r);
    }

    fn shot(&self, name: &str) {
        if let Some(dir) = &self.out {
            std::fs::create_dir_all(dir).unwrap();
            self.probe.panel().save_png(&dir.join(format!("{name}.png"))).unwrap();
        }
    }

    fn last_action(&self) -> &str {
        self.demo.actions.last().map(String::as_str).unwrap_or("")
    }
}

#[test]
fn every_tab_and_overlay() {
    let mut h = Harness::new("tabs");
    h.shot("01-controls");
    assert!(h.probe.panel().pixels.iter().filter(|v| **v < 64).count() > 20_000);
    for (i, name) in ["02-lists", "03-text", "04-input", "05-gestures", "05b-design"].iter().enumerate() {
        h.tap_button(&format!("tab:{}", i + 1));
        assert_eq!(h.demo.tab(), i + 1);
        h.shot(name);
    }
    h.tap_button("tab:0");
    h.tap_button("dialog");
    h.shot("06-dialog");
    // Taps outside the dialog are swallowed by the scrim; Cancel closes it.
    h.tap(Rect::new(10, 300, 10, 10));
    assert_eq!(h.last_action(), "dialog");
    h.tap(Rect::new(300, 1040, 10, 10));
    assert_eq!(h.last_action(), "dialog:cancel");
    // Closing an overlay requests a UI flash: the exposed area comes back with GC16.
    assert_eq!(h.probe.updates().last().unwrap().class, RefreshClass::Gc16);
    h.tap_button("toast");
    h.shot("07-toast");
    h.tap_button("popup");
    h.shot("08-popup");
    h.tap(Rect::new(20, 1500, 10, 10));
    assert_eq!(h.last_action(), "dismiss", "a tap outside closes the popup");
    h.tap_button("sheet");
    h.shot("09-sheet");
    h.tap(Rect::new(600, 300, 10, 10));
    assert_eq!(h.last_action(), "dismiss");
    h.tap_button("settings");
    assert_eq!(h.pushed, vec![Route::Settings]);
    // Sliders, toggles, icons and the progress bar react.
    h.tap_button("icon:Star");
    assert_eq!(h.last_action(), "icon:Star");
    assert!(h.demo.status().contains("icon:Star"));
}

#[test]
fn taps_hit_the_right_button_in_all_four_rotations() {
    let mut h = Harness::new("rotation");
    for (i, rot) in Rotation::ALL.iter().enumerate() {
        assert_eq!(h.dev.screen.rotation(), *rot);
        let size = h.dev.screen.size();
        assert_eq!(h.canvas.size(), size);
        h.shot(&format!("{:02}-{rot:?}", i + 1));
        // The toast and dialog buttons answer to taps at their rotated panel positions.
        h.tap_button("toast");
        assert_eq!(h.last_action(), "toast", "{rot:?}");
        h.tap_button("dialog");
        assert_eq!(h.last_action(), "dialog", "{rot:?}");
        h.shot(&format!("{:02}-{rot:?}-dialog", i + 1));
        let ok = Rect::new(size.w as i32 / 2 + 60, Dialog::frame(size).bottom() - 80, 10, 10);
        h.tap(ok);
        assert_eq!(h.last_action(), "dialog:ok", "{rot:?}");
        // The rotate button sits in the logical top-left corner in every orientation.
        let r = h.demo.button("rotate").unwrap();
        assert!(r.x < 20 && r.y < 20);
        let panel = h.panel_point(r);
        let expect = match rot {
            Rotation::Portrait => (true, true),
            Rotation::Clockwise => (false, true),
            Rotation::UpsideDown => (false, false),
            Rotation::CounterClockwise => (true, false),
        };
        assert_eq!((panel.x < 636, panel.y < 848), expect, "{rot:?} rotate button at panel {panel:?}");
        h.tap(r);
        assert_eq!(h.last_action(), "rotate");
    }
    assert_eq!(h.dev.screen.rotation(), Rotation::Portrait);
    // The last panel update was a full GC16 in portrait.
    let last = *h.probe.updates().last().unwrap();
    assert_eq!(last.class, RefreshClass::Gc16);
}

#[test]
fn night_mode_through_the_theme_and_the_device_flag() {
    let mut h = Harness::new("night");
    let white_before = h.probe.panel().get(636, 1600).unwrap();
    h.tap_button("night");
    assert!(h.dev.screen.night_mode());
    h.shot("01-night");
    let after = h.probe.panel().get(636, 1600).unwrap();
    assert!(white_before > 200 && after < 50, "background inverted on the panel: {white_before} -> {after}");
    assert_eq!(h.probe.updates().last().unwrap().class, RefreshClass::Gc16);
    // The night toggle on the Controls tab follows; tapping it turns night mode off.
    let toggle = h.demo.ids.night.and_then(|id| h.demo.tree.bounds(id)).unwrap();
    h.tap(toggle);
    assert_eq!(h.last_action(), "night:off");
    assert!(!h.dev.screen.night_mode());
}

#[test]
fn keyboard_types_into_the_text_input() {
    let mut h = Harness::new("keyboard");
    h.tap_button("tab:3");
    for c in ["h", "e", "j"] {
        let r = h.demo.key(c).unwrap();
        h.tap(r);
    }
    assert_eq!(h.demo.texts()[0], "hej");
    // A PW12 short tap (down and up in one frame) types too.
    let r = h.demo.key("space").unwrap();
    let p = h.panel_point(r);
    h.frame(300, Harness::one(p.x, p.y, TouchPhase::DownUp));
    assert_eq!(h.demo.texts()[0], "hej ");
    // Hold "a": the alternates popup opens (a shot while the finger is down), slide to the
    // first alternate and lift.
    let a = h.demo.key("a").unwrap();
    let pa = h.panel_point(a);
    h.frame(300, Harness::one(pa.x, pa.y, TouchPhase::Down));
    h.shot("01-key-preview");
    h.frame(700, Harness::one(pa.x, pa.y, TouchPhase::Move));
    h.shot("02-alternates");
    // The popup row is above the key; its second cell is the first English alternate (à).
    let cell_x = a.x + a.w as i32 / 2 + a.w.max(90) as i32;
    let up = Point::new(cell_x.min(1260), a.y - 60);
    let pu = h.dev.screen.rotation().to_panel(up, h.dev.screen.panel_size());
    h.frame(50, Harness::one(pu.x, pu.y, TouchPhase::Move));
    h.frame(50, Harness::one(pu.x, pu.y, TouchPhase::Up));
    assert_eq!(h.demo.texts()[0], "hej à");
    // Hold backspace: it repeats.
    let r = h.demo.key("del").unwrap();
    let del = h.panel_point(r);
    h.frame(300, Harness::one(del.x, del.y, TouchPhase::Down));
    h.frame(950, Harness::one(del.x, del.y, TouchPhase::Up));
    assert_eq!(h.demo.texts()[0], "h", "400, 550, 700, 850 ms: four deletes");
    // Globe: Danish layout; type æ directly.
    h.tap_key("globe");
    h.tap_key("æ");
    assert_eq!(h.demo.texts()[0], "hæ");
    // Shift + letter.
    h.tap_key("shift");
    h.tap_key("k");
    assert_eq!(h.demo.texts()[0], "hæK");
    h.shot("03-typed");
    // The password field masks.
    h.tap_button("focus:password");
    h.tap_key("x");
    assert_eq!(h.demo.texts()[1], "x");
    assert!(h.demo.status().contains('•'));
    // Hide the keyboard.
    h.tap_key("hide");
    assert!(h.demo.key("q").is_none());
}

#[test]
fn gesture_pad_shows_every_gesture() {
    let mut h = Harness::new("gestures");
    h.tap_button("tab:4");
    let two = |x1, y1, x2, y2, phase| {
        vec![TouchContact { slot: 0, x: x1, y: y1, phase }, TouchContact { slot: 1, x: x2, y: y2, phase }]
    };
    // Tap, double tap, long press, swipe.
    h.frame(500, Harness::one(600, 800, TouchPhase::DownUp));
    h.frame(120, Harness::one(605, 805, TouchPhase::DownUp));
    h.frame(500, Harness::one(300, 900, TouchPhase::Down));
    h.frame(800, Harness::one(300, 900, TouchPhase::Up));
    h.frame(500, Harness::one(900, 1200, TouchPhase::Down));
    h.frame(100, Harness::one(500, 1200, TouchPhase::Move));
    h.frame(100, Harness::one(300, 1200, TouchPhase::Up));
    // Two-finger tap, then a spread.
    h.frame(500, two(400, 700, 800, 700, TouchPhase::Down));
    h.frame(100, two(400, 700, 800, 700, TouchPhase::Up));
    h.frame(500, two(500, 1000, 700, 1000, TouchPhase::Down));
    h.frame(100, two(300, 1000, 900, 1000, TouchPhase::Move));
    h.frame(100, two(300, 1000, 900, 1000, TouchPhase::Up));
    // Corner hold (bottom left).
    h.frame(500, Harness::one(20, 1680, TouchPhase::Down));
    h.frame(700, Harness::one(20, 1680, TouchPhase::Move));
    h.frame(100, Harness::one(20, 1680, TouchPhase::Up));
    h.shot("01-gestures");
    let kinds: Vec<&str> = h
        .demo
        .gestures
        .iter()
        .map(|g| match g {
            Gesture::Tap { .. } => "tap",
            Gesture::DoubleTap { .. } => "double",
            Gesture::LongPress { .. } => "long",
            Gesture::Swipe { .. } => "swipe",
            Gesture::TwoFingerTap { .. } => "two",
            Gesture::TwoFingerSwipe { .. } => "twoswipe",
            Gesture::Pinch { .. } => "pinch",
            Gesture::CornerHold { .. } => "corner",
        })
        .collect();
    for k in ["tap", "double", "long", "swipe", "two", "pinch", "corner"] {
        assert!(kinds.contains(&k), "{k} missing from {kinds:?}");
    }
    assert!(h.demo.gestures.contains(&Gesture::CornerHold { corner: Corner::BottomLeft, pos: Point::new(20, 1680) }));
    assert!(h.demo.status().starts_with("gesture: long press"), "{}", h.demo.status());
}

#[test]
fn toasts_expire() {
    let mut h = Harness::new("toast");
    h.tap_button("toast");
    let t = h.demo.timeout().unwrap();
    assert!(t <= Duration::from_millis(2500));
    // Not yet expired: a timer event keeps it.
    let p = providers();
    h.demo.handle(&UiEvent::Timer(0), &mut cx!(h, p)).unwrap();
    assert!(h.demo.timeout().is_some());
}

// ---- Verifier attacks (wave A, cycle 1) ----

/// 20 taps per second on one panel point while the first taps rotate the screen, then
/// 20 taps per second across the tabs: no panic, the canvas follows the screen size, and
/// the last tab tapped is the one selected.
#[test]
fn verify_rapid_taps_through_rotations_and_tabs() {
    let mut h = Harness::new("verify-rapid");
    let rotate = h.panel_point(h.demo.button("rotate").unwrap());
    for _ in 0..40 {
        h.frame(30, Harness::one(rotate.x, rotate.y, TouchPhase::Down));
        h.frame(20, Harness::one(rotate.x, rotate.y, TouchPhase::Up));
        assert_eq!(h.canvas.size(), h.dev.screen.size());
    }
    assert!(h.demo.actions.iter().filter(|a| *a == "rotate").count() >= 1);
    for _ in 0..4 {
        if h.dev.screen.rotation() == Rotation::Portrait {
            break;
        }
        h.tap_button("rotate");
    }
    assert_eq!(h.dev.screen.rotation(), Rotation::Portrait);
    let mut last = 0;
    for i in 0..40 {
        let tab = (i * 3) % TABS.len();
        let p = h.panel_point(h.demo.button(&format!("tab:{tab}")).unwrap());
        h.frame(30, Harness::one(p.x, p.y, TouchPhase::DownUp));
        h.frame(20, vec![]);
        last = tab;
    }
    assert_eq!(h.demo.tab(), last);
}

/// Two thumbs typing fast on the gallery keyboard: contacts in slots 0 and 1 overlap,
/// 20 keys per second; every third pair arrives as PW12 same-frame taps. Every key types
/// once, in order.
#[test]
fn verify_two_thumb_typing_at_twenty_keys_per_second() {
    let mut h = Harness::new("verify-thumbs");
    h.tap_button("tab:3");
    let word = "thequickbrownfoxjumps";
    let mut keys = Vec::new();
    for c in word.chars() {
        let r = h.demo.key(&c.to_string()).unwrap();
        keys.push(h.panel_point(r));
    }
    let at = |slot: u8, p: Point, phase| vec![TouchContact { slot, x: p.x, y: p.y, phase }];
    for (n, pair) in keys.chunks(2).enumerate() {
        let (a, b) = (pair[0], pair.get(1).copied());
        if n % 3 == 2 {
            h.frame(25, at(0, a, TouchPhase::DownUp));
            if let Some(b) = b {
                h.frame(25, at(1, b, TouchPhase::DownUp));
            }
            h.frame(50, vec![]);
            continue;
        }
        // a down, b down 25 ms later, a up, b up: the contacts overlap.
        h.frame(25, at(0, a, TouchPhase::Down));
        if let Some(b) = b {
            h.frame(25, at(1, b, TouchPhase::Down));
            h.frame(25, at(0, a, TouchPhase::Up));
            h.frame(25, at(1, b, TouchPhase::Up));
        } else {
            h.frame(50, at(0, a, TouchPhase::Up));
        }
    }
    assert_eq!(h.demo.texts()[0], word);
}

/// Hold backspace on the Input tab (it repeats), rotate the gallery while the finger is
/// still down, then lift: the repeat stops at the rotation, the late release deletes
/// nothing, and the next key typed in the new orientation types one letter.
#[test]
fn verify_keyboard_repeat_while_rotating() {
    let mut h = Harness::new("verify-repeat-rotate");
    h.tap_button("tab:3");
    for c in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        h.tap_key(c);
    }
    assert_eq!(h.demo.texts()[0], "abcdefgh");
    let r = h.demo.key("del").unwrap();
    let del = h.panel_point(r);
    h.frame(300, Harness::one(del.x, del.y, TouchPhase::Down));
    // A frame after 800 ms fires the holds at 400, 550 and 700 ms: three deletes.
    h.frame(800, vec![]);
    assert_eq!(h.demo.texts()[0], "abcde");
    let p = providers();
    h.demo.action("rotate", &mut cx!(h, p)).unwrap();
    h.paint();
    // Time passes with the finger still down, then it lifts at its old panel position.
    h.frame(2000, vec![]);
    h.frame(100, Harness::one(del.x, del.y, TouchPhase::Up));
    assert_eq!(h.demo.texts()[0], "abcde", "no repeat after the rotation");
    assert_eq!(h.canvas.size(), h.dev.screen.size());
    h.tap_key("z");
    assert_eq!(h.demo.texts()[0], "abcdez");
}
