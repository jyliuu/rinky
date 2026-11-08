//! Reader gesture map: a recognised [`Gesture`] becomes a trigger name from
//! [`ink_store::prefs::GESTURES`], the user's map (`settings.gestures`) gives the action
//! name from [`ink_store::prefs::ACTIONS`], and the action runs on the reader.
//!
//! Classification ([`triggers`]) is pure and depends only on the gesture and the screen
//! size:
//! - taps: corner squares of 1/8 of the screen width first (`tap_top_left`, ...), then
//!   the bottom middle (middle third across, bottom fifth down: `tap_bottom_center`),
//!   then the zones: left third `tap_left`; middle third across and middle three fifths down, or
//!   the top tenth, `tap_center`; the rest `tap_right`;
//! - swipes that start within 1/20 of the screen of an edge and move away from it are
//!   edge swipes (`swipe_up_from_bottom`, `swipe_down_from_top`, `swipe_right_from_left`,
//!   `swipe_left_from_right`); other swipes are `swipe_<direction>`;
//! - two-finger swipes, two-finger tap, pinch in / out, double tap, long press.
//!
//! A corner or bottom-middle tap or an edge swipe whose action is `none` falls back to
//! the plain trigger (the zone tap or the plain swipe), so an unused corner still turns
//! the page. Edge slides (`slide_left_edge`, `slide_right_edge`) follow the finger and
//! are not gestures here: see [`super::light_gestures`]. A
//! gesture whose action is `none` after that goes to plugin gesture bindings.
//!
//! There is no hidden exit anywhere on the page: `exit` is an action like any other and
//! asks first ("Exit to Kindle?") when `settings.ui.confirm_exit` is on.

use super::overlay::{Overlay, OverlayKind};
use super::{light_gestures, ReaderScreen};
use crate::router::{AppCtx, Transition};
use crate::screens::settings::quick_light;
use ink_device::{Point, Size};
use ink_store::prefs::{GestureMap, LONG_PRESS_ACTIONS};
use ink_store::Settings;
use ink_ui::{Direction, DoubleTapMode, Gesture};

/// Action of the exit dialog's "Exit" button (handled in `ReaderScreen::action`).
pub const EXIT_CONFIRMED: &str = "exit-confirmed";

/// Light and warmth gestures change the level by this fraction of the range.
const LIGHT_STEP_PERCENT: u32 = 10;

/// Trigger names for `g` on a `size` screen: the specific one and the plain one it falls
/// back to when the specific one maps to `none` (corner tap -> zone tap, edge swipe ->
/// plain swipe). `None` for gestures the map does not cover (corner holds).
pub fn triggers(g: &Gesture, size: Size) -> Option<(&'static str, Option<&'static str>)> {
    let (w, h) = (size.w as i32, size.h as i32);
    Some(match *g {
        Gesture::Tap { pos } => {
            let zone = tap_zone(pos, w, h);
            match tap_corner(pos, w, h).or_else(|| tap_bottom_center(pos, w, h)) {
                Some(c) => (c, Some(zone)),
                None => (zone, None),
            }
        }
        Gesture::DoubleTap { .. } => ("double_tap", None),
        Gesture::LongPress { .. } => ("long_press", None),
        Gesture::Swipe { from, dir, .. } => {
            let plain = swipe_name(dir);
            match edge_swipe(from, dir, w, h) {
                Some(e) => (e, Some(plain)),
                None => (plain, None),
            }
        }
        Gesture::TwoFingerSwipe { dir, .. } => (
            match dir {
                Direction::Up => "two_finger_swipe_up",
                Direction::Down => "two_finger_swipe_down",
                Direction::Left => "two_finger_swipe_left",
                Direction::Right => "two_finger_swipe_right",
            },
            None,
        ),
        Gesture::TwoFingerTap { .. } => ("two_finger_tap", None),
        Gesture::Pinch { scale_milli, .. } => (if scale_milli > 1000 { "pinch_out" } else { "pinch_in" }, None),
        Gesture::CornerHold { .. } => return None,
    })
}

/// The trigger and action `map` gives `g`: the specific trigger's action, or the plain
/// trigger's when the specific one is `none`. The action is `none` when neither is set.
pub fn resolve<'m>(g: &Gesture, size: Size, map: &'m GestureMap) -> Option<(&'static str, &'m str)> {
    let (first, plain) = triggers(g, size)?;
    let a = map.action(first);
    if a != "none" {
        return Some((first, a));
    }
    Some(match plain {
        Some(p) => (p, map.action(p)),
        None => (first, a),
    })
}

fn tap_corner(p: Point, w: i32, h: i32) -> Option<&'static str> {
    let k = w / 8;
    let (left, right, top, bottom) = (p.x < k, p.x >= w - k, p.y < k, p.y >= h - k);
    match (left, right, top, bottom) {
        (true, _, true, _) => Some("tap_top_left"),
        (_, true, true, _) => Some("tap_top_right"),
        (true, _, _, true) => Some("tap_bottom_left"),
        (_, true, _, true) => Some("tap_bottom_right"),
        _ => None,
    }
}

fn tap_bottom_center(p: Point, w: i32, h: i32) -> Option<&'static str> {
    (p.x >= w / 3 && p.x < w * 2 / 3 && p.y >= h * 4 / 5).then_some("tap_bottom_center")
}

fn tap_zone(p: Point, w: i32, h: i32) -> &'static str {
    if p.y < h / 10 || (p.x >= w / 3 && p.x < w * 2 / 3 && p.y >= h / 5 && p.y < h * 4 / 5) {
        "tap_center"
    } else if p.x < w / 3 {
        "tap_left"
    } else {
        "tap_right"
    }
}

fn swipe_name(dir: Direction) -> &'static str {
    match dir {
        Direction::Up => "swipe_up",
        Direction::Down => "swipe_down",
        Direction::Left => "swipe_left",
        Direction::Right => "swipe_right",
    }
}

fn edge_swipe(from: Point, dir: Direction, w: i32, h: i32) -> Option<&'static str> {
    let (ex, ey) = (w / 20, h / 20);
    match dir {
        Direction::Up if from.y >= h - ey => Some("swipe_up_from_bottom"),
        Direction::Down if from.y < ey => Some("swipe_down_from_top"),
        Direction::Right if from.x < ex => Some("swipe_right_from_left"),
        Direction::Left if from.x >= w - ex => Some("swipe_left_from_right"),
        _ => None,
    }
}

/// The two page-turn actions.
const TURNS: [&str; 2] = ["next_page", "prev_page"];

/// `g` seen in a mirror: left and right swapped on a `size` screen (taps, swipes and
/// two-finger swipes; other gestures stay).
fn mirrored(g: &Gesture, size: Size) -> Gesture {
    let w = size.w as i32;
    let m = |p: Point| Point::new(w - 1 - p.x, p.y);
    let flip = |d: Direction| match d {
        Direction::Left => Direction::Right,
        Direction::Right => Direction::Left,
        d => d,
    };
    match *g {
        Gesture::Tap { pos } => Gesture::Tap { pos: m(pos) },
        Gesture::Swipe { from, to, dir } => Gesture::Swipe { from: m(from), to: m(to), dir: flip(dir) },
        Gesture::TwoFingerSwipe { from, to, dir } => Gesture::TwoFingerSwipe { from: m(from), to: m(to), dir: flip(dir) },
        other => other,
    }
}

/// The page turn `g` makes in a right-to-left book, when `action` (its mapped action)
/// is a page turn: the turn mapped where the gesture lands on a mirrored screen, so the
/// wide tap zone on the left and a swipe to the right go forward; the other turn when
/// the mirrored side has none. `None` for any other action, which stays as it is.
pub fn rtl_turn(g: &Gesture, size: Size, map: &GestureMap, action: &str) -> Option<&'static str> {
    if !TURNS.contains(&action) {
        return None;
    }
    let mirror = resolve(&mirrored(g, size), size, map).map(|(_, a)| a);
    let other = if action == TURNS[0] { TURNS[1] } else { TURNS[0] };
    Some(TURNS.into_iter().find(|t| mirror == Some(*t)).unwrap_or(other))
}

/// The turn of a long press on blank space at `x` on a screen `w` wide: back in the
/// left third, forward elsewhere; mirrored when the book reads right to left (back in
/// the right third), like the tap zones.
fn long_press_turn(x: i32, w: u32, rtl: bool) -> i64 {
    let x = if rtl { w as i32 - 1 - x } else { x };
    if x < w as i32 / 3 {
        -1
    } else {
        1
    }
}

/// True when a long press on a word selects it (`long_press` is `dictionary` or
/// `select`); otherwise a hold runs the mapped action and starts no selection.
pub fn long_press_selects(settings: &Settings) -> bool {
    let a = settings.gestures.action("long_press");
    LONG_PRESS_ACTIONS.iter().any(|(n, _)| *n == a)
}

/// Double-tap mode for the reader: delayed (single taps wait at most 250 ms) only when
/// `double_tap` has an action and no overlay is open; otherwise taps come at once.
pub fn double_tap_mode(overlay_open: bool, settings: &Settings) -> DoubleTapMode {
    if !overlay_open && settings.gestures.action("double_tap") != "none" {
        DoubleTapMode::Delayed
    } else {
        DoubleTapMode::After
    }
}

/// Every action name [`ReaderScreen::run_action`] knows (besides `plugin:<p>:<item>`).
pub const KNOWN: &[&str] = &[
    "none",
    "next_page",
    "prev_page",
    "menu",
    "reading_settings",
    "toc",
    "goto",
    "bookmark",
    "bookmarks",
    "search",
    "back",
    "control_center",
    "light_up",
    "light_down",
    "warmth_up",
    "warmth_down",
    "light_toggle",
    "warmth_toggle",
    "light_slide",
    "warmth_slide",
    "night_mode",
    "font_bigger",
    "font_smaller",
    "zoom_in",
    "zoom_out",
    "status_bars",
    "full_refresh",
    "rotate",
    "library",
    "home",
    "sleep",
    "exit",
    "dictionary",
    "select",
];

impl ReaderScreen {
    /// Runs the mapped action of `g`. `Ok(None)` when the map has no action for it (the
    /// caller offers it to plugins).
    pub(super) fn gesture_action(&mut self, g: &Gesture, cx: &mut AppCtx) -> anyhow::Result<Option<Transition>> {
        let size = cx.screen.size();
        let Some((trigger, action)) = resolve(g, size, &cx.settings.gestures) else { return Ok(None) };
        let p = g.pos();
        log::info!("reader gesture {trigger} at {},{} -> {action}", p.x, p.y);
        let mut action = action.to_string();
        // `back` with no link to go back from: the plain trigger's action (a swipe in from
        // the left edge then turns back a page like any right swipe).
        if action == "back" && self.link_return_page().is_none() {
            if let Some((_, Some(plain))) = triggers(g, size) {
                action = cx.settings.gestures.action(plain).to_string();
                log::info!("reader gesture {trigger}: nothing to go back to, {plain} -> {action}");
            }
        }
        if action == "none" {
            return Ok(None);
        }
        // Right to left (manga): page turns go the other way round.
        let rtl = self.reads_right_to_left();
        if let Some(turn) = rtl.then(|| rtl_turn(g, size, &cx.settings.gestures, &action)).flatten() {
            log::info!("reader gesture {trigger}: right to left, {action} -> {turn}");
            action = turn.to_string();
        }
        // A long press that selected nothing (blank space): it turns the page by side,
        // as before the gesture map (the left side forward when right to left).
        if trigger == "long_press" && LONG_PRESS_ACTIONS.iter().any(|(n, _)| *n == action) {
            self.turn(long_press_turn(p.x, size.w, rtl), cx);
            return Ok(Some(Transition::None));
        }
        self.run_action(&action, cx).map(Some)
    }

    /// Runs one action from [`ink_store::prefs::ACTIONS`] (or `plugin:<p>:<item>`).
    pub(super) fn run_action(&mut self, action: &str, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        let fixed = self.doc.as_deref().is_some_and(|d| d.kind() == ink_doc::DocKind::Fixed);
        match action {
            "none" | "dictionary" | "select" => {}
            "next_page" => self.next_page(cx),
            "prev_page" => self.prev_page(cx),
            "menu" => self.open_menu(cx),
            "reading_settings" => self.open_aa(cx),
            "toc" => self.open_toc(cx)?,
            "goto" => self.open_goto(cx)?,
            "bookmark" => {
                let on = self.toggle_bookmark();
                log::info!("reader: bookmark {}", if on { "added" } else { "removed" });
            }
            "bookmarks" => self.open_bookmarks(cx)?,
            "search" => self.open_search(cx),
            "back" => {
                if !self.go_back_from_link() {
                    log::info!("reader: back: no link to go back from");
                }
            }
            "control_center" => {
                log::info!("reader: control center");
                return Ok(quick_light::open_control_center(cx));
            }
            "light_up" | "light_down" | "warmth_up" | "warmth_down" => light_step(action, cx),
            "light_toggle" | "warmth_toggle" => {
                let now = cx.services.clock.local_now();
                light_gestures::toggle(action == "warmth_toggle", &mut *cx.services.light, &now);
            }
            // Edge slides follow the finger (light_gestures); as a tap or swipe: nothing.
            "light_slide" | "warmth_slide" => {}
            "night_mode" => self.toggle_night(cx)?,
            "font_bigger" | "zoom_in" if fixed => {
                self.zoom_step(1);
            }
            "font_smaller" | "zoom_out" if fixed => {
                self.zoom_step(-1);
            }
            "font_bigger" => self.font_step(1, cx)?,
            "font_smaller" => self.font_step(-1, cx)?,
            "zoom_in" | "zoom_out" => log::info!("reader: {action} is for fixed-layout books (PDF, CBZ)"),
            "status_bars" => self.toggle_status_bars(),
            "full_refresh" => self.full_refresh(),
            "rotate" => {
                let mut s = cx.settings_update.take().unwrap_or_else(|| cx.settings.clone());
                s.device.rotation = (s.device.rotation + 1) % 4;
                log::info!("reader: rotate to {}", s.device.rotation);
                *cx.settings_update = Some(s);
            }
            // The library opened the book (the launcher starts the library): back to it.
            "library" | "home" => {
                log::info!("reader: {action}: back to the library");
                return Ok(Transition::Pop);
            }
            "sleep" => {
                log::info!("reader: sleep now");
                if let Err(e) = cx.services.power.sleep_now() {
                    log::warn!("reader: sleep: {e}");
                }
            }
            "exit" if cx.settings.ui.confirm_exit => self.confirm_exit(cx),
            "exit" => {
                log::info!("reader: exit to Kindle");
                return Ok(Transition::Exit);
            }
            _ if action.starts_with("plugin:") => {
                let mut parts = action.splitn(3, ':').skip(1);
                let (Some(plugin), Some(item)) = (parts.next(), parts.next()) else {
                    log::warn!("reader: bad plugin action {action:?}");
                    return Ok(Transition::None);
                };
                log::info!("reader: plugin {plugin} item {item}");
                let doc = self.doc.as_deref_mut().map(|d| d as &mut dyn ink_doc::Document);
                cx.plugins.trigger(plugin, item, doc);
            }
            _ => log::warn!("reader: unknown gesture action {action:?}"),
        }
        Ok(Transition::None)
    }

    /// Opens the "Exit to Kindle?" dialog (Cancel closes it, Exit leaves the app).
    fn confirm_exit(&mut self, cx: &mut AppCtx) {
        let size = cx.screen.size();
        let mut o = Overlay::over_page(OverlayKind::Popup, size, "close");
        let dialog = ink_ui::Dialog {
            title: format!("Exit to {}?", ink_platform::current().device_name()),
            body: format!("rinky closes and the {} home screen starts.", ink_platform::current().device_name()),
            buttons: vec![("Cancel".into(), "close".into()), ("Exit".into(), EXIT_CONFIRMED.into())],
        };
        let frame = ink_ui::Dialog::frame(size);
        o.buttons.push((EXIT_CONFIRMED.into(), frame));
        o.tree.add(None, Box::new(dialog), frame);
        log::info!("reader: exit asks first");
        self.open_overlay(o, cx);
    }
}

/// Brighter, dimmer, warmer or cooler by [`LIGHT_STEP_PERCENT`] of the range, through
/// the control center's light code (auto warmth keeps the user's warmth).
fn light_step(action: &str, cx: &mut AppCtx) {
    let warmth = action.starts_with("warmth");
    let Some(((lo, hi), cur)) = light_gestures::level(&*cx.services.light, warmth, action) else { return };
    let step = ((hi.saturating_sub(lo)) * LIGHT_STEP_PERCENT / 100).max(1);
    let v = if action.ends_with("_up") { cur.saturating_add(step).min(hi) } else { cur.saturating_sub(step).max(lo) };
    let now = cx.services.clock.local_now();
    if let Some(text) = light_gestures::set_level(&mut *cx.services.light, warmth, v, &now) {
        log::info!("reader: {action}: {cur} -> {text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_store::prefs::{ACTIONS, DEFAULT_GESTURES, SLIDE_ACTIONS};

    const SIZE: Size = Size::new(1272, 1696);

    fn tap(x: i32, y: i32) -> Gesture {
        Gesture::Tap { pos: Point::new(x, y) }
    }

    fn swipe(x: i32, y: i32, dir: Direction) -> Gesture {
        Gesture::Swipe { from: Point::new(x, y), to: Point::new(x, y), dir }
    }

    /// Right to left: the left side and a swipe to the right turn forward, the bottom
    /// middle (no side) still turns forward, and other actions are left alone.
    #[test]
    fn right_to_left_turns_mirror_the_page_turn_sides() {
        let map = GestureMap::default();
        let rtl = |g: Gesture| {
            let (_, a) = resolve(&g, SIZE, &map).unwrap();
            rtl_turn(&g, SIZE, &map, a).unwrap_or(a)
        };
        assert_eq!(rtl(tap(100, 900)), "next_page");
        assert_eq!(rtl(tap(1000, 900)), "prev_page");
        // Low in the middle third, inside the mirrored wide zone (the left two thirds):
        // forward too. The zones are mirrored, not only the actions swapped.
        assert_eq!(rtl(tap(600, 1500)), "next_page");
        assert_eq!(rtl(tap(636, 1600)), "next_page");
        assert_eq!(rtl(swipe(600, 800, Direction::Right)), "next_page");
        assert_eq!(rtl(swipe(600, 800, Direction::Left)), "prev_page");
        // Swipe left from the right edge: its mirror is the "back" edge, so the plain
        // turn is swapped instead.
        assert_eq!(rtl(swipe(1260, 800, Direction::Left)), "prev_page");
        assert_eq!(rtl(tap(636, 848)), "menu");
        assert_eq!(rtl(tap(50, 50)), "bookmark");
        assert_eq!(rtl(tap(40, 1680)), "light_toggle");
        // A long press on blank space turns by side, mirrored the same way: only the
        // right third goes back, the middle goes forward.
        let w = SIZE.w;
        assert_eq!([100, 636, 1100].map(|x| long_press_turn(x, w, false)), [-1, 1, 1]);
        assert_eq!([100, 636, 1100].map(|x| long_press_turn(x, w, true)), [1, 1, -1]);
    }

    #[test]
    fn defaults_resolve_with_fallback_to_plain_triggers() {
        let map = GestureMap::default();
        let r = |g: Gesture| resolve(&g, SIZE, &map).unwrap();
        assert_eq!(r(tap(100, 900)), ("tap_left", "prev_page"));
        assert_eq!(r(tap(1000, 900)), ("tap_right", "next_page"));
        assert_eq!(r(tap(636, 848)), ("tap_center", "menu"));
        // The top band opens the menu across the whole width.
        assert_eq!(r(tap(400, 60)), ("tap_center", "menu"));
        assert_eq!(r(tap(50, 50)), ("tap_top_left", "bookmark"));
        // Unused corners fall back to their zone: no hidden exit top right.
        assert_eq!(r(tap(1250, 40)), ("tap_center", "menu"));
        // Bottom corners: light and warmth off / on (KOReader).
        assert_eq!(r(tap(40, 1680)), ("tap_bottom_left", "light_toggle"));
        assert_eq!(r(tap(1250, 1680)), ("tap_bottom_right", "warmth_toggle"));
        // Bottom middle: unused by default, so it turns the page as before.
        assert_eq!(r(tap(636, 1600)), ("tap_right", "next_page"));
        // Edge swipes, and an unused edge falls back to the plain swipe.
        assert_eq!(r(swipe(600, 1690, Direction::Up)), ("swipe_up_from_bottom", "reading_settings"));
        assert_eq!(r(swipe(600, 10, Direction::Down)), ("swipe_down_from_top", "control_center"));
        assert_eq!(r(swipe(1260, 800, Direction::Left)), ("swipe_left", "next_page"));
        assert_eq!(r(swipe(600, 800, Direction::Up)), ("swipe_up", "none"));
        let pinch = Gesture::Pinch { center: Point::new(600, 800), scale_milli: 600 };
        assert_eq!(r(pinch), ("pinch_in", "font_smaller"));
        // Every listed action has a handler.
        for (a, _) in ACTIONS.iter().chain(LONG_PRESS_ACTIONS).chain(SLIDE_ACTIONS) {
            assert!(KNOWN.contains(a), "{a} has no handler");
        }
        for (_, a) in DEFAULT_GESTURES {
            assert!(KNOWN.contains(a), "{a} has no handler");
        }
    }

    #[test]
    fn bottom_zones_do_not_overlap_and_stay_assignable() {
        let (w, h) = (SIZE.w as i32, SIZE.h as i32);
        let first = |x, y| triggers(&tap(x, y), SIZE).unwrap();
        // Corners, bottom middle and the page-turn zones they fall back to.
        assert_eq!(first(w / 8 - 1, h - 1), ("tap_bottom_left", Some("tap_left")));
        assert_eq!(first(w / 8, h - 1), ("tap_left", None));
        assert_eq!(first(w - w / 8, h - 1), ("tap_bottom_right", Some("tap_right")));
        assert_eq!(first(w / 3, h * 4 / 5), ("tap_bottom_center", Some("tap_right")));
        assert_eq!(first(w / 3 - 1, h - 1), ("tap_left", None));
        assert_eq!(first(w * 2 / 3, h - 1), ("tap_right", None));
        assert_eq!(first(w / 2, h * 4 / 5 - 1), ("tap_center", None));
        // No point is in two specific zones: corners are inside the outer thirds.
        for x in (0..w).step_by(7) {
            for y in (h * 3 / 4..h).step_by(7) {
                let p = Point::new(x, y);
                assert!(!(tap_corner(p, w, h).is_some() && tap_bottom_center(p, w, h).is_some()), "{x},{y}");
            }
        }
        let mut map = GestureMap::default();
        map.set("tap_bottom_center", "toc");
        map.set("tap_bottom_left", "none");
        assert_eq!(resolve(&tap(636, 1690), SIZE, &map).unwrap(), ("tap_bottom_center", "toc"));
        assert_eq!(resolve(&tap(10, 1690), SIZE, &map).unwrap(), ("tap_left", "prev_page"));
    }
}
