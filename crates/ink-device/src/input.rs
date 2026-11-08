//! Input events: touch frames, keys, power events, and the [`Input`] trait.
//!
//! Touch arrives as [`TouchFrame`]s, one per evdev `SYN_REPORT`, in *panel* pixels.
//! Gesture recognition (tap, swipe, long press, pinch) is done in `ink-ui`, which also
//! maps panel coordinates to the rotated logical screen.

use crate::Result;
use std::time::{Duration, Instant};

/// Time of an event, measured from an arbitrary fixed origin (the kernel's
/// `input_event` time on the Kindle, process start on the host).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(pub Duration);

impl Timestamp {
    /// Builds a timestamp from milliseconds.
    pub const fn from_millis(ms: u64) -> Self {
        Timestamp(Duration::from_millis(ms))
    }

    /// Milliseconds since the origin.
    pub fn as_millis(self) -> u64 {
        self.0.as_millis() as u64
    }

    /// `self - earlier`, or zero when `earlier` is later.
    pub fn since(self, earlier: Timestamp) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

/// What happened to one contact during a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TouchPhase {
    /// The contact started in this frame.
    Down,
    /// The contact moved (or was reported unchanged) while down.
    Move,
    /// The contact ended in this frame; `x`/`y` are its last position.
    Up,
    /// The contact started *and* ended within this one frame. The PW12 touch chip does
    /// this for very short taps (it drops the frame in between); treat it as a tap.
    DownUp,
}

/// One finger in a [`TouchFrame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TouchContact {
    /// Multi-touch slot (0 for the first finger).
    pub slot: u8,
    /// Panel column.
    pub x: i32,
    /// Panel row.
    pub y: i32,
    /// Phase of this contact in this frame.
    pub phase: TouchPhase,
}

/// All contacts that changed in one evdev `SYN_REPORT` frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TouchFrame {
    /// Time of the `SYN_REPORT`.
    pub time: Timestamp,
    /// Contacts that are down, went down or went up in this frame.
    pub contacts: Vec<TouchContact>,
}

/// Physical and simulated keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Power button (`KEY_POWER` on `bd71828-pwrkey`).
    Power,
    /// Next-page key (host simulator: Right, Page Down, Space).
    PageNext,
    /// Previous-page key (host simulator: Left, Page Up).
    PagePrev,
    /// Home (host simulator: H).
    Home,
    /// Menu (host simulator: M).
    Menu,
    /// Back (host simulator: Escape, Backspace).
    Back,
    /// Any other evdev key code.
    Other(u16),
}

/// A key press or release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    /// Which key.
    pub key: Key,
    /// True on press (and auto-repeat), false on release.
    pub pressed: bool,
    /// When it happened.
    pub time: Timestamp,
}

/// Power-management events from `powerd` (`lipc-wait-event -m com.lab126.powerd '*'`).
/// The event names seen on firmware 5.19.5 are listed in `docs/ARCHITECTURE.md`.
/// The first argument of `goingToScreenSaver`/`outOfScreenSaver` is the source
/// (KOReader: 1 button wake, 2 button sleep, 4 cover sleep, 6 cover wake); it is kept
/// only in [`PowerEvent::Other`] for now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerEvent {
    /// `goingToScreenSaver`: the device is about to show the screensaver (power key or timeout).
    GoingToScreenSaver,
    /// `outOfScreenSaver`: the user woke the device.
    OutOfScreenSaver,
    /// `exitingScreenSaver`: the screensaver is being left.
    ExitingScreenSaver,
    /// `wakeupFromSuspend`: the kernel resumed (argument: timestamp).
    WakeupFromSuspend,
    /// `readyToSuspend`: powerd will suspend soon unless suspend is deferred.
    ReadyToSuspend,
    /// `suspending`: the kernel is entering suspend.
    Suspending,
    /// `resuming`: the kernel resumed.
    Resuming,
    /// `charging` / `notCharging`.
    Charging(bool),
    /// `battLevelChanged` with the new percentage.
    BatteryLevel(u8),
    /// Any other powerd event, with its name and arguments.
    Other(String, Vec<String>),
    /// The battery fell to or below the low-battery threshold (percent now). Sent once
    /// per crossing (with hysteresis), by the device layer, not by powerd.
    LowBattery(u8),
    /// The cover's hall sensor: closed (`true`) or opened (`false`)
    /// (`com.lab126.hal magSensorClosed/magSensorOpened`).
    Cover(bool),
    /// USB drive mode changed state (see [`crate::usb::DriveModeState`]).
    DriveMode(crate::usb::DriveModeState),
    /// The userstore is back after USB drive mode (files may have changed): rescan the
    /// library and reopen files.
    LibraryRescan,
    /// The wall clock was set (NTP or the user); seconds of change.
    TimeChanged(i64),
    /// Wi-Fi connection manager state changed (`CONNECTED`, `PENDING`, `NA`, ...).
    WifiState(String),
    /// Any other event from a subscribed lipc publisher (`com.lab126.wifid`, `cmd`,
    /// `volumd`, `hal`), with the publisher's full name.
    Lipc {
        /// Publisher, e.g. `com.lab126.cmd`.
        publisher: String,
        /// Event name.
        name: String,
        /// Arguments.
        args: Vec<String>,
    },
}

impl PowerEvent {
    /// Parses one line of `lipc-wait-event` output (`name arg1 arg2 ...`).
    pub fn parse(line: &str) -> Option<PowerEvent> {
        let mut parts = line.split_whitespace();
        let name = parts.next()?;
        let args: Vec<String> = parts.map(|s| s.trim_matches('"').to_string()).collect();
        Some(match name {
            "goingToScreenSaver" => PowerEvent::GoingToScreenSaver,
            "outOfScreenSaver" => PowerEvent::OutOfScreenSaver,
            "exitingScreenSaver" => PowerEvent::ExitingScreenSaver,
            "wakeupFromSuspend" => PowerEvent::WakeupFromSuspend,
            "readyToSuspend" => PowerEvent::ReadyToSuspend,
            "suspending" => PowerEvent::Suspending,
            "resuming" => PowerEvent::Resuming,
            "charging" => PowerEvent::Charging(true),
            "notCharging" => PowerEvent::Charging(false),
            "battLevelChanged" => match args.first().and_then(|a| a.parse().ok()) {
                Some(level) => PowerEvent::BatteryLevel(level),
                None => PowerEvent::Other(name.to_string(), args),
            },
            _ => PowerEvent::Other(name.to_string(), args),
        })
    }
}

/// Everything the event loop can receive from a device.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// A touch frame.
    Touch(TouchFrame),
    /// A key press or release.
    Key(KeyEvent),
    /// A power-management event.
    Power(PowerEvent),
    /// USB cable plugged (`true`) or unplugged.
    Usb(bool),
    /// The host window was closed, a script ended, or the process was asked to stop.
    Quit,
}

/// A source of input events.
pub trait Input {
    /// Waits for the next event. `timeout` of `None` blocks; `Some(d)` returns `Ok(None)`
    /// when nothing arrived within `d`. Input that is already ready is returned even when
    /// `d` is zero (backends check their sources once, see [`wait_step`]).
    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<InputEvent>>;

    /// Discards queued events (for example taps made during a slow refresh).
    fn flush(&mut self) -> Result<()> {
        while self.next_event(Some(Duration::ZERO))?.is_some() {}
        Ok(())
    }

    /// True for a real-time source: input that is already waiting may be read in one
    /// burst and touch moves merged to the newest position
    /// (`ink_ui::pump::next_events`). Scripted sources keep the default, false: every
    /// event is handled and painted on its own.
    fn coalesce_moves(&self) -> bool {
        false
    }
}

/// What a backend's wait loop does next in [`Input::next_event`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitStep {
    /// Give `Ok(None)` to the caller: its timeout passed.
    Return,
    /// Poll the event sources for at most this long.
    Poll(Duration),
}

/// The next step of a wait loop at `now`. `caller` is when the caller's timeout ends
/// (`None`: no timeout), `deadline` the backend's own next wake-up, `polled` whether
/// this call already polled its sources once.
///
/// A call polls at least once, also with an expired timeout (then for 0 ms): input that
/// is ready is returned, never left in the kernel buffer. An event loop whose timer is
/// always overdue (a key repeat whose redraw takes longer than the repeat interval)
/// would otherwise never read the lift of the held key.
pub fn wait_step(caller: Option<Instant>, deadline: Instant, now: Instant, polled: bool) -> WaitStep {
    if polled && caller.is_some_and(|c| now >= c) {
        return WaitStep::Return;
    }
    let end = caller.map_or(deadline, |c| c.min(deadline));
    WaitStep::Poll(end.saturating_duration_since(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_event_lines() {
        assert_eq!(PowerEvent::parse("goingToScreenSaver 2"), Some(PowerEvent::GoingToScreenSaver));
        assert_eq!(PowerEvent::parse("battLevelChanged 57"), Some(PowerEvent::BatteryLevel(57)));
        assert_eq!(
            PowerEvent::parse("t1TimerReset \"x\""),
            Some(PowerEvent::Other("t1TimerReset".into(), vec!["x".into()]))
        );
        assert_eq!(PowerEvent::parse("   "), None);
    }

    /// A caller whose timeout already passed (a key-repeat timer that is overdue because
    /// the redraw took longer than the repeat interval) still gets input that is ready:
    /// the sources are polled once with 0 ms before `Ok(None)`. Otherwise the lift of a
    /// held key is never read and the key repeats on.
    #[test]
    fn expired_timeout_polls_once_before_returning() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_millis(400);
        // Timeout 0: expired on entry.
        assert_eq!(wait_step(Some(t0), later, t0, false), WaitStep::Poll(Duration::ZERO));
        assert_eq!(wait_step(Some(t0), later, t0, true), WaitStep::Return);
        // Overdue by 50 ms.
        let now = t0 + Duration::from_millis(50);
        assert_eq!(wait_step(Some(t0), later, now, false), WaitStep::Poll(Duration::ZERO));
        assert_eq!(wait_step(Some(t0), later, now, true), WaitStep::Return);
        // Not expired: poll until the earlier of the two deadlines.
        let c = t0 + Duration::from_millis(100);
        assert_eq!(wait_step(Some(c), later, t0, true), WaitStep::Poll(Duration::from_millis(100)));
        assert_eq!(wait_step(Some(later), c, t0, true), WaitStep::Poll(Duration::from_millis(100)));
        assert_eq!(wait_step(None, later, t0, true), WaitStep::Poll(Duration::from_millis(400)));
    }
}
