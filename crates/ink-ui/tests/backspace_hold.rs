//! Holding backspace on a slow device: the E2E scenario `ui-gallery` holds the key for
//! 0.95 s; with the 400 ms hold time (`ink_ui::LONG_PRESS`) that is four deletes
//! (repeats at 400, 550, 700 and 850 ms).
//!
//! The raw protocol-B records of the emulator's fts_ts controller go through the same
//! chain as on the device: `TouchDecoder` -> `AppContacts` -> an input that waits like
//! `KindleInput::next_event` (its wait decision is `ink_device::input::wait_step`) ->
//! `GestureRecognizer` -> `Keyboard`. Time is virtual. A loop pass that changed the text
//! costs 200 ms (a redraw on the slow emulator), longer than the 150 ms repeat interval, so
//! the repeat timer is overdue whenever the loop asks for input.

use ink_device::evdev::{AppContacts, RawEvent, TouchDecoder, TouchMapping, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_TRACKING_ID, BTN_TOUCH, EV_ABS, EV_KEY, EV_SYN, SYN_REPORT};
use ink_device::input::{wait_step, WaitStep};
use ink_device::{Input, InputEvent, Point, Rect, Rotation, Size, Timestamp};
use ink_ui::{translate, EventResult, GestureRecognizer, KeyAction, Keyboard, ScreenSpace, UiEvent, Widget};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const PANEL: Size = Size::new(1272, 1696);
const AREA: Rect = Rect::new(0, 1000, 1272, 680);

/// Raw records readable at their time; a virtual clock.
struct SlowDevice {
    base: Instant,
    now: Duration,
    raw: VecDeque<RawEvent>,
    decoder: TouchDecoder,
    app: AppContacts,
    queue: VecDeque<InputEvent>,
}

impl SlowDevice {
    fn instant(&self) -> Instant {
        self.base + self.now
    }

    /// Reads every record whose time has come (one `read()` of the evdev node).
    fn read_ready(&mut self) {
        while self.raw.front().is_some_and(|e| e.time.0 <= self.now) {
            let e = self.raw.pop_front().unwrap();
            if let Some(frame) = self.decoder.feed(e) {
                if let Some(f) = self.app.filter(frame) {
                    self.queue.push_back(InputEvent::Touch(f));
                }
            }
        }
    }
}

impl Input for SlowDevice {
    /// The control flow of `KindleInput::next_event` with a virtual `poll()`.
    fn next_event(&mut self, timeout: Option<Duration>) -> ink_device::Result<Option<InputEvent>> {
        let caller = timeout.map(|t| self.instant() + t);
        let mut polled = false;
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Ok(Some(ev));
            }
            let now = self.instant();
            let deadline = caller.map_or(now + Duration::from_secs(5), |c| c.max(now));
            let wait = match wait_step(caller, deadline, now, polled) {
                WaitStep::Return => return Ok(None),
                WaitStep::Poll(d) => d,
            };
            polled = true;
            // poll(): ready at once, or sleep until the next record or the deadline.
            if self.raw.front().is_none_or(|e| e.time.0 > self.now) {
                let next = self.raw.front().map_or(Duration::MAX, |e| e.time.0);
                self.now = next.min(self.now + wait);
            }
            self.read_ready();
            if self.raw.is_empty() && self.queue.is_empty() && timeout.is_none() {
                return Ok(Some(InputEvent::Quit));
            }
        }
    }
}

fn ev(ms: u64, ty: u16, code: u16, value: i32) -> RawEvent {
    RawEvent { time: Timestamp::from_millis(ms), ty, code, value }
}

/// A hold of `ms` at `p` as the emulator's fts_ts reports it (protocol B, `BTN_TOUCH`).
fn hold(start: u64, ms: u64, p: Point) -> Vec<RawEvent> {
    vec![
        ev(start, EV_ABS, ABS_MT_TRACKING_ID, 7),
        ev(start, EV_ABS, ABS_MT_POSITION_X, p.x),
        ev(start, EV_ABS, ABS_MT_POSITION_Y, p.y),
        ev(start, EV_KEY, BTN_TOUCH, 1),
        ev(start, EV_SYN, SYN_REPORT, 0),
        ev(start + ms, EV_ABS, ABS_MT_TRACKING_ID, -1),
        ev(start + ms, EV_KEY, BTN_TOUCH, 0),
        ev(start + ms, EV_SYN, SYN_REPORT, 0),
    ]
}

fn del_center(kb: &Keyboard) -> Point {
    let (r, _) = kb.layout.key_rects(kb.keys_area(AREA)).into_iter().find(|(_, k)| k.label == "del").unwrap();
    Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)
}

/// Runs the app loop until the input ends (or 200 iterations); returns the key actions.
fn run(hold_ms: u64, redraw: Duration) -> Vec<KeyAction> {
    let mut kb = Keyboard::new();
    let start = 1000;
    let mut dev = SlowDevice {
        base: Instant::now(),
        now: Duration::from_millis(start),
        raw: hold(start, hold_ms, del_center(&kb)).into(),
        decoder: TouchDecoder::new(TouchMapping::identity(PANEL.w as i32, PANEL.h as i32)),
        app: AppContacts::default(),
        queue: VecDeque::new(),
    };
    let mut rec = GestureRecognizer::new(ScreenSpace { panel: PANEL, rotation: Rotation::Portrait });
    let mut actions = Vec::new();
    for _ in 0..200 {
        let now = Timestamp(dev.now);
        let timeout = rec.next_deadline().map(|d| d.since(now));
        let events = match dev.next_event(timeout).unwrap() {
            Some(InputEvent::Quit) => break,
            Some(e) => translate(&mut rec, e),
            None => rec.tick(Timestamp(dev.now)),
        };
        let mut changed = false;
        for e in &events {
            if let UiEvent::Touch(_) = e {
                if let EventResult::Action(_) = kb.event(e, AREA) {
                    actions.extend(kb.take_actions());
                    changed = true;
                }
            }
        }
        if changed {
            // The text changed: the loop redraws once before it asks for input again.
            dev.now += redraw;
        }
    }
    actions
}

#[test]
fn backspace_hold_on_a_fast_loop_deletes_four() {
    assert_eq!(run(950, Duration::ZERO), vec![KeyAction::Backspace; 4]);
}

#[test]
fn backspace_hold_with_slow_redraws_stops_at_the_lift() {
    // Redraws slower than the repeat interval: the timer is overdue at every wait, and
    // the lift (input time 950 ms) must still be read. Holds due before the lift's input
    // time fire when its frame arrives, so the count is the same as on a fast loop.
    let got = run(950, Duration::from_millis(200));
    assert!(got.iter().all(|a| *a == KeyAction::Backspace), "{got:?}");
    assert_eq!(got.len(), 4, "deletes after a 0.95 s hold");
}
