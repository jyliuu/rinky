//! Network state and power hooks for transfers.
//!
//! This crate does not depend on ink-device. The app connects the two:
//!
//! ```ignore
//! // Wi-Fi: ink_device::Wifi::state() -> WifiState { enabled, state, .. }
//! let online = || match device.wifi.state() {
//!     Ok(s) => ink_net::connectivity::wifi_rule(s.enabled, &s.state),
//!     Err(e) => Err(ink_net::NetError::Offline(format!("wifi state: {e}"))),
//! };
//! // Power: ink_device::Power::defer_suspend(&mut self, Duration), behind a Mutex.
//! // defer_error maps powerd's lipcPropErrInvalidState ("no countdown to defer") to
//! // NetError::Unsupported, which the KeepAwake contract treats as "nothing to defer".
//! let power = std::sync::Mutex::new(device.power);
//! let keep = |d| power.lock().unwrap().defer_suspend(d).map_err(|e| ink_net::connectivity::defer_error(&e.to_string()));
//! ```
//!
//! - [`Connectivity`]: is the network usable? Downloads check it before they start and
//!   fail with [`NetError::Offline`] instead of waiting for a DNS timeout.
//! - [`KeepAwake`]: asks powerd not to suspend (`deferSuspend`) while a transfer runs.
//!   powerd accepts `deferSuspend` only during its `readyToSuspend` countdown (measured in
//!   the emulator: in state `active` it answers `lipcPropErrInvalidState`; the binary's
//!   state machine has an `e_defer_suspend` event). So the app calls
//!   [`NetActivity::on_ready_to_suspend`] when ink-device reports
//!   `PowerEvent::ReadyToSuspend`, and [`ActivityGuard`] also renews from the read loop;
//!   a hook answers [`NetError::Unsupported`] when there is no countdown to defer.
//! - [`NetActivity`]: a shared count of running transfers plus their cancel tokens. With
//!   the framework stopped powerd does not suspend for us (`docs/ARCHITECTURE.md`), so
//!   rinky's own sleep path asks [`NetActivity::may_auto_suspend`] before an idle
//!   sleep, and calls [`NetActivity::interrupt_all`] before a power-key sleep: transfers
//!   stop with [`NetError::Cancelled`], keep their `.part` files, and resume with `Range`
//!   after wake.
//!
//! Wi-Fi rule (measured in the emulator, firmware 5.19.5, 2026-10-24): `com.lab126.wifid
//! cmState` is `PENDING` with the radio on but not associated and `CONNECTED` after the
//! join; `com.lab126.cmd wirelessEnable` and `wifid enable` are 1 in both cases.

use crate::{CancelToken, NetError, Result};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Whether the network can be used now.
pub trait Connectivity {
    /// `Ok` when online; [`NetError::Offline`] with a reason otherwise.
    fn check(&self) -> Result<()>;

    /// True when [`Connectivity::check`] passes.
    fn is_online(&self) -> bool {
        self.check().is_ok()
    }
}

impl<F: Fn() -> Result<()>> Connectivity for F {
    fn check(&self) -> Result<()> {
        self()
    }
}

/// Always online (host builds, tests).
#[derive(Clone, Copy, Debug, Default)]
pub struct AlwaysOnline;

impl Connectivity for AlwaysOnline {
    fn check(&self) -> Result<()> {
        Ok(())
    }
}

/// The Kindle rule: online when the radio is enabled and wifid's `cmState` is `CONNECTED`.
pub fn wifi_rule(enabled: bool, cm_state: &str) -> Result<()> {
    if !enabled {
        return Err(NetError::Offline("Wi-Fi is off".into()));
    }
    if cm_state.trim().eq_ignore_ascii_case("CONNECTED") {
        Ok(())
    } else {
        Err(NetError::Offline(format!("Wi-Fi not connected (cmState {})", cm_state.trim())))
    }
}

/// Polls `c` every `every` until it is online, `max` passes ([`NetError::Offline`]) or
/// `cancel` fires ([`NetError::Cancelled`]).
pub fn wait_online(c: &dyn Connectivity, max: Duration, every: Duration, cancel: &CancelToken) -> Result<()> {
    let end = Instant::now() + max;
    loop {
        cancel.check()?;
        match c.check() {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= end => return Err(e),
            Err(_) => crate::sse::sleep_cancellable(every.min(end.saturating_duration_since(Instant::now())), cancel)?,
        }
    }
}

/// Keeps the device awake.
pub trait KeepAwake {
    /// Asks the power manager not to suspend for `d` (powerd `deferSuspend`). Returns
    /// [`NetError::Unsupported`] when there is nothing to defer (powerd is not counting
    /// down to a suspend); callers treat that as success.
    fn defer_suspend(&self, d: Duration) -> Result<()>;
}

impl<F: Fn(Duration) -> Result<()>> KeepAwake for F {
    fn defer_suspend(&self, d: Duration) -> Result<()> {
        self(d)
    }
}

/// Maps the error text of a failed `deferSuspend` (lipc) to the [`KeepAwake`] contract:
/// powerd's `lipcPropErrInvalidState` (no suspend countdown running, state `active`)
/// becomes [`NetError::Unsupported`] ("nothing to defer"), anything else
/// [`NetError::Http`].
pub fn defer_error(message: &str) -> NetError {
    if message.contains("lipcPropErrInvalidState") {
        NetError::Unsupported("powerd is not counting down to a suspend (lipcPropErrInvalidState)".into())
    } else {
        NetError::Http(format!("deferSuspend: {message}"))
    }
}

/// Does nothing (host builds, tests).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoKeepAwake;

impl KeepAwake for NoKeepAwake {
    fn defer_suspend(&self, _d: Duration) -> Result<()> {
        Ok(())
    }
}

/// How long one `deferSuspend` asks for.
pub const DEFER_HOLD: Duration = Duration::from_secs(60);
/// How often the hold is renewed while data flows.
pub const DEFER_RENEW: Duration = Duration::from_secs(20);

#[derive(Debug, Default)]
struct Inner {
    active: AtomicUsize,
    started: AtomicU64,
    tokens: Mutex<Vec<(u64, CancelToken)>>,
}

/// Running transfers, shared by everything that downloads or streams and by the app's
/// sleep logic. Cloning shares the count.
#[derive(Clone, Debug, Default)]
pub struct NetActivity(Arc<Inner>);

impl NetActivity {
    /// A new, idle tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of running transfers.
    pub fn active(&self) -> usize {
        self.0.active.load(Ordering::SeqCst)
    }

    /// True while a transfer runs.
    pub fn busy(&self) -> bool {
        self.active() > 0
    }

    /// Transfers started since creation.
    pub fn started(&self) -> u64 {
        self.0.started.load(Ordering::SeqCst)
    }

    /// False while a transfer runs: the inactivity timer must not suspend then.
    pub fn may_auto_suspend(&self) -> bool {
        !self.busy()
    }

    /// Cancels every running transfer (before a forced sleep). Downloads keep their
    /// `.part` file and resume later. Returns how many were cancelled.
    ///
    /// It cancels the transfers' own [`CancelToken`]s, which stay cancelled: the resume
    /// after wake needs a new token (a fresh [`crate::download::Hooks`]). A transfer
    /// blocked in a read stops at its next chunk or at its read timeout (default 30 s for
    /// downloads), so the power-key path must not wait for transfers to return.
    pub fn interrupt_all(&self) -> usize {
        let tokens = self.0.tokens.lock().unwrap_or_else(|e| e.into_inner());
        for (_, t) in tokens.iter() {
            t.cancel();
        }
        tokens.len()
    }

    /// Call when powerd announces `readyToSuspend`: while a transfer runs, asks `keep` to
    /// defer the suspend by [`DEFER_HOLD`]. Returns whether a deferral was requested and
    /// accepted (`Ok(false)` when idle or when there was nothing to defer).
    pub fn on_ready_to_suspend(&self, keep: &dyn KeepAwake) -> Result<bool> {
        if !self.busy() {
            return Ok(false);
        }
        match keep.defer_suspend(DEFER_HOLD) {
            Ok(()) => Ok(true),
            Err(NetError::Unsupported(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Registers a running transfer; it ends when the guard drops. `keep` is asked for
    /// [`DEFER_HOLD`] now and again every [`DEFER_RENEW`] of [`ActivityGuard::tick`]s.
    pub fn begin<'a>(&self, cancel: &CancelToken, keep: Option<&'a dyn KeepAwake>) -> ActivityGuard<'a> {
        self.begin_at(cancel, keep, Instant::now())
    }

    /// [`NetActivity::begin`] with an explicit clock (tests).
    pub fn begin_at<'a>(&self, cancel: &CancelToken, keep: Option<&'a dyn KeepAwake>, now: Instant) -> ActivityGuard<'a> {
        let id = self.0.started.fetch_add(1, Ordering::SeqCst);
        self.0.active.fetch_add(1, Ordering::SeqCst);
        self.0.tokens.lock().unwrap_or_else(|e| e.into_inner()).push((id, cancel.clone()));
        let mut g = ActivityGuard { activity: self.clone(), id, keep, last_renew: None, hold: DEFER_HOLD, renew_every: DEFER_RENEW, renewals: 0, keep_skipped: 0, keep_errors: 0 };
        g.tick_at(now);
        g
    }
}

/// A running transfer (see [`NetActivity::begin`]).
pub struct ActivityGuard<'a> {
    activity: NetActivity,
    id: u64,
    keep: Option<&'a dyn KeepAwake>,
    last_renew: Option<Instant>,
    /// Requested hold per renewal.
    pub hold: Duration,
    /// Renewal interval.
    pub renew_every: Duration,
    /// `deferSuspend` calls made.
    pub renewals: u32,
    /// Calls answered "nothing to defer" ([`NetError::Unsupported`]).
    pub keep_skipped: u32,
    /// `deferSuspend` calls that failed otherwise (logged, not fatal).
    pub keep_errors: u32,
}

debug_fields!([] ActivityGuard<'_>, "ActivityGuard", |s| "id": s.id, "renewals": s.renewals, "keep_skipped": s.keep_skipped, "keep_errors": s.keep_errors);

impl ActivityGuard<'_> {
    /// Call from the read loop: renews the hold when [`ActivityGuard::renew_every`] passed.
    pub fn tick(&mut self) {
        self.tick_at(Instant::now());
    }

    /// [`ActivityGuard::tick`] with an explicit clock (tests).
    pub fn tick_at(&mut self, now: Instant) {
        let due = self.last_renew.is_none_or(|t| now.saturating_duration_since(t) >= self.renew_every);
        if !due {
            return;
        }
        self.last_renew = Some(now);
        if let Some(k) = self.keep {
            self.renewals += 1;
            match k.defer_suspend(self.hold) {
                Ok(()) => {}
                Err(NetError::Unsupported(_)) => self.keep_skipped += 1,
                Err(e) => {
                    self.keep_errors += 1;
                    log::warn!("deferSuspend failed: {e}");
                }
            }
        }
    }
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.activity.0.active.fetch_sub(1, Ordering::SeqCst);
        self.activity.0.tokens.lock().unwrap_or_else(|e| e.into_inner()).retain(|(id, _)| *id != self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn wifi_rule_matches_measured_states() {
        assert!(wifi_rule(true, "CONNECTED").is_ok());
        assert!(wifi_rule(true, "connected\n").is_ok());
        assert!(matches!(wifi_rule(true, "PENDING"), Err(NetError::Offline(m)) if m.contains("PENDING")));
        assert!(matches!(wifi_rule(true, "NA"), Err(NetError::Offline(_))));
        assert!(matches!(wifi_rule(false, "CONNECTED"), Err(NetError::Offline(m)) if m.contains("off")));
    }

    #[test]
    fn closures_are_hooks() {
        let state = RefCell::new("PENDING");
        let c = || wifi_rule(true, *state.borrow());
        assert!(!c.is_online());
        *state.borrow_mut() = "CONNECTED";
        assert!(c.is_online());
        assert!(AlwaysOnline.is_online());
    }

    #[test]
    fn wait_online_polls_until_connected() {
        let calls = std::cell::Cell::new(0);
        let c = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 3 {
                Ok(())
            } else {
                Err(NetError::Offline("not yet".into()))
            }
        };
        wait_online(&c, Duration::from_secs(5), Duration::from_millis(5), &CancelToken::new()).unwrap();
        assert_eq!(calls.get(), 3);
        let never = || Err(NetError::Offline("never".into()));
        let t = Instant::now();
        assert!(matches!(wait_online(&never, Duration::from_millis(60), Duration::from_millis(10), &CancelToken::new()), Err(NetError::Offline(_))));
        assert!(t.elapsed() < Duration::from_secs(2));
        let token = CancelToken::new();
        token.cancel();
        assert!(matches!(wait_online(&never, Duration::from_secs(5), Duration::from_millis(10), &token), Err(NetError::Cancelled)));
    }

    #[test]
    fn guard_counts_and_renews_on_a_fake_clock() {
        let calls: RefCell<Vec<Duration>> = RefCell::new(Vec::new());
        let keep = |d: Duration| {
            calls.borrow_mut().push(d);
            Ok(())
        };
        let act = NetActivity::new();
        assert!(act.may_auto_suspend());
        let t0 = Instant::now();
        {
            let mut g = act.begin_at(&CancelToken::new(), Some(&keep), t0);
            assert!(act.busy());
            assert!(!act.may_auto_suspend());
            assert_eq!(calls.borrow().len(), 1, "hold requested at start");
            g.tick_at(t0 + Duration::from_secs(5));
            g.tick_at(t0 + Duration::from_secs(19));
            assert_eq!(calls.borrow().len(), 1, "no renewal before 20 s");
            g.tick_at(t0 + Duration::from_secs(20));
            assert_eq!(calls.borrow().len(), 2);
            g.tick_at(t0 + Duration::from_secs(39));
            g.tick_at(t0 + Duration::from_secs(41));
            assert_eq!(calls.borrow().len(), 3);
            assert!(calls.borrow().iter().all(|d| *d == DEFER_HOLD));
            assert_eq!(g.renewals, 3);
            let _second = act.begin_at(&CancelToken::new(), None, t0);
            assert_eq!(act.active(), 2);
        }
        assert_eq!(act.active(), 0);
        assert!(act.may_auto_suspend());
        assert_eq!(act.started(), 2);
    }

    #[test]
    fn keep_awake_failures_are_counted_not_fatal() {
        let keep = |_d: Duration| Err(NetError::Http("lipc failed".into()));
        let act = NetActivity::new();
        let g = act.begin(&CancelToken::new(), Some(&keep));
        assert_eq!(g.keep_errors, 1);
        assert!(act.busy());
    }

    #[test]
    fn ready_to_suspend_is_deferred_only_while_busy() {
        let calls = std::cell::Cell::new(0);
        let keep = |d: Duration| {
            assert_eq!(d, DEFER_HOLD);
            calls.set(calls.get() + 1);
            Ok(())
        };
        let act = NetActivity::new();
        assert!(!act.on_ready_to_suspend(&keep).unwrap(), "idle: let it sleep");
        assert_eq!(calls.get(), 0);
        let g = act.begin(&CancelToken::new(), None);
        assert!(act.on_ready_to_suspend(&keep).unwrap());
        assert_eq!(calls.get(), 1);
        let active_state = |_d: Duration| Err(NetError::Unsupported("powerd state active".into()));
        assert!(!act.on_ready_to_suspend(&active_state).unwrap());
        let broken = |_d: Duration| Err(NetError::Http("lipc missing".into()));
        assert!(act.on_ready_to_suspend(&broken).is_err());
        drop(g);
        assert!(!act.on_ready_to_suspend(&keep).unwrap());
    }

    #[test]
    fn powerd_defer_errors() {
        let measured = "lipc-set-prop -i com.lab126.powerd deferSuspend 60: exit status Some(1), stdout \"\", stderr \"com.lab126.powerd failed to set value for property deferSuspend (0x100 lipcPropErrInvalidState)\"";
        assert!(matches!(defer_error(measured), NetError::Unsupported(_)));
        assert!(matches!(defer_error("lipc-set-prop: not found"), NetError::Http(m) if m.contains("not found")));
    }

    #[test]
    fn nothing_to_defer_is_not_an_error() {
        let keep = |_d: Duration| Err(NetError::Unsupported("no countdown".into()));
        let act = NetActivity::new();
        let t0 = Instant::now();
        let mut g = act.begin_at(&CancelToken::new(), Some(&keep), t0);
        g.tick_at(t0 + DEFER_RENEW);
        assert_eq!((g.renewals, g.keep_skipped, g.keep_errors), (2, 2, 0));
    }

    #[test]
    fn interrupt_all_cancels_running_transfers_only() {
        let act = NetActivity::new();
        let (a, b, done) = (CancelToken::new(), CancelToken::new(), CancelToken::new());
        drop(act.begin(&done, None));
        let _ga = act.begin(&a, None);
        let _gb = act.begin(&b, None);
        assert_eq!(act.interrupt_all(), 2);
        assert!(a.is_cancelled() && b.is_cancelled());
        assert!(!done.is_cancelled());
    }
}
