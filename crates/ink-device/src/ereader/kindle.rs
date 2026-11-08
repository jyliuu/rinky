//! Light, battery, power, Wi-Fi, USB and clock on the Kindle: lipc properties of
//! powerd, wifid, cmd and volumd, sysfs where it is cheaper, and requests to the
//! input loop for sleep and USB drive mode.

use super::input::Request;
use super::{Shared, SharedHandle};
use crate::battery;
use crate::journal::Entry;
use crate::light::{AmberMode, AmberSchedule, DayTime};
use crate::lipc::Lipc;
use crate::screensaver::Screensaver;
use crate::services::{Battery, BatteryState, Light, Power, Usb, UsbState, Wifi, WifiNetwork, WifiState};
use crate::sleep::SleepState;
use crate::sysfs::SysRoot;
use crate::usb::{DriveModeState, UsbCable};
use crate::netman::{self, link, Cmd, NetBackend, NetHandle, NetQueue, NetStatus, Probe};
use crate::wifi::{self, WifiProfile};
use crate::{DeviceError, Result};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub(super) const POWERD: &str = "com.lab126.powerd";
const WIFID: &str = "com.lab126.wifid";
const CMD: &str = "com.lab126.cmd";

/// Reads powerd's warmth schedule.
pub fn read_amber_schedule(lipc: &Lipc) -> Result<AmberSchedule> {
    let enabled = lipc.get_int(POWERD, "amberSchedEnable")? != 0;
    let mode = AmberMode::parse(&lipc.get_str(POWERD, "amberSchedMode").unwrap_or_default());
    let start = DayTime::parse(&lipc.get_str(POWERD, "amberSchedStartTime")?).unwrap_or(DayTime::hm(20, 0));
    let end = DayTime::parse(&lipc.get_str(POWERD, "amberSchedEndTime")?).unwrap_or(DayTime::hm(8, 0));
    let level = lipc.get_int(POWERD, "schedAmberLevel").unwrap_or(10).clamp(0, 24) as u32;
    Ok(AmberSchedule { enabled, mode, start, end, level, ..Default::default() })
}

/// Scans and returns what was found: wifid reports "scanning" then "idle" (KOReader
/// polls the same way, 20 s max), checked every `poll`.
fn scan_now(lipc: &Lipc, poll: Duration) -> Result<Vec<WifiNetwork>> {
    lipc.set_str(WIFID, "scan", "")?;
    std::thread::sleep(Duration::from_millis(250));
    let end = Instant::now() + Duration::from_secs(20);
    while lipc.get_str(WIFID, "scanState")? != "idle" {
        if Instant::now() >= end {
            return Err(DeviceError::Timeout("Wi-Fi scan".into()));
        }
        std::thread::sleep(poll);
    }
    Ok(wifi::networks_from_scan(&lipc.hash_get(WIFID, "scanList")?))
}

/// Turns the radio on or off: both cmd's radio switch and wifid, as KOReader does.
fn set_radio(lipc: &Lipc, on: bool) -> Result<()> {
    if on {
        lipc.set_int(CMD, "wirelessEnable", 1)?;
        lipc.set_int(WIFID, "enable", 1)
    } else {
        lipc.set_int(WIFID, "enable", 0)?;
        lipc.set_int(CMD, "wirelessEnable", 0)
    }
}

/// The radio is on: wifid enabled and cmd's radio switch on.
fn radio_on(lipc: &Lipc) -> bool {
    lipc.get_int(WIFID, "enable").is_ok_and(|v| v != 0) && lipc.get_int(CMD, "wirelessEnable").is_ok_and(|v| v != 0)
}

/// Saves a wifid profile (`password` `None` = open network) and waits until wifid lists
/// it.
fn create_profile(lipc: &Lipc, essid: &str, password: Option<&str>) -> Result<()> {
    let h = wifi::profile_hash(essid, password)?;
    log::info!("wifi: createProfile {}", crate::lipc::redacted(&h));
    lipc.hash_set(WIFID, "createProfile", &h)?;
    // wifid can answer before profileData lists the new profile (seen on the emulator
    // under load): wait for it, so a list or connect right after sees it.
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        if lipc.hash_get(WIFID, "profileData").is_ok_and(|h| wifi::profiles_from(&h).iter().any(|p| p.essid == essid)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    log::warn!("wifi: profile {essid:?} created but not listed after 5 s");
    Ok(())
}

/// Starts joining `essid` (a profile is created first for a new network or a new
/// password): `cmd ensureConnection wifi:ESSID`, which returns at once.
fn start_join(lipc: &Lipc, essid: &str, password: Option<&str>) -> Result<()> {
    let known = lipc.hash_get(WIFID, "profileData").map(|h| wifi::profiles_from(&h).iter().any(|p| p.essid == essid))?;
    if password.is_some() || !known {
        create_profile(lipc, essid, password)?;
    }
    lipc.set_str(CMD, "ensureConnection", &format!("wifi:{essid}"))
}

/// The Kindle's Wi-Fi interface.
const WLAN: &str = "wlan0";

/// The Kindle's [`NetBackend`] for the network worker: wifid joins and runs DHCP; the
/// link is read from sysfs and `getifaddrs` (connected), with wifid's `currentEssid`
/// for the network's name. With Amazon's framework stopped wifid neither scans nor
/// joins by itself, so the worker does both.
struct KindleNet {
    lipc: Lipc,
    sys: SysRoot,
    /// The radio as last read or set (only rinky switches it while it runs).
    radio: Option<bool>,
}

impl NetBackend for KindleNet {
    fn radio_on(&mut self) -> bool {
        let on = radio_on(&self.lipc);
        self.radio = Some(on);
        on
    }

    fn set_radio(&mut self, on: bool) -> Result<()> {
        self.radio = Some(on);
        set_radio(&self.lipc, on)
    }

    fn probe(&mut self) -> Probe {
        let radio = match self.radio {
            Some(r) => r,
            None => self.radio_on(),
        };
        if !radio {
            return Probe::default();
        }
        let sys = self.sys.path("/sys");
        let state = link::operstate(&sys, WLAN);
        if !link::may_be_associated(state.as_deref()) {
            // sysfs says plainly there is no association: no lipc process (this runs
            // every 10 s while no network is found).
            return Probe { radio, ..Probe::default() };
        }
        let address = if sys.join("class/net").join(WLAN).exists() {
            link::has_address(&sys, WLAN)
        } else {
            // A model whose interface is not wlan0: wifid's word, as before the worker.
            self.lipc.get_str(WIFID, "cmState").is_ok_and(|s| s == "CONNECTED")
        };
        let current = self.lipc.hash_get(WIFID, "currentEssid").ok().and_then(|h| wifi::current_from(&h));
        let signal = address
            .then(|| self.lipc.get_str(WIFID, "signalStrength").ok().and_then(|s| wifi::parse_signal(&s)))
            .flatten()
            .or_else(|| current.as_ref().filter(|c| c.connected).map(|c| c.signal));
        Probe {
            radio,
            address,
            associated: address || state.as_deref() == Some("up") || current.as_ref().is_some_and(|c| c.connected),
            essid: current.map(|c| c.essid).filter(|e| !e.is_empty()),
            signal,
        }
    }

    fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
        scan_now(&self.lipc, Duration::from_millis(250))
    }

    fn join(&mut self, essid: &str, password: Option<&str>) -> Result<()> {
        start_join(&self.lipc, essid, password)
    }
}

/// Reads Wi-Fi state from wifid (3-4 lipc calls).
fn read_wifi_state(lipc: &Lipc) -> Result<WifiState> {
    let state = lipc.get_str(WIFID, "cmState")?;
    let current = lipc.hash_get(WIFID, "currentEssid").ok().and_then(|h| wifi::current_from(&h));
    // currentEssid also names a network wifid only tries to join: a name only when
    // connected.
    let essid = current.as_ref().filter(|c| c.connected && state == "CONNECTED").map(|c| c.essid.clone()).filter(|s| !s.is_empty());
    let signal = lipc
        .get_str(WIFID, "signalStrength")
        .ok()
        .and_then(|s| wifi::parse_signal(&s))
        .or_else(|| current.as_ref().filter(|c| c.connected).map(|c| c.signal));
    let enabled = lipc.get_int(WIFID, "enable").map(|v| v != 0).unwrap_or(false);
    Ok(WifiState { enabled, state, essid, signal })
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Last known values of slow lipc properties (each `lipc-get-prop` is a process,
/// 40-90 ms on the PW12; the control center needs about 15 of them).
#[derive(Clone, Debug, Default)]
struct Known {
    brightness: Option<u32>,
    warmth: Option<u32>,
    schedule: Option<AmberSchedule>,
    wifi: Option<WifiState>,
    airplane: Option<bool>,
}

/// State shared by every clone of [`LipcServices`] (and its threads).
#[derive(Default)]
struct StateCache {
    known: Mutex<Known>,
    /// Reads answer from `known` (see [`Light::set_cached_reads`]).
    cached_reads: AtomicBool,
    /// A background read runs.
    refreshing: AtomicBool,
    /// Finished background reads.
    generation: AtomicU64,
}

/// Newest dragged light values waiting for the writer thread: `[brightness, warmth]`.
#[derive(Default)]
struct LiveWriter {
    pending: Mutex<[Option<u32>; 2]>,
    wake: Condvar,
    /// Held during every light write, so a final write never races a live one.
    write: Mutex<()>,
    started: AtomicBool,
}

/// Shortest time between two live light writes.
const LIVE_WRITE_EVERY: Duration = Duration::from_millis(50);

/// All lipc-backed services share one [`Lipc`] runner and the backend state.
#[derive(Clone)]
pub struct LipcServices {
    /// The runner.
    pub lipc: Lipc,
    shared: SharedHandle,
    sys: SysRoot,
    fl_max: Cell<Option<u32>>,
    cache: Arc<StateCache>,
    live: Arc<LiveWriter>,
    /// The network worker (none without the input loop: `rinky device`).
    net: Option<NetHandle>,
    /// Wi-Fi was on when airplane mode was turned on: leaving it turns Wi-Fi on again.
    wifi_before_airplane: Arc<AtomicBool>,
}

impl LipcServices {
    /// The model has a warmth light (true without the input loop: `rinky device`).
    fn has_warmth(&self) -> bool {
        self.shared.model().is_none_or(|m| m.warmth)
    }

    fn check_warmth(&self) -> Result<()> {
        if self.has_warmth() {
            Ok(())
        } else {
            Err(DeviceError::Unsupported("warmth (no natural light on this model)"))
        }
    }

    /// A brightness to set, clamped to the range and remembered.
    fn new_brightness(&self, level: u32) -> u32 {
        let (lo, hi) = self.brightness_range();
        let level = level.clamp(lo, hi);
        self.remember(|k| k.brightness = Some(level));
        level
    }

    /// A warmth to set, clamped to the range and remembered.
    fn new_warmth(&self, level: u32) -> Result<u32> {
        self.check_warmth()?;
        let (lo, hi) = self.warmth_range();
        let level = level.clamp(lo, hi);
        self.remember(|k| k.warmth = Some(level));
        Ok(level)
    }
}

impl std::fmt::Debug for LipcServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LipcServices").field("lipc", &self.lipc).finish_non_exhaustive()
    }
}

impl Default for LipcServices {
    fn default() -> Self {
        LipcServices::new(Lipc::new(), None, SysRoot::system())
    }
}

impl LipcServices {
    /// Services over `lipc`; `shared` connects them to a running input loop.
    pub(crate) fn new(lipc: Lipc, shared: Option<Rc<RefCell<Shared>>>, sys: SysRoot) -> Self {
        LipcServices { lipc, shared: SharedHandle(shared), sys, fl_max: Cell::new(None), cache: Arc::default(), live: Arc::default(), net: None, wifi_before_airplane: Arc::default() }
    }

    /// Starts the network worker; its events go to `queue` (the input loop's).
    pub(crate) fn with_network(mut self, queue: Arc<NetQueue>) -> Self {
        let backend = KindleNet { lipc: self.lipc.clone(), sys: self.sys.clone(), radio: None };
        self.net = netman::spawn(Box::new(backend), queue);
        self
    }

    /// Sends `cmd` to the network worker; false without one.
    fn net_send(&self, cmd: Cmd) -> bool {
        self.net.as_ref().is_some_and(|n| n.send(cmd))
    }

    /// A slow value: the one in `slot` when cached reads are on and it is known, else
    /// `read` now (without holding the cache lock) and remembered.
    fn cached<T: Clone>(&self, slot: fn(&mut Known) -> &mut Option<T>, read: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.cache.cached_reads.load(Ordering::Relaxed) {
            if let Some(v) = slot(&mut lock(&self.cache.known)).clone() {
                return Ok(v);
            }
        }
        let v = read()?;
        *slot(&mut lock(&self.cache.known)) = Some(v.clone());
        Ok(v)
    }

    fn remember(&self, f: impl FnOnce(&mut Known)) {
        f(&mut lock(&self.cache.known));
    }

    /// Reads every slow value now (on the calling thread) into `cache`.
    fn read_all(lipc: &Lipc, cache: &StateCache) {
        let t = Instant::now();
        let brightness = lipc.get_int(POWERD, "flIntensity").ok().map(|v| v.max(0) as u32);
        let warmth = lipc.get_int(POWERD, "currentAmberLevel").ok().map(|v| v.max(0) as u32);
        let schedule = read_amber_schedule(lipc).ok();
        let wifi = read_wifi_state(lipc).ok();
        let airplane = lipc.get_int(CMD, "wirelessEnable").ok().map(|v| v == 0);
        *lock(&cache.known) = Known { brightness, warmth, schedule, wifi, airplane };
        let n = cache.generation.fetch_add(1, Ordering::Relaxed) + 1;
        log::info!("services: device state read in {} ms (read {n})", t.elapsed().as_millis());
    }

    /// Starts a background read of the slow values after `delay` (none when one runs).
    pub fn refresh_after(&self, delay: Duration) {
        if self.cache.refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        let (lipc, cache) = (self.lipc.clone(), self.cache.clone());
        let r = std::thread::Builder::new().name("state-read".into()).spawn(move || {
            std::thread::sleep(delay);
            Self::read_all(&lipc, &cache);
            cache.refreshing.store(false, Ordering::Release);
        });
        if let Err(e) = r {
            log::warn!("services: cannot start the state reader: {e}");
            self.cache.refreshing.store(false, Ordering::Release);
        }
    }

    /// Hands `level` to the light writer thread (slot 0 brightness, 1 warmth).
    fn write_live(&self, slot: usize, level: u32) -> Result<()> {
        lock(&self.live.pending)[slot] = Some(level);
        self.live.wake.notify_one();
        if self.live.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let (lipc, live) = (self.lipc.clone(), self.live.clone());
        let r = std::thread::Builder::new().name("light-writer".into()).spawn(move || loop {
            let next = {
                let mut p = lock(&live.pending);
                while p.iter().all(Option::is_none) {
                    p = live.wake.wait(p).unwrap_or_else(|e| e.into_inner());
                }
                std::mem::take(&mut *p)
            };
            let t = Instant::now();
            {
                let _w = lock(&live.write);
                for (prop, v) in ["flIntensity", "currentAmberLevel"].iter().zip(next) {
                    let Some(v) = v else { continue };
                    match lipc.set_int(POWERD, prop, v as i64) {
                        Ok(()) => log::debug!("light: live {prop} {v} in {} us", t.elapsed().as_micros()),
                        Err(e) => log::warn!("light: live {prop} {v}: {e}"),
                    }
                }
            }
            // At most one write per LIVE_WRITE_EVERY; newer values replace older ones
            // meanwhile.
            std::thread::sleep(LIVE_WRITE_EVERY.saturating_sub(t.elapsed()));
        });
        if let Err(e) = r {
            self.live.started.store(false, Ordering::Release);
            return Err(DeviceError::Backend(format!("light writer thread: {e}")));
        }
        Ok(())
    }

    /// Drops a waiting live value of `slot` and waits for a live write in progress, so
    /// the caller's write comes last.
    fn before_final_write(&self, slot: usize) -> MutexGuard<'_, ()> {
        lock(&self.live.pending)[slot] = None;
        lock(&self.live.write)
    }

    /// Services without an input loop (selftest, `rinky device`).
    pub fn standalone(lipc: Lipc) -> Self {
        LipcServices::new(lipc, None, SysRoot::system())
    }

    fn request(&self, r: Request) -> Result<()> {
        self.shared.request(r)
    }

    fn journal(&self, e: Entry) {
        self.shared.journal(e)
    }

    fn wait_for(&self, what: &str, limit: Duration, mut done: impl FnMut() -> Result<Option<bool>>) -> Result<()> {
        let end = Instant::now() + limit;
        loop {
            match done() {
                Ok(Some(true)) => return Ok(()),
                Ok(Some(false)) => return Err(DeviceError::Backend(format!("{what} failed"))),
                Ok(None) | Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(500)),
                Ok(None) => return Err(DeviceError::Timeout(format!("{what} after {limit:?}"))),
                Err(e) => return Err(e),
            }
        }
    }
}

impl Light for LipcServices {
    fn brightness_range(&self) -> (u32, u32) {
        if self.fl_max.get().is_none() {
            let m = self.lipc.get_int(POWERD, "flMaxIntensity").ok().filter(|m| *m > 0).unwrap_or(24) as u32;
            self.fl_max.set(Some(m));
        }
        (0, self.fl_max.get().unwrap_or(24))
    }

    fn brightness(&self) -> Result<u32> {
        self.cached(|k| &mut k.brightness, || Ok(self.lipc.get_int(POWERD, "flIntensity")?.max(0) as u32))
    }

    fn set_brightness(&mut self, level: u32) -> Result<()> {
        let level = self.new_brightness(level);
        let _w = self.before_final_write(0);
        self.lipc.set_int(POWERD, "flIntensity", level as i64)
    }

    fn set_brightness_live(&mut self, level: u32) -> Result<()> {
        let level = self.new_brightness(level);
        self.write_live(0, level)
    }

    fn warmth(&self) -> Result<u32> {
        self.check_warmth()?;
        self.cached(|k| &mut k.warmth, || Ok(self.lipc.get_int(POWERD, "currentAmberLevel")?.max(0) as u32))
    }

    fn set_warmth(&mut self, level: u32) -> Result<()> {
        let level = self.new_warmth(level)?;
        {
            let _w = self.before_final_write(1);
            self.lipc.set_int(POWERD, "currentAmberLevel", level as i64)?;
        }
        let _ = self.request(Request::Warmth(level));
        Ok(())
    }

    fn set_warmth_live(&mut self, level: u32) -> Result<()> {
        let level = self.new_warmth(level)?;
        self.write_live(1, level)
    }

    fn amber_schedule(&self) -> Result<AmberSchedule> {
        self.check_warmth()?;
        self.cached(|k| &mut k.schedule, || read_amber_schedule(&self.lipc))
    }

    fn set_cached_reads(&self, on: bool) {
        self.cache.cached_reads.store(on, Ordering::Relaxed);
    }

    fn refresh_state(&self) {
        self.refresh_after(Duration::ZERO);
    }

    fn state_generation(&self) -> u64 {
        self.cache.generation.load(Ordering::Relaxed)
    }

    fn set_amber_schedule(&mut self, s: &AmberSchedule) -> Result<()> {
        self.check_warmth()?;
        self.remember(|k| k.schedule = Some(s.clone()));
        let written = (|| {
            self.lipc.set_str(POWERD, "amberSchedMode", s.mode.as_powerd())?;
            self.lipc.set_str(POWERD, "amberSchedStartTime", &s.start.to_powerd())?;
            self.lipc.set_str(POWERD, "amberSchedEndTime", &s.end.to_powerd())?;
            self.lipc.set_int(POWERD, "schedAmberLevel", s.level.min(24) as i64)?;
            self.lipc.set_int(POWERD, "amberSchedEnable", s.enabled as i64)
        })();
        // Also after a failed write: some properties may have changed, and the input
        // loop keeps the schedule it read until this request.
        let _ = self.request(Request::AmberChanged);
        written
    }
}

impl Battery for LipcServices {
    fn state(&self) -> Result<BatteryState> {
        let i = self.info()?;
        Ok(BatteryState { percent: i.percent, charging: i.charging })
    }

    fn info(&self) -> Result<battery::BatteryInfo> {
        if let Some(b) = battery::read_sysfs(&self.sys) {
            return Ok(b);
        }
        let percent = self.lipc.get_int(POWERD, "battLevel")?.clamp(0, 100) as u8;
        let charging = self.lipc.get_int(POWERD, "isCharging")? != 0;
        Ok(battery::BatteryInfo { percent, charging, source: "powerd".into(), ..Default::default() })
    }
}

impl Power for LipcServices {
    fn defer_suspend(&mut self, duration: Duration) -> Result<()> {
        if self.shared.sleep_enabled() {
            // rinky's own sleep controller suspends while the framework is stopped;
            // it also tells powerd, best effort: with lab126_gui stopped powerd refuses
            // deferSuspend (lipcPropErrInvalidState, seen on 5.19.5), which is no error.
            return self.request(Request::Defer(duration));
        }
        self.lipc.set_int(POWERD, "deferSuspend", duration.as_secs() as i64)
    }

    fn state(&self) -> Result<String> {
        if self.shared.sleep_enabled() {
            return Ok(self.shared.sleep_state().name().into());
        }
        self.lipc.get_str(POWERD, "state")
    }

    fn suspend(&mut self, wake_after: Option<Duration>) -> Result<()> {
        self.shared.suspend(super::system::kernel_suspend, &self.sys, wake_after)
    }

    fn set_screensaver(&mut self, s: Screensaver) -> Result<()> {
        self.shared.set_screensaver(s)
    }

    fn set_auto_sleep(&mut self, after: Option<Duration>) -> Result<()> {
        self.request(Request::AutoSleep(after))
    }

    fn auto_sleep(&self) -> Option<Duration> {
        self.shared.auto_sleep()
    }

    fn sleep_now(&mut self) -> Result<()> {
        self.request(Request::Sleep)
    }

    fn sleep_state(&self) -> SleepState {
        self.shared.sleep_state()
    }
}

impl Wifi for LipcServices {
    fn state(&self) -> Result<WifiState> {
        if let Some(st) = self.net.as_ref().and_then(NetHandle::status) {
            return Ok(st.to_state());
        }
        self.cached(|k| &mut k.wifi, || read_wifi_state(&self.lipc))
    }

    fn status(&self) -> Result<NetStatus> {
        match self.net.as_ref().and_then(NetHandle::status) {
            Some(st) => Ok(st),
            None => Wifi::state(self).map(|s| NetStatus::from_state(&s)),
        }
    }

    fn set_enabled(&mut self, on: bool) -> Result<()> {
        self.remember(|k| (k.wifi, k.airplane) = (None, None));
        if self.net_send(if on { Cmd::Enable } else { Cmd::Disable }) {
            return Ok(());
        }
        set_radio(&self.lipc, on)
    }

    fn start_network(&mut self, restore: bool) {
        self.net_send(Cmd::Boot { restore });
    }

    fn set_awake(&mut self, awake: bool) {
        self.net_send(Cmd::Awake(awake));
    }

    fn link_changed(&mut self, state: &str) {
        self.remember(|k| k.wifi = None);
        self.net_send(Cmd::Link(state.to_string()));
    }

    fn connect_in_background(&mut self, essid: &str, password: Option<&str>) -> Result<bool> {
        if self.net_send(Cmd::Connect { essid: essid.to_string(), password: password.map(str::to_string) }) {
            return Ok(true);
        }
        self.connect(essid, password).map(|()| false)
    }

    fn scan_in_background(&mut self) -> Result<bool> {
        if self.net_send(Cmd::Scan) {
            return Ok(true);
        }
        self.scan().map(|_| false)
    }

    fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
        scan_now(&self.lipc, Duration::from_millis(250))
    }

    fn scan_list(&self) -> Result<Vec<WifiNetwork>> {
        Ok(wifi::networks_from_scan(&self.lipc.hash_get(WIFID, "scanList")?))
    }

    fn connect(&mut self, essid: &str, password: Option<&str>) -> Result<()> {
        start_join(&self.lipc, essid, password)?;
        self.wait_for(&format!("connect to {essid}"), Duration::from_secs(30), || {
            let st = self.lipc.get_str(WIFID, "cmState")?;
            if st != "CONNECTED" {
                return Ok(None);
            }
            let cur = self.lipc.hash_get(WIFID, "currentEssid").ok().and_then(|h| wifi::current_from(&h));
            Ok(cur.filter(|c| c.essid == essid).map(|_| true))
        })
    }

    fn disconnect(&mut self) -> Result<()> {
        let cur = self.lipc.hash_get(WIFID, "currentEssid").ok().and_then(|h| wifi::current_from(&h));
        self.lipc.set_str(WIFID, "cmDisconnect", cur.map(|c| c.essid).as_deref().unwrap_or(""))
    }

    fn profiles(&self) -> Result<Vec<WifiProfile>> {
        Ok(wifi::profiles_from(&self.lipc.hash_get(WIFID, "profileData")?))
    }

    fn create_profile(&mut self, essid: &str, password: Option<&str>) -> Result<()> {
        create_profile(&self.lipc, essid, password)
    }

    fn delete_profile(&mut self, essid: &str) -> Result<()> {
        let p = self
            .profiles()?
            .into_iter()
            .find(|p| p.essid == essid)
            .ok_or_else(|| DeviceError::Backend(format!("no saved profile {essid:?}")))?;
        self.lipc.set_int(WIFID, "deleteProfile", p.netid)
    }

    fn airplane_mode(&self) -> Result<bool> {
        self.cached(|k| &mut k.airplane, || Ok(self.lipc.get_int(CMD, "wirelessEnable")? == 0))
    }

    fn set_airplane_mode(&mut self, on: bool) -> Result<()> {
        self.remember(|k| (k.wifi, k.airplane) = (None, None));
        let before = self.lipc.get_int(CMD, "wirelessEnable");
        if let Ok(v) = before {
            self.journal(Entry::lipc_int(CMD, "wirelessEnable", v));
        }
        if on && before.is_ok_and(|v| v != 0) {
            let radio = self.net.as_ref().and_then(NetHandle::status).is_some_and(|s| s.radio_on());
            self.wifi_before_airplane.store(radio, Ordering::Relaxed);
        }
        self.lipc.set_int(CMD, "wirelessEnable", (!on) as i64)?;
        // The worker keeps the radio as it was told: airplane mode turns Wi-Fi off, and
        // leaving it turns Wi-Fi on only when it was on before (else it stays off).
        if on {
            self.net_send(Cmd::Disable);
        } else if self.wifi_before_airplane.swap(false, Ordering::Relaxed) {
            self.net_send(Cmd::Enable);
        }
        Ok(())
    }
}

impl Usb for LipcServices {
    fn state(&self) -> Result<UsbState> {
        let (cable, drive) = (self.shared.cable(&self.sys), self.shared.drive_state().unwrap_or(DriveModeState::Off));
        Ok(if drive.active() {
            UsbState::Exported
        } else if cable.present() {
            UsbState::Charging
        } else {
            UsbState::Unplugged
        })
    }

    fn set_drive_mode(&mut self, on: bool) -> Result<()> {
        if !self.shared.usb_enabled() {
            return Err(DeviceError::Unsupported("usb drive mode needs the input loop with RINKY_USB != off"));
        }
        self.request(Request::DriveMode(on))
    }

    fn cable(&self) -> Result<UsbCable> {
        Ok(self.shared.cable(&self.sys))
    }

    fn set_charge_only(&mut self, on: bool) -> Result<()> {
        self.request(Request::ChargeOnly(on))
    }
}

