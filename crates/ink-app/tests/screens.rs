//! Every registered screen runs headless with scripted input: it paints, handles taps and
//! keys, and returns. Coordinates are the phase 0 layouts of each screen.

use ink_app::app::App;
use ink_app::router::Route;
use ink_device::headless::{self, HeadlessOptions, HeadlessProbe};
use ink_device::script::parse;
use ink_device::RefreshClass;
use ink_store::{Paths, Settings};
use std::path::{Path, PathBuf};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-screens-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(route: Route, script: &str, settings: Settings, out: Option<PathBuf>) -> HeadlessProbe {
    let (device, probe) = headless::open(HeadlessOptions { script: parse(script).unwrap(), out_dir: out, ..Default::default() });
    let paths = Paths { root: tmp("home") };
    App::new(device, settings, paths).run(route).expect("screen run");
    probe
}

fn dark_pixels(p: &HeadlessProbe) -> usize {
    p.panel().pixels.iter().filter(|v| **v < 64).count()
}

#[test]
fn demo_gallery_dialog_keyboard_and_exit() {
    let out = std::env::var_os("INK_SCREENS_OUT").map(PathBuf::from);
    let script = "shot demo-start\n\
                  tap 200 320\n\
                  shot demo-dialog\n\
                  tap 600 1040\n\
                  tap 60 1264\n\
                  tap 190 1264\n\
                  shot demo-typed\n\
                  key power\n";
    let p = run(Route::Demo, script, Settings::default(), out);
    let updates = p.updates();
    assert!(updates.len() >= 4, "{updates:?}");
    // Typing refreshes only parts of the screen, in GL16 (keeps the gray keys gray):
    // the updates after the keyboard opened are partial, none is full screen.
    let after_open = updates.iter().rposition(|u| u.class == RefreshClass::Gc16).unwrap_or(0);
    let typed = &updates[after_open + 1..];
    assert!(!typed.is_empty() && typed.iter().all(|u| u.class == RefreshClass::Gl16 && u.rect.h < 400), "typing refreshes only parts: {updates:?}");
    assert!(dark_pixels(&p) > 10_000);
}

#[test]
fn settings_back_button_pops() {
    let p = run(Route::Settings, "shot s\ntap 200 1530\nwait 100\nkey power\n", Settings::default(), None);
    assert!(!p.updates().is_empty());
    assert!(dark_pixels(&p) > 1_000);
}

#[test]
fn library_lists_books_and_opens_one() {
    let dir = tmp("lib");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/synthetic.epub");
    std::fs::copy(&src, dir.join("synthetic.epub")).unwrap();
    let settings = Settings { library_dirs: vec![dir.clone()], ..Settings::default() };
    // Tap the first book button, turn a page in the reader, power key exits.
    let p = run(Route::Library, "tap 500 270\ntap 1000 900\nkey power\n", settings, None);
    let updates = p.updates();
    // Library paint, "Opening", first page (GC16), page turn (REAGL).
    assert!(updates.iter().filter(|u| u.class == RefreshClass::Gc16).count() >= 2, "{updates:?}");
    assert_eq!(updates.last().unwrap().class, RefreshClass::Reagl);
    // The reader saved its position next to the copied book.
    let state = ink_store::BookState::load(&dir.join("synthetic.epub")).unwrap();
    let pos = state.position.expect("position saved");
    assert!(pos.starts_with("/body/DocFragment") && pos != "/body/DocFragment[1].0", "page 2 saved, got {pos}");
    std::fs::remove_dir_all(dir).unwrap();
}

/// Rows of the panel that hold dark pixels (first, last), or None for a white panel.
fn dark_row_span(p: &HeadlessProbe) -> Option<(usize, usize)> {
    let panel = p.panel();
    let w = panel.width;
    let rows: Vec<usize> = (0..panel.height).filter(|y| panel.pixels[y * w..(y + 1) * w].iter().any(|v| *v < 100)).collect();
    Some((*rows.first()?, *rows.last()?))
}

/// A library holding only the DRM test book (title sorting cannot move it).
fn drm_library(name: &str) -> (PathBuf, Settings) {
    let dir = tmp(name);
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/formats/drm-adept.epub");
    std::fs::copy(&src, dir.join("drm-adept.epub")).unwrap();
    let settings = Settings { library_dirs: vec![dir.clone()], ..Settings::default() };
    (dir, settings)
}

#[test]
fn library_drm_book_shows_an_error_and_a_tap_returns_to_the_library() {
    let out = std::env::var_os("INK_SCREENS_OUT").map(PathBuf::from);
    // Tap the DRM book: the reader shows its error and the app keeps running.
    let (dir, settings) = drm_library("drm1");
    let p = run(Route::Library, "tap 500 270\nwait 200\nshot drm-error\n", settings, out.clone());
    let (top, bottom) = dark_row_span(&p).expect("error text drawn");
    let h = p.panel().height;
    // "Opening <path>" is 2 lines from h/3; the error screen has title, name, error
    // text and "Tap to go back" (at least 5 lines of 70 px).
    assert!(top >= h / 3 - 60 && bottom > h / 3 + 4 * 70, "dark rows {top}..{bottom}, h {h}");
    std::fs::remove_dir_all(dir).unwrap();

    // A tap on the error goes back to the library ("Library" title at y 60..180).
    let (dir, settings) = drm_library("drm2");
    let p = run(Route::Library, "tap 500 270\nwait 200\ntap 600 900\nwait 200\nshot drm-back\nkey power\n", settings, out);
    let (top, _) = dark_row_span(&p).expect("library drawn");
    assert!(top < 200, "library title not on the panel after the tap (first dark row {top})");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reader_started_on_a_missing_or_refused_book_shows_the_error_and_returns_ok() {
    let dir = tmp("missing");
    for book in [dir.join("nothere.epub"), Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/formats/kfx-drmion.kfx")] {
        let p = run(Route::Reader(book.clone()), "wait 100\ntap 600 900\n", Settings::default(), None);
        let (_, bottom) = dark_row_span(&p).expect("error text drawn");
        assert!(bottom > p.panel().height / 3 + 4 * 70, "{}: only {bottom} rows", book.display());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Rotating from the control center while the reader is below it: the reader must lay
/// the page out again for the landscape canvas and not paint its portrait page buffer
/// (same pixel count, other row length) into it. The panel must match a book opened in
/// landscape from the start.
#[test]
fn reader_after_control_center_rotate_matches_landscape_open() {
    let book = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/juliet.epub");
    // Swipe down from the top: control center; Rotate tile; swipe up (panel coordinates
    // after the turn) closes it; a short wait lets the reader draw.
    let script = "swipe 600 10 600 800\nwait 300\ntap 1100 852\nwait 300\nswipe 200 1500 1100 1500\nwait 500\nkey power\n";
    let turned = run(Route::Reader(book.clone()), script, Settings::default(), None).panel();
    let mut landscape = Settings::default();
    landscape.device.rotation = 1;
    let direct = run(Route::Reader(book), "wait 500\nkey power\n", landscape, None).panel();
    assert_eq!((turned.width, turned.height), (1272, 1696));
    // Not four copies of one strip: the panel rows (landscape columns) differ per quarter.
    let q = turned.pixels.len() / 4;
    assert_ne!(turned.pixels[..q], turned.pixels[q..2 * q], "page repeated in the panel");
    let differing = turned.pixels.iter().zip(&direct.pixels).filter(|(a, b)| a != b).count();
    assert!(differing * 100 < turned.pixels.len(), "{differing} of {} panel pixels differ from a landscape open", turned.pixels.len());
}

/// A library with one book and the probe after `script`.
fn held_library(name: &str, script: &str) -> (HeadlessProbe, PathBuf) {
    let dir = tmp(name);
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/synthetic.epub");
    std::fs::copy(&src, dir.join("synthetic.epub")).unwrap();
    let settings = Settings { library_dirs: vec![dir.clone()], ..Settings::default() };
    let out = std::env::var_os("INK_SCREENS_OUT").map(|d| PathBuf::from(d).join(name));
    (run(Route::Library, script, settings, out), dir)
}

/// Real-time scripts: the finger goes down at (`x`, `y`) after `before`, the hold time
/// passes with the finger still down, and the run ends there (no lift). A second run
/// lifts the finger and waits. Returns the panels: without the hold, held, lifted.
fn hold_panels(name: &str, before: &str, x: i32, y: i32) -> [ink_device::Canvas; 3] {
    let none = held_library(&format!("{name}-none"), &format!("{before}sleep 300\n"));
    let held = held_library(&format!("{name}-held"), &format!("{before}down {x} {y}\nsleep 900\nshot held\n"));
    let lifted = held_library(&format!("{name}-lifted"), &format!("{before}down {x} {y}\nsleep 900\nshot held\nup {x} {y}\nsleep 400\nshot lifted\n"));
    let panels = [none.0.panel(), held.0.panel(), lifted.0.panel()];
    for d in [none.1, held.1, lifted.1] {
        let _ = std::fs::remove_dir_all(d);
    }
    panels
}

/// The menu is on the panel while the finger is still down, and the lift changes
/// nothing (it opens no item and closes nothing).
fn assert_menu_before_the_lift(name: &str, before: &str, x: i32, y: i32) {
    let [none, held, lifted] = hold_panels(name, before, x, y);
    let differing = none.pixels.iter().zip(&held.pixels).filter(|(a, b)| a != b).count();
    assert!(differing > 20_000, "{name}: the menu is not on the panel before the lift ({differing} pixels differ)");
    assert!(held.pixels == lifted.pixels, "{name}: the lift changed the panel");
}

#[test]
fn holding_a_book_on_home_opens_its_menu_before_the_lift() {
    assert_menu_before_the_lift("hold-home", "sleep 1500\n", 500, 270);
}

#[test]
fn holding_a_book_in_the_library_opens_its_menu_before_the_lift() {
    assert_menu_before_the_lift("hold-library", "sleep 1500\ntap 477 1610\nsleep 1000\n", 500, 340);
}

/// The app loop over the library: the 3-dots menu opens and closes, and the control
/// center opens and closes, each without a flash (no GC16 after the first frame) and
/// without a full-screen update for the menu.
#[test]
fn library_popups_and_the_control_center_never_flash() {
    let dir = tmp("lib-popups");
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/synthetic.epub");
    std::fs::copy(&src, dir.join("synthetic.epub")).unwrap();
    let settings = Settings { library_dirs: vec![dir.clone()], ..Settings::default() };
    // The 3-dots button is the app bar's last action (centre 1182, 68); Back closes the
    // menu; a swipe down from the top opens the control center, a tap below it closes it.
    let script = "wait 100\ntap 1182 68\nwait 50\nkey back\nwait 50\ntap 1182 68\nwait 50\ntap 300 1400\nwait 50\n\
                  swipe 600 20 600 700\nwait 100\ntap 600 1500\nwait 100\nkey power\n";
    let p = run(Route::Library, script, settings, None);
    let ups = p.updates();
    let full = ink_device::Rect::new(0, 0, 1272, 1696);
    let first = ups.iter().position(|u| u.class == RefreshClass::Gc16).expect("the library comes up with a flash");
    let after = &ups[first + 1..];
    assert!(after.len() >= 4, "{ups:?}");
    assert!(after.iter().all(|u| u.class != RefreshClass::Gc16), "no flash after the first frame: {after:?}");
    // The menu frames (the first four after the library came up) are partial.
    assert!(after[..4].iter().all(|u| u.rect != full && u.rect.h < 1000), "menu updates are the menu only: {after:?}");
}

/// The reader's menu opens and closes without a flash: after the first page (GC16) no
/// update is a GC16 and none covers the whole screen.
#[test]
fn reader_menu_opens_and_closes_without_a_flash() {
    let book = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/synthetic.epub");
    let script = "wait 100\ntap 636 848\nwait 100\nkey back\nwait 100\ntap 636 848\nwait 100\ntap 636 848\nwait 100\nkey power\n";
    let p = run(Route::Reader(book), script, Settings::default(), None);
    let ups = p.updates();
    let first = ups.iter().position(|u| u.class == RefreshClass::Gc16).expect("the first page flashes");
    let after = &ups[first + 1..];
    assert!(after.len() >= 3, "{ups:?}");
    let full = ink_device::Rect::new(0, 0, 1272, 1696);
    assert!(after.iter().all(|u| u.class != RefreshClass::Gc16 && u.rect != full), "menu updates are partial, no flash: {after:?}");
}
