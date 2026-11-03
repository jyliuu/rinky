//! Light, battery, power, Wi-Fi, USB and clock on the Kindle: lipc properties of
//! powerd, wifid, cmd and volumd, sysfs where it is cheaper, and requests to the
//! input loop for sleep and USB drive mode.

use super::input::Request;
use super::Shared;
use crate::battery;
use crate::clock::NtpSample;
use crate::journal::Entry;
use crate::light::{AmberMode, AmberSchedule, DayTime};
use crate::lipc::Lipc;
use crate::screensaver::Screensaver;
use crate::services::{Battery, BatteryState, Clock, Light, Power, Usb, UsbState, Wifi, WifiNetwork, WifiState};
use crate::sleep::SleepState;
use crate::sysfs::SysRoot;
use crate::usb::{DriveModeState, UsbCable};
use crate::wifi::{self, WifiProfile};
use crate::{DeviceError, Result};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

const POWERD: &str = "com.lab126.powerd";
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

/// Reads Wi-Fi state from wifid (3-4 lipc calls).
fn read_wifi_state(lipc: &Lipc) -> Result<WifiState> {
    let state = lipc.get_str(WIFID, "cmState")?;
    let current = lipc.hash_get(WIFID, "currentEssid").ok().and_then(|h| wifi::current_from(&h));
    let essid = current.as_ref().map(|c| c.essid.clone()).filter(|s| !s.is_empty());
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
    shared: Option<Rc<RefCell<Shared>>>,
    sys: SysRoot,
    fl_max: Cell<Option<u32>>,
    cache: Arc<StateCache>,
    live: Arc<LiveWriter>,
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
        LipcServices { lipc, shared, sys, fl_max: Cell::new(None), cache: Arc::default(), live: Arc::default() }
    }

    /// A cached value when cached reads are on.
    fn known<T>(&self, f: impl FnOnce(&Known) -> Option<T>) -> Option<T> {
        if !self.cache.cached_reads.load(Ordering::Relaxed) {
            return None;
        }
        f(&lock(&self.cache.known))
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
        match &self.shared {
            Some(s) => {
                s.borrow_mut().requests.push_back(r);
                Ok(())
            }
            None => Err(DeviceError::Unsupported("needs the running input loop")),
        }
    }

    fn journal(&self, e: Entry) {
        if let Some(s) = &self.shared {
            let _ = s.borrow().journal.record(e);
        }
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
        if let Some(v) = self.known(|k| k.brightness) {
            return Ok(v);
        }
        let v = self.lipc.get_int(POWERD, "flIntensity")?.max(0) as u32;
        self.remember(|k| k.brightness = Some(v));
        Ok(v)
    }

    fn set_brightness(&mut self, level: u32) -> Result<()> {
        let (lo, hi) = self.brightness_range();
        let level = level.clamp(lo, hi);
        self.remember(|k| k.brightness = Some(level));
        let _w = self.before_final_write(0);
        self.lipc.set_int(POWERD, "flIntensity", level as i64)
    }

    fn set_brightness_live(&mut self, level: u32) -> Result<()> {
        let (lo, hi) = self.brightness_range();
        let level = level.clamp(lo, hi);
        self.remember(|k| k.brightness = Some(level));
        self.write_live(0, level)
    }

    fn warmth(&self) -> Result<u32> {
        if let Some(v) = self.known(|k| k.warmth) {
            return Ok(v);
        }
        let v = self.lipc.get_int(POWERD, "currentAmberLevel")?.max(0) as u32;
        self.remember(|k| k.warmth = Some(v));
        Ok(v)
    }

    fn set_warmth(&mut self, level: u32) -> Result<()> {
        let (lo, hi) = self.warmth_range();
        let level = level.clamp(lo, hi);
        self.remember(|k| k.warmth = Some(level));
        {
            let _w = self.before_final_write(1);
            self.lipc.set_int(POWERD, "currentAmberLevel", level as i64)?;
        }
        let _ = self.request(Request::Warmth(level));
        Ok(())
    }

    fn set_warmth_live(&mut self, level: u32) -> Result<()> {
        let (lo, hi) = self.warmth_range();
        let level = level.clamp(lo, hi);
        self.remember(|k| k.warmth = Some(level));
        self.write_live(1, level)
    }

    fn amber_schedule(&self) -> Result<AmberSchedule> {
        if let Some(s) = self.known(|k| k.schedule.clone()) {
            return Ok(s);
        }
        let s = read_amber_schedule(&self.lipc)?;
        self.remember(|k| k.schedule = Some(s.clone()));
        Ok(s)
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
        self.remember(|k| k.schedule = Some(s.clone()));
        self.lipc.set_str(POWERD, "amberSchedMode", s.mode.as_powerd())?;
        self.lipc.set_str(POWERD, "amberSchedStartTime", &s.start.to_powerd())?;
        self.lipc.set_str(POWERD, "amberSchedEndTime", &s.end.to_powerd())?;
        self.lipc.set_int(POWERD, "schedAmberLevel", s.level.min(24) as i64)?;
        self.lipc.set_int(POWERD, "amberSchedEnable", s.enabled as i64)?;
        let _ = self.request(Request::AmberChanged);
        Ok(())
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
        if self.shared.as_ref().is_some_and(|s| s.borrow().sleep_enabled) {
            // rinky's own sleep controller suspends while the framework is stopped;
            // it also tells powerd, best effort: with lab126_gui stopped powerd refuses
            // deferSuspend (lipcPropErrInvalidState, seen on 5.19.5), which is no error.
            return self.request(Request::Defer(duration));
        }
        self.lipc.set_int(POWERD, "deferSuspend", duration.as_secs() as i64)
    }

    fn prevent_screensaver(&mut self, prevent: bool) -> Result<()> {
        if let Ok(v) = self.lipc.get_int(POWERD, "preventScreenSaver") {
            self.journal(Entry::LipcInt { publisher: POWERD.into(), prop: "preventScreenSaver".into(), value: v });
        }
        self.lipc.set_int(POWERD, "preventScreenSaver", prevent as i64)
    }

    fn request_screensaver(&mut self) -> Result<()> {
        if self.shared.as_ref().is_some_and(|s| s.borrow().sleep_enabled) {
            return self.request(Request::Sleep);
        }
        self.lipc.set_int(POWERD, "powerButton", 1)
    }

    fn state(&self) -> Result<String> {
        match &self.shared {
            Some(s) if s.borrow().sleep_enabled => Ok(match s.borrow().sleep_state {
                SleepState::Awake => "active".into(),
                SleepState::Asleep => "screenSaver".into(),
            }),
            _ => self.lipc.get_str(POWERD, "state"),
        }
    }

    fn rtc_wakeup(&mut self, after: Duration) -> Result<()> {
        self.lipc.set_int(POWERD, "rtcWakeup", after.as_secs() as i64)
    }

    fn suspend(&mut self, wake_after: Option<Duration>) -> Result<()> {
        let mut lines = Vec::new();
        let r = super::system::kernel_suspend(&self.sys, wake_after, &mut |s| lines.push(s));
        for l in lines {
            log::info!("suspend: {l}");
        }
        r.map(|_| ())
    }

    fn set_screensaver(&mut self, s: Screensaver) -> Result<()> {
        match &self.shared {
            Some(sh) => {
                log::info!("screensaver: {}", s.describe());
                sh.borrow_mut().screensaver = s;
                Ok(())
            }
            None => Err(DeviceError::Unsupported("screensaver needs the running input loop")),
        }
    }

    fn set_auto_sleep(&mut self, after: Option<Duration>) -> Result<()> {
        self.request(Request::AutoSleep(after))
    }

    fn auto_sleep(&self) -> Option<Duration> {
        self.shared.as_ref().and_then(|s| s.borrow().auto_sleep)
    }

    fn sleep_now(&mut self) -> Result<()> {
        self.request(Request::Sleep)
    }

    fn sleep_state(&self) -> SleepState {
        self.shared.as_ref().map(|s| s.borrow().sleep_state).unwrap_or_default()
    }
}

impl Wifi for LipcServices {
    fn state(&self) -> Result<WifiState> {
        if let Some(w) = self.known(|k| k.wifi.clone()) {
            return Ok(w);
        }
        let w = read_wifi_state(&self.lipc)?;
        self.remember(|k| k.wifi = Some(w.clone()));
        Ok(w)
    }

    fn set_enabled(&mut self, on: bool) -> Result<()> {
        self.remember(|k| (k.wifi, k.airplane) = (None, None));
        // As KOReader: both cmd's radio switch and wifid.
        if on {
            self.lipc.set_int(CMD, "wirelessEnable", 1)?;
            self.lipc.set_int(WIFID, "enable", 1)
        } else {
            self.lipc.set_int(WIFID, "enable", 0)?;
            self.lipc.set_int(CMD, "wirelessEnable", 0)
        }
    }

    fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
        self.lipc.set_str(WIFID, "scan", "")?;
        // wifid reports "scanning" then "idle" (KOReader polls the same way, 20 s max).
        std::thread::sleep(Duration::from_millis(250));
        self.wait_for("Wi-Fi scan", Duration::from_secs(20), || {
            Ok((self.lipc.get_str(WIFID, "scanState")? == "idle").then_some(true))
        })?;
        self.scan_list()
    }

    fn scan_list(&self) -> Result<Vec<WifiNetwork>> {
        Ok(wifi::networks_from_scan(&self.lipc.hash_get(WIFID, "scanList")?))
    }

    fn connect(&mut self, essid: &str, password: Option<&str>) -> Result<()> {
        let known = self.profiles()?.iter().any(|p| p.essid == essid);
        if password.is_some() || !known {
            self.create_profile(essid, password)?;
        }
        self.lipc.set_str(CMD, "ensureConnection", &format!("wifi:{essid}"))?;
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
        let h = wifi::profile_hash(essid, password)?;
        log::info!("wifi: createProfile {}", crate::lipc::redacted(&h));
        self.lipc.hash_set(WIFID, "createProfile", &h)?;
        // wifid can answer before profileData lists the new profile (seen on the
        // emulator under load): wait for it, so a list or connect right after sees it.
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            if self.profiles().is_ok_and(|ps| ps.iter().any(|p| p.essid == essid)) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        log::warn!("wifi: profile {essid:?} created but not listed after 5 s");
        Ok(())
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
        if let Some(a) = self.known(|k| k.airplane) {
            return Ok(a);
        }
        let a = self.lipc.get_int(CMD, "wirelessEnable")? == 0;
        self.remember(|k| k.airplane = Some(a));
        Ok(a)
    }

    fn set_airplane_mode(&mut self, on: bool) -> Result<()> {
        self.remember(|k| (k.wifi, k.airplane) = (None, None));
        if let Ok(v) = self.lipc.get_int(CMD, "wirelessEnable") {
            self.journal(Entry::LipcInt { publisher: CMD.into(), prop: "wirelessEnable".into(), value: v });
        }
        self.lipc.set_int(CMD, "wirelessEnable", (!on) as i64)
    }
}

impl Usb for LipcServices {
    fn state(&self) -> Result<UsbState> {
        let (cable, drive) = match &self.shared {
            Some(s) => {
                let s = s.borrow();
                (s.cable, s.drive_state)
            }
            None => {
                let udc = crate::usb::read_udc(&self.sys).map(|(_, s)| s);
                (crate::usb::cable_from(udc.as_deref(), battery::external_power(&self.sys), None), DriveModeState::Off)
            }
        };
        Ok(if drive.active() {
            UsbState::Exported
        } else if cable.present() {
            UsbState::Charging
        } else {
            UsbState::Unplugged
        })
    }

    fn set_drive_mode(&mut self, on: bool) -> Result<()> {
        match &self.shared {
            Some(s) if s.borrow().usb_enabled => self.request(Request::DriveMode(on)),
            _ => Err(DeviceError::Unsupported("usb drive mode needs the input loop with RINKY_USB != off")),
        }
    }

    fn cable(&self) -> Result<UsbCable> {
        match &self.shared {
            Some(s) => Ok(s.borrow().cable),
            None => {
                let udc = crate::usb::read_udc(&self.sys).map(|(_, s)| s);
                Ok(crate::usb::cable_from(udc.as_deref(), battery::external_power(&self.sys), None))
            }
        }
    }

    fn set_charge_only(&mut self, on: bool) -> Result<()> {
        self.request(Request::ChargeOnly(on))
    }

    fn drive_mode(&self) -> Result<DriveModeState> {
        match &self.shared {
            Some(s) => Ok(s.borrow().drive_state),
            None => Ok(DriveModeState::from_volumd(self.lipc.get_int("com.lab126.volumd", "driveModeState")?)),
        }
    }
}

/// The system clock, settable, with NTP.
#[derive(Clone, Debug, Default)]
pub struct KindleClock {
    /// lipc runner (for the `timeChanged` event).
    pub lipc: Lipc,
    /// Default NTP server.
    pub ntp_server: Option<String>,
}

impl Clock for KindleClock {
    fn set_time(&mut self, t: SystemTime) -> Result<()> {
        let before = SystemTime::now();
        let rtc = super::system::set_system_time(t)?;
        log::info!("clock set; {rtc}");
        let delta = match t.duration_since(before) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        };
        let _ = self.lipc.send_event("com.lab126.time", "timeChanged", &[crate::lipc::LipcValue::Int(delta)]);
        Ok(())
    }

    fn sync_ntp(&mut self, server: Option<&str>) -> Result<NtpSample> {
        let server = server
            .map(str::to_string)
            .or_else(|| self.ntp_server.clone())
            .ok_or(DeviceError::Unsupported("no NTP server configured"))?;
        let s = crate::clock::sntp_query(&server, Duration::from_secs(10))?;
        if s.offset_ms.abs() >= 1000 {
            self.set_time(s.time)?;
        }
        Ok(s)
    }
}
