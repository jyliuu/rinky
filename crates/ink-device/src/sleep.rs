//! Sleep and wake while rinky replaces the framework.
//!
//! With `lab126_gui` stopped, powerd ignores the power button ("Splash screen is on.
//! Ignoring power button", see `docs/ARCHITECTURE.md`) and never starts its screensaver
//! or suspend flow, so rinky does it:
//!
//! ```text
//! Awake --power key / cover closed / inactivity--> ScreenSaver
//!   (frontlight off, screensaver drawn, GoingToScreenSaver)
//! ScreenSaver --after suspend_delay, unless deferred--> kernel suspend
//!   (Suspending; RTC alarm + "mem" > /sys/power/state; returns on resume)
//!   resumed by the RTC alarm  -> stay asleep, suspend again after rtc_grace
//!   resumed by anything else  -> wake (the key that woke us may not be delivered)
//!   refused (EBUSY: USB cable / active wakeup source) -> stay asleep, retry later
//! ScreenSaver --power key / cover opened--> Awake
//!   (page restored with GC16, input re-grabbed and re-seeded, light back,
//!   OutOfScreenSaver)
//! ```
//!
//! [`SleepController`] holds the policy; a [`SleepPlatform`] does the work (the
//! Kindle backend, or a fake in tests). The emulator refuses kernel suspend (USB
//! wakeup source always active), so real suspend/resume is a device test.

use crate::input::PowerEvent;
use crate::Result;
use std::time::{Duration, Instant};

/// Sleep settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SleepConfig {
    /// Go to sleep after this long without touch or keys (`None` = never).
    pub auto_sleep: Option<Duration>,
    /// Try a kernel suspend while the screensaver shows.
    pub suspend: bool,
    /// Delay between drawing the screensaver and suspending (lets the panel finish).
    pub suspend_delay: Duration,
    /// Retry interval after a refused suspend.
    pub retry: Duration,
    /// RTC alarm for each suspend (a maintenance wake; the device sleeps again).
    pub rtc_wake: Option<Duration>,
    /// After an RTC wake, suspend again after this long unless the user woke it.
    pub rtc_grace: Duration,
    /// Ignore a second power-key press within this time of a sleep/wake change.
    pub debounce: Duration,
    /// Turn the frontlight off while asleep.
    pub light_off: bool,
    /// Sleep when the cover closes and wake when it opens.
    pub cover: bool,
}

impl Default for SleepConfig {
    fn default() -> Self {
        SleepConfig {
            auto_sleep: Some(Duration::from_secs(10 * 60)),
            suspend: true,
            suspend_delay: Duration::from_secs(2),
            retry: Duration::from_secs(60),
            rtc_wake: Some(Duration::from_secs(3600)),
            rtc_grace: Duration::from_secs(5),
            debounce: Duration::from_millis(600),
            light_off: true,
            cover: true,
        }
    }
}

impl SleepConfig {
    /// Defaults changed by environment variables (the launcher and tests set them):
    /// `RINKY_AUTOSLEEP` (minutes, 0 = never), `RINKY_SUSPEND` (0/1),
    /// `RINKY_RTC_WAKE` (seconds, 0 = none), `RINKY_SUSPEND_RETRY` (seconds),
    /// `RINKY_COVER` (0 = ignore the sleep cover).
    pub fn from_env() -> Self {
        let mut c = SleepConfig::default();
        let num = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<u64>().ok());
        if let Some(m) = num("RINKY_AUTOSLEEP") {
            c.auto_sleep = (m > 0).then(|| Duration::from_secs(m * 60));
        }
        if let Some(s) = num("RINKY_SUSPEND") {
            c.suspend = s != 0;
        }
        if let Some(s) = num("RINKY_RTC_WAKE") {
            c.rtc_wake = (s > 0).then(|| Duration::from_secs(s));
        }
        if let Some(s) = num("RINKY_SUSPEND_RETRY") {
            c.retry = Duration::from_secs(s.max(1));
        }
        if let Some(s) = num("RINKY_COVER") {
            c.cover = s != 0;
        }
        c
    }
}

/// Where the controller is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SleepState {
    /// Normal use.
    #[default]
    Awake,
    /// Screensaver shown; the kernel may be suspended between ticks.
    Asleep,
}

/// Why the device goes to sleep or wakes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The power key.
    PowerKey,
    /// The cover (hall sensor).
    Cover,
    /// Inactivity timer.
    Inactivity,
    /// The app asked.
    App,
    /// Resume from a kernel suspend not caused by the RTC alarm.
    Resume,
}

/// How a suspend ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resume {
    /// Time spent in the suspend call.
    pub slept: Duration,
    /// The RTC alarm we set has fired (a maintenance wake, not the user).
    pub by_rtc: bool,
}

/// The work behind the policy.
pub trait SleepPlatform {
    /// Snapshot the panel and draw the screensaver.
    fn show_screensaver(&mut self) -> Result<()>;
    /// Redraw the snapshot (with whatever the app drew meanwhile) with a full refresh.
    fn restore_screen(&mut self) -> Result<()>;
    /// Frontlight off (`false`) or back to the level before (`true`).
    fn set_light(&mut self, on: bool) -> Result<()>;
    /// Suspend the kernel now; returns after resume, or an error if refused.
    fn suspend(&mut self, rtc_wake: Option<Duration>) -> Result<Resume>;
    /// After a resume or wake: re-grab and re-seed input, reopen lost devices.
    fn after_resume(&mut self) -> Result<()>;
    /// Tell powerd to keep the device awake for `d` (`deferSuspend`); best effort.
    fn defer_powerd(&mut self, _d: Duration) {}
    /// The monotonic clock after a suspend returned. (On Linux `Instant` does not
    /// advance while the kernel is suspended, so this is not `now + slept`.)
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The sleep policy. Call its methods from the input loop; it returns the power
/// events to deliver to the app.
#[derive(Clone, Debug)]
pub struct SleepController {
    /// Settings.
    pub config: SleepConfig,
    state: SleepState,
    last_activity: Instant,
    last_change: Option<Instant>,
    deferred_until: Option<Instant>,
    next_suspend: Option<Instant>,
    /// Log lines for the backend to print (drained by [`SleepController::take_log`]).
    log: Vec<String>,
    suspends: u32,
    refusals: u32,
    /// `Suspending` was delivered and no `Resuming` or wake has followed yet.
    announced: bool,
    /// A cover change that came within the debounce window (state, source), applied
    /// when the window ends: the last one wins, so a bouncing magnet does not flash the
    /// screen, and the device never stays asleep with the cover open.
    pending_cover: Option<(bool, String)>,
}

impl SleepController {
    /// A controller that is awake at `now`.
    pub fn new(config: SleepConfig, now: Instant) -> Self {
        SleepController {
            config,
            state: SleepState::Awake,
            last_activity: now,
            last_change: None,
            deferred_until: None,
            next_suspend: None,
            log: Vec::new(),
            suspends: 0,
            refusals: 0,
            announced: false,
            pending_cover: None,
        }
    }

    /// Current state.
    pub fn state(&self) -> SleepState {
        self.state
    }

    /// Successful suspends so far.
    pub fn suspends(&self) -> u32 {
        self.suspends
    }

    /// Refused suspends so far.
    pub fn refusals(&self) -> u32 {
        self.refusals
    }

    /// Drains the log lines.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    fn note(&mut self, s: String) {
        log::info!("sleep: {s}");
        self.log.push(s);
    }

    /// A touch or key: resets the inactivity timer.
    pub fn activity(&mut self, now: Instant) {
        if self.state == SleepState::Awake {
            self.last_activity = now;
        }
    }

    /// Keeps the device from auto-sleeping and from suspending until `now + d`
    /// (`Duration::ZERO` cancels). Also forwarded to powerd.
    pub fn defer_suspend(&mut self, d: Duration, now: Instant, p: &mut impl SleepPlatform) {
        if d.is_zero() {
            self.deferred_until = None;
            self.note("suspend no longer deferred".into());
        } else {
            self.deferred_until = Some(now + d);
            p.defer_powerd(d);
            self.note(format!("suspend deferred for {d:?}"));
        }
    }

    /// Suspend is deferred at `now`.
    pub fn deferred(&self, now: Instant) -> bool {
        self.deferred_until.is_some_and(|t| now < t)
    }

    fn debounced(&self, now: Instant) -> bool {
        self.last_change.is_some_and(|t| now.duration_since(t) < self.config.debounce)
    }

    /// The power key was pressed.
    pub fn power_key(&mut self, now: Instant, p: &mut impl SleepPlatform) -> Vec<PowerEvent> {
        if self.debounced(now) {
            self.note("power key ignored (debounce)".into());
            return Vec::new();
        }
        match self.state {
            SleepState::Awake => self.sleep(Trigger::PowerKey, now, p),
            SleepState::Asleep => self.wake(Trigger::PowerKey, now, p),
        }
    }

    /// The cover closed (`true`) or opened.
    pub fn cover(&mut self, closed: bool, now: Instant, p: &mut impl SleepPlatform) -> Vec<PowerEvent> {
        self.cover_from(closed, "unknown", now, p)
    }

    /// The cover closed (`true`) or opened; `source` names the event for the log
    /// (`uevent`, `lipc-hal`, `powerd(4)`). Every call logs what it decided. Within
    /// [`SleepConfig::debounce`] of the last sleep/wake change the state is kept and
    /// applied by [`SleepController::tick`] when the window ends.
    pub fn cover_from(&mut self, closed: bool, source: &str, now: Instant, p: &mut impl SleepPlatform) -> Vec<PowerEvent> {
        let what = if closed { "closed" } else { "opened" };
        if !self.config.cover {
            self.note(format!("cover {what} ({source}) ignored: sleep cover is off"));
            return Vec::new();
        }
        if self.debounced(now) {
            self.note(format!("cover {what} ({source}) within debounce; applied when it ends"));
            self.pending_cover = Some((closed, source.to_string()));
            return Vec::new();
        }
        self.pending_cover = None;
        match (closed, self.state) {
            (true, SleepState::Awake) => {
                self.note(format!("cover closed ({source}): going to sleep"));
                self.sleep(Trigger::Cover, now, p)
            }
            (false, SleepState::Asleep) => {
                self.note(format!("cover opened ({source}): waking"));
                self.wake(Trigger::Cover, now, p)
            }
            (true, SleepState::Asleep) => {
                self.note(format!("cover closed ({source}) ignored: already asleep"));
                Vec::new()
            }
            (false, SleepState::Awake) => {
                self.note(format!("cover opened ({source}) ignored: already awake"));
                Vec::new()
            }
        }
    }

    fn apply_pending_cover(&mut self, now: Instant, p: &mut impl SleepPlatform) -> Vec<PowerEvent> {
        if self.pending_cover.is_none() || self.debounced(now) {
            return Vec::new();
        }
        let (closed, source) = self.pending_cover.take().unwrap_or_default();
        self.cover_from(closed, &format!("{source}, after debounce"), now, p)
    }

    /// Goes to sleep now.
    pub fn sleep(&mut self, why: Trigger, now: Instant, p: &mut impl SleepPlatform) -> Vec<PowerEvent> {
        if self.state == SleepState::Asleep {
            return Vec::new();
        }
        if self.config.light_off {
            if let Err(e) = p.set_light(false) {
                self.note(format!("frontlight off failed: {e}"));
            }
        }
        match p.show_screensaver() {
            Ok(()) => self.note(format!("screensaver drawn ({why:?})")),
            Err(e) => self.note(format!("screensaver failed: {e}")),
        }
        self.state = SleepState::Asleep;
        self.last_change = Some(now);
        self.next_suspend = self.config.suspend.then(|| now + self.config.suspend_delay);
        vec![PowerEvent::GoingToScreenSaver]
    }

    /// Wakes now.
    pub fn wake(&mut self, why: Trigger, now: Instant, p: &mut impl SleepPlatform) -> Vec<PowerEvent> {
        if self.state == SleepState::Awake {
            return Vec::new();
        }
        if let Err(e) = p.after_resume() {
            self.note(format!("input re-grab failed: {e}"));
        }
        match p.restore_screen() {
            Ok(()) => self.note(format!("awake, screen restored ({why:?})")),
            Err(e) => self.note(format!("screen restore failed: {e}")),
        }
        if self.config.light_off {
            if let Err(e) = p.set_light(true) {
                self.note(format!("frontlight on failed: {e}"));
            }
        }
        self.state = SleepState::Awake;
        self.last_change = Some(now);
        self.last_activity = now;
        self.next_suspend = None;
        self.announced = false;
        vec![PowerEvent::OutOfScreenSaver]
    }

    /// Runs timers: inactivity sleep and suspend attempts. `p.suspend` blocks for the
    /// whole suspend, so `now` is re-read by the caller afterwards.
    pub fn tick(&mut self, now: Instant, p: &mut impl SleepPlatform) -> Vec<PowerEvent> {
        let ev = self.apply_pending_cover(now, p);
        if !ev.is_empty() {
            return ev;
        }
        match self.state {
            SleepState::Awake => {
                let idle = now.duration_since(self.last_activity);
                match self.config.auto_sleep {
                    Some(limit) if idle >= limit && !self.deferred(now) => {
                        self.note(format!("inactive for {idle:?}"));
                        self.sleep(Trigger::Inactivity, now, p)
                    }
                    _ => Vec::new(),
                }
            }
            SleepState::Asleep => {
                let Some(at) = self.next_suspend else { return Vec::new() };
                if now < at {
                    return Vec::new();
                }
                if self.deferred(now) {
                    self.next_suspend = self.deferred_until;
                    return Vec::new();
                }
                // `Suspending` once per attempt series: a refused attempt is retried
                // (every minute on a charger) without telling the app again.
                let mut out = Vec::new();
                if !std::mem::replace(&mut self.announced, true) {
                    out.push(PowerEvent::Suspending);
                }
                self.note(format!("suspend requested (rtc alarm {:?})", self.config.rtc_wake));
                match p.suspend(self.config.rtc_wake) {
                    Ok(r) => {
                        self.suspends += 1;
                        self.announced = false;
                        out.push(PowerEvent::Resuming);
                        self.note(format!("resumed after {:?} ({})", r.slept, if r.by_rtc { "rtc alarm" } else { "wake source" }));
                        let after = p.now().max(now);
                        if r.by_rtc {
                            // Maintenance wake: back to sleep soon unless the user acts.
                            self.next_suspend = Some(after + self.config.rtc_grace);
                        } else {
                            self.last_change = None;
                            out.extend(self.wake(Trigger::Resume, after, p));
                            // The key press that woke us reaches the input loop only after
                            // the wake work (re-grab, GC16 restore that waits for the panel,
                            // light): start the debounce when that work is done, not at the
                            // resume, so that press is ignored however long the restore took.
                            self.last_change = Some(p.now().max(after));
                        }
                    }
                    Err(e) => {
                        self.refusals += 1;
                        // Count the retry from the end of the refused attempt (it can block
                        // for the wakeup_count limit); counted from `now`, a short retry is
                        // already due when the attempt returns and starves the input loop.
                        self.next_suspend = Some(p.now().max(now) + self.config.retry);
                        self.note(format!("suspend refused: {e}; staying on the screensaver, retry in {:?}", self.config.retry));
                    }
                }
                out
            }
        }
    }

    /// When [`SleepController::tick`] must run next.
    pub fn deadline(&self) -> Option<Instant> {
        let pending = self.pending_cover.as_ref().and(self.last_change).map(|t| t + self.config.debounce);
        let own = match self.state {
            SleepState::Awake => self.config.auto_sleep.map(|limit| {
                let t = self.last_activity + limit;
                self.deferred_until.map_or(t, |d| d.max(t))
            }),
            SleepState::Asleep => self.next_suspend,
        };
        match (own, pending) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceError;

    #[derive(Default)]
    struct Fake {
        calls: Vec<String>,
        refuse: bool,
        rtc: bool,
        slept: Duration,
        clock: Option<Instant>,
    }

    impl SleepPlatform for Fake {
        fn show_screensaver(&mut self) -> Result<()> {
            self.calls.push("screensaver".into());
            Ok(())
        }
        fn restore_screen(&mut self) -> Result<()> {
            self.calls.push("restore".into());
            Ok(())
        }
        fn set_light(&mut self, on: bool) -> Result<()> {
            self.calls.push(format!("light {on}"));
            Ok(())
        }
        fn suspend(&mut self, rtc: Option<Duration>) -> Result<Resume> {
            self.calls.push(format!("suspend {rtc:?}"));
            if self.refuse {
                return Err(DeviceError::Io(std::io::Error::from_raw_os_error(16)));
            }
            Ok(Resume { slept: self.slept, by_rtc: self.rtc })
        }
        fn after_resume(&mut self) -> Result<()> {
            self.calls.push("regrab".into());
            Ok(())
        }
        fn defer_powerd(&mut self, d: Duration) {
            self.calls.push(format!("powerd defer {}", d.as_secs()));
        }
        fn now(&self) -> Instant {
            self.clock.unwrap_or_else(Instant::now)
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn power_key_sleeps_suspends_and_wakes() {
        let t0 = Instant::now();
        let mut c = SleepController::new(SleepConfig::default(), t0);
        let mut p = Fake { refuse: false, rtc: false, slept: Duration::from_secs(30), clock: Some(t0 + ms(2000) + Duration::from_secs(30)), ..Default::default() };
        assert_eq!(c.power_key(t0, &mut p), vec![PowerEvent::GoingToScreenSaver]);
        assert_eq!(c.state(), SleepState::Asleep);
        assert_eq!(p.calls, vec!["light false", "screensaver"]);
        assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(2)));
        assert!(c.tick(t0 + ms(1000), &mut p).is_empty(), "not before the delay");
        // The suspend returns after a power-key wake (not the RTC): wake at once.
        let ev = c.tick(t0 + ms(2000), &mut p);
        assert_eq!(ev, vec![PowerEvent::Suspending, PowerEvent::Resuming, PowerEvent::OutOfScreenSaver]);
        assert_eq!(c.state(), SleepState::Awake);
        assert_eq!(&p.calls[2..], ["suspend Some(3600s)", "regrab", "restore", "light true"]);
        // The key press that caused the wake arrives late and is ignored.
        assert!(c.power_key(t0 + ms(2000) + Duration::from_secs(30) + ms(100), &mut p).is_empty());
        assert_eq!(c.suspends(), 1);
    }

    #[test]
    fn refused_suspend_keeps_the_screensaver_and_retries() {
        let t0 = Instant::now();
        let mut c = SleepController::new(SleepConfig::default(), t0);
        let mut p = Fake { refuse: true, ..Default::default() };
        c.power_key(t0, &mut p);
        let ev = c.tick(t0 + Duration::from_secs(2), &mut p);
        assert_eq!(ev, vec![PowerEvent::Suspending]);
        assert_eq!(c.state(), SleepState::Asleep);
        assert_eq!(c.refusals(), 1);
        assert!(c.take_log().iter().any(|l| l.contains("suspend refused")));
        assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(62)));
        // The retry is not announced again; a wake and a new sleep are.
        assert!(c.tick(t0 + Duration::from_secs(62), &mut p).is_empty());
        assert_eq!(c.refusals(), 2);
        // Power key wakes: screen restored, input re-grabbed.
        let ev = c.power_key(t0 + Duration::from_secs(70), &mut p);
        assert_eq!(ev, vec![PowerEvent::OutOfScreenSaver]);
        assert!(p.calls.ends_with(&["regrab".to_string(), "restore".to_string(), "light true".to_string()]));
        c.power_key(t0 + Duration::from_secs(80), &mut p);
        assert_eq!(c.tick(t0 + Duration::from_secs(82), &mut p), vec![PowerEvent::Suspending]);
    }

    #[test]
    fn rtc_wake_goes_back_to_sleep() {
        let t0 = Instant::now();
        let mut c = SleepController::new(SleepConfig::default(), t0);
        // Monotonic time barely moves across a suspend on Linux: 3 s here.
        let mut p = Fake { rtc: true, slept: Duration::from_secs(3600), clock: Some(t0 + Duration::from_secs(3)), ..Default::default() };
        c.power_key(t0, &mut p);
        let ev = c.tick(t0 + Duration::from_secs(2), &mut p);
        assert_eq!(ev, vec![PowerEvent::Suspending, PowerEvent::Resuming]);
        assert_eq!(c.state(), SleepState::Asleep);
        assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(3 + 5)));
    }

    #[test]
    fn inactivity_and_defer() {
        let t0 = Instant::now();
        let cfg = SleepConfig { auto_sleep: Some(Duration::from_secs(60)), ..Default::default() };
        let mut c = SleepController::new(cfg, t0);
        let mut p = Fake::default();
        c.activity(t0 + Duration::from_secs(30));
        assert!(c.tick(t0 + Duration::from_secs(60), &mut p).is_empty());
        assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(90)));
        c.defer_suspend(Duration::from_secs(300), t0 + Duration::from_secs(31), &mut p);
        assert!(p.calls.contains(&"powerd defer 300".to_string()));
        assert!(c.tick(t0 + Duration::from_secs(100), &mut p).is_empty(), "deferred");
        assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(331)));
        // Asleep while deferred: no suspend until the deferral ends.
        c.power_key(t0 + Duration::from_secs(200), &mut p);
        assert!(c.tick(t0 + Duration::from_secs(210), &mut p).is_empty());
        assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(331)));
        c.defer_suspend(Duration::ZERO, t0 + Duration::from_secs(211), &mut p);
        assert!(!c.deferred(t0 + Duration::from_secs(212)));
        // After a wake, inactivity sleeps again.
        c.power_key(t0 + Duration::from_secs(220), &mut p);
        assert_eq!(
            c.tick(t0 + Duration::from_secs(281), &mut p),
            vec![PowerEvent::GoingToScreenSaver],
            "60 s after the wake"
        );
    }

    #[test]
    fn cover_and_debounce() {
        let t0 = Instant::now();
        let mut c = SleepController::new(SleepConfig { suspend: false, ..Default::default() }, t0);
        let mut p = Fake::default();
        assert_eq!(c.cover(true, t0, &mut p), vec![PowerEvent::GoingToScreenSaver]);
        assert!(c.cover(true, t0 + ms(10), &mut p).is_empty());
        assert!(c.power_key(t0 + ms(100), &mut p).is_empty(), "bounce");
        // The repeated close is re-checked when the debounce ends (still asleep: nothing).
        assert_eq!(c.deadline(), Some(t0 + ms(600)));
        assert!(c.tick(t0 + ms(600), &mut p).is_empty());
        assert_eq!(c.deadline(), None, "no suspend configured");
        assert_eq!(c.cover(false, t0 + Duration::from_secs(5), &mut p), vec![PowerEvent::OutOfScreenSaver]);
        let mut off = SleepController::new(SleepConfig { cover: false, ..Default::default() }, t0);
        assert!(off.cover(true, t0, &mut p).is_empty());
    }

    /// One cover close as the device reports it: uevent, then udev's lipc event, then
    /// powerd's `goingToScreenSaver 4`. One sleep; the repeats are logged and ignored.
    #[test]
    fn cover_close_from_three_sources_sleeps_once_and_open_wakes() {
        let t0 = Instant::now();
        let mut c = SleepController::new(SleepConfig::default(), t0);
        let mut p = Fake { clock: Some(t0), ..Default::default() };
        assert_eq!(c.cover_from(true, "uevent", t0, &mut p), vec![PowerEvent::GoingToScreenSaver]);
        assert!(c.cover_from(true, "lipc-hal", t0 + ms(40), &mut p).is_empty());
        assert!(c.cover_from(true, "powerd(4)", t0 + ms(300), &mut p).is_empty());
        assert_eq!(p.calls, vec!["light false", "screensaver"], "one screensaver");
        let log = c.take_log();
        assert!(log.iter().any(|l| l == "cover closed (uevent): going to sleep"), "{log:?}");
        assert!(log.iter().any(|l| l == "cover closed (lipc-hal) within debounce; applied when it ends"), "{log:?}");
        // The debounce ends before the suspend delay: the pending close is a no-op.
        assert!(c.tick(t0 + ms(600), &mut p).is_empty());
        assert!(c.take_log().iter().any(|l| l == "cover closed (powerd(4), after debounce) ignored: already asleep"));
        // Opened later: wake, the same path as the power key.
        let t1 = t0 + ms(1500);
        assert_eq!(c.cover_from(false, "uevent", t1, &mut p), vec![PowerEvent::OutOfScreenSaver]);
        assert_eq!(c.state(), SleepState::Awake);
        assert!(p.calls.ends_with(&["regrab".to_string(), "restore".to_string(), "light true".to_string()]));
        assert!(c.cover_from(false, "lipc-hal", t1 + ms(50), &mut p).is_empty());
        assert!(c.tick(t1 + ms(600), &mut p).is_empty());
        assert!(c.take_log().iter().any(|l| l.contains("cover opened (lipc-hal, after debounce) ignored: already awake")));
    }

    /// Closed, then opened again within the debounce window: the device must not stay
    /// asleep with the cover open; it wakes when the window ends.
    #[test]
    fn cover_opened_during_debounce_wakes_when_it_ends() {
        let t0 = Instant::now();
        let mut c = SleepController::new(SleepConfig::default(), t0);
        let mut p = Fake { clock: Some(t0), ..Default::default() };
        c.cover_from(true, "uevent", t0, &mut p);
        assert!(c.cover_from(false, "uevent", t0 + ms(200), &mut p).is_empty());
        assert_eq!(c.state(), SleepState::Asleep);
        assert_eq!(c.deadline(), Some(t0 + ms(600)), "earlier than the suspend delay");
        assert!(c.tick(t0 + ms(599), &mut p).is_empty());
        assert_eq!(c.tick(t0 + ms(600), &mut p), vec![PowerEvent::OutOfScreenSaver]);
        assert_eq!(c.state(), SleepState::Awake);
        assert_eq!(c.suspends(), 0, "no suspend happened");
        // A bounce close-open-close inside the window: the last state (closed) wins and
        // the screen is not flashed by a wake.
        let t1 = t0 + Duration::from_secs(10);
        c.cover_from(true, "uevent", t1, &mut p);
        c.cover_from(false, "uevent", t1 + ms(100), &mut p);
        c.cover_from(true, "uevent", t1 + ms(200), &mut p);
        let n = p.calls.len();
        assert!(c.tick(t1 + ms(600), &mut p).is_empty());
        assert_eq!(c.state(), SleepState::Asleep);
        assert_eq!(p.calls.len(), n, "no restore/screensaver redraw");
    }

    #[test]
    fn cover_off_is_logged() {
        let t0 = Instant::now();
        let mut c = SleepController::new(SleepConfig { cover: false, ..Default::default() }, t0);
        let mut p = Fake::default();
        assert!(c.cover_from(true, "uevent", t0, &mut p).is_empty());
        assert_eq!(c.take_log(), vec!["cover closed (uevent) ignored: sleep cover is off".to_string()]);
        assert!(p.calls.is_empty());
    }

    #[test]
    fn env_overrides() {
        // Only check parsing helpers indirectly: defaults are sane.
        let d = SleepConfig::default();
        assert_eq!(d.auto_sleep, Some(Duration::from_secs(600)));
        assert!(d.suspend && d.light_off && d.cover);
    }
}
