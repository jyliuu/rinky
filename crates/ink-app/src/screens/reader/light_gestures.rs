//! KOReader's frontlight gestures on the reading page:
//! - edge slides: a vertical drag that starts in the left (or right) 1/8 of the screen,
//!   outside the top and bottom 1/20 (the edge swipes there keep their actions), sets
//!   brightness (or warmth) while the finger moves: up = more, the whole range over
//!   [`SLIDE_SPAN`] of the screen height. Triggers `slide_left_edge` /
//!   `slide_right_edge`, actions from [`ink_store::prefs::SLIDE_ACTIONS`] (`none` = off);
//! - `light_toggle` / `warmth_toggle` (default on the bottom-left and bottom-right
//!   corners): to the minimum, and back to the level before on the next toggle.
//!
//! A slide writes the light through [`Light::set_brightness_live`] at most as often as
//! the control center's sliders ([`quick_light::should_write`]) and the final value on
//! release. Nothing is drawn: the light itself is the feedback, so the e-ink panel does
//! no refresh while the finger moves.
//!
//! The levels the toggles restore are kept for the session (all books), like the auto
//! warmth state.

use crate::screens::settings::quick_light;
use ink_device::clock::DateTime;
use ink_device::Light;
use ink_device::{Point, Size};
use ink_store::prefs::GestureMap;
use std::cell::RefCell;
use std::time::Instant;

/// Moving the finger this fraction of the screen height (per mille) covers the whole
/// brightness or warmth range.
pub const SLIDE_SPAN: i32 = 600;

/// The slide action for a contact that started at `start` and is now at `pos`, when it
/// is an edge slide with an action in `map`: `Some(true)` warmth, `Some(false)`
/// brightness. `None` while the move is not clearly vertical (|dy| at least twice |dx|).
pub fn slide_start(start: Point, pos: Point, size: Size, map: &GestureMap) -> Option<bool> {
    let (w, h) = (size.w as i32, size.h as i32);
    let (dx, dy) = (pos.x - start.x, pos.y - start.y);
    if dy.abs() < 2 * dx.abs() || dy == 0 || start.y < h / 20 || start.y >= h - h / 20 {
        return None;
    }
    let trigger = if start.x < w / 8 {
        "slide_left_edge"
    } else if start.x >= w - w / 8 {
        "slide_right_edge"
    } else {
        return None;
    };
    match map.action(trigger) {
        "light_slide" => Some(false),
        "warmth_slide" => Some(true),
        _ => None,
    }
}

/// The level after moving `dy` pixels (down positive) from `base` on a screen `h` high,
/// within `range`.
pub fn slide_level(base: u32, dy: i32, (lo, hi): (u32, u32), h: u32) -> u32 {
    let span = (h as i64 * SLIDE_SPAN as i64 / 1000).max(1);
    let delta = -(dy as i64) * (hi.saturating_sub(lo)) as i64;
    // Rounded to the nearest step, both ways.
    let steps = (delta + delta.signum() * span / 2) / span;
    (base as i64 + steps).clamp(lo as i64, hi as i64) as u32
}

/// The level after a toggle from `cur`: on (above `lo`) it goes to `lo` and `cur` is
/// kept in `saved`; off it goes back to `saved`, or to the middle of the range when
/// nothing was kept (the light was off already when rinky started).
pub fn toggle_level(cur: u32, (lo, hi): (u32, u32), saved: &mut Option<u32>) -> u32 {
    if cur > lo {
        *saved = Some(cur);
        lo
    } else {
        saved.take().filter(|v| *v > lo).unwrap_or(lo + (hi - lo).div_ceil(2)).min(hi)
    }
}

/// The range and the current level of the warmth (`warmth`) or brightness control;
/// `None`, logged for `what`, when the level cannot be read.
pub fn level(light: &dyn Light, warmth: bool, what: &str) -> Option<((u32, u32), u32)> {
    let (range, cur) = if warmth { (light.warmth_range(), light.warmth()) } else { (light.brightness_range(), light.brightness()) };
    cur.map(|v| (range, v)).map_err(|e| log::warn!("reader: {what}: {e}")).ok()
}

/// Sets the warmth (`warmth`) or brightness to `v` through the control center's light
/// code, and lets auto warmth keep a warmth set by hand. Returns the applied value's
/// text (`None`: not applied).
pub fn set_level(light: &mut dyn Light, warmth: bool, v: u32, now: &DateTime) -> Option<String> {
    let name = if warmth { quick_light::WARMTH } else { quick_light::BRIGHTNESS };
    let text = quick_light::apply(&format!("{name}:{v}"), light);
    if warmth {
        quick_light::with_auto_warmth(light, |w, light| w.manual(v, light, now));
    }
    text
}

thread_local! {
    /// Levels the toggles go back to: (brightness, warmth).
    static SAVED: RefCell<(Option<u32>, Option<u32>)> = const { RefCell::new((None, None)) };
}

/// An edge slide in progress.
#[derive(Clone, Debug)]
pub struct Slide {
    /// Warmth (else brightness).
    pub warmth: bool,
    /// Level when the slide started.
    base: u32,
    range: (u32, u32),
    /// Where the finger was when the slide started.
    from: Point,
    /// Last value written while sliding, and when.
    last: Option<(u32, Instant)>,
    /// Current value.
    pub value: u32,
}

impl Slide {
    /// Starts a slide at `from` from the light's current level; `None` when the light
    /// has no such control.
    pub fn new(warmth: bool, from: Point, light: &dyn Light) -> Option<Slide> {
        let (range, base) = level(light, warmth, "edge slide")?;
        log::info!("reader: edge slide ({}) from {base}", if warmth { "warmth" } else { "brightness" });
        Some(Slide { warmth, base, range, from, last: None, value: base })
    }

    /// The finger is at `pos`: writes the new level when it is due.
    pub fn moved(&mut self, pos: Point, h: u32, light: &mut dyn Light) {
        self.value = slide_level(self.base, pos.y - self.from.y, self.range, h);
        let now = Instant::now();
        if self.value == self.base && self.last.is_none() || !quick_light::should_write(self.last, self.value, now) {
            return;
        }
        let r = if self.warmth { light.set_warmth_live(self.value) } else { light.set_brightness_live(self.value) };
        if let Err(e) = r {
            log::warn!("reader: edge slide: {e}");
        }
        self.last = Some((self.value, now));
    }

    /// The finger lifted: writes the final level (auto warmth keeps it as the user's).
    pub fn finish(self, light: &mut dyn Light, now: &DateTime) {
        if self.value == self.base && self.last.is_none() {
            return;
        }
        if let Some(text) = set_level(light, self.warmth, self.value, now) {
            log::info!("reader: edge slide: {} -> {text}", self.base);
        }
    }
}

/// `light_toggle` / `warmth_toggle`: the light (warmth) off, or back to its level before.
pub fn toggle(warmth: bool, light: &mut dyn Light, now: &DateTime) {
    let Some((range, cur)) = level(light, warmth, "light toggle") else { return };
    let v = SAVED.with(|s| {
        let mut s = s.borrow_mut();
        toggle_level(cur, range, if warmth { &mut s.1 } else { &mut s.0 })
    });
    if let Some(text) = set_level(light, warmth, v, now) {
        log::info!("reader: {} toggle: {cur} -> {text}", if warmth { "warmth" } else { "light" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Size = Size::new(1272, 1696);

    #[test]
    fn toggle_turns_off_and_restores_the_level_before() {
        let mut saved = None;
        assert_eq!(toggle_level(14, (0, 24), &mut saved), 0);
        assert_eq!(saved, Some(14));
        assert_eq!(toggle_level(0, (0, 24), &mut saved), 14);
        assert_eq!(saved, None);
        // Off with nothing kept: the middle of the range.
        assert_eq!(toggle_level(0, (0, 24), &mut saved), 12);
        // Changed by hand after a toggle: the new level is the one kept.
        let mut saved = Some(14);
        assert_eq!(toggle_level(5, (0, 24), &mut saved), 0);
        assert_eq!(toggle_level(0, (0, 24), &mut saved), 5);
        // A range that does not start at 0.
        let mut saved = None;
        assert_eq!(toggle_level(3, (2, 10), &mut saved), 2);
        assert_eq!(toggle_level(2, (2, 10), &mut saved), 3);
    }

    #[test]
    fn slide_follows_the_finger_within_the_range() {
        let h = SIZE.h;
        let span = h as i32 * SLIDE_SPAN / 1000;
        assert_eq!(slide_level(10, 0, (0, 24), h), 10);
        assert_eq!(slide_level(10, -span / 2, (0, 24), h), 22);
        assert_eq!(slide_level(10, span / 2, (0, 24), h), 0);
        assert_eq!(slide_level(10, -span, (0, 24), h), 24);
        assert_eq!(slide_level(10, -span / 24, (0, 24), h), 11);
        assert_eq!(slide_level(10, span / 24, (0, 24), h), 9);
    }

    #[test]
    fn slides_start_only_vertically_from_the_side_edges() {
        let map = GestureMap::default();
        let s = |x0, y0, x1, y1| slide_start(Point::new(x0, y0), Point::new(x1, y1), SIZE, &map);
        assert_eq!(s(40, 900, 45, 800), Some(false));
        assert_eq!(s(1250, 900, 1245, 1000), Some(true));
        // Middle of the page, horizontal moves, the top and bottom edge bands.
        assert_eq!(s(600, 900, 600, 800), None);
        assert_eq!(s(40, 900, 140, 880), None);
        assert_eq!(s(40, 1690, 40, 1500), None);
        assert_eq!(s(1250, 10, 1250, 200), None);
        // Turned off.
        let mut map = GestureMap::default();
        map.set("slide_left_edge", "none");
        map.set("slide_right_edge", "light_slide");
        assert_eq!(slide_start(Point::new(40, 900), Point::new(40, 800), SIZE, &map), None);
        assert_eq!(slide_start(Point::new(1250, 900), Point::new(1250, 800), SIZE, &map), Some(false));
    }
}
