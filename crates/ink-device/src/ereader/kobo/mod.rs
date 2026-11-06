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
use super::system::{self, DeviceClock};
use super::{Shared, SharedHandle};
use crate::battery;
use crate::screensaver::Screensaver;
use crate::services::{Battery, BatteryState, Light, Power, Usb, UsbState, Wifi, WifiNetwork, WifiState};
use crate::sleep::{Resume, SleepState};
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

const NICKEL_CONF: &str = "/mnt/onboard/.kobo/Kobo/Kobo eReader.conf";

/// A value from Nickel's settings file (`Key=value` lines).
pub(crate) fn nickel_setting(key: &str) -> Option<String> {
    let conf = std::fs::read_to_string(NICKEL_CONF).ok()?;
    let prefix = format!("{key}=");
    conf.lines().find_map(|l| l.strip_prefix(&prefix)).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// `ioctl(/dev/ntx_io, cmd, arg)`, the argument passed by value (KOReader `ntx_io.lua`).
pub(crate) fn ntx_io(cmd: libc::c_ulong, arg: libc::c_int) -> Result<()> {
    let path = c"/dev/ntx_io";
    // SAFETY: plain open/ioctl/close on a NUL-terminated path; the ioctl takes an int.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let r = libc::ioctl(fd, cmd as _, arg);
        let e = std::io::Error::last_os_error();
        libc::close(fd);
        if r < 0 {
            return Err(DeviceError::Backend(format!("ntx_io {cmd} {arg}: {e}")));
        }
    }
    Ok(())
}

/// Suspends a Kobo as KOReader does (`Kobo:suspend`): Wi-Fi off, `1` to
/// `/sys/power/state-extended` (the NTX drivers prepare for sleep), 2 s, then the
/// kernel suspend without the `wakeup_count` handshake (KOReader leaves it out on
/// Kobo; on the Tolino emulator its write failed with EINVAL every time), then `0` back.
/// (A MediaTek Kobo must not suspend while charging: [`Model::suspend_on_charger`].)
pub fn suspend(sys: &SysRoot, rtc_wake: Option<Duration>, log: &mut dyn FnMut(String)) -> Result<Resume> {
    if wifi::is_up() {
        log("wifi: turning off before suspend".into());
        if let Err(e) = wifi::stop() {
            log(format!("wifi: {e}"));
        }
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
    /// Sets the clock after Wi-Fi connects.
    clock: DeviceClock,
}

impl std::fmt::Debug for KoboServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KoboServices").field("white", &self.white).field("mixer", &self.mixer).finish_non_exhaustive()
    }
}

impl KoboServices {
    pub(crate) fn new(shared: Option<Rc<RefCell<Shared>>>, sys: SysRoot, clock: DeviceClock) -> Self {
        let shared = SharedHandle(shared);
        let warmth = shared.model().is_none_or(|m| m.warmth);
        let mixer = MIXERS.into_iter().find(|p| warmth && sys.path(p).exists());
        let white = sys.path(WHITE).exists();
        log::info!("light: white {}, warmth mixer {mixer:?}", if white { WHITE } else { "ntx_io 241" });
        let nickel_level = nickel_setting("FrontLightLevel").and_then(|v| v.parse().ok());
        KoboServices {
            shared,
            sys,
            white,
            mixer,
            ioctl_level: Rc::new(Cell::new(nickel_level)),
            clock,
        }
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
        ntx_io(NTX_FRONTLIGHT, level as libc::c_int)?;
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
        self.shared.suspend(suspend, &self.sys, wake_after)
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
        Ok(wifi::state())
    }

    /// On: returns at once; the radio comes up, joins a saved network and syncs the
    /// clock on a thread.
    fn set_enabled(&mut self, on: bool) -> Result<()> {
        if on {
            let clock = self.clock.clone();
            wifi::start_in_background(move || clock.sync_in_background());
            Ok(())
        } else {
            wifi::stop()
        }
    }

    fn after_wake(&mut self) {
        // Suspend turns the radio off ([`suspend`]); this matters only when it stayed on.
        if !wifi::is_up() {
            return;
        }
        let r = std::thread::Builder::new().name("wifi-wake".into()).spawn(|| {
            std::thread::sleep(crate::services::WIFI_WAKE_SETTLE);
            let s = wifi::state();
            if s.searching() {
                log::info!("wifi: not connected after wake ({}): scanning", s.state);
                if let Err(e) = wifi::request_scan() {
                    log::warn!("wifi: scan after wake: {e}");
                }
            }
        });
        if let Err(e) = r {
            log::warn!("wifi: cannot start the wake scan: {e}");
        }
    }

    fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
        wifi::scan()
    }

    fn scan_list(&self) -> Result<Vec<WifiNetwork>> {
        wifi::scan_results()
    }

    fn connect(&mut self, essid: &str, password: Option<&str>) -> Result<()> {
        let id = match (password, wifi::network_id(essid)?) {
            (None, Some(id)) => id,
            _ => wifi::save_network(essid, password)?,
        };
        wifi::connect(id)?;
        self.clock.sync_in_background();
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
        Ok(if self.shared.cable(&self.sys).present() { UsbState::Charging } else { UsbState::Unplugged })
    }

    fn cable(&self) -> Result<UsbCable> {
        Ok(self.shared.cable(&self.sys))
    }

    fn drive_mode(&self) -> Result<DriveModeState> {
        Ok(DriveModeState::Off)
    }
}
