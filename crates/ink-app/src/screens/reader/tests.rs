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
    // The FB2 note's number (its title) is the label, not part of the text.
    let ink_doc::LinkTarget::Internal(nt) = &l.target else { panic!() };
    let mut note = h.reader.doc.as_deref_mut().unwrap().note(nt).unwrap().unwrap();
    assert!(note.text.starts_with("The first footnote"), "{:?}", note.text);
    assert_eq!(note_popup_title(&mut note, &l.text), "Footnote 1");
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
    h.tap_at(636, 1600);
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
fn note_popup_title_and_spans() {
    use ink_doc::{Note, NoteKind, NoteSpan};
    let pos = Position::Page(Default::default());
    let span = |t: &str, italic| NoteSpan { text: t.into(), bold: false, italic };
    // The note has its own number: the title shows it, the text stays.
    let mut n = Note { kind: NoteKind::Footnote, position: pos.clone(), text: "Text.".into(), label: Some("12".into()), spans: vec![span("Text.", false)] };
    assert_eq!(note_popup_title(&mut n, "[12]"), "Footnote 12");
    // A bare number repeated in the text: removed when it equals the reference.
    let mut n = Note { kind: NoteKind::Endnote, position: pos.clone(), text: "3 See ".into(), label: None, spans: vec![span("3 See ", false), span("Hamlet", true)] };
    assert_eq!(note_popup_title(&mut n, "3"), "Note 3");
    assert_eq!(n.text, "See Hamlet");
    assert_eq!(note_spans(&n, 700), [("See ".to_string(), Style::Regular), ("Hamlet".to_string(), Style::Italic)]);
    // A number that differs from the reference stays; the title takes the reference's.
    let mut n = Note::plain(NoteKind::Footnote, pos.clone(), "1984 was a year.");
    assert_eq!(note_popup_title(&mut n, "[4]"), "Footnote 4");
    assert_eq!(n.text, "1984 was a year.");
    // A reference that is no number (a word link): just "Footnote".
    let mut n = Note::plain(NoteKind::Footnote, pos, "Text.");
    assert_eq!(note_popup_title(&mut n, "see the appendix"), "Footnote");
    // Long notes are cut at a word.
    n.spans = vec![span("one two ", false), span("three four", true)];
    assert_eq!(note_spans(&n, 14), [("one two ".to_string(), Style::Regular), ("three ...".to_string(), Style::Italic)]);
    assert_eq!(note_spans(&n, 9), [("one two ".to_string(), Style::Regular), (" ...".to_string(), Style::Italic)]);
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
