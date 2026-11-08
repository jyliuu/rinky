//! Scripted input for headless runs and self-closing simulator runs.
//!
//! One step per line; `#` starts a comment. Coordinates are panel pixels.
//!
//! ```text
//! tap X Y              finger down, 60 ms, finger up
//! quicktap X Y         down and up in the same SYN frame (the PW12 short-tap case)
//! hold X Y MS          long press for MS milliseconds
//! swipe X1 Y1 X2 Y2    finger down, 6 moves over 300 ms, finger up
//! key NAME             press and release: power, next, prev, home, menu, back, or a number
//! power EVENT          a powerd event line, e.g. `power goingToScreenSaver`
//! net NEWS             a network worker event: `connected ESSID`, `connecting [ESSID]`,
//!                      `nonetwork`, `off`, `failed ESSID`, `scanned N`
//! wait MS              advance the clock by MS milliseconds
//! sleep MS             real time: no input for MS milliseconds; the app's timeouts run
//!                      as on the device (headless and simulator backends)
//! shot NAME            write the panel to NAME.png in the output directory
//! quit                 end the run (also implied at the end of the script)
//! down X Y [SLOT]      one finger down (slot 0 by default); shots can follow while it is down
//! move X Y [SLOT]      that finger moves to X Y
//! up X Y [SLOT]        that finger lifts at X Y
//! twotap X1 Y1 X2 Y2   two fingers down in one frame, up 80 ms later
//! pinch CX CY D0 D1    two fingers on a horizontal line through CX CY move from distance
//!                      D0 to D1 (D1 > D0: spread) in 6 steps, then lift
//! ```

use crate::input::{InputEvent, Key, KeyEvent, PowerEvent, Timestamp, TouchContact, TouchFrame, TouchPhase};
use crate::{DeviceError, Result};
use std::collections::VecDeque;
use std::time::Duration;

/// One parsed script line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Normal tap.
    Tap(i32, i32),
    /// Down and up in one frame.
    QuickTap(i32, i32),
    /// Long press.
    Hold(i32, i32, u64),
    /// Swipe from the first point to the second.
    Swipe(i32, i32, i32, i32),
    /// Key press and release.
    Key(Key),
    /// A powerd event.
    Power(String),
    /// A network worker event ([`net_event`]).
    Net(crate::netman::NetEvent),
    /// Advance the clock.
    Wait(u64),
    /// No input for this many real milliseconds.
    Sleep(u64),
    /// Save a screenshot under this name.
    Shot(String),
    /// Stop.
    Quit,
    /// One finger down (x, y, slot).
    Down(i32, i32, u8),
    /// A finger moves (x, y, slot).
    Move(i32, i32, u8),
    /// A finger lifts (x, y, slot).
    Up(i32, i32, u8),
    /// Two-finger tap.
    TwoTap(i32, i32, i32, i32),
    /// Pinch or spread around a centre: (cx, cy, from distance, to distance).
    Pinch(i32, i32, i32, i32),
}

fn key_by_name(name: &str) -> Option<Key> {
    Some(match name {
        "power" => Key::Power,
        "next" => Key::PageNext,
        "prev" => Key::PagePrev,
        "home" => Key::Home,
        "menu" => Key::Menu,
        "back" => Key::Back,
        n => Key::Other(n.parse().ok()?),
    })
}

/// Parses a whole script.
pub fn parse(text: &str) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    for (no, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let w: Vec<&str> = line.split_whitespace().collect();
        let bad = || DeviceError::Script(format!("line {}: {raw:?}", no + 1));
        let n = |i: usize| -> Result<i64> { w.get(i).and_then(|s| s.parse().ok()).ok_or_else(bad) };
        let step = match (w[0], w.len()) {
            ("tap", 3) => Step::Tap(n(1)? as i32, n(2)? as i32),
            ("quicktap", 3) => Step::QuickTap(n(1)? as i32, n(2)? as i32),
            ("hold", 4) => Step::Hold(n(1)? as i32, n(2)? as i32, n(3)? as u64),
            ("swipe", 5) => Step::Swipe(n(1)? as i32, n(2)? as i32, n(3)? as i32, n(4)? as i32),
            ("key", 2) => Step::Key(key_by_name(w[1]).ok_or_else(bad)?),
            ("power", 2..) => Step::Power(w[1..].join(" ")),
            ("net", 2..) => Step::Net(net_event(&w[1..]).ok_or_else(bad)?),
            ("wait", 2) => Step::Wait(n(1)? as u64),
            ("sleep", 2) => Step::Sleep(n(1)? as u64),
            ("shot", 2) => Step::Shot(w[1].to_string()),
            ("quit", 1) => Step::Quit,
            ("down" | "move" | "up", 3 | 4) => {
                let slot = if w.len() == 4 { n(3)? as u8 } else { 0 };
                let (x, y) = (n(1)? as i32, n(2)? as i32);
                match w[0] {
                    "down" => Step::Down(x, y, slot),
                    "move" => Step::Move(x, y, slot),
                    _ => Step::Up(x, y, slot),
                }
            }
            ("twotap", 5) => Step::TwoTap(n(1)? as i32, n(2)? as i32, n(3)? as i32, n(4)? as i32),
            ("pinch", 5) => Step::Pinch(n(1)? as i32, n(2)? as i32, n(3)? as i32, n(4)? as i32),
            _ => return Err(bad()),
        };
        steps.push(step);
    }
    Ok(steps)
}

/// A `net` step's event: `connected ESSID`, `connecting [ESSID]`, `nonetwork`, `off`,
/// `failed ESSID`, `scanned N` (a name may have spaces).
pub fn net_event(w: &[&str]) -> Option<crate::netman::NetEvent> {
    use crate::netman::{NetEvent, NetStatus};
    let name = (w.len() > 1).then(|| w[1..].join(" "));
    Some(match (*w.first()?, name) {
        ("connected", Some(essid)) => NetEvent::Status(NetStatus::Connected { essid, signal: None }),
        ("connecting", essid) => NetEvent::Status(NetStatus::Connecting { essid }),
        ("nonetwork", None) => NetEvent::Status(NetStatus::NoNetwork),
        ("off", None) => NetEvent::Status(NetStatus::Off),
        ("failed", Some(essid)) => NetEvent::JoinFailed(essid),
        ("scanned", Some(n)) => NetEvent::Scanned(Ok(n.parse().ok()?)),
        _ => return None,
    })
}

/// Output of a script player: an input event, or a request to save a screenshot.
#[derive(Clone, Debug, PartialEq)]
pub enum Played {
    /// Deliver this event to the app.
    Event(InputEvent),
    /// Save the panel as `NAME.png`.
    Shot(String),
    /// No input for this long (real time). The backend returns `None` to a caller whose
    /// timeout ends first and keeps the rest of the pause for the next call.
    Sleep(Duration),
}

/// Expands steps into timed events on a virtual clock.
pub struct ScriptPlayer {
    queue: VecDeque<Played>,
    clock: Duration,
}

impl ScriptPlayer {
    /// Expands `steps`. The virtual clock starts at 1 s.
    pub fn new(steps: &[Step]) -> Self {
        let mut p = ScriptPlayer { queue: VecDeque::new(), clock: Duration::from_secs(1) };
        for s in steps {
            p.expand(s);
        }
        p
    }

    fn frame(&mut self, ms: u64, contacts: Vec<TouchContact>) {
        self.clock += Duration::from_millis(ms);
        let time = Timestamp(self.clock);
        self.queue.push_back(Played::Event(InputEvent::Touch(TouchFrame { time, contacts })));
    }

    fn contact(x: i32, y: i32, phase: TouchPhase) -> Vec<TouchContact> {
        vec![TouchContact { slot: 0, x, y, phase }]
    }

    fn expand(&mut self, s: &Step) {
        match *s {
            Step::Tap(x, y) => {
                self.frame(10, Self::contact(x, y, TouchPhase::Down));
                self.frame(60, Self::contact(x, y, TouchPhase::Up));
            }
            Step::QuickTap(x, y) => self.frame(10, Self::contact(x, y, TouchPhase::DownUp)),
            Step::Hold(x, y, ms) => {
                self.frame(10, Self::contact(x, y, TouchPhase::Down));
                self.frame(ms, Self::contact(x, y, TouchPhase::Up));
            }
            Step::Swipe(x1, y1, x2, y2) => {
                self.frame(10, Self::contact(x1, y1, TouchPhase::Down));
                for i in 1..=6 {
                    let x = x1 + (x2 - x1) * i / 6;
                    let y = y1 + (y2 - y1) * i / 6;
                    self.frame(50, Self::contact(x, y, TouchPhase::Move));
                }
                self.frame(10, Self::contact(x2, y2, TouchPhase::Up));
            }
            Step::Key(key) => {
                for pressed in [true, false] {
                    self.clock += Duration::from_millis(50);
                    let time = Timestamp(self.clock);
                    self.queue.push_back(Played::Event(InputEvent::Key(KeyEvent { key, pressed, time })));
                }
            }
            Step::Power(ref line) => {
                if let Some(ev) = PowerEvent::parse(line) {
                    self.queue.push_back(Played::Event(InputEvent::Power(ev)));
                }
            }
            Step::Net(ref e) => self.queue.push_back(Played::Event(InputEvent::Power(PowerEvent::Network(e.clone())))),
            Step::Wait(ms) => self.clock += Duration::from_millis(ms),
            Step::Shot(ref name) => self.queue.push_back(Played::Shot(name.clone())),
            Step::Sleep(ms) => self.queue.push_back(Played::Sleep(Duration::from_millis(ms))),
            Step::Quit => self.queue.push_back(Played::Event(InputEvent::Quit)),
            Step::Down(x, y, slot) => self.frame(10, vec![TouchContact { slot, x, y, phase: TouchPhase::Down }]),
            Step::Move(x, y, slot) => self.frame(30, vec![TouchContact { slot, x, y, phase: TouchPhase::Move }]),
            Step::Up(x, y, slot) => self.frame(10, vec![TouchContact { slot, x, y, phase: TouchPhase::Up }]),
            Step::TwoTap(x1, y1, x2, y2) => {
                let two = |phase| {
                    vec![TouchContact { slot: 0, x: x1, y: y1, phase }, TouchContact { slot: 1, x: x2, y: y2, phase }]
                };
                self.frame(10, two(TouchPhase::Down));
                self.frame(80, two(TouchPhase::Up));
            }
            Step::Pinch(cx, cy, d0, d1) => {
                let at = |d: i32, phase| {
                    vec![
                        TouchContact { slot: 0, x: cx - d / 2, y: cy, phase },
                        TouchContact { slot: 1, x: cx + d / 2, y: cy, phase },
                    ]
                };
                self.frame(10, at(d0, TouchPhase::Down));
                for i in 1..=6 {
                    self.frame(50, at(d0 + (d1 - d0) * i / 6, TouchPhase::Move));
                }
                self.frame(10, at(d1, TouchPhase::Up));
            }
        }
    }

    /// Next item; `Quit` forever once the script is exhausted.
    pub fn next_item(&mut self) -> Played {
        self.queue.pop_front().unwrap_or(Played::Event(InputEvent::Quit))
    }

    /// Plays a [`Played::Sleep`] of `d` for a caller that waits at most `timeout`, like
    /// a device with no input: sleeps until `d` or the timeout ends (in steps of at most
    /// 10 ms; a stop request, [`crate::trace::request_stop`], ends it at once). The rest
    /// of the pause goes back to the front of the queue. Returns true when the caller's
    /// timeout ended first (the backend returns `Ok(None)`).
    pub fn sleep(&mut self, d: Duration, timeout: Option<Duration>) -> bool {
        let start = std::time::Instant::now();
        let end = timeout.map_or(d, |t| t.min(d));
        while start.elapsed() < end && crate::trace::stop_requested().is_none() {
            std::thread::sleep(end.saturating_sub(start.elapsed()).min(Duration::from_millis(10)));
        }
        let slept = start.elapsed();
        if slept < d {
            self.queue.push_front(Played::Sleep(d - slept));
        }
        slept < d && crate::trace::stop_requested().is_none()
    }

    /// Items left.
    pub fn remaining(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_expand() {
        let steps = parse("# turn pages\ntap 1000 900\nquicktap 10 10\nswipe 1000 800 200 800\nkey power\nwait 5\nshot p1\n").unwrap();
        assert_eq!(steps.len(), 6);
        assert_eq!(steps[0], Step::Tap(1000, 900));
        let mut p = ScriptPlayer::new(&steps);
        assert_eq!(p.remaining(), 2 + 1 + 8 + 2 + 1);
        match p.next_item() {
            Played::Event(InputEvent::Touch(f)) => assert_eq!(f.contacts[0].phase, TouchPhase::Down),
            other => panic!("{other:?}"),
        }
        assert!(parse("tap 1").is_err());
        assert!(parse("key nonsense").is_err());
    }

    #[test]
    fn net_steps_become_network_events() {
        use crate::netman::{NetEvent, NetStatus};
        let steps = parse("net connected Home Wifi
net connecting
net failed Cafe
net scanned 3
net off
").unwrap();
        assert_eq!(steps[0], Step::Net(NetEvent::Status(NetStatus::Connected { essid: "Home Wifi".into(), signal: None })));
        assert_eq!(steps[1], Step::Net(NetEvent::Status(NetStatus::Connecting { essid: None })));
        assert_eq!(steps[2], Step::Net(NetEvent::JoinFailed("Cafe".into())));
        assert_eq!(steps[3], Step::Net(NetEvent::Scanned(Ok(3))));
        let mut p = ScriptPlayer::new(&steps[4..]);
        assert_eq!(p.next_item(), Played::Event(InputEvent::Power(PowerEvent::Network(NetEvent::Status(NetStatus::Off)))));
        assert!(parse("net connected").is_err());
        assert!(parse("net scanned x").is_err());
    }

    #[test]
    fn multi_touch_steps() {
        let steps = parse("down 10 20\nmove 30 20\nshot held\nup 30 20\ndown 5 5 1\ntwotap 100 100 300 100\npinch 600 800 200 600\n").unwrap();
        assert_eq!(steps[0], Step::Down(10, 20, 0));
        assert_eq!(steps[4], Step::Down(5, 5, 1));
        let mut p = ScriptPlayer::new(&steps);
        // down, move, shot, up, down, twotap (2 frames), pinch (8 frames)
        assert_eq!(p.remaining(), 3 + 1 + 1 + 2 + 8);
        let mut frames = Vec::new();
        while p.remaining() > 0 {
            if let Played::Event(InputEvent::Touch(f)) = p.next_item() {
                frames.push(f);
            }
        }
        assert_eq!(frames[4].contacts.len(), 2, "two fingers in one frame");
        let last = frames.last().unwrap();
        assert_eq!((last.contacts[0].x, last.contacts[1].x), (300, 900));
        assert!(parse("down 1").is_err());
        assert!(parse("pinch 1 2 3").is_err());
    }
}
