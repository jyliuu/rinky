//! Kobo services without Nickel: frontlight and warmth through sysfs (or the
//! `ntx_io` ioctl), battery from `power_supply`, sleep through the input loop, Wi-Fi
//! with `wpa_supplicant` ([`super::wifi`]), the system clock.
//!
//! Frontlight (KOReader `powerd.lua`, `sysfs_light.lua`, `device.lua`):
//! - brightness 0-100 to `/sys/class/backlight/mxc_msp430.0/brightness`, else
//!   `ioctl(/dev/ntx_io, 241, level)` (no read-back: the last level set is kept);
//! - warmth 0-10 to the mixer's `color` file, whichever of `aw99703-bl_FL1`
//!   (`/sys/class/leds`), `lm3630a_led` or `tlc5947_bl` (`/sys/class/backlight`)
//!   exists (the Libra 2 had two hardware revisions with different chips). The value is
//!   inverted (10 = coolest) on every model rinky knows.


pub mod wifi;

use crate::battery;
use super::input::Request;
use super::Shared;
use crate::screensaver::Screensaver;
use crate::services::{Battery, BatteryState, Light, Power, Usb, UsbState, Wifi, WifiNetwork, WifiState};
use crate::sleep::SleepState;
use crate::sysfs::SysRoot;
use crate::usb::{DriveModeState, UsbCable};
use crate::wifi::WifiProfile;
use crate::{DeviceError, Result};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

const WHITE: &str = "/sys/class/backlight/mxc_msp430.0/brightness";
const MIXERS: [&str; 3] =
    ["/sys/class/leds/aw99703-bl_FL1/color", "/sys/class/backlight/lm3630a_led/color", "/sys/class/backlight/tlc5947_bl/color"];
/// `ioctl(/dev/ntx_io, 241, level)`: brightness on models without the sysfs file.
const NTX_FRONTLIGHT: libc::c_ulong = 241;
const WARMTH_MAX: u32 = 10;

/// The Kobo services; clones share the backend state.
#[derive(Clone)]
pub struct KoboServices {
    shared: Option<Rc<RefCell<Shared>>>,
    sys: SysRoot,
    /// The warmth mixer's `color` file, when the model has one.
    mixer: Option<String>,
    /// Last brightness set through the ioctl (it cannot be read back).
    ioctl_level: Rc<Cell<Option<u32>>>,
    /// NTP server queried after Wi-Fi connects.
    ntp_server: Option<String>,
}

impl std::fmt::Debug for KoboServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KoboServices").field("mixer", &self.mixer).finish_non_exhaustive()
    }
}

impl KoboServices {
    pub(crate) fn new(shared: Option<Rc<RefCell<Shared>>>, sys: SysRoot, ntp_server: Option<String>) -> Self {
        let warmth = shared.as_ref().is_none_or(|s| s.borrow().screen.model.warmth);
        let mixer = warmth.then(|| MIXERS.iter().find(|p| sys.path(p).exists()).map(|p| p.to_string())).flatten();
        log::info!("light: white {}, warmth mixer {mixer:?}", if sys.path(WHITE).exists() { WHITE } else { "ntx_io 241" });
        KoboServices { shared, sys, mixer, ioctl_level: Rc::new(Cell::new(None)), ntp_server }
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

    fn sleep_enabled(&self) -> bool {
        self.shared.as_ref().is_some_and(|s| s.borrow().sleep_enabled)
    }

    /// Nickel's last frontlight level (`FrontLightLevel=` in its settings), for the
    /// ioctl models before rinky set one.
    fn nickel_level() -> Option<u32> {
        let conf = std::fs::read_to_string("/mnt/onboard/.kobo/Kobo/Kobo eReader.conf").ok()?;
        conf.lines().find_map(|l| l.strip_prefix("FrontLightLevel=")).and_then(|v| v.trim().parse().ok())
    }

    /// Joins the saved network wpa_supplicant picks, gets an address and syncs the
    /// clock, on a thread (after the radio comes on).
    fn connect_in_background(&self) {
        let ntp = self.ntp_server.clone();
        let spawned = std::thread::Builder::new().name("wifi-connect".into()).spawn(move || {
            match wifi::wait_associated(Duration::from_secs(15)).and_then(|()| wifi::obtain_ip()) {
                Ok(()) => {
                    log::info!("wifi: connected");
                    if let Some(server) = ntp {
                        sync_clock(&server);
                    }
                }
                Err(e) => log::info!("wifi: no saved network joined: {e}"),
            }
        });
        if let Err(e) = spawned {
            log::warn!("wifi: no connect thread: {e}");
        }
    }
}

/// Sets the clock from an NTP server when it is off by a second or more.
fn sync_clock(server: &str) {
    match crate::clock::sntp_query(server, Duration::from_secs(10)) {
        Ok(s) if s.offset_ms.abs() >= 1000 => match super::system::set_system_time(s.time) {
            Ok(rtc) => log::info!("ntp: clock set from {server} (off by {} ms); {rtc}", s.offset_ms),
            Err(e) => log::warn!("ntp: {e}"),
        },
        Ok(s) => log::info!("ntp: clock off by {} ms; kept", s.offset_ms),
        Err(e) => log::info!("ntp: {server}: {e}"),
    }
}

impl Light for KoboServices {
    fn brightness_range(&self) -> (u32, u32) {
        (0, 100)
    }

    fn brightness(&self) -> Result<u32> {
        if self.sys.path(WHITE).exists() {
            return Ok(self.sys.read_int(WHITE)?.clamp(0, 100) as u32);
        }
        self.ioctl_level.get().or_else(Self::nickel_level).ok_or(DeviceError::Unsupported("frontlight level unknown"))
    }

    fn set_brightness(&mut self, level: u32) -> Result<()> {
        let level = level.min(100);
        if self.sys.path(WHITE).exists() {
            return Ok(self.sys.write(WHITE, &level.to_string())?);
        }
        wifi::ntx_io(NTX_FRONTLIGHT, level as libc::c_int)?;
        self.ioctl_level.set(Some(level));
        Ok(())
    }

    fn warmth_range(&self) -> (u32, u32) {
        (0, WARMTH_MAX)
    }

    fn warmth(&self) -> Result<u32> {
        let m = self.mixer.as_deref().ok_or(DeviceError::Unsupported("warmth (no natural light on this model)"))?;
        Ok(WARMTH_MAX - (self.sys.read_int(m)?.clamp(0, WARMTH_MAX as i64) as u32))
    }

    fn set_warmth(&mut self, level: u32) -> Result<()> {
        let m = self.mixer.as_deref().ok_or(DeviceError::Unsupported("warmth (no natural light on this model)"))?;
        Ok(self.sys.write(m, &(WARMTH_MAX - level.min(WARMTH_MAX)).to_string())?)
    }
}

impl Battery for KoboServices {
    fn state(&self) -> Result<BatteryState> {
        let b = self.info()?;
        Ok(BatteryState { percent: b.percent, charging: b.charging })
    }

    fn info(&self) -> Result<battery::BatteryInfo> {
        battery::read_sysfs(&self.sys).ok_or(DeviceError::Unsupported("no battery in /sys/class/power_supply"))
    }
}

impl Power for KoboServices {
    fn defer_suspend(&mut self, duration: Duration) -> Result<()> {
        self.request(Request::Defer(duration))
    }

    fn request_screensaver(&mut self) -> Result<()> {
        self.request(Request::Sleep)
    }

    fn state(&self) -> Result<String> {
        Ok(match self.sleep_state() {
            SleepState::Awake => "active".into(),
            SleepState::Asleep => "screenSaver".into(),
        })
    }

    fn suspend(&mut self, wake_after: Option<Duration>) -> Result<()> {
        let mut lines = Vec::new();
        let r = super::system::kobo_suspend(&self.sys, wake_after, &mut |s| lines.push(s));
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
        if !self.sleep_enabled() {
            return Err(DeviceError::Unsupported("sleep needs the input loop"));
        }
        self.request(Request::Sleep)
    }

    fn sleep_state(&self) -> SleepState {
        self.shared.as_ref().map(|s| s.borrow().sleep_state).unwrap_or_default()
    }
}

impl Wifi for KoboServices {
    fn state(&self) -> Result<WifiState> {
        Ok(wifi::state())
    }

    fn set_enabled(&mut self, on: bool) -> Result<()> {
        if on {
            wifi::start()?;
            self.connect_in_background();
            Ok(())
        } else {
            wifi::stop()
        }
    }

    fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
        wifi::scan()
    }

    fn scan_list(&self) -> Result<Vec<WifiNetwork>> {
        wifi::scan_results()
    }

    fn connect(&mut self, essid: &str, password: Option<&str>) -> Result<()> {
        if password.is_some() || !wifi::networks()?.iter().any(|(_, s)| s == essid) {
            wifi::save_network(essid, password)?;
        }
        wifi::connect(essid)?;
        if let Some(server) = self.ntp_server.clone() {
            let _ = std::thread::Builder::new().name("ntp".into()).spawn(move || sync_clock(&server));
        }
        Ok(())
    }

    fn disconnect(&mut self) -> Result<()> {
        wifi::disconnect()
    }

    fn profiles(&self) -> Result<Vec<WifiProfile>> {
        Ok(wifi::networks()?.into_iter().map(|(netid, essid)| WifiProfile { essid, netid, secured: true }).collect())
    }

    fn create_profile(&mut self, essid: &str, password: Option<&str>) -> Result<()> {
        wifi::save_network(essid, password).map(|_| ())
    }

    fn delete_profile(&mut self, essid: &str) -> Result<()> {
        wifi::forget(essid)
    }

    /// A Kobo has no airplane switch apart from the radio: airplane mode is the radio off.
    fn airplane_mode(&self) -> Result<bool> {
        Ok(!wifi::is_up())
    }

    fn set_airplane_mode(&mut self, on: bool) -> Result<()> {
        self.set_enabled(!on)
    }
}

impl Usb for KoboServices {
    fn state(&self) -> Result<UsbState> {
        Ok(if self.cable()?.present() { UsbState::Charging } else { UsbState::Unplugged })
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

    fn drive_mode(&self) -> Result<DriveModeState> {
        Ok(DriveModeState::Off)
    }
}
