//! Kobo services without Nickel: frontlight and warmth through sysfs (or the
//! `ntx_io` ioctl), battery from `power_supply`, sleep through the input loop and
//! [`suspend`], Wi-Fi with `wpa_supplicant` ([`wifi`]).
//!
//! Frontlight (KOReader `powerd.lua`, `sysfs_light.lua`, `device.lua`):
//! - brightness 0-100 to `/sys/class/backlight/mxc_msp430.0/brightness`, else
//!   `ioctl(/dev/ntx_io, 241, level)` (no read-back: the last level set is kept);
//! - warmth 0-10 to the mixer's `color` file, whichever of `aw99703-bl_FL1`
//!   (`/sys/class/leds`), `lm3630a_led` or `tlc5947_bl` (`/sys/class/backlight`)
//!   exists (the Libra 2 had two hardware revisions with different chips). The value is
//!   inverted (10 = coolest) on every model rinky knows.

pub mod wifi;

use super::input::Request;
use super::model::{Epdc, Model};
use super::system::{self, DeviceClock};
use super::{Shared, SharedHandle};
use crate::battery;
use crate::screensaver::Screensaver;
use crate::services::{Battery, BatteryState, Clock, Light, Power, Usb, UsbState, Wifi, WifiNetwork, WifiState};
use crate::sleep::{Resume, SleepState};
use crate::sysfs::SysRoot;
use crate::usb::{DriveModeState, UsbCable};
use crate::wifi::WifiProfile;
use crate::{DeviceError, Result};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

const WHITE: &str = "/sys/class/backlight/mxc_msp430.0/brightness";
const MIXERS: [&str; 3] =
    ["/sys/class/leds/aw99703-bl_FL1/color", "/sys/class/backlight/lm3630a_led/color", "/sys/class/backlight/tlc5947_bl/color"];
/// `ioctl(/dev/ntx_io, 241, level)`: brightness on models without the sysfs file.
const NTX_FRONTLIGHT: libc::c_ulong = 241;
const WARMTH_MAX: u32 = 10;
/// How long a Wi-Fi state read is reused (each one runs `wpa_cli`; the control center
/// and the settings pages read it on every build).
const WIFI_STATE_FOR: Duration = Duration::from_millis(1500);

/// Suspends a Kobo as KOReader does (`Kobo:suspend`): Wi-Fi off, `1` to
/// `/sys/power/state-extended` (the NTX drivers prepare for sleep), 2 s, then the
/// kernel suspend without the `wakeup_count` handshake (KOReader leaves it out on
/// Kobo; on the Tolino emulator its write failed with EINVAL every time), then `0` back.
/// A MediaTek Kobo's kernel hangs when it suspends while charging: refused then.
pub fn suspend(sys: &SysRoot, rtc_wake: Option<Duration>, log: &mut dyn FnMut(String)) -> Result<Resume> {
    if wifi::is_up() {
        log("wifi: turning off before suspend".into());
        if let Err(e) = wifi::stop() {
            log(format!("wifi: {e}"));
        }
    }
    if Model::detect(sys).epdc == Epdc::KoboMtk && battery::charging(sys) {
        return Err(DeviceError::Backend("charging: a MediaTek Kobo does not suspend while charging".into()));
    }
    if let Err(e) = sys.write("/sys/power/state-extended", "1") {
        log(format!("state-extended 1: {e}"));
    }
    std::thread::sleep(Duration::from_secs(2));
    let r = system::suspend_with(sys, rtc_wake, false, log);
    if let Err(e) = sys.write("/sys/power/state-extended", "0") {
        log(format!("state-extended 0: {e}"));
    }
    if r.is_ok() {
        std::thread::sleep(Duration::from_millis(100));
    }
    r
}

/// The Kobo services; clones share the backend state.
#[derive(Clone)]
pub struct KoboServices {
    shared: SharedHandle,
    sys: SysRoot,
    /// The brightness file exists (else the `ntx_io` ioctl).
    white: bool,
    /// The warmth mixer's `color` file, when the model has one.
    mixer: Option<&'static str>,
    /// Last brightness set through the ioctl (it cannot be read back); at first Nickel's.
    ioctl_level: Rc<Cell<Option<u32>>>,
    /// The last Wi-Fi state read and when.
    wifi_state: Rc<RefCell<Option<(Instant, WifiState)>>>,
    /// Sets the clock after Wi-Fi connects.
    clock: DeviceClock,
}

impl std::fmt::Debug for KoboServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KoboServices").field("white", &self.white).field("mixer", &self.mixer).finish_non_exhaustive()
    }
}

impl KoboServices {
    pub(crate) fn new(shared: Option<Rc<RefCell<Shared>>>, sys: SysRoot, ntp_server: Option<String>) -> Self {
        let shared = SharedHandle(shared);
        let warmth = shared.model().is_none_or(|m| m.warmth);
        let mixer = MIXERS.into_iter().find(|p| warmth && sys.path(p).exists());
        let white = sys.path(WHITE).exists();
        log::info!("light: white {}, warmth mixer {mixer:?}", if white { WHITE } else { "ntx_io 241" });
        let nickel_level = wifi::nickel_setting("FrontLightLevel").and_then(|v| v.parse().ok());
        KoboServices {
            shared,
            sys,
            white,
            mixer,
            ioctl_level: Rc::new(Cell::new(nickel_level)),
            wifi_state: Rc::default(),
            clock: DeviceClock { lipc: None, ntp_server },
        }
    }

    /// Turns the radio on and joins the saved network wpa_supplicant picks, gets an
    /// address and syncs the clock, on a thread: the radio takes seconds to come up.
    fn start_wifi_in_background(&self) {
        let mut clock = self.clock.clone();
        let spawned = std::thread::Builder::new().name("wifi-on".into()).spawn(move || {
            let joined = wifi::start()
                .and_then(|()| wifi::wait_associated(Duration::from_secs(15)))
                .and_then(|()| wifi::obtain_ip());
            match joined {
                Ok(()) => {
                    log::info!("wifi: connected");
                    sync_clock(&mut clock);
                }
                Err(e) => log::info!("wifi: no saved network joined: {e}"),
            }
        });
        if let Err(e) = spawned {
            log::warn!("wifi: no thread: {e}");
        }
    }

    fn forget_wifi_state(&self) {
        self.wifi_state.borrow_mut().take();
    }
}

/// Sets the clock from NTP (when it is off by a second or more), logging the outcome.
fn sync_clock(clock: &mut DeviceClock) {
    if clock.ntp_server.is_none() {
        return;
    }
    match clock.sync_ntp(None) {
        Ok(s) => log::info!("ntp: {} offset {} ms", s.server, s.offset_ms),
        Err(e) => log::info!("ntp: {e}"),
    }
}

impl Light for KoboServices {
    fn brightness_range(&self) -> (u32, u32) {
        (0, 100)
    }

    fn brightness(&self) -> Result<u32> {
        if self.white {
            return Ok(self.sys.read_int(WHITE)?.clamp(0, 100) as u32);
        }
        self.ioctl_level.get().ok_or(DeviceError::Unsupported("frontlight level unknown"))
    }

    fn set_brightness(&mut self, level: u32) -> Result<()> {
        let level = level.min(100);
        if self.white {
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
        let m = self.mixer.ok_or(DeviceError::Unsupported("warmth (no natural light on this model)"))?;
        Ok(WARMTH_MAX - (self.sys.read_int(m)?.clamp(0, WARMTH_MAX as i64) as u32))
    }

    fn set_warmth(&mut self, level: u32) -> Result<()> {
        let m = self.mixer.ok_or(DeviceError::Unsupported("warmth (no natural light on this model)"))?;
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
        self.shared.request(Request::Defer(duration))
    }

    fn request_screensaver(&mut self) -> Result<()> {
        self.shared.request(Request::Sleep)
    }

    fn state(&self) -> Result<String> {
        Ok(self.shared.sleep_state().name().into())
    }

    fn suspend(&mut self, wake_after: Option<Duration>) -> Result<()> {
        super::logged_suspend(suspend, &self.sys, wake_after)
    }

    fn set_screensaver(&mut self, s: Screensaver) -> Result<()> {
        self.shared.set_screensaver(s)
    }

    fn set_auto_sleep(&mut self, after: Option<Duration>) -> Result<()> {
        self.shared.request(Request::AutoSleep(after))
    }

    fn auto_sleep(&self) -> Option<Duration> {
        self.shared.auto_sleep()
    }

    fn sleep_now(&mut self) -> Result<()> {
        if !self.shared.sleep_enabled() {
            return Err(DeviceError::Unsupported("sleep needs the input loop"));
        }
        self.shared.request(Request::Sleep)
    }

    fn sleep_state(&self) -> SleepState {
        self.shared.sleep_state()
    }
}

impl Wifi for KoboServices {
    fn state(&self) -> Result<WifiState> {
        let mut cached = self.wifi_state.borrow_mut();
        if let Some((_, s)) = cached.as_ref().filter(|(at, _)| at.elapsed() < WIFI_STATE_FOR) {
            return Ok(s.clone());
        }
        let s = wifi::state();
        *cached = Some((Instant::now(), s.clone()));
        Ok(s)
    }

    /// On: returns at once; the radio comes up and joins a saved network on a thread.
    fn set_enabled(&mut self, on: bool) -> Result<()> {
        self.forget_wifi_state();
        if on {
            self.start_wifi_in_background();
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
        self.forget_wifi_state();
        let id = match (password, wifi::network_id(essid)?) {
            (None, Some(id)) => id,
            _ => wifi::save_network(essid, password)?,
        };
        wifi::connect(id)?;
        sync_clock(&mut self.clock);
        Ok(())
    }

    fn disconnect(&mut self) -> Result<()> {
        self.forget_wifi_state();
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
        Ok(if self.shared.cable(&self.sys).present() { UsbState::Charging } else { UsbState::Unplugged })
    }

    fn cable(&self) -> Result<UsbCable> {
        Ok(self.shared.cable(&self.sys))
    }

    fn drive_mode(&self) -> Result<DriveModeState> {
        Ok(DriveModeState::Off)
    }
}
