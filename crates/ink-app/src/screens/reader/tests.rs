//! The reader driven headless on a temporary copy of a test book. With
//! `RINKY_OUT=DIR` each test writes PNGs of the simulated panel to `DIR/<test>/`.

use super::*;
use crate::app::providers;
use crate::plugin_host::PluginHost;
use crate::router::AppCtx;
use ink_device::headless::{self, HeadlessOptions, HeadlessProbe};
use ink_device::{Device, InputEvent, KeyEvent, Rect, Timestamp, TouchContact, TouchFrame, TouchPhase};
use ink_store::{Paths, Settings};
use ink_ui::{GestureRecognizer, RefreshScheduler, ScreenSpace, Theme};

pub(super) struct Harness {
    pub dev: Device,
    pub probe: HeadlessProbe,
    pub canvas: Canvas,
    pub fonts: Fonts,
    pub theme: Theme,
    pub refresh: RefreshScheduler,
    pub plugins: PluginHost,
    pub settings: Settings,
    pub paths: Paths,
    pub rec: GestureRecognizer,
    pub reader: ReaderScreen,
    pub clock_ms: u64,
    pub out: Option<PathBuf>,
    pub transitions: Vec<Transition>,
    pub dir: PathBuf,
    /// A settings change made by the screen (App::take_settings_update).
    pub settings_update: Option<Settings>,
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
            settings_update: &mut $h.settings_update,
        }
    };
}

/// A fresh temporary directory for one test.
/// The whole portrait screen.
const FULL: Rect = Rect::new(0, 0, 1272, 1696);

pub(super) fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-reader-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

impl Harness {
    /// Opens a copy of `testdata/<book>` in a reader and waits for its pagination.
    pub fn new(test: &str, book: &str) -> Self {
        Self::open(test, book, true)
    }

    /// Opens a copy of `testdata/<book>`; `wait`: until pagination is complete.
    pub fn open(test: &str, book: &str, wait: bool) -> Self {
        let dir = tmp(test);
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(book);
        let path = dir.join(Path::new(book).file_name().unwrap());
        std::fs::copy(&src, &path).unwrap();
        Self::at(test, dir, path, wait)
    }

    /// Opens a book written from `bytes` as `name` (a generated PDF or CBZ:
    /// `ink_mupdf::testgen`).
    pub fn generated(test: &str, name: &str, bytes: &[u8]) -> Self {
        let dir = tmp(test);
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        Self::at(test, dir, path, true)
    }

    /// Opens `path` (inside the test's directory `dir`, removed on drop).
    fn at(test: &str, dir: PathBuf, path: PathBuf, wait: bool) -> Self {
        let (dev, probe) = headless::open(HeadlessOptions::default());
        let size = dev.screen.size();
        let space = ScreenSpace { panel: dev.screen.panel_size(), rotation: dev.screen.rotation() };
        let out = std::env::var_os("RINKY_OUT").map(|d| PathBuf::from(d).join(test));
        let mut h = Harness {
            dev,
            probe,
            canvas: Canvas::new(size.w as usize, size.h as usize),
            fonts: Fonts::load(),
            theme: Theme::day(),
            refresh: RefreshScheduler::new(8),
            // No compiled-in plugins: their menu entries would change the menu layout.
            plugins: PluginHost::new(Vec::new()),
            settings: Settings::default(),
            paths: Paths { root: dir.join("home") },
            rec: GestureRecognizer::new(space),
            reader: ReaderScreen::new(path.clone()),
            clock_ms: 1000,
            out,
            transitions: Vec::new(),
            dir,
            settings_update: None,
        };
        let p = providers();
        h.reader.on_enter(&mut cx!(h, p)).unwrap();
        // Page counts must not change under the tests' feet.
        if let Some(d) = h.reader.doc.as_deref_mut().filter(|_| wait) {
            assert!(d.wait_paginated(std::time::Duration::from_secs(60)));
        }
        h.paint();
        h
    }

    /// Runs `f` on the reader with the app context the app loop would give it.
    pub fn with_cx<R>(&mut self, f: impl FnOnce(&mut ReaderScreen, &mut AppCtx) -> R) -> R {
        let p = providers();
        let mut cx = cx!(self, p);
        f(&mut self.reader, &mut cx)
    }

    pub fn paint(&mut self) {
        let p = providers();
        let mut cx = cx!(self, p);
        for d in crate::plugin_host::hooks::app_paint(&mut self.reader, &mut cx).unwrap() {
            cx.screen.update(cx.canvas, d.rect, d.class).unwrap();
        }
        // As the app loop: the headless screen checks every drawn pixel was shown.
        self.dev.screen.frame_done(&self.canvas);
    }

    /// Feeds one input event as the app's loop does, then paints.
    pub fn feed(&mut self, ev: InputEvent) {
        // The bars' minute tick follows the wall clock: a test that runs across a minute
        // boundary (a loaded CI machine) would see the clock redrawn. A tick not yet due
        // is moved out of reach; a test that makes it due (`next_tick = now`) still gets it.
        let now = std::time::Instant::now();
        if let Some(t) = self.reader.bars.next_tick.as_mut().filter(|t| **t > now) {
            *t = now + std::time::Duration::from_secs(3600);
        }
        let events = ink_ui::translate(&mut self.rec, ev);
        let p = providers();
        for ev in events {
            self.handle_event(&ev, &p);
        }
        self.paint();
    }

    pub fn key(&mut self, key: Key) {
        self.clock_ms += 100;
        let time = Timestamp::from_millis(self.clock_ms);
        self.feed(InputEvent::Key(KeyEvent { key, pressed: true, time }));
        self.feed(InputEvent::Key(KeyEvent { key, pressed: false, time }));
    }

    fn frame(&mut self, after_ms: u64, x: i32, y: i32, phase: TouchPhase) {
        self.clock_ms += after_ms;
        let contacts = vec![TouchContact { slot: 0, x, y, phase }];
        self.feed(InputEvent::Touch(TouchFrame { time: Timestamp::from_millis(self.clock_ms), contacts }));
    }

    /// Taps a point (portrait: logical = panel).
    pub fn tap_at(&mut self, x: i32, y: i32) {
        self.frame(400, x, y, TouchPhase::Down);
        self.frame(60, x, y, TouchPhase::Up);
    }

    /// Taps the centre of `r`.
    pub fn tap(&mut self, r: Rect) {
        self.tap_at(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
    }

    /// Taps the overlay button with `action`.
    pub fn tap_button(&mut self, action: &str) {
        let r = self.reader.overlay().and_then(|o| o.button_rect(action)).unwrap_or_else(|| panic!("no button {action}"));
        self.tap(r);
    }

    pub fn shot(&self, name: &str) {
        if let Some(dir) = &self.out {
            std::fs::create_dir_all(dir).unwrap();
            self.probe.panel().save_png(&dir.join(format!("{name}.png"))).unwrap();
        }
    }

    pub fn kind(&self) -> Option<overlay::OverlayKind> {
        self.reader.overlay().map(|o| o.kind)
    }

    pub fn panel_rows_differ(&self, other: &Canvas, y0: usize, y1: usize) -> usize {
        let p = self.probe.panel();
        let w = p.width;
        (y0 * w..y1 * w).filter(|&i| p.pixels[i] != other.pixels[i]).count()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn edge_slides_set_the_light_and_bottom_corners_toggle_it_without_repaints() {
    let mut h = Harness::new("light-gestures", "juliet.epub");
    let light = crate::screens::settings::fakes::install(&mut h.dev);
    light.borrow_mut().brightness = 10;
    light.borrow_mut().warmth = 4;
    let before = h.probe.updates().len();
    // Up the left edge: brighter while the finger moves, no page turn, nothing drawn.
    h.frame(400, 40, 1000, TouchPhase::Down);
    h.frame(50, 42, 900, TouchPhase::Move);
    let mid = light.borrow().brightness;
    assert!(mid > 10, "{mid}");
    h.frame(50, 44, 700, TouchPhase::Move);
    h.frame(50, 44, 700, TouchPhase::Up);
    let up = light.borrow().brightness;
    assert!(up > mid, "{up}");
    // Down the right edge: cooler.
    h.frame(400, 1250, 600, TouchPhase::Down);
    h.frame(50, 1250, 800, TouchPhase::Move);
    h.frame(50, 1250, 1000, TouchPhase::Up);
    assert!(light.borrow().warmth < 4, "{}", light.borrow().warmth);
    assert_eq!(h.reader.page(), 0);
    assert!(h.transitions.is_empty(), "{:?}", h.transitions);
    assert_eq!(h.probe.updates().len(), before, "an edge slide draws nothing");
    // Bottom-left corner: off, then back to the level before.
    h.tap_at(30, 1680);
    assert_eq!(light.borrow().brightness, 0);
    h.tap_at(30, 1680);
    assert_eq!(light.borrow().brightness, up);
    // Bottom-right corner: warmth off and back.
    light.borrow_mut().warmth = 7;
    h.tap_at(1250, 1680);
    assert_eq!(light.borrow().warmth, 0);
    h.tap_at(1250, 1680);
    assert_eq!(light.borrow().warmth, 7);
    assert_eq!(h.reader.page(), 0, "corner taps turn no page");
    // Turned off in the settings: the left-edge slide is a plain swipe up again.
    h.settings.gestures.set("slide_left_edge", "none");
    h.frame(400, 40, 1000, TouchPhase::Down);
    h.frame(50, 42, 800, TouchPhase::Move);
    h.frame(50, 42, 600, TouchPhase::Up);
    assert_eq!(light.borrow().brightness, up);
}

#[test]
fn center_tap_opens_the_menu_and_a_tap_outside_restores_the_page() {
    let mut h = Harness::new("menu", "juliet.epub");
    h.tap_at(1000, 900);
    assert_eq!(h.reader.page(), 1);
    let page = h.probe.panel().clone();
    h.shot("01-page2");
    h.tap_at(636, 848);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Menu));
    h.shot("02-menu");
    // The bars cover the top and bottom; the middle of the page is unchanged.
    let height = page.height;
    assert!(h.panel_rows_differ(&page, 0, menu::TOP_H as usize) > 1000);
    assert!(h.panel_rows_differ(&page, height - menu::BOTTOM_H as usize, height) > 1000);
    assert_eq!(h.panel_rows_differ(&page, menu::TOP_H as usize + 4, height - menu::BOTTOM_H as usize - 4), 0);
    // A tap between the bars closes the menu: the page is back exactly.
    let before = h.probe.updates().len();
    h.tap_at(636, 848);
    assert_eq!(h.kind(), None);
    assert_eq!(h.reader.actions().last().map(String::as_str), Some("close"));
    assert_eq!(h.panel_rows_differ(&page, 0, height), 0);
    let ups = &h.probe.updates()[before..];
    // Only the bars' rects come back (from the page buffer), not the full screen.
    assert!(!ups.is_empty() && ups.iter().all(|u| u.rect != Rect::new(0, 0, 1272, 1696)), "{ups:?}");
    // Page turns work again; the top-right corner opens the menu (no hidden exit).
    h.tap_at(1000, 900);
    assert_eq!(h.reader.page(), 2);
    h.tap_at(1200, 60);
    assert!(h.transitions.is_empty(), "{:?}", h.transitions);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Menu));
}

#[test]
fn menu_key_and_back() {
    let mut h = Harness::new("menukey", "synthetic.epub");
    h.key(Key::Menu);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Menu));
    h.key(Key::Back);
    assert_eq!(h.kind(), None);
    h.key(Key::Menu);
    h.tap_button("back");
    assert_eq!(h.transitions.last(), Some(&Transition::Pop));
}

#[test]
fn menu_shows_progress_and_the_slider_jumps() {
    let mut h = Harness::new("slider", "juliet.epub");
    h.tap_at(636, 848);
    let info = h.reader.menu_info();
    assert_eq!(info.page, 0);
    assert!(info.pages > 10, "{info:?}");
    assert!(!info.title.is_empty());
    // Tap the slider's plus button: page 2, the menu closes.
    let slider = h.reader.overlay().unwrap().ids["slider"];
    let b = h.reader.overlay().unwrap().tree.bounds(slider).unwrap();
    let (_, _, plus) = ink_ui::Slider::parts(b);
    h.tap(plus);
    assert_eq!(h.reader.actions().last().map(String::as_str), Some("page:2"));
    assert_eq!(h.kind(), None);
    assert_eq!(h.reader.page(), 1);
    // A drag to the end of the track goes to the last known page.
    h.tap_at(636, 848);
    let (_, track, _) = ink_ui::Slider::parts(b);
    let y = track.y + track.h as i32 / 2;
    h.frame(400, track.x + 10, y, TouchPhase::Down);
    h.frame(100, track.x + track.w as i32 / 2, y, TouchPhase::Move);
    h.frame(100, track.right() + 20, y, TouchPhase::Move);
    h.frame(100, track.right() + 20, y, TouchPhase::Up);
    let last = h.reader.doc.as_ref().unwrap().page_count().pages - 1;
    assert_eq!(h.reader.page(), last, "{:?}", h.reader.actions());
    h.shot("03-last-page");
}

#[test]
fn chapter_and_progress_helpers() {
    let h = Harness::new("chapter", "juliet.epub");
    let doc = h.reader.doc.as_deref().unwrap();
    let toc = doc.toc();
    assert!(!toc.is_empty());
    let last = doc.page_count().pages - 1;
    let c = current_chapter(doc, &toc, last).unwrap();
    assert!(c + 1 >= toc.len() || doc.page_of(&toc[c + 1].position).unwrap() > last);
    assert!(progress(doc, 0) < 0.05);
    assert!(progress(doc, last) > 0.95);
}

#[test]
fn goto_contents_marks_the_current_chapter_and_jumps() {
    let mut h = Harness::new("goto", "juliet.epub");
    // Somewhere in the middle of the book.
    h.reader.go_to_page(100);
    h.paint();
    h.tap_at(636, 848);
    h.tap_button("goto");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Goto));
    h.shot("01-contents");
    let toc = h.reader.doc.as_deref().unwrap().toc();
    let doc = h.reader.doc.as_deref().unwrap();
    let cur = current_chapter(doc, &toc, 100).expect("a chapter");
    let id = h.reader.overlay().unwrap().ids["list"];
    let o = h.reader.overlay.as_mut().unwrap();
    let b = o.tree.bounds(id).unwrap();
    let l = o.tree.get_mut::<ink_ui::List>(id).unwrap();
    assert_eq!(l.selected, Some(cur));
    // Tap the first row shown on the list's page.
    let first = l.page * l.per_page(b);
    let target = doc.page_of(&toc[first].position).unwrap();
    h.tap_at(300, b.y + goto::ROW_H as i32 / 2);
    assert_eq!(h.reader.actions().last().unwrap(), &format!("toc:{first}"));
    assert_eq!(h.kind(), None);
    assert_eq!(h.reader.page(), target);
    // Go to 50 %.
    h.tap_at(636, 848);
    h.tap_button("goto");
    let sid = h.reader.overlay().unwrap().ids["percent"];
    let sb = h.reader.overlay().unwrap().tree.bounds(sid).unwrap();
    let (_, track, _) = ink_ui::Slider::parts(sb);
    h.tap_at(track.x + track.w as i32 / 2, track.y + track.h as i32 / 2);
    let pages = h.reader.doc.as_deref().unwrap().page_count().pages;
    assert!(h.reader.actions().last().unwrap().starts_with("pct:"), "{:?}", h.reader.actions());
    assert!((h.reader.page() as i64 - (pages / 2) as i64).abs() <= 3, "page {} of {pages}", h.reader.page());
}

#[test]
fn bookmarks_add_list_jump_remove() {
    let mut h = Harness::new("bookmarks", "juliet.epub");
    h.reader.go_to_page(20);
    h.paint();
    let plain = h.probe.panel().clone();
    h.tap_at(636, 848);
    let n = h.probe.updates().len();
    h.tap_button("bookmark");
    assert_eq!(h.kind(), None);
    assert_eq!(h.reader.page(), 20);
    // The dog-ear and the closed menu bars are refreshed, not the full screen.
    let ups = &h.probe.updates()[n..];
    assert!(!ups.is_empty() && ups.iter().all(|u| u.rect != FULL), "{ups:?}");
    // Saved in the sidecar and drawn as a dog-ear in the top-right corner.
    let state = BookState::load(&h.reader.path).unwrap();
    assert_eq!(state.bookmarks.len(), 1);
    assert!(state.bookmarks[0].created.len() == 19, "{:?}", state.bookmarks[0]);
    let p = h.probe.panel();
    assert_eq!(p.get(p.width as i32 - 3, 3), Some(0));
    assert_eq!(plain.get(plain.width as i32 - 3, 3), Some(255));
    h.shot("01-dog-ear");
    // The menu shows the page as marked.
    h.tap_at(636, 848);
    assert!(h.reader.menu_info().bookmarked);
    h.tap_at(636, 848);
    // Elsewhere: list the bookmark and jump to it.
    h.reader.go_to_page(60);
    h.paint();
    h.tap_at(636, 848);
    h.tap_button("goto");
    h.tap_button("gtab:1");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Goto));
    h.shot("02-bookmarks");
    let b = h.reader.overlay().unwrap().tree.bounds(h.reader.overlay().unwrap().ids["list"]).unwrap();
    h.tap_at(300, b.y + 40);
    assert_eq!(h.reader.actions().last().map(String::as_str), Some("bm:0"));
    assert_eq!(h.reader.page(), 20);
    // Toggle again: removed from the sidecar and the dog-ear is gone.
    h.tap_at(636, 848);
    h.tap_button("bookmark");
    assert!(BookState::load(&h.reader.path).unwrap().bookmarks.is_empty());
    let p = h.probe.panel();
    assert_eq!(p.get(p.width as i32 - 3, 3), Some(255));
}

#[test]
fn touching_empty_menu_parts_refreshes_nothing() {
    let mut h = Harness::new("quiet", "synthetic.epub");
    h.tap_at(636, 848);
    let n = h.probe.updates().len();
    // A finger down and up between two tool buttons, and on the bottom bar's caption.
    h.frame(400, 600, 1400, TouchPhase::Down);
    h.frame(500, 600, 1400, TouchPhase::Move);
    assert_eq!(h.probe.updates().len(), n, "{:?}", &h.probe.updates()[n..]);
    h.frame(60, 600, 1400, TouchPhase::Up);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Menu));
}

fn slider_plus(h: &Harness, key: &str) -> Rect {
    aa::slider_parts(h.reader.overlay().unwrap(), key).unwrap().2
}

/// Waits out the Aa relayout debounce and delivers the timer event that runs it.
fn settle_aa(h: &mut Harness) {
    std::thread::sleep(aa::DEBOUNCE + std::time::Duration::from_millis(30));
    let p = providers();
    h.handle_event(&UiEvent::Timer(0), &p);
    h.paint();
}

/// Ink in `r` of the panel (0 white).
fn panel_ink(h: &Harness, r: Rect) -> u64 {
    let p = h.probe.panel();
    let mut ink = 0;
    for y in r.y.max(0) as usize..(r.bottom() as usize).min(p.height) {
        for x in r.x.max(0) as usize..(r.right() as usize).min(p.width) {
            ink += 255 - p.pixels[y * p.width + x] as u64;
        }
    }
    ink
}

#[test]
fn aa_preview_shows_the_text_weight() {
    let mut h = Harness::new("aa-weight", "juliet.epub");
    h.tap_at(636, 848);
    h.tap_button("aa");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Aa));
    let o = h.reader.overlay().unwrap();
    let preview = aa::preview_rect(o).unwrap();
    let seg = o.tree.bounds(o.ids["weight"]).unwrap();
    // Inside the frame: the text only.
    let text = Rect::new(preview.x + 16, preview.y + 16, preview.w - 32, preview.h - 32);
    let mut ink = vec![panel_ink(&h, text)];
    h.shot("01-normal");
    // Darker, then Darkest: each tap draws the preview darker, the text in place.
    for (i, name) in [(1, "darker"), (2, "darkest")] {
        h.tap_at(seg.x + seg.w as i32 * (2 * i + 1) / 6, seg.y + seg.h as i32 / 2);
        assert_eq!(h.reader.actions().last().map(String::as_str), Some(format!("aa-weight:{i}").as_str()));
        settle_aa(&mut h);
        assert_eq!(h.reader.doc.as_deref().unwrap().layout().embolden, i as u8);
        ink.push(panel_ink(&h, text));
        h.shot(&format!("0{}-{name}", i + 1));
    }
    assert!(ink[1] as f64 > ink[0] as f64 * 1.1 && ink[2] as f64 > ink[1] as f64 * 1.1, "preview ink normal, darker, darkest: {ink:?}");
}

/// A change on one control of the Aa sheet that other controls show (a preset sets the
/// size, spacing, margins, justify and hyphenation; a size step can select or clear a
/// preset; a font changes the preview) is shown in the same frame: no pixel stays
/// drawn outside every update rect, also through key presses and the relayout.
#[test]
fn aa_controls_show_every_value_they_change() {
    let mut h = Harness::new("aa-damage", "juliet.epub");
    h.tap_at(636, 848);
    h.tap_button("aa");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Aa));
    let o = h.reader.overlay().unwrap();
    let presets = o.tree.bounds(o.ids["preset"]).unwrap();
    let weight = o.tree.bounds(o.ids["weight"]).unwrap();
    let justify = ink_ui::Toggle::switch_rect(o.tree.bounds(o.ids["justify"]).unwrap());
    let n = aa::PRESETS.len() as i32;
    let chip = |i: i32| (presets.x + presets.w as i32 * (2 * i + 1) / (2 * n), presets.y + presets.h as i32 / 2);
    for i in [3, 0, 2, 1] {
        let (x, y) = chip(i);
        h.tap_at(x, y);
        assert_eq!(h.reader.actions().last().map(String::as_str), Some(format!("aa-preset:{i}").as_str()));
        assert_eq!(h.probe.uncovered(), vec![], "preset {i}");
        h.key(Key::PageNext);
        h.key(Key::PagePrev);
        assert_eq!(h.probe.uncovered(), vec![], "keys after preset {i}");
    }
    // Standard is selected; a size step clears it and a step back selects it again.
    for r in [slider_plus(&h, "size"), aa::slider_parts(h.reader.overlay().unwrap(), "size").unwrap().0] {
        let n = h.probe.updates().len();
        h.tap(r);
        assert_eq!(h.probe.uncovered(), vec![], "{:?}", h.reader.actions().last());
        let ups = &h.probe.updates()[n..];
        assert!(ups.iter().any(|u| u.rect.intersect(&presets) == presets), "the preset row is shown again: {ups:?}");
    }
    h.tap(justify);
    h.tap_at(weight.x + weight.w as i32 / 2, weight.y + weight.h as i32 / 2);
    assert_eq!(h.probe.uncovered(), vec![], "{:?}", h.reader.actions());
    settle_aa(&mut h);
    h.key(Key::PageNext);
    assert_eq!(h.probe.uncovered(), vec![], "after the relayout");
}

#[test]
fn aa_panel_relayouts_keeps_the_position_and_saves_per_book_and_global() {
    let mut h = Harness::new("aa", "juliet.epub");
    h.reader.go_to_page(50);
    h.paint();
    let pos = h.reader.doc.as_deref().unwrap().position_of_page(50).unwrap();
    let pages0 = h.reader.doc.as_deref().unwrap().page_count().pages;
    h.tap_at(636, 848);
    h.tap_button("aa");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Aa));
    h.shot("01-aa");
    // Font size + 2 px, three times: the sheet stays open, the book is laid out again.
    for _ in 0..3 {
        let r = slider_plus(&h, "size");
        h.tap(r);
    }
    assert_eq!(h.reader.actions().last().map(String::as_str), Some("aa-size:46"));
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Aa));
    settle_aa(&mut h);
    let p = h.reader.page();
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert_eq!(doc.layout().font_px, 46.0);
    assert!(doc.wait_paginated(std::time::Duration::from_secs(60)));
    assert!(doc.page_count().pages > pages0, "{} -> {}", pages0, doc.page_count().pages);
    // The page shown still holds the old reading position.
    let start = doc.position_of_page(p).unwrap();
    assert!(doc.page_of(&pos).unwrap() == p, "position {pos} not on page {p} (starts {start})");
    h.shot("02-bigger");
    // Justify off, saved in the sidecar.
    let o = h.reader.overlay().unwrap();
    let jr = ink_ui::Toggle::switch_rect(o.tree.bounds(o.ids["justify"]).unwrap());
    h.tap(jr);
    settle_aa(&mut h);
    let st = BookState::load(&h.reader.path).unwrap();
    assert_eq!(st.settings.font_px, Some(46.0));
    assert_eq!(st.settings.justify, Some(false));
    // Use for all books: settings.toml has the values.
    h.tap_button("aa-global");
    let g = Settings::load(&h.paths.settings()).unwrap();
    assert_eq!((g.reader.font_px, g.reader.justify), (46.0, false));
    h.tap_button("close");
    assert_eq!(h.kind(), None);
    h.shot("03-closed");
    // Opening the book again uses its own settings.
    let mut h2 = ReaderScreen::new(h.reader.path.clone());
    let p = providers();
    h2.on_enter(&mut cx!(h, p)).unwrap();
    assert_eq!(h2.doc.as_deref().unwrap().layout().font_px, 46.0);
    assert!(!h2.doc.as_deref().unwrap().layout().justify);
}

/// The first page (from `from`) with a link matching `want`, and that link.
fn find_link(h: &mut Harness, from: usize, want: impl Fn(&ink_doc::Link) -> bool) -> (usize, ink_doc::Link) {
    let doc = h.reader.doc.as_deref_mut().unwrap();
    for page in from..doc.page_count().pages {
        let pos = doc.position_of_page(page).unwrap();
        if let Some(l) = doc.links(&pos).unwrap().into_iter().find(|l| want(l) && l.rect.is_some()) {
            return (page, l);
        }
    }
    panic!("no such link");
}

#[test]
fn footnote_popup_internal_link_and_back() {
    let mut h = Harness::new("links", "formats/sample.fb2");
    // A footnote reference: a popup with the note text, the page stays.
    let (page, l) = find_link(&mut h, 0, |l| l.footnote);
    // The FB2 note's title (its number) runs in bold into the text, as KOReader shows it.
    let ink_doc::LinkTarget::Internal(nt) = &l.target else { panic!() };
    let note = h.reader.doc.as_deref_mut().unwrap().note(nt).unwrap().unwrap();
    assert_eq!(note.text, "1 The first footnote.");
    assert!(note.spans[0].bold && note.spans[0].text.starts_with('1'), "{:?}", note.spans);
    h.reader.go_to_page(page);
    h.paint();
    let r = l.rect.unwrap();
    h.tap(r);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Popup), "{:?}", h.reader.actions());
    assert_eq!(h.reader.page(), page);
    h.shot("01-footnote");
    // "Go to note" jumps there; Back returns.
    h.tap_button("note-go");
    assert_eq!(h.kind(), None);
    let at_note = h.reader.page();
    assert_eq!(h.reader.back.len(), 1);
    h.shot("02-note");
    h.tap(back_rect(Size::new(1272, 1696)));
    assert_eq!(h.reader.page(), page);
    assert!(h.reader.back.is_empty());
    let _ = at_note;
    // A tap outside the popup closes it.
    h.tap(r);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Popup));
    h.tap_at(636, 1000);
    assert_eq!(h.kind(), None);
    // A plain internal link jumps at once; the Back key returns.
    let (page2, l2) = find_link(&mut h, 0, |l| !l.footnote && matches!(l.target, ink_doc::LinkTarget::Internal(_)));
    h.reader.go_to_page(page2);
    h.paint();
    let ink_doc::LinkTarget::Internal(t) = &l2.target else { unreachable!() };
    let want = h.reader.doc.as_deref().unwrap().page_of(t).unwrap();
    h.tap(l2.rect.unwrap());
    assert_eq!(h.reader.page(), want);
    h.key(Key::Back);
    assert_eq!(h.reader.page(), page2);
    assert!(h.transitions.is_empty(), "{:?}", h.transitions);
}

#[test]
fn excerpt() {
    assert_eq!(note_excerpt("a  b\n c", 10), "a b c");
    assert_eq!(note_excerpt("one two three four", 9), "one two ...");
}

#[test]
fn note_popup_text_and_spans() {
    use ink_doc::{Note, NoteKind, NoteSpan};
    let pos = Position::Page(Default::default());
    let span = |t: &str, italic| NoteSpan { text: t.into(), bold: false, italic };
    // The note's spans as they are, number included, with their styles.
    let n = Note { kind: NoteKind::Endnote, position: pos.clone(), text: "3 See Hamlet".into(), spans: vec![span("3 See ", false), span("Hamlet", true)] };
    assert_eq!(note_spans(&n, 700), [("3 See ".to_string(), Style::Regular), ("Hamlet".to_string(), Style::Italic)]);
    let mut n = Note::plain(NoteKind::Footnote, pos, "Text.");
    // Long notes are cut at a word, also at a paragraph break.
    n.spans = vec![span("one two ", false), span("three four", true)];
    assert_eq!(note_spans(&n, 14), [("one two ".to_string(), Style::Regular), ("three ...".to_string(), Style::Italic)]);
    assert_eq!(note_spans(&n, 9), [("one two ".to_string(), Style::Regular), (" ...".to_string(), Style::Italic)]);
    n.spans = vec![span("one two\nthree", false)];
    assert_eq!(note_spans(&n, 10), [("one two ...".to_string(), Style::Regular)]);
}

#[test]
fn link_popup_height_follows_the_text() {
    assert_eq!(popup_height("A short note.", 1100, 1696), 240, "the smallest popup");
    // Each paragraph starts a line of its own.
    let one = popup_height(&"word ".repeat(30), 1100, 1696);
    let two = popup_height(&format!("{}\n{}", "word ".repeat(15), "word ".repeat(15)), 1100, 1696);
    assert!(two > one, "{two} {one}");
    assert_eq!(popup_height(&"word ".repeat(1000), 1100, 1696), 1696 * 3 / 5);
}

/// The open note panel and its bounds.
fn note_panel(h: &mut Harness) -> (note::NotePanel, Rect) {
    let o = h.reader.overlay.as_mut().expect("an overlay");
    (0..16)
        .find_map(|i| {
            let id = ink_ui::WidgetId(i);
            let p = o.tree.get_mut::<note::NotePanel>(id).cloned()?;
            Some((p, o.tree.bounds(id)?))
        })
        .expect("a note panel")
}

/// A book with notes 57 (one line, italic), 58 (two paragraphs) and 59 (long), their
/// references on the first page.
fn notes_book(test: &str) -> Harness {
    let long: String = (1..=40).map(|i| format!("Sentence {i} of the long note, which goes on for a while. ")).collect();
    let html = format!(
        r##"<html><head><title>Notes</title></head><body>
        <p>A sentence with a note<a href="#fn57" id="ref57"><sup>57</sup></a>, a second one<a href="#fn58" id="ref58"><sup>58</sup></a> and a long one<a href="#fn59" id="ref59"><sup>59</sup></a>.</p>
        <p>More of the chapter so the notes are further down.</p>
        <p id="fn57"><a href="#ref57">57</a>. The text of note 57, which says something about <i>Hamlet</i>.</p>
        <p id="fn58"><a href="#ref58">58</a>. The first paragraph of note 58.</p><p>Its second paragraph.</p>
        <p id="fn59"><a href="#ref59">59</a>. {long}</p>
        </body></html>"##
    );
    let mut h = Harness::generated(test, "notes.html", html.as_bytes());
    let (page, _) = find_link(&mut h, 0, |l| l.footnote);
    h.reader.go_to_page(page);
    h.paint();
    h
}

fn tap_note(h: &mut Harness, number: &str) {
    let (_, l) = find_link(h, 0, |l| l.footnote && l.text.trim() == number);
    h.tap(l.rect.unwrap());
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Popup), "{:?}", h.reader.actions());
}

/// The note shows as KOReader shows it: in a panel across the whole width at the bottom
/// of the screen, as high as its text, with the note as the book prints it (its number
/// and the period after it on the same line, no heading), paragraphs kept.
#[test]
fn footnote_panel_at_the_bottom_shows_only_the_note() {
    let mut h = notes_book("note-popup");
    let full = Size::new(1272, 1696);
    tap_note(&mut h, "57");
    let (p, r) = note_panel(&mut h);
    assert_eq!((r.x, r.w, r.bottom()), (0, full.w, full.h as i32), "full width, on the bottom edge: {r:?}");
    assert!(!p.at_top);
    let text: String = p.spans.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(text, "57. The text of note 57, which says something about Hamlet.");
    assert_eq!(p.spans[1], ("Hamlet".to_string(), Style::Italic));
    assert_eq!(p.pages.len(), 1);
    let one_line = r.h;
    // One line of text: no empty space (the text, padding and the button row).
    assert!(one_line < 260, "{one_line}");
    h.shot("01-note-57");
    // Go to note sits in the panel; there is no Close button (a tap outside closes).
    let go = h.reader.overlay().unwrap().button_rect("note-go").unwrap();
    assert!(r.contains(Point::new(go.x, go.y)));
    assert!(go.x > (full.w / 2) as i32, "Go to note sits at the right: {go:?}");
    assert!(h.reader.overlay().unwrap().button_rect("close").is_none());
    h.tap_at(636, r.y - 200);
    assert_eq!(h.kind(), None);
    // Two paragraphs: taller by the second line and the gap between them.
    tap_note(&mut h, "58");
    let (p, r) = note_panel(&mut h);
    let text: String = p.spans.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(text, "58. The first paragraph of note 58.\nIts second paragraph.");
    assert!(r.h > one_line + 40 && r.h < one_line + 100, "{} {one_line}", r.h);
    assert_eq!(r.bottom(), full.h as i32);
    h.shot("02-note-58");
    // A tap outside the panel closes it.
    h.tap_at(636, r.y - 200);
    assert_eq!(h.kind(), None);
    // A long note: at most half the screen, a page at a time; a tap on the text turns.
    tap_note(&mut h, "59");
    let (p, r) = note_panel(&mut h);
    assert!(p.pages.len() > 1, "{:?}", p.pages);
    assert!(r.h <= full.h / 2, "{}", r.h);
    h.shot("03-note-59");
    h.tap_at(636, r.y + 60);
    assert_eq!(note_panel(&mut h).0.page, 1);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Popup));
    h.shot("04-note-59-page-2");
    // Go to note jumps there.
    h.tap_button("note-go");
    assert_eq!(h.kind(), None);
}

/// Italic and bold in a note are drawn with real italic and bold faces (the UI fonts
/// draw them in the regular face).
#[test]
fn note_panel_draws_italic_and_bold() {
    let size = Size::new(1272, 1696);
    let paint = |style: Style| {
        let (mut p, r) = note::NotePanel::new(vec![("Hamlet".into(), style)], Vec::new(), 36.0, 60.0, size, Rect::default());
        let mut canvas = Canvas::new(size.w as usize, size.h as usize);
        canvas.clear(255);
        let mut fonts = crate::fonts::builtin_ui_fonts();
        let theme = Theme::day();
        ink_ui::Widget::paint(&mut p, &mut PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme }, r);
        canvas
    };
    let (regular, italic, bold) = (paint(Style::Regular), paint(Style::Italic), paint(Style::Bold));
    assert!(regular.pixels.iter().filter(|&&v| v < 128).count() > 100, "text drawn");
    assert!(regular.pixels != italic.pixels, "italic face");
    assert!(regular.pixels != bold.pixels, "bold face");
}

/// The panel moves to the top of the screen when at the bottom it would cover the link.
#[test]
fn note_panel_keeps_off_the_tapped_link() {
    let size = Size::new(1272, 1696);
    let spans = vec![("A note.".to_string(), Style::Regular)];
    let (p, r) = note::NotePanel::new(spans.clone(), Vec::new(), 36.0, 60.0, size, Rect::new(500, 1600, 40, 40));
    assert!(p.at_top);
    assert_eq!((r.x, r.y, r.w), (0, 0, size.w));
    let (p, r) = note::NotePanel::new(spans, Vec::new(), 36.0, 60.0, size, Rect::new(500, 100, 40, 40));
    assert!(!p.at_top);
    assert_eq!(r.bottom(), size.h as i32);
}

impl Harness {
    /// Taps a key of the overlay's keyboard by label.
    fn tap_key(&mut self, label: &str) {
        let o = self.reader.overlay.as_mut().unwrap();
        let id = o.ids["keyboard"];
        let b = o.tree.bounds(id).unwrap();
        let kb = o.tree.get_mut::<ink_ui::Keyboard>(id).unwrap();
        let area = kb.keys_area(b);
        let r = kb.layout.key_rects(area).into_iter().find(|(_, k)| k.label == label).map(|(r, _)| r).unwrap_or_else(|| panic!("no key {label}"));
        self.tap(r);
    }
}

#[test]
fn search_types_finds_lists_and_steps_through_hits() {
    let mut h = Harness::new("search", "juliet.epub");
    h.tap_at(636, 848);
    h.tap_button("search");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Search));
    assert!(h.reader.overlay().unwrap().ids.contains_key("keyboard"));
    for k in ["n", "u", "r", "s", "e"] {
        h.tap_key(k);
    }
    assert_eq!(h.reader.query, "nurse");
    h.shot("01-typed");
    h.tap_button("search-go");
    let n = h.reader.hits.as_ref().unwrap().len();
    assert!(n > 20, "{n} hits");
    assert!(h.reader.overlay().unwrap().ids.contains_key("list"));
    assert!(!h.reader.overlay().unwrap().ids.contains_key("keyboard"));
    h.shot("02-results");
    // The second result row.
    let lb = h.reader.overlay().unwrap().tree.bounds(h.reader.overlay().unwrap().ids["list"]).unwrap();
    h.tap_at(400, lb.y + 150);
    assert_eq!(h.reader.actions().last().map(String::as_str), Some("hit:1"));
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Hits));
    let doc = h.reader.doc.as_deref().unwrap();
    let want = doc.page_of(&h.reader.hits.as_ref().unwrap()[1].position).unwrap();
    assert_eq!(h.reader.page(), want);
    h.shot("03-hit");
    // Next hit, then a page turn on the page under the bar, then back to the list.
    h.tap_button("hit-next");
    let want = h.reader.doc.as_deref().unwrap().page_of(&h.reader.hits.as_ref().unwrap()[2].position).unwrap();
    assert_eq!(h.reader.page(), want);
    h.tap_at(1000, 900);
    assert_eq!(h.reader.page(), want + 1);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Hits));
    h.tap_button("hits-list");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Search));
    h.tap_button("close");
    assert_eq!(h.kind(), None);
    // No matches.
    h.reader.query = "zzqxj".into();
    h.with_cx(|r, cx| r.run_search(cx));
    assert_eq!(h.reader.hits.as_ref().map(Vec::len), Some(0));
}

impl Harness {
    /// One event as App::run gives it: plugins first, then the screen; then a settings
    /// change is kept and saved.
    fn handle_event(&mut self, ev: &UiEvent, p: &[&dyn ink_doc::Provider]) {
        let mut cx = cx!(self, p);
        let t = if crate::plugin_host::hooks::app_handle(&mut self.reader, ev, &mut cx) {
            Transition::None
        } else {
            self.reader.handle(ev, &mut cx).unwrap()
        };
        if t != Transition::None {
            self.transitions.push(t);
        }
        if let Some(s) = self.settings_update.take() {
            s.save(&self.paths.settings()).unwrap();
            self.settings = s;
        }
    }

    /// Advances the clock and feeds the recogniser's timer events (holds).
    fn tick(&mut self, ms: u64) {
        self.clock_ms += ms;
        let events = self.rec.tick(Timestamp::from_millis(self.clock_ms));
        let p = providers();
        for ev in events {
            self.handle_event(&ev, &p);
        }
        self.paint();
    }

    /// A point on a text line of the shown page (left part, so not a link).
    fn text_point(&mut self) -> Point {
        let page = self.reader.page();
        let doc = self.reader.doc.as_deref_mut().unwrap();
        for y in (500..1300).step_by(20) {
            let p = Point::new(300, y);
            if let Ok(Some(s)) = doc.select(page, p, p) {
                if s.text.chars().any(char::is_alphabetic) && s.rects.iter().any(|r| r.contains(p)) {
                    doc.clear_selection();
                    return p;
                }
            }
        }
        panic!("no text on page {page}");
    }
}

#[test]
fn long_press_selects_drag_extends_and_highlight_is_saved() {
    let mut h = Harness::new("select", "juliet.epub");
    h.reader.go_to_page(30);
    h.paint();
    let page = h.probe.panel().clone();
    let p = h.text_point();
    h.frame(400, p.x, p.y, TouchPhase::Down);
    h.tick(700);
    assert!(h.reader.sel.is_some(), "hold on text selects");
    let one = h.reader.sel.as_ref().unwrap().text.clone();
    // A whole word, not one character.
    assert!(one.chars().count() > 1 && !one.contains(' '), "{one:?}");
    h.frame(100, p.x + 500, p.y + 120, TouchPhase::Move);
    let sel = h.reader.sel.clone().unwrap();
    assert!(sel.text.len() > one.len(), "{one:?} -> {:?}", sel.text);
    // Shown inverted with a DU update.
    assert_eq!(h.probe.updates().last().unwrap().class, RefreshClass::Du);
    h.shot("01-dragging");
    let n = h.probe.updates().len();
    h.frame(100, p.x + 500, p.y + 120, TouchPhase::Up);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Selection));
    assert_eq!(h.reader.page(), 30, "the long press did not turn the page");
    // The lift shows the selection box again with GL16 (anti-aliasing back after DU).
    let box_ = selection::bounds(&sel.rects).unwrap();
    assert!(h.probe.updates()[n..].iter().any(|u| u.rect.contains(Point::new(box_.x, box_.y)) && u.class == RefreshClass::Gl16), "{:?}", &h.probe.updates()[n..]);
    h.shot("02-popup");
    let n = h.probe.updates().len();
    h.tap_button("sel-highlight");
    assert_eq!(h.kind(), None);
    // Only the changed pixels (highlight lines, the closed card) are refreshed.
    let ups = &h.probe.updates()[n..];
    assert!(!ups.is_empty() && ups.iter().all(|u| u.rect != FULL), "{ups:?}");
    let st = BookState::load(&h.reader.path).unwrap();
    assert_eq!(st.highlights.len(), 1);
    assert_eq!(st.highlights[0].text, sel.text);
    assert!(st.highlights[0].start.starts_with("/body/DocFragment"), "{}", st.highlights[0].start);
    // The highlight is drawn: some white pixels of the page turned gray.
    let r = sel.rects[0];
    let panel = h.probe.panel();
    let grayed = (r.y..r.bottom()).flat_map(|y| (r.x..r.right()).map(move |x| (x, y))).filter(|&(x, y)| page.get(x, y) == Some(255) && panel.get(x, y).is_some_and(|v| (150..240).contains(&v))).count();
    assert!(grayed > 100, "{grayed}");
    h.shot("03-highlight");
    // Cancel leaves no highlight; Search searches the selection.
    h.frame(400, p.x, p.y + 200, TouchPhase::Down);
    h.tick(700);
    h.frame(100, p.x, p.y + 200, TouchPhase::Up);
    if h.kind() == Some(overlay::OverlayKind::Selection) {
        h.tap_button("sel-search");
        assert_eq!(h.kind(), Some(overlay::OverlayKind::Search));
        assert!(!h.reader.query.is_empty());
        assert!(!h.reader.hits.as_ref().unwrap().is_empty());
    }
    assert_eq!(BookState::load(&h.reader.path).unwrap().highlights.len(), 1);
}

/// A tap on a saved highlight opens its selection card with "Remove highlight" in
/// place of "Highlight"; removing it clears the page and the sidecar.
#[test]
fn tap_on_a_highlight_opens_its_card_and_removes_it() {
    let mut h = Harness::new("hl-tap", "juliet.epub");
    h.reader.go_to_page(30);
    h.paint();
    let p = h.text_point();
    h.frame(400, p.x, p.y, TouchPhase::Down);
    h.tick(700);
    h.frame(100, p.x + 300, p.y, TouchPhase::Move);
    h.frame(100, p.x + 300, p.y, TouchPhase::Up);
    let sel = h.reader.sel.clone().unwrap();
    h.tap_button("sel-highlight");
    assert_eq!(BookState::load(&h.reader.path).unwrap().highlights.len(), 1);
    assert!(h.reader.overlay().is_none() && h.reader.sel.is_none());
    // Tap the highlight.
    let r = sel.rects[0];
    h.tap(Rect::new(r.x + 4, r.y + 2, 8, r.h.saturating_sub(4).max(1)));
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Selection), "{:?}", h.reader.actions());
    let o = h.reader.overlay().unwrap();
    assert!(o.button_rect("sel-unhighlight").is_some() && o.button_rect("sel-highlight").is_none());
    assert_eq!(h.reader.sel.as_ref().unwrap().text, sel.text, "the card quotes the highlight");
    assert_eq!(h.reader.doc.as_deref().unwrap().selection().map(|s| s.text.clone()), Some(sel.text.clone()), "plugins read it");
    h.shot("01-highlight-card");
    h.tap_button("sel-unhighlight");
    assert_eq!(h.kind(), None);
    assert!(h.reader.saved_hl.is_empty());
    assert!(BookState::load(&h.reader.path).unwrap().highlights.is_empty());
    // A new selection afterwards offers "Highlight" again.
    h.frame(400, p.x, p.y, TouchPhase::Down);
    h.tick(700);
    h.frame(100, p.x, p.y, TouchPhase::Up);
    if h.kind() == Some(overlay::OverlayKind::Selection) {
        assert!(h.reader.overlay().unwrap().button_rect("sel-highlight").is_some());
    }
}

impl Harness {
    /// The selection drag has rested in its turn zone long enough: backdates the rest
    /// and delivers the timer the reader asked for (as the app loop does).
    fn rest_in_turn_zone(&mut self) {
        let (_, since) = self.reader.sel_edge.as_mut().expect("the drag is in a turn zone");
        *since -= selection::EDGE_DWELL;
        assert!(self.reader.timeout().is_some_and(|t| t.is_zero()));
        let p = providers();
        self.handle_event(&UiEvent::Timer(0), &p);
        self.paint();
    }

    /// A point on a word of `page` (not shown), well inside the page's text.
    fn word_point(&mut self, page: usize, y: i32) -> Point {
        let doc = self.reader.doc.as_deref_mut().unwrap();
        let p = Point::new(300, y);
        let s = doc.select(page, p, p).unwrap().unwrap();
        let r = s.rects[0];
        doc.clear_selection();
        Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)
    }
}

/// A drag held at the bottom edge moves on half a page: the lower half of the page at
/// the top, the upper half of the next page below, and the selection continues at the
/// next page's first word in the middle of the screen. The card, the document's
/// selection (plugins) and the saved highlight have the whole passage; when the card
/// closes the reader shows the page the selection ended on, with the highlight.
#[test]
fn a_selection_drag_continues_on_the_next_page() {
    let mut h = Harness::new("selnext", "juliet.epub");
    h.reader.go_to_page(30);
    h.paint();
    let plain30 = h.probe.panel().clone();
    let p = h.text_point();
    h.frame(400, p.x, p.y + 400, TouchPhase::Down);
    h.tick(700);
    let anchor = h.reader.sel.clone().expect("hold on text selects");
    h.frame(100, p.x + 300, p.y + 460, TouchPhase::Move);
    // Into the bottom band: nothing moves before the rest.
    h.frame(100, 636, 1690, TouchPhase::Move);
    h.frame(100, 640, 1688, TouchPhase::Move);
    assert_eq!(h.reader.page(), 30);
    assert!(h.reader.sel_edge.is_some_and(|(d, _)| d == 1));
    let n = h.probe.updates().len();
    h.rest_in_turn_zone();
    let b = h.reader.bridge.expect("half a page on");
    assert_eq!((h.reader.page(), b.page), (30, 30));
    // The cut is about the middle of the text; the next page's part is below it.
    assert!(b.cut > 500 && b.cut < 1200 && b.split > b.top && b.cut2 > b.top2, "{b:?}");
    // One page-turn update of the whole screen (the reader's turn policy, not DU), with
    // the selection inverted in it.
    let ups = &h.probe.updates()[n..];
    assert!(ups.iter().any(|u| u.rect == FULL && u.class != RefreshClass::Du), "{ups:?}");
    let sel = h.reader.sel.clone().unwrap();
    assert_eq!(sel.page, 31, "the selection runs to the next page's first word");
    let first31 = b.to_screen(&[(31, sel.rects.clone())]);
    let r = *first31.first().expect("the first word of page 31 is shown");
    assert!(r.y >= b.split && r.y < 1696 * 2 / 3, "the selection goes on in the middle of the screen: {r:?}");
    let inverted = h.probe.panel().get(r.x + 2, r.y + 2).unwrap();
    assert!(inverted < 60, "{inverted}");
    // The top of the screen is page 30 from its cut: a row above the selection there.
    let row = |c: &Canvas, y: i32| (0..1272).map(|x| c.get(x, y).unwrap()).collect::<Vec<_>>();
    let above = anchor.rects[0].y - 4;
    if above >= b.cut {
        assert_eq!(row(&h.probe.panel(), above - b.cut + b.top), row(&plain30, above));
    }
    h.shot("01-half-page");
    // More rests in the zone do not move again; leaving it and dragging up extends.
    h.frame(100, 641, 1689, TouchPhase::Move);
    assert!(h.reader.sel_edge.is_none());
    let target = h.word_point(31, b.top2 + (b.cut2 - b.top2) / 2);
    let target = Point::new(target.x, target.y - b.top2 + b.split);
    assert_eq!(b.to_page(target).0, 31);
    h.frame(100, target.x, target.y, TouchPhase::Move);
    let sel = h.reader.sel.clone().unwrap();
    assert_eq!(sel.page, 31);
    h.frame(100, target.x, target.y, TouchPhase::Up);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Selection));
    assert!(h.reader.bridge.is_some(), "the half-page view stays while the card is up");
    h.shot("02-half-page-card");
    // The whole passage, from the anchor on page 30 to the word dragged to on 31.
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert_eq!(doc.selection().map(|s| s.text.clone()), Some(sel.text.clone()), "plugins read the whole passage");
    assert_eq!(sel.start, anchor.start);
    let end30 = selection::page_end(Size::new(1272, 1696));
    let last30 = doc.select(30, end30, end30).unwrap().unwrap().text;
    let first31 = doc.select(31, Point::new(0, 0), Point::new(0, 0)).unwrap().unwrap().text;
    assert!(sel.text.starts_with(&anchor.text), "{:?}", sel.text);
    assert!(sel.text.contains(&format!("{last30}\n{first31}")), "{:?} / {last30:?} {first31:?}", sel.text);
    assert_eq!(doc.page_of(&sel.start).unwrap(), 30);
    assert_eq!(doc.page_of(&sel.end).unwrap(), 31);
    let o = h.reader.overlay.as_mut().unwrap();
    let id = o.ids["card"];
    let card_quote = o.tree.get_mut::<selection::SelectionCard>(id).unwrap().quote.clone();
    assert_eq!(card_quote, selection::quote(&sel, 300));
    // Highlight: saved once, from page 30 to page 31; the reader shows page 31 whole.
    h.tap_button("sel-highlight");
    assert_eq!(h.kind(), None);
    assert!(h.reader.bridge.is_none());
    assert_eq!(h.reader.page(), 31, "the page the selection ended on");
    let st = BookState::load(&h.reader.path).unwrap();
    assert_eq!(st.highlights.len(), 1);
    assert_eq!(st.highlights[0].text, sel.text);
    assert_eq!(st.highlights[0].start, sel.start.to_string());
    assert_eq!(st.highlights[0].end, sel.end.to_string());
    let shown31 = h.probe.panel().clone();
    let doc = h.reader.doc.as_deref_mut().unwrap();
    let r31 = doc.range_rects(31, &sel.start, &sel.end);
    let r30 = doc.range_rects(30, &sel.start, &sel.end);
    assert!(!r31.is_empty() && !r30.is_empty());
    let gray31 = r31.iter().flat_map(|r| (r.y..r.bottom()).flat_map(move |y| (r.x..r.right()).map(move |x| (x, y)))).filter(|&(x, y)| shown31.get(x, y).is_some_and(|v| (150..240).contains(&v))).count();
    assert!(gray31 > 100, "page 31: {gray31}");
    h.shot("03-highlight-page-31");
    // ...and on page 30, from the anchor word to the page end.
    h.reader.go_to_page(30);
    h.paint();
    let n = grayed(&h, &plain30, &r30);
    assert!(n > 100, "page 30: {n}");
    assert!(r30.last().unwrap().bottom() > anchor.rects[0].bottom(), "the highlight runs to the end of page 30");
}

/// Held at the bottom again, the half-page view goes on to the next page whole (the
/// selection runs to the last word that was shown, now in the middle of the screen);
/// the top edge goes back half a page at a time the same way.
#[test]
fn a_selection_drag_moves_half_a_page_each_rest() {
    let mut h = Harness::new("selhalf", "juliet.epub");
    h.reader.go_to_page(30);
    h.paint();
    let p = h.text_point();
    h.frame(400, p.x, p.y + 400, TouchPhase::Down);
    h.tick(700);
    let rest = |h: &mut Harness, y: i32| {
        h.frame(100, 640, 900, TouchPhase::Move);
        h.frame(100, 636, y, TouchPhase::Move);
        h.frame(100, 640, y + if y > 800 { -2 } else { 2 }, TouchPhase::Move);
        h.rest_in_turn_zone();
    };
    rest(&mut h, 1690);
    let b = h.reader.bridge.expect("half a page on");
    rest(&mut h, 1690);
    assert_eq!((h.reader.page(), h.reader.bridge), (31, None), "the next page whole");
    let sel = h.reader.sel.clone().unwrap();
    assert_eq!(sel.page, 31);
    let end = *sel.rects.last().unwrap();
    assert!((end.bottom() - b.cut2).abs() < 8, "the selection ends at the last line the half-page view showed: {end:?} {b:?}");
    assert!(end.bottom() < 1696 * 2 / 3, "in the middle of the screen: {end:?}");
    // Back: half a page (pages 30-31), then page 30 whole.
    rest(&mut h, 8);
    let back = h.reader.bridge.expect("half a page back");
    assert_eq!((h.reader.page(), back.page), (30, 30));
    rest(&mut h, 8);
    assert_eq!((h.reader.page(), h.reader.bridge), (30, None));
    let sel = h.reader.sel.clone().unwrap();
    assert_eq!(sel.page, 30);
    // The anchor is below the cut on page 30: the passage runs from the first line the
    // half-page view showed down to it.
    let anchor_y = h.reader.sel_word.as_ref().unwrap().sel.rects[0].y;
    let start = sel.rects[0];
    assert!(anchor_y > back.cut && (start.y - back.cut).abs() < 8, "back at the first line the half-page view showed: {start:?} {back:?}");
    h.frame(100, 640, 900, TouchPhase::Up);
    h.tap_button("sel-cancel");
    assert!(h.reader.sel.is_none() && h.reader.bridge.is_none());
}

/// Dragging the selection to the top edge goes back half a page: the end of the
/// previous page above the start of this one, and the selection continues at the
/// previous page's last word. When the card closes the reader shows the previous page.
#[test]
fn a_selection_drag_continues_on_the_previous_page() {
    let mut h = Harness::new("selprev", "juliet.epub");
    h.reader.go_to_page(31);
    h.paint();
    let p = h.text_point();
    h.frame(400, p.x, p.y, TouchPhase::Down);
    h.tick(700);
    let anchor = h.reader.sel.clone().unwrap();
    h.frame(100, 636, 8, TouchPhase::Move);
    h.rest_in_turn_zone();
    let b = h.reader.bridge.expect("half a page back");
    assert_eq!((h.reader.page(), b.page), (30, 30));
    let target = h.word_point(30, b.cut + 60);
    let target = Point::new(target.x, target.y - b.cut + b.top);
    h.frame(100, 640, 400, TouchPhase::Move);
    h.frame(100, target.x, target.y, TouchPhase::Move);
    h.frame(100, target.x, target.y, TouchPhase::Up);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Selection));
    let sel = h.reader.sel.clone().unwrap();
    assert_eq!(sel.page, 30);
    assert_eq!(sel.end, anchor.end, "the passage ends at the word the press selected");
    assert!(sel.text.ends_with(&anchor.text), "{:?}", sel.text);
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert_eq!(doc.selection().map(|s| s.text.clone()), Some(sel.text.clone()));
    assert_eq!(doc.page_of(&sel.start).unwrap(), 30);
    h.tap_button("sel-cancel");
    assert!(h.reader.sel.is_none());
    assert_eq!((h.reader.page(), h.reader.bridge), (30, None));
}

#[test]
fn long_press_on_blank_space_still_turns_the_page() {
    let mut h = Harness::new("blank", "synthetic.epub");
    // Bottom margin: no text there.
    h.frame(400, 1000, 1640, TouchPhase::Down);
    h.tick(700);
    h.frame(100, 1000, 1640, TouchPhase::Up);
    assert!(h.reader.sel.is_none());
    assert_eq!(h.reader.page(), 1);
}


#[test]
fn page_numbers_follow_an_aa_change() {
    let mut h = Harness::new("stale", "juliet.epub");
    h.reader.go_to_page(100);
    h.paint();
    // Fill the caches: Go to (TOC pages) and links of the page.
    let before = h.reader.goto_info(0);
    let _ = h.reader.link_at(Point::new(10, 10));
    h.tap_at(636, 848);
    h.tap_button("aa");
    for _ in 0..4 {
        let r = slider_plus(&h, "size");
        h.tap(r);
    }
    h.tap_button("close");
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert!(doc.wait_paginated(std::time::Duration::from_secs(60)));
    let page = h.reader.page();
    let after = h.reader.goto_info(0);
    let doc = h.reader.doc.as_deref().unwrap();
    let toc = doc.toc();
    for (row, e) in after.toc.iter().zip(&toc) {
        assert_eq!(row.page, Some(doc.page_of(&e.position).unwrap()), "{}", row.title);
    }
    assert_ne!(before.toc.iter().map(|r| r.page).collect::<Vec<_>>(), after.toc.iter().map(|r| r.page).collect::<Vec<_>>());
    assert_eq!(after.current, current_chapter(doc, &toc, page));
}

#[test]
fn jumps_work_while_pagination_runs() {
    let mut h = Harness::open("unpaginated", "juliet.epub", false);
    let doc = h.reader.doc.as_deref().unwrap();
    let toc = doc.toc();
    let last = toc.len() - 1;
    let target = toc[last].position.clone();
    h.with_cx(|r, cx| r.action(&format!("toc:{last}"), cx)).unwrap();
    h.paint();
    let page = h.reader.page();
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert!(doc.wait_paginated(std::time::Duration::from_secs(60)));
    assert_eq!(page, doc.page_of(&target).unwrap(), "last TOC entry");
    // Percent on a fresh reader.
    let mut h = Harness::open("unpaginated2", "juliet.epub", false);
    h.with_cx(|r, cx| r.action("pct:90", cx)).unwrap();
    let page = h.reader.page();
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert!(doc.wait_paginated(std::time::Duration::from_secs(60)));
    let pages = doc.page_count().pages;
    let want = (0.9 * (pages - 1) as f32).round() as usize;
    assert!((page as i64 - want as i64).abs() <= 2, "page {page} want {want} of {pages}");
}

mod plugin_stub {
    use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Registrar};

    /// Registers a menu item and a selection action; both show a toast with what the
    /// plugin saw.
    pub struct Stub(pub PluginManifest);

    impl Plugin for Stub {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_load(&mut self, reg: &mut Registrar) -> ink_plugin::Result<()> {
            reg.menu_item("hello", "Hello", 1);
            reg.selection_action("explain", "Explain");
            Ok(())
        }
        fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> ink_plugin::Result<()> {
            let sel = cx.document()?.selection().map(|s| s.text.clone()).unwrap_or_default();
            cx.ui()?.toast(&format!("{id}:{sel}"));
            Ok(())
        }
    }

    pub fn new() -> Stub {
        Stub(PluginManifest::new("stub", "1", &[Permission::Document, Permission::Ui]))
    }
}

#[test]
fn plugin_menu_items_and_selection_actions_reach_the_plugin_and_its_toast_shows() {
    let mut h = Harness::new("plugins", "juliet.epub");
    h.plugins = PluginHost::new(vec![Box::new(plugin_stub::new())]);
    h.reader.go_to_page(30);
    h.paint();
    h.tap_at(636, 848);
    h.shot("01-menu");
    let before = h.probe.panel().clone();
    h.tap_button("plugin-menu:0");
    assert_eq!(h.kind(), None);
    assert!(h.plugins.manager.errors.is_empty(), "{:?}", h.plugins.manager.errors);
    // The toast is painted by the plugin overlay.
    let changed = h.panel_rows_differ(&before, 0, 1696);
    assert!(changed > 0);
    h.shot("02-toast");
    // Selection action: the plugin reads the selection.
    h.tick(10_000);
    let p = h.text_point();
    h.frame(400, p.x, p.y, TouchPhase::Down);
    h.tick(700);
    h.frame(100, p.x, p.y, TouchPhase::Up);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Selection));
    let word = h.reader.sel.as_ref().unwrap().text.clone();
    assert!(word.chars().count() > 1, "{word:?}");
    h.tap_button("plugin-sel:0");
    assert_eq!(h.reader.actions().last().map(String::as_str), Some("plugin-sel:0"));
    assert!(h.plugins.manager.errors.is_empty(), "{:?}", h.plugins.manager.errors);
    h.shot("03-selection-toast");
}

/// After a rotation the App gives a new canvas and calls on_resize: the book is laid
/// out for the landscape size and the reading position stays.
#[test]
fn on_resize_lays_the_book_out_for_the_new_size() {
    let mut h = Harness::new("resize", "juliet.epub");
    h.reader.go_to_page(20);
    h.paint();
    let pos = h.reader.doc.as_deref_mut().unwrap().position_of_page(20).unwrap();
    h.dev.screen.set_rotation(ink_device::Rotation::Clockwise).unwrap();
    let size = h.dev.screen.size();
    assert!(size.w > size.h);
    h.canvas = Canvas::new(size.w as usize, size.h as usize);
    h.with_cx(|r, cx| r.on_resize(cx)).unwrap();
    h.paint();
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert_eq!(doc.layout().width, size.w);
    let page = doc.page_of(&pos).unwrap();
    assert!(page.abs_diff(h.reader.page) <= 1, "position page {page}, shown {}", h.reader.page);
    h.shot("01-landscape");
}

/// A half turn keeps the canvas size; the App still calls on_resize (new blank canvas)
/// and the page must come back as a full GC16 damage on the same page.
#[test]
fn on_resize_after_a_half_turn_redraws_the_same_page() {
    let mut h = Harness::new("half-turn", "juliet.epub");
    h.reader.go_to_page(20);
    h.paint();
    h.dev.screen.set_rotation(ink_device::Rotation::UpsideDown).unwrap();
    let size = h.dev.screen.size();
    h.canvas = Canvas::new(size.w as usize, size.h as usize);
    h.with_cx(|r, cx| r.on_resize(cx)).unwrap();
    let n = h.probe.updates().len();
    h.with_cx(|r, cx| crate::plugin_host::hooks::app_paint(r, cx)).unwrap();
    assert_eq!(h.reader.page, 20);
    // The reader shows a full page itself (not as returned damage): one full GC16.
    let all = h.canvas.bounds();
    let u = &h.probe.updates()[n..];
    assert!(u.iter().any(|d| d.class == RefreshClass::Gc16 && d.rect == all), "{u:?}");
    assert!(h.canvas.pixels.iter().any(|&v| v < 0x40), "the page is drawn on the new canvas");
}

/// The sidecar percent is the location of the page start, not (page + 1) / pages: page 1
/// of a book still being paginated is not 17 %.
#[test]
fn sidecar_percent_is_position_based_while_pagination_runs() {
    let mut h = Harness::open("percent", "juliet.epub", false);
    let st = BookState::load(&h.reader.path).unwrap();
    assert_eq!(st.percent, Some(0.0), "page 1 is at 0 %");
    h.tap_at(1000, 900);
    h.tap_at(1000, 900);
    assert_eq!(h.reader.page(), 2);
    // Page turns do no file I/O: the position is written once the page rested
    // (POSITION_SAVE_IDLE) or on a power event (sleep, cover).
    assert_eq!(BookState::load(&h.reader.path).unwrap().percent, Some(0.0), "written at the turn");
    assert!(h.reader.timeout().is_some_and(|t| t <= POSITION_SAVE_IDLE));
    h.feed(InputEvent::Power(ink_device::PowerEvent::GoingToScreenSaver));
    assert!(h.reader.position_unsaved.is_none());
    let pct = BookState::load(&h.reader.path).unwrap().percent.unwrap();
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert!(doc.wait_paginated(std::time::Duration::from_secs(60)));
    assert!((pct - progress(doc, 2)).abs() < 1e-6, "{pct}");
    assert!(pct < 0.05, "{pct}");
    // The last page of a fully paginated book is 100 %.
    let last = doc.page_count().pages - 1;
    h.reader.go_to_page(last);
    h.paint();
    assert_eq!(BookState::load(&h.reader.path).unwrap().percent, Some(1.0));
}

mod gesture_stub {
    use ink_plugin::registry::StatusSlot;
    use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Registrar};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Times `on_action("dt")` ran (double tap binding).
    pub static CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Binds a double tap and a corner hold, and shows a status text bottom-left.
    pub struct Stub(pub PluginManifest);

    impl Plugin for Stub {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_load(&mut self, reg: &mut Registrar) -> ink_plugin::Result<()> {
            reg.gesture("dt", "double_tap", "Double tap");
            reg.gesture("bl", "corner_hold:bottom_left", "Corner");
            reg.status_widget("st", StatusSlot::Left, "STATUS 42");
            Ok(())
        }
        fn on_action(&mut self, _cx: &mut HostCtx, id: &str) -> ink_plugin::Result<()> {
            if id == "dt" || id == "bl" {
                CALLS.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    pub fn new() -> Stub {
        Stub(PluginManifest::new("gstub", "1", &[Permission::Ui]))
    }
}

/// Plugin gesture bindings get the gestures the reader does not use (double tap, corner
/// holds other than the exit corner) and status widgets are drawn in the page margins.
#[test]
fn plugin_gestures_and_status_widgets() {
    use std::sync::atomic::Ordering;
    let mut h = Harness::new("pgest", "juliet.epub");
    h.reader.go_to_page(30);
    h.paint();
    let plain = h.probe.panel().clone();
    h.plugins = PluginHost::new(vec![Box::new(gesture_stub::new())]);
    let n = h.probe.updates().len();
    h.paint();
    assert!(h.plugins.manager.errors.is_empty(), "{:?}", h.plugins.manager.errors);
    // Only the bar text rows are refreshed, with GL16 (not the full page).
    let ups = &h.probe.updates()[n..];
    assert!(!ups.is_empty() && ups.iter().all(|u| u.rect.h < 200 && u.class == RefreshClass::Gl16), "{ups:?}");
    // The status text appears bottom-left, only in the bottom margin.
    let height = plain.height;
    let margin = h.reader.doc.as_deref().unwrap().layout().margin_bottom as usize;
    assert!(h.panel_rows_differ(&plain, height - margin, height) > 50);
    assert_eq!(h.panel_rows_differ(&plain, 0, height - margin), 0);
    h.shot("01-status");
    // Double tap: the plugin runs. The recogniser's DoubleTapMode::After delivers both
    // taps first (two page turns) until screens can choose their gesture config.
    let before = gesture_stub::CALLS.load(Ordering::SeqCst);
    h.frame(400, 1000, 900, TouchPhase::Down);
    h.frame(50, 1000, 900, TouchPhase::Up);
    h.frame(80, 1000, 900, TouchPhase::Down);
    h.frame(50, 1000, 900, TouchPhase::Up);
    h.tick(600);
    assert_eq!(gesture_stub::CALLS.load(Ordering::SeqCst), before + 1, "{:?}", h.transitions);
    let page = h.reader.page();
    // Bottom-left corner hold (no text there): the plugin, not a page turn.
    h.frame(400, 20, 1680, TouchPhase::Down);
    h.tick(700);
    h.frame(100, 20, 1680, TouchPhase::Up);
    assert_eq!(gesture_stub::CALLS.load(Ordering::SeqCst), before + 2);
    assert_eq!(h.reader.page(), page, "the corner hold did not turn the page");
}

/// Gray (highlight) pixels inside `rects` on the panel where `plain` was white.
fn grayed(h: &Harness, plain: &Canvas, rects: &[Rect]) -> usize {
    let panel = h.probe.panel();
    rects
        .iter()
        .flat_map(|r| (r.y..r.bottom()).flat_map(move |y| (r.x..r.right()).map(move |x| (x, y))))
        .filter(|&(x, y)| plain.get(x, y) == Some(255) && panel.get(x, y).is_some_and(|v| (150..240).contains(&v)))
        .count()
}

/// A highlight saved in the sidecar is drawn after the book is opened again and after
/// a font size change (Document::range_rects through the app's providers).
#[test]
fn saved_highlights_are_drawn_after_reopen_and_relayout() {
    let mut h = Harness::new("hlreopen", "juliet.epub");
    h.reader.go_to_page(30);
    h.paint();
    let plain = h.probe.panel().clone();
    let p = h.text_point();
    h.frame(400, p.x, p.y, TouchPhase::Down);
    h.tick(700);
    h.frame(100, p.x + 400, p.y + 60, TouchPhase::Move);
    h.frame(100, p.x + 400, p.y + 60, TouchPhase::Up);
    let sel = h.reader.sel.clone().unwrap();
    h.tap_button("sel-highlight");
    assert_eq!(BookState::load(&h.reader.path).unwrap().highlights.len(), 1);
    // Open the book again in a new reader: no session highlights, only the sidecar.
    let path = h.reader.path.clone();
    h.reader = ReaderScreen::new(path);
    h.with_cx(|r, cx| r.on_enter(cx)).unwrap();
    assert!(h.reader.doc.as_deref_mut().unwrap().wait_paginated(std::time::Duration::from_secs(60)));
    h.paint();
    assert_eq!(h.reader.page(), 30);
    assert!(h.reader.session_hl.is_empty());
    let n = grayed(&h, &plain, &sel.rects);
    assert!(n > 100, "reopened: {n} gray pixels");
    h.shot("01-reopened");
    // Bigger font: the highlight moves with its text.
    h.tap_at(636, 848);
    h.tap_button("aa");
    let r = slider_plus(&h, "size");
    h.tap(r);
    h.tap_button("close");
    let doc = h.reader.doc.as_deref_mut().unwrap();
    assert!(doc.wait_paginated(std::time::Duration::from_secs(60)));
    let np = doc.page_of(&sel.start).unwrap();
    let rects = doc.range_rects(np, &sel.start, &sel.end);
    assert!(!rects.is_empty());
    h.reader.go_to_page(np);
    h.paint();
    let panel = h.probe.panel().clone();
    let gray = rects.iter().flat_map(|r| (r.y..r.bottom()).flat_map(move |y| (r.x..r.right()).map(move |x| (x, y)))).filter(|&(x, y)| panel.get(x, y).is_some_and(|v| (150..240).contains(&v))).count();
    assert!(gray > 100, "after relayout: {gray}");
    h.shot("02-relayout");
}

/// While the hit bar shows, the hits on the page are framed; closing it removes them.
#[test]
fn search_hits_are_marked_on_the_page() {
    let mut h = Harness::new("hitmarks", "juliet.epub");
    h.reader.query = "Romeo".into();
    h.with_cx(|r, cx| r.run_search(cx));
    h.paint();
    h.with_cx(|r, cx| r.action("hit:3", cx)).unwrap();
    h.paint();
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Hits));
    let page = h.reader.page();
    let hit = h.reader.hits.as_ref().unwrap()[3].clone();
    let rects = h.reader.doc.as_deref_mut().unwrap().hit_rects(page, &hit);
    assert_eq!(rects.len(), 1, "{hit:?}");
    let r = rects[0];
    // The frame: black pixels just left of the word, inside the line height.
    let black = |h: &Harness| (r.y..r.bottom()).filter(|&y| h.probe.panel().get(r.x - 2, y) == Some(0)).count();
    assert!(black(&h) as u32 > r.h / 2, "frame left of {r:?}");
    h.shot("01-hit-marked");
    h.key(Key::Back);
    assert_eq!(h.kind(), None);
    assert_eq!(black(&h), 0, "marks gone after the hit bar closed");
    h.shot("02-closed");
}

#[test]
fn a_plugin_go_to_moves_the_reader() {
    use crate::router::AppScreen;
    let mut h = Harness::new("plugin-goto", "juliet.epub");
    let pos = h.reader.document().unwrap().position_of_page(4).unwrap().to_string();
    assert!(h.reader.plugin_go_to(&pos));
    assert_eq!(h.reader.page(), 4);
    assert!(h.reader.plugin_go_to("not a position"));
    assert_eq!(h.reader.page(), 4);
}

mod lookup_stub {
    use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Registrar};
    use std::sync::{Arc, Mutex};

    /// What the stubs saw: `action:<id>:<selection>` and `answer:<id>:<result>`.
    pub type Log = Arc<Mutex<Vec<String>>>;

    /// A dictionary stand-in: the selection action `lookup` shows a paged card (two
    /// dictionaries) for the selected text; `next`/`prev` send the other page.
    pub struct Dict(pub PluginManifest, pub Log, pub usize, pub String);

    impl Dict {
        fn card(&self, cx: &mut HostCtx) -> ink_plugin::Result<()> {
            let name = ["Alpha", "Beta"][self.2];
            let body = format!("_noun_\n{} in {name}\n— {}/2 · {name}", self.3, self.2 + 1);
            cx.ui()?.dialog(1, &self.3, &body, &[("Close", "close"), ("Add", "add"), ("Full view", "full")]);
            Ok(())
        }
    }

    impl Plugin for Dict {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_load(&mut self, reg: &mut Registrar) -> ink_plugin::Result<()> {
            reg.selection_action("lookup", "Look up");
            Ok(())
        }
        fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> ink_plugin::Result<()> {
            self.3 = cx.document()?.selection().map(|s| s.text.clone()).unwrap_or_default();
            self.1.lock().unwrap().push(format!("action:{id}:{}", self.3));
            self.2 = 0;
            self.card(cx)
        }
        fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> ink_plugin::Result<()> {
            self.1.lock().unwrap().push(format!("answer:{id}:{result}"));
            match result {
                "next" | "prev" => {
                    self.2 = 1 - self.2;
                    self.card(cx)
                }
                _ => Ok(()),
            }
        }
    }

    /// An Insightful stand-in: its selection action `menu` records the selection.
    pub struct Insightful(pub PluginManifest, pub Log);

    impl Plugin for Insightful {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_load(&mut self, reg: &mut Registrar) -> ink_plugin::Result<()> {
            reg.selection_action("menu", "Insightful");
            Ok(())
        }
        fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> ink_plugin::Result<()> {
            let sel = cx.document()?.selection().map(|s| s.text.clone()).unwrap_or_default();
            self.1.lock().unwrap().push(format!("insightful:{id}:{sel}"));
            Ok(())
        }
    }

    /// Both stubs sharing one log.
    /// A lookup provider for one language only; logs `picky:<selection>` when used.
    pub struct Picky(pub PluginManifest, pub Log, pub &'static str);

    impl Plugin for Picky {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_load(&mut self, reg: &mut Registrar) -> ink_plugin::Result<()> {
            reg.selection_action("lookup", "Look up");
            Ok(())
        }
        fn on_action(&mut self, cx: &mut HostCtx, _id: &str) -> ink_plugin::Result<()> {
            let t = cx.document()?.selection().map(|s| s.text.clone()).unwrap_or_default();
            self.1.lock().unwrap().push(format!("picky:{t}"));
            Ok(())
        }
        fn claims_lookup(&self, _text: &str, lang: Option<&str>) -> bool {
            lang.is_some_and(|l| l.starts_with(self.2))
        }
    }

    /// [`new`] with a [`Picky`] provider for `lang` before the dictionary.
    pub fn with_picky(lang: &'static str) -> (Vec<Box<dyn Plugin>>, Log) {
        let (mut plugins, log) = new();
        plugins.insert(0, Box::new(Picky(PluginManifest::new("picky", "1", &[Permission::Document]), log.clone(), lang)));
        (plugins, log)
    }

    pub fn new() -> (Vec<Box<dyn Plugin>>, Log) {
        let log = Log::default();
        let dict = Dict(PluginManifest::new("dictionary", "1", &[Permission::Document, Permission::Ui]), log.clone(), 0, String::new());
        let ins = Insightful(PluginManifest::new(insightful::plugin::NAME, "1", &[Permission::Document, Permission::Ui]), log.clone());
        (vec![Box::new(dict), Box::new(ins)], log)
    }
}

impl Harness {
    /// Long-presses a word on page 30 with the lookup stubs loaded: returns the log, the
    /// word, the selection card's and the dictionary card's rectangles.
    fn long_press_lookup(test: &str) -> (Harness, lookup_stub::Log, String, Rect, Rect) {
        let mut h = Harness::new(test, "juliet.epub");
        let (plugins, log) = lookup_stub::new();
        h.plugins = PluginHost::new(plugins);
        h.reader.go_to_page(30);
        h.paint();
        let p = h.text_point();
        h.frame(400, p.x, p.y, TouchPhase::Down);
        h.tick(700);
        h.frame(100, p.x, p.y, TouchPhase::Up);
        let word = h.reader.sel.as_ref().map(|s| s.text.clone()).unwrap_or_default();
        assert!(!word.is_empty() && !word.contains(' '), "{word:?}");
        assert_eq!(h.kind(), Some(overlay::OverlayKind::Selection));
        let o = h.reader.overlay().unwrap();
        let card = o.tree.bounds(o.ids["card"]).unwrap();
        let dict = o.tree.bounds(*o.ids.get("dict").expect("the dictionary card is in the selection overlay")).unwrap();
        (h, log, word, card, dict)
    }

    /// The dictionary card's footer.
    fn dict_footer(&mut self) -> Option<String> {
        let o = self.reader.overlay.as_mut()?;
        let id = *o.ids.get("dict")?;
        o.tree.get_mut::<crate::plugin_host::CardDialog>(id)?.footer.clone()
    }

    /// A one-finger swipe from `from` by `dx` px.
    fn swipe(&mut self, from: Point, dx: i32) {
        self.frame(400, from.x, from.y, TouchPhase::Down);
        for k in 1..=4 {
            self.frame(30, from.x + dx * k / 4, from.y, TouchPhase::Move);
        }
        self.frame(30, from.x + dx, from.y, TouchPhase::Up);
    }
}

/// A lookup provider for another language placed before the dictionary does not take
/// the long press; one for the book's language does.
#[test]
fn long_press_lookup_goes_to_the_first_provider_that_claims_it() {
    for (lang, want) in [("de", "action:lookup:"), ("en", "picky:")] {
        let mut h = Harness::new(&format!("lookup-chain-{lang}"), "juliet.epub");
        let (plugins, log) = lookup_stub::with_picky(lang);
        h.plugins = PluginHost::new(plugins);
        h.reader.go_to_page(30);
        h.paint();
        let p = h.text_point();
        h.frame(400, p.x, p.y, TouchPhase::Down);
        h.tick(700);
        h.frame(100, p.x, p.y, TouchPhase::Up);
        let word = h.reader.sel.as_ref().map(|s| s.text.clone()).unwrap_or_default();
        assert_eq!(log.lock().unwrap().as_slice(), [format!("{want}{word}")], "provider for {lang}");
    }
}

/// Long press on a word: the dictionary card and the selection card are siblings in one
/// overlay tree (no plugin layer). A swipe on the card switches dictionaries in one
/// partial update and never turns the page; one tap on the page closes both cards.
#[test]
fn long_press_lookup_cards_are_siblings_swipes_switch_and_one_tap_closes_both() {
    let (mut h, log, word, card, dict) = Harness::long_press_lookup("lookup");
    assert!(h.plugins.ui.layer_names().is_empty(), "no plugin layer: {:?}", h.plugins.ui.layer_names());
    assert_eq!(h.plugins.ui.pending(), 0);
    assert!(card.intersect(&dict).is_empty(), "the cards do not overlap: {card:?} {dict:?}");
    assert_eq!(log.lock().unwrap().as_slice(), [format!("action:lookup:{word}")]);
    assert_eq!(h.dict_footer().as_deref(), Some("1/2 · Alpha"));
    h.shot("01-cards");
    // Swipe left on the card: the next dictionary, one update of the card only.
    let n = h.probe.updates().len();
    h.swipe(Point::new(dict.x + 900, dict.y + 250), -500);
    let ups: Vec<_> = h.probe.updates()[n..].iter().map(|u| (u.rect, u.class)).collect();
    assert_eq!(h.dict_footer().as_deref(), Some("2/2 · Beta"));
    assert_eq!(ups, vec![(dict, RefreshClass::Gl16)], "one partial update per swipe");
    assert_eq!(h.reader.page(), 30, "the swipe did not turn the page");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Selection));
    h.shot("02-swiped");
    // Swipe right: back to the first dictionary.
    h.swipe(Point::new(dict.x + 300, dict.y + 250), 500);
    assert_eq!(h.dict_footer().as_deref(), Some("1/2 · Alpha"));
    assert_eq!(h.reader.page(), 30);
    // A tap on the book text outside both cards closes both at once.
    let outside = [Point::new(636, 120), Point::new(636, 700), Point::new(100, 400), Point::new(1150, 400)]
        .into_iter()
        .find(|p| !card.contains(*p) && !dict.contains(*p))
        .unwrap();
    let n = h.probe.updates().len();
    h.tap_at(outside.x, outside.y);
    assert_eq!(h.kind(), None);
    assert!(h.reader.overlay().is_none() && h.reader.sel.is_none());
    assert!(h.plugins.ui.layer_names().is_empty());
    assert_eq!(h.reader.page(), 30, "the closing tap did not turn the page");
    let ups = &h.probe.updates()[n..];
    assert!(!ups.is_empty() && ups.iter().all(|u| u.rect != FULL), "{ups:?}");
    let log = log.lock().unwrap().clone();
    assert_eq!(log[1..], ["answer:1:next", "answer:1:prev", "answer:1:"].map(String::from), "{log:?}");
    assert!(h.plugins.manager.errors.is_empty(), "{:?}", h.plugins.manager.errors);
    h.shot("03-closed");
}

/// Taps inside either card work as before: a card button answers the plugin and closes
/// only the card; the selection card's buttons still work.
#[test]
fn taps_inside_the_cards_still_work() {
    let (mut h, log, word, card, _) = Harness::long_press_lookup("lookup-taps");
    h.tap_button("pcard:add");
    let o = h.reader.overlay().unwrap();
    assert_eq!(o.tree.bounds(o.ids["card"]), Some(card), "the selection card stays where it was");
    assert_eq!(log.lock().unwrap().last().map(String::as_str), Some("answer:1:add"));
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Selection), "the selection card stays");
    assert!(!h.reader.overlay().unwrap().ids.contains_key("dict"));
    assert!(h.reader.overlay().unwrap().button_rect("pcard:close").is_none());
    h.tap_button("sel-highlight");
    assert_eq!(h.kind(), None);
    assert_eq!(BookState::load(&h.reader.path).unwrap().highlights[0].text, word);
    // The card's close button: the card goes, no second answer when the selection closes.
    let (mut h, log, _, _, _) = Harness::long_press_lookup("lookup-close");
    h.tap_button("pcard:close");
    assert!(!h.reader.overlay().unwrap().ids.contains_key("dict"));
    h.tap_button("sel-cancel");
    assert_eq!(h.kind(), None);
    assert_eq!(log.lock().unwrap()[1..], ["answer:1:close"].map(String::from));
}

/// The selection card shows Insightful in Copy's old place; it runs Insightful's
/// selection action with the selection still set (the same path as its plugin button).
#[test]
fn highlight_popup_has_insightful_and_no_copy() {
    let (mut h, log, word, _, _) = Harness::long_press_lookup("lookup-insightful");
    let i = h.plugins.selection_actions().iter().position(|a| a.plugin == "insightful").unwrap();
    let o = h.reader.overlay.as_mut().unwrap();
    let id = o.ids["card"];
    let labels: Vec<String> = o.tree.get_mut::<selection::SelectionCard>(id).unwrap().tools.iter().map(|t| t.label.clone()).collect();
    assert_eq!(labels[..3], ["Highlight", "Note", "Insightful"].map(String::from), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "Copy"), "{labels:?}");
    assert!(o.button_rect("sel-copy").is_none());
    h.tap_button(&format!("plugin-sel:{i}"));
    assert_eq!(h.kind(), None);
    assert!(log.lock().unwrap().contains(&format!("insightful:menu:{word}")), "{:?}", log.lock().unwrap());
}

/// Screen updates for: menu open, menu close, menu open, Aa sheet open. Printed with
/// `--nocapture` (refresh measurements); the asserts state the partial-refresh policy.
#[test]
fn menu_and_aa_refresh_only_their_rects() {
    let mut h = Harness::new("menu-refresh", "juliet.epub");
    let full = Rect::new(0, 0, 1272, 1696);
    let step = |h: &mut Harness, name: &str, f: &dyn Fn(&mut Harness)| {
        let n = h.probe.updates().len();
        f(h);
        let ups: Vec<_> = h.probe.updates()[n..].iter().map(|u| (u.rect, u.class)).collect();
        println!("{name}: {} updates {ups:?}", ups.len());
        ups
    };
    let open = step(&mut h, "menu open", &|h| h.tap_at(636, 848));
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Menu));
    let close = step(&mut h, "menu close", &|h| h.tap_at(636, 848));
    assert_eq!(h.kind(), None);
    step(&mut h, "menu open", &|h| h.tap_at(636, 848));
    step(&mut h, "Aa open", &|h| h.tap_button("aa"));
    assert!(open.iter().all(|(r, _)| *r != full), "menu open refreshed the full screen: {open:?}");
    assert!(close.iter().all(|(r, _)| *r != full), "menu close refreshed the full screen: {close:?}");
}

/// Back from quick light (a panel over the top of the page): only the panel's rect
/// comes back from the page buffer, with REAGL; not a full GC16.
#[test]
fn reveal_after_a_panel_restores_only_its_rect() {
    let mut h = Harness::new("reveal", "juliet.epub");
    let page = h.probe.panel().clone();
    let panel = Rect::new(0, 0, 1272, 700);
    // The panel drew over the canvas (and the panel showed it).
    h.canvas.fill_rect(panel.x, panel.y, panel.w as i32, panel.h as i32, 0x55);
    let n = h.probe.updates().len();
    h.with_cx(|r, cx| r.on_reveal(cx, panel, RefreshClass::Reagl)).unwrap();
    h.paint();
    let ups: Vec<_> = h.probe.updates()[n..].iter().map(|u| (u.rect, u.class)).collect();
    assert_eq!(ups, vec![(panel, RefreshClass::Reagl)]);
    assert_eq!(h.panel_rows_differ(&page, 0, page.height), 0, "the page is back");
    // A full-screen pop takes the full path.
    let n = h.probe.updates().len();
    h.with_cx(|r, cx| r.on_reveal(cx, FULL, RefreshClass::Gc16)).unwrap();
    h.paint();
    assert!(h.probe.updates()[n..].iter().any(|u| u.rect == FULL && u.class == RefreshClass::Gc16));
}

/// A page turn is one update of the whole page with its status bars: no animation
/// frames, no separate update of the bars afterwards (plugin status texts included),
/// day and night. A turn after the minute tick fell due shows the new clock in its own
/// update (no tick update of the bar rows follows), the Nth turn is the flash, and every
/// pixel drawn is inside an update rect.
#[test]
fn a_page_turn_is_one_update_with_the_bars() {
    for night in [false, true] {
        let mut h = Harness::new(if night { "one-update-night" } else { "one-update" }, "juliet.epub");
        h.refresh.flash_every = 3;
        if night {
            h.dev.screen.set_night_mode(true).unwrap();
            h.refresh.set_night(true);
            h.dev.screen.update(&h.canvas, FULL, RefreshClass::Gc16).unwrap();
        }
        assert!(h.reader.bars.next_tick.is_some(), "the default bars show the clock");
        for k in 0..4 {
            let page = h.reader.page();
            let n = h.probe.updates().len();
            h.frame(400, 1100, 900, TouchPhase::Down);
            if k == 1 {
                // The minute starts while the finger is down; the clock read before it
                // is stale.
                h.reader.bars.next_tick = Some(std::time::Instant::now());
                h.reader.bars.device.as_mut().unwrap().clock = "stale".into();
            }
            h.frame(60, 1100, 900, TouchPhase::Up);
            // A later loop paints nothing more for this turn.
            h.paint();
            h.paint();
            let ups: Vec<_> = h.probe.updates()[n..].iter().map(|u| (u.rect, u.class)).collect();
            assert_eq!(h.reader.page(), page + 1, "night {night} turn {k}");
            assert_eq!(ups.len(), 1, "night {night} turn {k}: {ups:?}");
            assert_eq!(ups[0].0, FULL);
            let want = if k == 2 { RefreshClass::Gc16 } else { RefreshClass::Reagl };
            assert_eq!(ups[0].1, want, "night {night} turn {k}");
            assert!(h.reader.bars.next_tick.is_some_and(|t| t > std::time::Instant::now()), "night {night} turn {k}: tick still due");
            assert_ne!(h.reader.bars.device.as_ref().unwrap().clock, "stale", "night {night} turn {k}: the turn shows the new minute");
        }
        assert_eq!(h.probe.uncovered(), vec![], "night {night}");
    }
}

/// The minute tick reads the device values once and shows the bar rows.
#[test]
fn the_minute_tick_reads_the_device_once() {
    struct Counted(std::rc::Rc<std::cell::Cell<u32>>);
    impl ink_device::Battery for Counted {
        fn state(&self) -> ink_device::Result<ink_device::BatteryState> {
            self.0.set(self.0.get() + 1);
            Ok(ink_device::BatteryState { percent: 57, charging: false })
        }
    }
    let mut h = Harness::new("tick-reads", "juliet.epub");
    let reads = std::rc::Rc::new(std::cell::Cell::new(0));
    h.dev.battery = Box::new(Counted(reads.clone()));
    h.reader.bars.next_tick = Some(std::time::Instant::now());
    let n = h.probe.updates().len();
    let damage = h.with_cx(|r, cx| r.bars_tick(cx)).unwrap();
    assert!(!damage.is_empty(), "the bar rows are redrawn");
    assert_eq!(reads.get(), 1, "battery reads in one tick");
    assert_eq!(h.reader.bars.device.as_ref().unwrap().battery, Some((57, false)));
    assert_eq!(h.probe.updates().len(), n);
}

/// A page turn's one update carries the hardware page-turn wave (next page sweeps left,
/// previous right); with `reader.page_animation = "off"` and for jumps it has none.
#[test]
fn page_turns_carry_the_hardware_wave() {
    use ink_device::Wave;
    let mut h = Harness::new("wave", "juliet.epub");
    let last = |h: &Harness| *h.probe.updates().last().unwrap();
    h.tap_at(1100, 900);
    assert_eq!((last(&h).rect, last(&h).wave), (FULL, Some(Wave::Left)));
    h.tap_at(100, 900);
    assert_eq!((last(&h).rect, last(&h).wave), (FULL, Some(Wave::Right)));
    h.reader.go_to_page(5);
    h.paint();
    assert_eq!(last(&h).wave, None, "a jump has no wave");
    h.settings.reader.page_animation = "off".into();
    h.tap_at(1100, 900);
    assert_eq!((last(&h).rect, last(&h).wave), (FULL, None));
    assert_eq!(h.probe.uncovered(), vec![]);
}

/// A Wi-Fi status change from the network worker redraws the bars' text rows at once
/// (as the minute tick does), only when a bar shows the Wi-Fi widget.
#[test]
fn a_wifi_status_change_redraws_only_the_bars() {
    use ink_device::{NetEvent, NetStatus, PowerEvent};
    let connected = || InputEvent::Power(PowerEvent::Network(NetEvent::Status(NetStatus::Connected { essid: "Home".into(), signal: None })));
    let mut h = Harness::new("wifi-bars", "juliet.epub");
    let n = h.probe.updates().len();
    h.feed(connected());
    assert_eq!(h.probe.updates().len(), n, "no Wi-Fi widget: nothing drawn");
    h.settings.status_bars.top.right = vec!["battery".into(), "wifi".into()];
    h.tap_at(1100, 900);
    let page = h.reader.page();
    let n = h.probe.updates().len();
    h.feed(connected());
    let ups: Vec<_> = h.probe.updates()[n..].iter().map(|u| (u.rect, u.class)).collect();
    assert!(!ups.is_empty(), "the bars were drawn again");
    assert!(ups.iter().all(|(r, c)| r.h < 200 && *c != RefreshClass::Gc16), "{ups:?}");
    assert_eq!(h.reader.page(), page);
    // Only the signal changed: the icon is the same, nothing is drawn.
    let home = |signal| NetStatus::Connected { essid: "Home".into(), signal };
    h.reader.bars.device.as_mut().unwrap().wifi = Some(home(Some(3)));
    let n = h.probe.updates().len();
    h.feed(InputEvent::Power(PowerEvent::Network(NetEvent::Status(home(Some(5))))));
    assert_eq!(h.probe.updates().len(), n, "a signal change draws nothing");
}

/// A book is opened with the read-ahead off (its first page does not wait for the
/// pages after it) and the reader turns it on once that page is on the panel.
#[test]
fn read_ahead_is_turned_on_after_the_first_page() {
    let mut h = Harness::open("read-ahead", "juliet.epub", true);
    // What on_enter leaves after opening a book, with a probe document.
    let probe = selection::tests::three_pages();
    let ahead = probe.read_ahead.clone();
    h.reader.doc = Some(Box::new(probe));
    h.reader.plugins_pending = true;
    h.reader.rerender = true;
    h.reader.dirty = Some(RefreshClass::Gc16);
    assert_eq!(ahead.get(), None);
    h.paint();
    assert_eq!(ahead.get(), Some(true), "turned on after the first page");
    h.paint();
    assert_eq!(ahead.get(), Some(true));
}

/// A document that cannot map a saved range to rectangles (fixed layouts): the
/// highlights made this session are drawn from the rectangles kept for their page and
/// layout, found under a tap, and forgotten when removed.
#[test]
fn session_highlights_of_an_unmappable_document() {
    let dir = tmp("session-hl");
    let mut r = ReaderScreen::new(dir.join("book.pdf"));
    let mut doc = selection::tests::three_pages();
    let key = ink_doc::Document::layout(&doc).cache_key();
    let start: Position = "flow:0.1.0".parse().unwrap();
    let end: Position = "flow:0.1.5".parse().unwrap();
    let rect = Rect::new(100, 160, 100, 50);
    let mark = |layout: &str, page: usize| SessionMark { layout: layout.into(), page, rects: vec![rect], start: start.clone() };
    let saved = vec![(start.clone(), end.clone())];
    let lit = |doc: &mut selection::tests::PagedDoc, page: usize, session: &[SessionMark]| {
        let mut c = Canvas::new(400, 400);
        c.pixels.fill(255);
        draw_highlights(doc, page, &mut c, &saved, session);
        c.pixels[200 * 400 + 150] != 255
    };
    assert!(lit(&mut doc, 0, &[mark(&key, 0)]), "drawn on its page");
    assert!(!lit(&mut doc, 1, &[mark(&key, 0)]), "not on another page");
    assert!(!lit(&mut doc, 0, &[mark("other-layout", 0)]), "not in another layout");
    r.doc = Some(Box::new(doc));
    r.saved_hl = saved.clone();
    r.session_hl = vec![mark("other-layout", 0), mark(&key, 0)];
    let (s, e, rects) = r.highlight_at(Point::new(150, 185)).expect("the mark under the tap");
    assert_eq!((s, e, rects), (start.clone(), end.clone(), vec![rect]));
    assert!(r.highlight_at(Point::new(150, 400)).is_none());
    r.remove_highlight(&start, &end);
    assert!(r.session_hl.is_empty() && r.saved_hl.is_empty());
    assert!(r.highlight_at(Point::new(150, 185)).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Shows the last page of the book as a jump and returns its page number.
fn to_last_page(h: &mut Harness) -> usize {
    let last = h.reader.doc.as_ref().unwrap().page_count().pages - 1;
    h.reader.go_to_page(last);
    h.paint();
    assert_eq!(h.reader.page(), last);
    last
}

#[test]
fn turning_past_the_last_page_shows_the_end_card_with_one_flash() {
    let mut h = Harness::new("end-card", "synthetic.epub");
    // Not at the end: a turn is a turn.
    h.key(Key::PageNext);
    assert_eq!((h.reader.page(), h.kind()), (1, None));
    let last = to_last_page(&mut h);
    let page = h.probe.panel().clone();
    h.shot("01-last-page");
    let before = h.probe.updates().len();
    h.key(Key::PageNext);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::End));
    assert_eq!(h.reader.page(), last, "the page stays");
    h.shot("02-end-card");
    let ups = &h.probe.updates()[before..];
    assert_eq!(ups.len(), 1, "one update: {ups:?}");
    assert_eq!((ups[0].class, ups[0].rect), (RefreshClass::Gc16, FULL), "{ups:?}");
    // More turns forward change nothing.
    let before = h.probe.updates().len();
    h.key(Key::PageNext);
    assert_eq!(h.probe.updates().len(), before);
    // Back: the last page again, exactly, without a flash.
    h.key(Key::Back);
    assert_eq!(h.kind(), None);
    assert_eq!(h.reader.page(), last);
    assert_eq!(h.panel_rows_differ(&page, 0, page.height), 0);
    let ups = &h.probe.updates()[before..];
    assert!(!ups.is_empty() && ups.iter().all(|u| u.class != RefreshClass::Gc16), "{ups:?}");
    // A tap beside the card and the previous-page key also go back to the page.
    h.key(Key::PageNext);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::End));
    let card = h.reader.overlay().unwrap().tree.bounds(h.reader.overlay().unwrap().ids["card"]).unwrap();
    h.tap_at(636, card.y / 2);
    assert_eq!(h.kind(), None);
    h.key(Key::PageNext);
    h.key(Key::PagePrev);
    assert_eq!((h.kind(), h.reader.page()), (None, last));
    // A tap on the right of the last page opens it too.
    h.tap_at(1100, 900);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::End));
    assert!(h.transitions.is_empty(), "{:?}", h.transitions);
}

#[test]
fn the_end_card_marks_finished_and_rates_without_flashing() {
    let mut h = Harness::new("end-rate", "synthetic.epub");
    to_last_page(&mut h);
    h.key(Key::PageNext);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::End));
    let path = h.reader.path.clone();
    let before = h.probe.updates().len();
    h.tap_button("end-finished");
    assert_eq!(BookState::load(&path).unwrap().status, ink_store::ReadingStatus::Complete);
    assert!(h.reader.end.as_ref().unwrap().finished);
    h.tap_button("end-star:4");
    assert_eq!(BookState::load(&path).unwrap().rating, Some(4));
    h.shot("03-finished-rated");
    h.tap_button("end-star:2");
    assert_eq!(BookState::load(&path).unwrap().rating, Some(2));
    // Each tap refreshes the button pressed and what changed; nothing flashes.
    let ups = &h.probe.updates()[before..];
    assert!(!ups.is_empty(), "the card changed on screen");
    assert!(ups.iter().all(|u| u.class != RefreshClass::Gc16 && u.rect != FULL), "{ups:?}");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::End), "the card stays open");
    // A second tap undoes "finished"; the position saved later keeps status and rating.
    h.tap_button("end-finished");
    let s = BookState::load(&path).unwrap();
    assert_eq!((s.status, s.rating), (ink_store::ReadingStatus::Reading, Some(2)));
    h.key(Key::Back);
    h.reader.position_unsaved = Some((Instant::now(), Instant::now()));
    h.reader.flush_position();
    let s = BookState::load(&path).unwrap();
    assert_eq!((s.status, s.rating), (ink_store::ReadingStatus::Reading, Some(2)));
    assert!(s.position.is_some());
    // Opened again, the card shows what the sidecar holds.
    h.key(Key::PageNext);
    let e = h.reader.end.as_ref().unwrap();
    assert_eq!((e.finished, e.rating), (false, Some(2)));
}

#[test]
fn the_end_card_opens_the_next_book_of_the_series_or_goes_to_the_library() {
    let mut h = Harness::new("end-next", "synthetic.epub");
    let cur = h.reader.path.clone();
    let next = h.dir.join("second.epub");
    std::fs::copy(&cur, &next).unwrap();
    let entry = |p: &Path, title: &str, i: f32| ink_store::BookEntry {
        path: p.to_path_buf(),
        title: title.into(),
        series: Some("Synthetic".into()),
        series_index: Some(i),
        format: "epub".into(),
        ..Default::default()
    };
    let mut index = ink_store::LibraryIndex::default();
    index.books = vec![entry(&cur, "First", 1.0), entry(&next, "Second", 2.0)];
    std::fs::create_dir_all(&h.paths.root).unwrap();
    index.save(&h.paths.library()).unwrap();
    to_last_page(&mut h);
    h.key(Key::PageNext);
    let n = h.reader.end.as_ref().unwrap().next.clone().expect("a suggestion");
    assert_eq!((n.path.as_path(), n.title.as_str(), n.why.as_str()), (next.as_path(), "Second", "Next in Synthetic · book 2"));
    h.tap_button("end-next");
    assert_eq!(h.transitions.last(), Some(&Transition::Replace(crate::router::Route::Reader(next.clone()))));
    // Without a library index: no suggestion; "Back to library" leaves the reader.
    std::fs::remove_file(h.paths.library()).unwrap();
    h.transitions.clear();
    h.key(Key::Back);
    h.key(Key::PageNext);
    assert!(h.reader.overlay().unwrap().button_rect("end-next").is_none());
    h.tap_button("end-library");
    assert_eq!(h.transitions, vec![Transition::Pop]);
}

/// The Aa sheet of a PDF has the View tab; choosing two columns there cuts every page
/// into two screens at once (the page drawn again), keeps the page read, is saved in
/// the book's sidecar, and comes back when the book is opened again.
#[test]
fn aa_view_tab_changes_a_pdf_and_saves_it_for_the_book() {
    let mut h = Harness::generated("aa-view", "paper.pdf", &ink_mupdf::testgen::fixture_pdf());
    let pages0 = h.reader.doc.as_deref().unwrap().page_count().pages;
    assert_eq!(pages0, 10, "fit page: one screen per page");
    // Page 2 (the two-column page of the fixture).
    h.reader.go_to_page(1);
    h.paint();
    let before = h.probe.panel().clone();
    h.tap_at(636, 848);
    h.tap_button("aa");
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Aa));
    let o = h.reader.overlay().unwrap();
    assert!(o.button_rect("aa-tab:1").is_some() && o.button_rect("aa-tab:2").is_none(), "View and Page tabs only");
    // Tab 0 is View: tap it in case another test left the shared tab elsewhere.
    h.tap_button("aa-tab:0");
    h.shot("01-view");
    let o = h.reader.overlay().unwrap();
    assert!(!o.ids.contains_key("size"), "no font size for a PDF");
    let cols = o.tree.bounds(o.ids["columns"]).unwrap();
    // "Two" is the right half of the two-choice row.
    h.tap_at(cols.x + cols.w as i32 * 3 / 4, cols.y + cols.h as i32 / 2);
    assert_eq!(h.reader.actions().last().map(String::as_str), Some("aa-columns:2"));
    settle_aa(&mut h);
    let doc = h.reader.doc.as_deref().unwrap();
    assert_eq!(doc.fixed_view().map(|v| v.columns), Some(2));
    assert_eq!(doc.page_count().pages, 20, "two screens per page");
    let Position::Page(at) = doc.position_of_page(h.reader.page()).unwrap() else { panic!("a page position") };
    assert_eq!(at.page, 1, "still on the page read before");
    h.tap_button("close");
    assert_eq!(h.kind(), None);
    h.shot("02-columns");
    // The page area shows the left column now, not the whole page.
    let changed = h.panel_rows_differ(&before, 200, 1400);
    assert!(changed > 10_000, "the page was drawn again with the new view ({changed} pixels changed)");
    let st = BookState::load(&h.reader.path).unwrap();
    assert_eq!(st.settings.columns, Some(2));
    assert_eq!((st.settings.zoom, st.settings.rtl), (None, None), "only what was chosen is saved");
    // Opening the book again: its own view, over the defaults.
    let mut again = ReaderScreen::new(h.reader.path.clone());
    let p = providers();
    again.on_enter(&mut cx!(h, p)).unwrap();
    let doc = again.doc.as_deref().unwrap();
    assert_eq!(doc.fixed_view().map(|v| (v.columns, v.fit)), Some((2, ink_doc::Fit::Page)));
    assert_eq!(doc.page_count().pages, 20);
}

/// A comic's own view (right to left, two-page spreads) reaches the document on a
/// relayout, and the sheet shows it.
#[test]
fn aa_view_tab_shows_a_comic_s_own_view() {
    let mut h = Harness::generated("aa-view-cbz", "comic.cbz", &ink_mupdf::testgen::fixture_cbz(6));
    h.reader.book_settings.rtl = Some(true);
    h.reader.book_settings.spreads = Some("always".into());
    h.with_cx(|r, cx| r.relayout(cx, RefreshClass::Gc16)).unwrap();
    let doc = h.reader.doc.as_deref().unwrap();
    assert_eq!(doc.fixed_view().map(|v| (v.rtl, v.spreads)), Some((true, ink_doc::Spreads::Always)));
    assert!(doc.page_count().pages < 6, "spreads: two pages on a screen");
    h.paint();
    h.tap_at(636, 848);
    h.tap_button("aa");
    h.tap_button("aa-tab:0");
    let o = h.reader.overlay().unwrap();
    assert!(o.ids.contains_key("rtl") && o.ids.contains_key("spreads"));
    h.shot("01-view");
}

/// A comic set to right to left: a tap on the left side goes forward, the right side
/// back, and the page turn wave sweeps the other way.
#[test]
fn right_to_left_comic_turns_forward_on_the_left() {
    use ink_device::Wave;
    let mut h = Harness::generated("rtl-turns", "manga.cbz", &ink_mupdf::testgen::fixture_cbz(6));
    let last = |h: &Harness| *h.probe.updates().last().unwrap();
    h.tap_at(1100, 900);
    assert_eq!(h.reader.page(), 1, "left to right: the right side goes forward");
    h.reader.book_settings.rtl = Some(true);
    h.with_cx(|r, cx| r.relayout(cx, RefreshClass::Gc16)).unwrap();
    h.paint();
    h.tap_at(100, 900);
    assert_eq!(h.reader.page(), 2, "right to left: the left side goes forward");
    assert_eq!(last(&h).wave, Some(Wave::Right));
    h.tap_at(1100, 900);
    assert_eq!(h.reader.page(), 1, "and the right side back");
    assert_eq!(last(&h).wave, Some(Wave::Left));
    // Page keys keep their meaning: forward is forward.
    h.key(Key::PageNext);
    assert_eq!(h.reader.page(), 2);
}

impl Harness {
    /// The reader's prefetch timer has run out: backdates it and delivers the timer the
    /// reader asked for (as the app loop does once no input came).
    fn rest_for_prefetch(&mut self) {
        let at = self.reader.prefetch_at.as_mut().expect("a prefetch is due");
        *at -= PREFETCH_IDLE;
        assert!(self.reader.timeout().is_some_and(|t| t.is_zero()));
        let p = providers();
        self.handle_event(&UiEvent::Timer(0), &p);
        self.paint();
    }

    /// Asks the document to prefetch `page`: true when it was not ready yet.
    fn prefetch(&mut self, page: usize) -> bool {
        self.reader.doc.as_deref_mut().unwrap().prefetch_page(page)
    }
}

/// A comic: once the page shown has rested, the page after it in the direction of the
/// last turn is drawn into the document's cache; a turn before that moves the timer on.
#[test]
fn idle_reader_prefetches_the_page_in_the_direction_of_the_last_turn() {
    let src = tmp("prefetch-src").join("comic.cbz");
    std::fs::write(&src, ink_mupdf::testgen::fixture_cbz(12)).unwrap();
    let mut h = Harness::new("prefetch", src.to_str().unwrap());
    assert!(h.reader.timeout().is_some_and(|t| t <= PREFETCH_IDLE), "the first page set the timer");
    h.rest_for_prefetch();
    assert!(h.reader.prefetch_at.is_none());
    assert!(!h.prefetch(1), "page 2 was drawn ahead");
    // A turn back: the page before is the one drawn ahead.
    h.reader.go_to_page(10);
    h.paint();
    h.key(Key::PagePrev);
    assert_eq!(h.reader.page(), 9);
    h.rest_for_prefetch();
    assert!(!h.prefetch(8), "page 9 was drawn ahead");
    assert!(h.prefetch(7), "page 8 was not");
    // A jump keeps the direction (page 4 would come next) but a turn forward before the
    // timer ran out moves the timer on: page 4 is never drawn, page 7 is.
    h.reader.go_to_page(4);
    h.paint();
    h.key(Key::PageNext);
    assert_eq!(h.reader.page(), 5);
    assert!(h.reader.prefetch_at.is_some_and(|d| d > Instant::now()));
    assert!(h.prefetch(3), "page 4 was not drawn ahead");
    h.rest_for_prefetch();
    assert!(!h.prefetch(6), "page 7 was drawn ahead");
    // The last page has nothing after it.
    h.reader.go_to_page(11);
    h.paint();
    h.rest_for_prefetch();
    assert!(h.reader.prefetch_at.is_none());
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// The page drawn ahead waits for the event loop to go idle: input moves the timer on,
/// and nothing is drawn while a finger is down or the menu is open.
#[test]
fn prefetch_waits_for_no_input_no_finger_and_no_overlay() {
    let src = tmp("prefetch-idle-src").join("comic.cbz");
    std::fs::write(&src, ink_mupdf::testgen::fixture_cbz(12)).unwrap();
    let mut h = Harness::new("prefetch-idle", src.to_str().unwrap());
    let p = providers();
    let backdate = |h: &mut Harness| *h.reader.prefetch_at.as_mut().expect("a prefetch is pending") -= PREFETCH_IDLE * 2;
    // A finger goes down after the timer ran out: the timer moves on and waits.
    backdate(&mut h);
    h.frame(400, 636, 848, TouchPhase::Down);
    assert!(h.reader.prefetch_at.is_some_and(|d| d > Instant::now()), "input moved the timer on");
    backdate(&mut h);
    assert!(h.reader.prefetch_due().is_none(), "no prefetch while the finger is down");
    h.handle_event(&UiEvent::Timer(0), &p);
    assert!(h.reader.prefetch_at.is_some(), "nothing drawn under the finger");
    // It lifts: a tap in the middle opens the menu, and the menu holds the prefetch back.
    h.frame(60, 636, 848, TouchPhase::Up);
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Menu));
    backdate(&mut h);
    assert!(h.reader.prefetch_due().is_none(), "no prefetch under the menu");
    h.handle_event(&UiEvent::Timer(0), &p);
    assert!(h.reader.prefetch_at.is_some(), "nothing drawn under the menu");
    // The menu closes: the page after is drawn once the reader rested.
    h.key(Key::Back);
    assert_eq!(h.kind(), None);
    assert!(h.reader.prefetch_at.is_some_and(|d| d > Instant::now()));
    h.rest_for_prefetch();
    assert!(h.reader.prefetch_at.is_none());
    assert!(!h.prefetch(1), "page 2 was drawn ahead");
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

impl Harness {
    /// The sharp page after a zoom preview is due: backdates the timer and delivers it.
    fn rest_for_sharp(&mut self) {
        let at = self.reader.sharp_at.as_mut().expect("a zoom preview is shown");
        *at -= ZOOM_SETTLE;
        assert!(self.reader.timeout().is_some_and(|t| t.is_zero()));
        let p = providers();
        self.handle_event(&UiEvent::Timer(0), &p);
        self.paint();
    }
}

/// A zoom step on a comic shows the page shown before, scaled, in one GL16 update; a
/// second step before the sharp page scales it again; once no input came for
/// `ZOOM_SETTLE` the sharp page follows in one more GL16 update, the same as a page
/// drawn without a preview.
#[test]
fn zoom_shows_a_scaled_preview_then_the_sharp_page() {
    let mut h = Harness::generated("zoom-preview", "comic.cbz", &ink_mupdf::testgen::fixture_cbz(6));
    h.reader.go_to_page(2);
    h.paint();
    let page = h.probe.panel().clone();
    let n = h.probe.updates().len();
    assert!(h.reader.zoom_step(1));
    h.paint();
    let ups = &h.probe.updates()[n..];
    assert_eq!(ups.len(), 1, "{ups:?}");
    assert_eq!(ups[0].class, RefreshClass::Gl16);
    assert!(h.reader.sharp_at.is_some() && h.reader.prefetch_at.is_none());
    assert_ne!(h.probe.panel().pixels, page.pixels, "the preview is zoomed");
    // A second step: the preview scales the sharp page shown before the first.
    assert!(h.reader.zoom_step(1));
    h.paint();
    assert_eq!(h.probe.updates().len(), n + 2);
    let preview = h.probe.panel().clone();
    // Input moves the sharp page on; the timer then draws it.
    h.key(Key::Other(0));
    assert!(h.reader.sharp_at.is_some_and(|d| d > Instant::now()));
    assert_eq!(h.probe.updates().len(), n + 2, "no update before the gesture settled");
    h.rest_for_sharp();
    let ups = &h.probe.updates()[n + 2..];
    assert_eq!(ups.len(), 1, "{ups:?}");
    assert_eq!(ups[0].class, RefreshClass::Gl16);
    assert!(h.reader.sharp_at.is_none() && h.reader.prefetch_at.is_some());
    let sharp = h.probe.panel().clone();
    assert_ne!(sharp.pixels, preview.pixels, "the sharp page replaced the preview");
    let shown = h.reader.page();
    h.reader.go_to_page(0);
    h.paint();
    h.reader.go_to_page(shown);
    h.paint();
    assert_eq!(h.probe.panel().pixels, sharp.pixels);
}

/// The search hit bar leaves the page in use, so a zoom under it shows the preview and
/// then the sharp page, as without the bar.
#[test]
fn zoom_under_the_hit_bar_sharpens_too() {
    let mut h = Harness::generated("zoom-hits", "comic.cbz", &ink_mupdf::testgen::fixture_cbz(6));
    h.reader.go_to_page(2);
    h.paint();
    h.reader.overlay = Some(search::hit_bar(0, 3, h.dev.screen.size()));
    h.paint();
    assert!(h.reader.zoom_step(1));
    h.paint();
    assert!(h.reader.preview_shown.is_some(), "the zoom shows a preview");
    h.rest_for_sharp();
    assert_eq!(h.kind(), Some(overlay::OverlayKind::Hits));
    assert!(h.reader.sharp_at.is_none() && h.reader.preview_shown.is_none(), "the sharp page came");
}

/// A test annotator: annotates every "Romeo" with "R", at the version the test sets.
/// The test sets its version (`.0`) and layout version (`.1`); `annotate` takes `.2` ms.
struct RomeoAnnotator(std::sync::atomic::AtomicU64, std::sync::atomic::AtomicU64, u64);

impl RomeoAnnotator {
    fn new(version: u64) -> std::sync::Arc<Self> {
        Self::slow(version, 0)
    }
    fn slow(version: u64, ms: u64) -> std::sync::Arc<Self> {
        std::sync::Arc::new(RomeoAnnotator(version.into(), 0.into(), ms))
    }
}

impl ink_doc::TextAnnotator for RomeoAnnotator {
    fn id(&self) -> &str {
        "romeo"
    }
    fn version(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn layout_version(&self) -> u64 {
        self.1.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn wants(&self, p: &str, _lang: Option<&str>) -> bool {
        p.contains("Romeo")
    }
    fn annotate(&self, p: &str, _lang: Option<&str>) -> Vec<ink_doc::Annotation> {
        std::thread::sleep(std::time::Duration::from_millis(self.2));
        p.match_indices("Romeo").map(|(i, w)| ink_doc::Annotation::over(i..i + w.len(), "R")).collect()
    }
}

/// A plugin that offers [`RomeoAnnotator`].
struct AnnotatingPlugin(ink_plugin::PluginManifest, std::sync::Arc<RomeoAnnotator>);

impl ink_plugin::Plugin for AnnotatingPlugin {
    fn manifest(&self) -> &ink_plugin::PluginManifest {
        &self.0
    }
    fn on_load(&mut self, _reg: &mut ink_plugin::Registrar) -> ink_plugin::Result<()> {
        Ok(())
    }
    fn text_annotator(&self) -> Option<std::sync::Arc<dyn ink_doc::TextAnnotator>> {
        Some(self.1.clone())
    }
}

/// The plugin's annotator is in the layout of a book opened with it; a version bump
/// draws the page again, a layout version bump lays the book out again; "Annotations"
/// off for this book (Aa) or as the default takes it out of the layout.
#[test]
fn plugin_annotator_follows_its_version_and_the_ruby_setting() {
    let mut h = Harness::new("annotator", "juliet.epub");
    let a = RomeoAnnotator::new(1);
    h.plugins = PluginHost::new(vec![Box::new(AnnotatingPlugin(ink_plugin::PluginManifest::new("romeo", "1", &[]), a.clone()))]);
    let p = providers();
    h.reader = ReaderScreen::new(h.reader.path.clone());
    h.reader.on_enter(&mut cx!(h, p)).unwrap();
    let version = |h: &Harness| h.reader.doc.as_deref().unwrap().layout().annotators.first().map(|x| x.version());
    assert_eq!(version(&h), Some(1), "asked when the book opens");
    // Nothing changed: an action keeps the layout.
    let key = h.reader.doc.as_deref().unwrap().layout().cache_key();
    h.with_cx(|r, cx| r.run_action("plugin:romeo:x", cx)).unwrap();
    assert_eq!(h.reader.doc.as_deref().unwrap().layout().cache_key(), key);
    // The plugin changed its annotations: the page is drawn again, the pages stay.
    a.0.store(2, std::sync::atomic::Ordering::Relaxed);
    h.with_cx(|r, cx| r.run_action("plugin:romeo:x", cx)).unwrap();
    h.paint();
    assert_eq!(h.reader.annot_versions, [2]);
    assert_eq!(h.reader.doc.as_deref().unwrap().layout().cache_key(), key, "not paginated again");
    // It changed what it annotates (layout version): the book is laid out again.
    a.1.store(1, std::sync::atomic::Ordering::Relaxed);
    h.with_cx(|r, cx| r.run_action("plugin:romeo:x", cx)).unwrap();
    assert_ne!(h.reader.doc.as_deref().unwrap().layout().cache_key(), key);
    let _ = version;
    // Off for this book: neither ruby nor the annotator.
    h.with_cx(|r, cx| {
        r.aa_action("aa-annotations:off", cx).unwrap();
        r.relayout(cx, RefreshClass::Gc16).unwrap();
    });
    let l = h.reader.doc.as_deref().unwrap().layout().clone();
    assert!(!l.ruby && l.annotators.is_empty());
    assert_eq!(h.reader.book_settings.annotations, Some(false));
    // Off as the default: a book without its own choice opens without them.
    h.settings.reader.annotations = false;
    h.reader = ReaderScreen::new(h.reader.path.clone());
    h.reader.book_settings = BookSettings::default();
    let _ = std::fs::remove_file(ink_store::sidecar::sidecar_path(&h.reader.path));
    h.reader.on_enter(&mut cx!(h, p)).unwrap();
    let l = h.reader.doc.as_deref().unwrap().layout().clone();
    assert!(!l.ruby && l.annotators.is_empty());
}

/// A plugin that sets its annotations up in `on_book_open` (bumps the version) and
/// counts the calls.
struct OpenTimeAnnotator(ink_plugin::PluginManifest, std::sync::Arc<RomeoAnnotator>, std::rc::Rc<std::cell::Cell<u32>>);

impl ink_plugin::Plugin for OpenTimeAnnotator {
    fn manifest(&self) -> &ink_plugin::PluginManifest {
        &self.0
    }
    fn on_load(&mut self, _reg: &mut ink_plugin::Registrar) -> ink_plugin::Result<()> {
        Ok(())
    }
    fn on_book_open(&mut self, _cx: &mut ink_plugin::HostCtx) -> ink_plugin::Result<()> {
        self.2.set(self.2.get() + 1);
        self.1 .0.store(7, std::sync::atomic::Ordering::Relaxed);
        self.1 .1.store(7, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    fn text_annotator(&self) -> Option<std::sync::Arc<dyn ink_doc::TextAnnotator>> {
        Some(self.1.clone())
    }
}

/// With an annotator, the plugins hear `on_book_open` before the first page and the
/// book is laid out with the version they leave; the first paint does not send it again.
#[test]
fn annotator_version_set_in_on_book_open_is_used_from_the_first_page() {
    let mut h = Harness::new("annotator-open", "juliet.epub");
    let a = RomeoAnnotator::new(1);
    let opens = std::rc::Rc::new(std::cell::Cell::new(0));
    h.plugins = PluginHost::new(vec![Box::new(OpenTimeAnnotator(ink_plugin::PluginManifest::new("romeo", "1", &[]), a, opens.clone()))]);
    let p = providers();
    h.reader = ReaderScreen::new(h.reader.path.clone());
    h.reader.on_enter(&mut cx!(h, p)).unwrap();
    assert_eq!(opens.get(), 1, "told before the first page");
    let version = |h: &Harness| h.reader.doc.as_deref().unwrap().layout().annotators.first().map(|x| x.layout_version());
    assert_eq!(version(&h), Some(7), "laid out with the layout set in on_book_open");
    assert_eq!(h.reader.annot_versions, [7], "and drawn with its annotations");
    h.paint();
    assert_eq!(opens.get(), 1, "the first paint does not send it again");
    assert_eq!(version(&h), Some(7));
}

/// A plugin that changes its annotations when a dialog or card is answered.
struct AnswerAnnotator(ink_plugin::PluginManifest, std::sync::Arc<RomeoAnnotator>);

impl ink_plugin::Plugin for AnswerAnnotator {
    fn manifest(&self) -> &ink_plugin::PluginManifest {
        &self.0
    }
    fn on_load(&mut self, _reg: &mut ink_plugin::Registrar) -> ink_plugin::Result<()> {
        Ok(())
    }
    fn on_ui_result(&mut self, _cx: &mut ink_plugin::HostCtx, _id: u32, result: &str) -> ink_plugin::Result<()> {
        if result == "more" {
            self.1 .0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }
    fn text_annotator(&self) -> Option<std::sync::Arc<dyn ink_doc::TextAnnotator>> {
        Some(self.1.clone())
    }
}

/// A version bump inside `on_ui_result` (a card or dialog answer, a settings menu)
/// draws the page again by the next frame, without paginating; an answer that changes
/// nothing draws nothing.
#[test]
fn annotator_version_bumped_in_a_ui_answer_redraws_the_page() {
    let mut h = Harness::new("annotator-answer", "juliet.epub");
    let a = RomeoAnnotator::new(1);
    h.plugins = PluginHost::new(vec![Box::new(AnswerAnnotator(ink_plugin::PluginManifest::new("romeo", "1", &[]), a))]);
    let p = providers();
    h.reader = ReaderScreen::new(h.reader.path.clone());
    h.reader.on_enter(&mut cx!(h, p)).unwrap();
    h.paint();
    let page = {
        let d = h.reader.doc.as_deref_mut().unwrap();
        d.search("Romeo", 50).unwrap().into_iter().filter_map(|hit| hit.page.or_else(|| d.page_of(&hit.position).ok())).find(|&pg| pg > 5).unwrap()
    };
    h.reader.go_to_page(page);
    h.paint();
    // The page's annotations arrive (computed in the background) and it is drawn again.
    for _ in 0..200 {
        if !h.reader.annot_wait {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        h.paint();
    }
    let key = h.reader.doc.as_deref().unwrap().layout().cache_key();
    let n = h.probe.updates().len();
    h.with_cx(|r, cx| cx.plugins.answer("romeo", 1, "ok", r.document()));
    h.paint();
    assert_eq!(h.probe.updates().len(), n, "nothing changed, nothing drawn");
    let t = std::time::Instant::now();
    h.with_cx(|r, cx| cx.plugins.answer("romeo", 1, "more", r.document()));
    h.paint();
    println!("version bump to the page drawn again: {:?}", t.elapsed());
    assert_eq!(h.reader.annot_versions, [2]);
    let ups = &h.probe.updates()[n..];
    assert!(!ups.is_empty() && ups.iter().all(|u| !u.class.flashes()), "drawn again without a flash: {:?}", ups.iter().map(|u| u.class).collect::<Vec<_>>());
    assert_eq!(h.reader.doc.as_deref().unwrap().layout().cache_key(), key, "not paginated again");
}

/// Annotations that are not ready when a page is drawn: the page shows without them,
/// and once they arrive it is drawn again, once, without a flash.
#[test]
fn a_page_drawn_before_its_annotations_are_ready_is_drawn_again_once() {
    let mut h = Harness::new("annotator-slow", "juliet.epub");
    let a = RomeoAnnotator::slow(1, 150);
    h.plugins = PluginHost::new(vec![Box::new(AnnotatingPlugin(ink_plugin::PluginManifest::new("romeo", "1", &[]), a))]);
    let p = providers();
    h.reader = ReaderScreen::new(h.reader.path.clone());
    h.reader.on_enter(&mut cx!(h, p)).unwrap();
    h.paint();
    // A page with Romeo in it, not drawn yet.
    let page = {
        let d = h.reader.doc.as_deref_mut().unwrap();
        d.search("Romeo", 50).unwrap().into_iter().filter_map(|hit| hit.page.or_else(|| d.page_of(&hit.position).ok())).find(|&pg| pg > 5).unwrap()
    };
    h.reader.go_to_page(page);
    h.paint();
    assert!(h.reader.annot_wait, "drawn before the annotations were ready");
    assert!(h.reader.timeout().is_some_and(|t| t <= std::time::Duration::from_millis(100)), "the reader looks again soon");
    let n = h.probe.updates().len();
    let t = std::time::Instant::now();
    while h.reader.doc.as_deref().unwrap().annotations_pending() && t.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    println!("annotations of the page ready after {:?}", t.elapsed());
    h.paint();
    let ups: Vec<_> = h.probe.updates()[n..].iter().map(|u| u.class).collect();
    assert!(!ups.is_empty() && ups.iter().all(|c| !c.flashes()), "{ups:?}");
    assert!(!h.reader.annot_wait);
    let m = h.probe.updates().len();
    h.paint();
    assert_eq!(h.probe.updates().len(), m, "drawn again once");
}
