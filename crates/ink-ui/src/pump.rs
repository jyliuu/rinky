//! The input step of an event loop.
//!
//! [`next_events`] waits for device input at most until the screen's own timeout or the
//! recogniser's next timer (holds, delayed taps), keeps the recogniser's rotation in step
//! with the screen (so a screen that rotates gets correct coordinates from the next
//! touch on; touches in progress end with [`crate::TouchKind::Cancel`]), and returns UI
//! events.

use crate::gesture::{GestureRecognizer, ScreenSpace};
use crate::{translate, UiEvent};
use ink_device::{Input, Screen};
use std::time::Duration;

/// Waits for input and translates it.
///
/// - `screen_timeout`: how long the screen lets the loop sleep (`None`: until input).
/// - Returns `[UiEvent::Timer(0)]` when the screen's timeout passed with no input and no
///   recogniser timer fired; recogniser timers give touch holds / delayed taps instead.
pub fn next_events(
    rec: &mut GestureRecognizer,
    input: &mut dyn Input,
    screen: &dyn Screen,
    screen_timeout: Option<Duration>,
) -> ink_device::Result<Vec<UiEvent>> {
    let space = ScreenSpace { panel: screen.panel_size(), rotation: screen.rotation() };
    if rec.space != space {
        // A rotation change: touches in progress belong to the old orientation. They end
        // with Cancel notifications, returned at once, so a widget that took a touch-down
        // (a pressed button, a held key) is released before the loop waits for input.
        let cancelled = rec.cancel_all();
        rec.space = space;
        if !cancelled.is_empty() {
            return Ok(cancelled);
        }
    }
    let rec_timeout = rec.timeout();
    let timeout = match (screen_timeout, rec_timeout) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    match input.next_event(timeout)? {
        Some(ev) => {
            let mut out = translate(rec, ev);
            if input.coalesce_moves() && out.iter().any(is_move) {
                // A finger is moving: take what else is waiting now and keep only the
                // newest position, so a slow paint never replays old positions.
                for _ in 0..MAX_BURST {
                    match input.next_event(Some(Duration::ZERO))? {
                        Some(ev) => out.extend(translate(rec, ev)),
                        None => break,
                    }
                }
                let n = out.len();
                out = merge_moves(out);
                if out.len() < n {
                    log::debug!("pump: {} old touch moves dropped", n - out.len());
                }
            }
            Ok(out)
        }
        None => {
            let ev = rec.poll();
            if ev.is_empty() && screen_timeout.is_some() {
                Ok(vec![UiEvent::Timer(0)])
            } else {
                Ok(ev)
            }
        }
    }
}

/// Most input events [`next_events`] reads in one burst after a touch move.
pub const MAX_BURST: usize = 64;

fn is_move(ev: &UiEvent) -> bool {
    matches!(ev, UiEvent::Touch(t) if t.kind == crate::TouchKind::Move)
}

/// Drops every touch move that a later move of the same contact replaces (nothing else
/// of that contact in between). Other events keep their order.
pub fn merge_moves(events: Vec<UiEvent>) -> Vec<UiEvent> {
    let mut out: Vec<UiEvent> = Vec::with_capacity(events.len());
    for ev in events {
        if let UiEvent::Touch(t) = &ev {
            if t.kind == crate::TouchKind::Move {
                let last = out.iter().rposition(|e| matches!(e, UiEvent::Touch(p) if p.slot == t.slot));
                if let Some(i) = last.filter(|i| is_move(&out[*i])) {
                    out.remove(i);
                }
            }
        }
        out.push(ev);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Gesture, TouchKind};
    use ink_device::headless::{self, HeadlessOptions};
    use ink_device::script::parse;
    use ink_device::{Point, Rotation};

    #[test]
    fn scripted_hold_gives_hold_notifications_and_rotation_follows_the_screen() {
        let script = parse("hold 600 800 1000\ntap 1271 0\n").unwrap();
        let (mut dev, _probe) = headless::open(HeadlessOptions { script, ..Default::default() });
        let space = ScreenSpace { panel: dev.screen.panel_size(), rotation: dev.screen.rotation() };
        let mut rec = GestureRecognizer::new(space);
        let mut all = Vec::new();
        for _ in 0..2 {
            all.extend(next_events(&mut rec, dev.input.as_mut(), dev.screen.as_ref(), None).unwrap());
        }
        let holds = all.iter().filter(|e| matches!(e, UiEvent::Touch(t) if t.kind == TouchKind::Hold)).count();
        assert_eq!(holds, 5, "400, 550, 700, 850 and 1000 ms (the lift): {all:?}");
        let presses: Vec<usize> = all.iter().enumerate().filter(|(_, e)| matches!(e, UiEvent::Gesture(Gesture::LongPress { .. }))).map(|(i, _)| i).collect();
        let first_hold = all.iter().position(|e| matches!(e, UiEvent::Touch(t) if t.kind == TouchKind::Hold)).unwrap();
        let up = all.iter().position(|e| matches!(e, UiEvent::Touch(t) if t.kind == TouchKind::Up)).unwrap();
        // One long press, right after the first hold and before the lift.
        assert_eq!(presses, vec![first_hold + 1], "{all:?}");
        assert!(presses[0] < up);
        // Rotate the screen: the next tap is mapped with the new rotation.
        dev.screen.set_rotation(Rotation::Clockwise).unwrap();
        let mut ev = Vec::new();
        for _ in 0..2 {
            ev.extend(next_events(&mut rec, dev.input.as_mut(), dev.screen.as_ref(), None).unwrap());
        }
        assert!(ev.contains(&UiEvent::Gesture(Gesture::Tap { pos: Point::new(0, 0) })), "{ev:?}");
        // The script is over: Quit.
        assert_eq!(next_events(&mut rec, dev.input.as_mut(), dev.screen.as_ref(), None).unwrap(), vec![UiEvent::Quit]);
    }

    #[test]
    fn moves_merge_to_the_newest_position_per_contact() {
        use crate::Touch;
        use ink_device::Point;
        let t = |slot: u8, kind: TouchKind, x: i32| {
            UiEvent::Touch(Touch { slot, kind, pos: Point::new(x, 0), start: Point::new(0, 0), held: Duration::ZERO, moved: false })
        };
        let ev = vec![
            t(0, TouchKind::Down, 0),
            t(0, TouchKind::Move, 10),
            t(1, TouchKind::Move, 5),
            t(0, TouchKind::Move, 20),
            t(0, TouchKind::Move, 30),
            t(0, TouchKind::Up, 30),
            t(0, TouchKind::Move, 40),
        ];
        let got = merge_moves(ev);
        let want = vec![t(0, TouchKind::Down, 0), t(1, TouchKind::Move, 5), t(0, TouchKind::Move, 30), t(0, TouchKind::Up, 30), t(0, TouchKind::Move, 40)];
        assert_eq!(got, want);
    }

    #[test]
    fn idle_timeouts() {
        struct Idle;
        impl Input for Idle {
            fn next_event(&mut self, timeout: Option<Duration>) -> ink_device::Result<Option<ink_device::InputEvent>> {
                std::thread::sleep(timeout.unwrap_or(Duration::ZERO));
                Ok(None)
            }
        }
        let (dev, _) = headless::open(HeadlessOptions::default());
        let mut rec = GestureRecognizer::new(ScreenSpace { panel: dev.screen.panel_size(), rotation: Rotation::Portrait });
        let ev = next_events(&mut rec, &mut Idle, dev.screen.as_ref(), Some(Duration::from_millis(1))).unwrap();
        assert_eq!(ev, vec![UiEvent::Timer(0)]);
        assert!(next_events(&mut rec, &mut Idle, dev.screen.as_ref(), None).unwrap().is_empty());
    }
}
