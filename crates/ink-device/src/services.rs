//! Device services besides screen and input: light, battery, power, Wi-Fi, USB, clock.
//!
//! Every method has a default that returns [`DeviceError::Unsupported`], so a backend
//! implements only what it has. On the Kindle these are thin wrappers over `lipc`
//! properties of `powerd`, `wifid`, `cmd` and `volumd` (see `docs/ARCHITECTURE.md`,
//! "Kindle system services").

use crate::{DeviceError, Result};
use std::time::{Duration, SystemTime};

fn unsupported<T>(what: &'static str) -> Result<T> {
    Err(DeviceError::Unsupported(what))
}

/// Frontlight.
pub trait Light {
    /// Brightness range `(min, max)`; the PW12 uses 0..=24 (`flIntensity`).
    fn brightness_range(&self) -> (u32, u32) {
        (0, 24)
    }
    /// Current brightness.
    fn brightness(&self) -> Result<u32> {
        unsupported("frontlight")
    }
    /// Sets brightness (clamped to the range).
    fn set_brightness(&mut self, _level: u32) -> Result<()> {
        unsupported("frontlight")
    }
    /// Warmth range `(min, max)`; the PW12 uses 0..=24 (`currentAmberLevel`).
    fn warmth_range(&self) -> (u32, u32) {
        (0, 24)
    }
    /// Current warmth.
    fn warmth(&self) -> Result<u32> {
        unsupported("warmth")
    }
    /// Sets warmth.
    fn set_warmth(&mut self, _level: u32) -> Result<()> {
        unsupported("warmth")
    }
    /// The warmth schedule (powerd's `amberSched*` properties on the Kindle).
    fn amber_schedule(&self) -> Result<crate::light::AmberSchedule> {
        unsupported("amber schedule")
    }
    /// Stores the warmth schedule. While the framework is stopped the Kindle backend
    /// also applies it itself (minute timer in the input loop).
    fn set_amber_schedule(&mut self, _s: &crate::light::AmberSchedule) -> Result<()> {
        unsupported("amber schedule")
    }
    /// Sets brightness while a slider is dragged. It may return before the device has
    /// the value: the Kindle backend hands the newest value to a writer thread (an
    /// older value still waiting there is dropped). Default: [`Light::set_brightness`].
    fn set_brightness_live(&mut self, level: u32) -> Result<()> {
        self.set_brightness(level)
    }
    /// [`Light::set_brightness_live`] for the warmth.
    fn set_warmth_live(&mut self, level: u32) -> Result<()> {
        self.set_warmth(level)
    }
    /// While on, reads of slow device state (brightness, warmth, warmth schedule, Wi-Fi
    /// state, airplane mode) answer with the last known values when there are any, so a
    /// panel can draw at once. Off (the default): every read asks the device. Backends
    /// with cheap reads ignore it.
    fn set_cached_reads(&self, _on: bool) {}
    /// Reads the slow device state again on a background thread (see
    /// [`Light::set_cached_reads`]); [`Light::state_generation`] counts finished reads.
    /// Default: nothing.
    fn refresh_state(&self) {}
    /// Number of finished [`Light::refresh_state`] reads.
    fn state_generation(&self) -> u64 {
        0
    }
}

/// Battery state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatteryState {
    /// Charge in percent.
    pub percent: u8,
    /// A charger is connected and charging.
    pub charging: bool,
}

/// Battery.
pub trait Battery {
    /// Current charge and charging state.
    fn state(&self) -> Result<BatteryState> {
        unsupported("battery")
    }
    /// Details (status text, temperature, voltage, charger) where available.
    fn info(&self) -> Result<crate::battery::BatteryInfo> {
        let s = self.state()?;
        Ok(crate::battery::BatteryInfo { percent: s.percent, charging: s.charging, ..Default::default() })
    }
}

/// Power management: suspend, screensaver and wake.
///
/// Power *events* (screensaver, suspend, resume) arrive through
/// [`crate::Input::next_event`] as [`crate::InputEvent::Power`].
pub trait Power {
    /// Keeps powerd from suspending for `duration` (`deferSuspend`), e.g. during a download.
    fn defer_suspend(&mut self, _duration: Duration) -> Result<()> {
        unsupported("defer suspend")
    }
    /// powerd's state name (`active`, `screenSaver`, `readyToSuspend`, ...).
    fn state(&self) -> Result<String> {
        unsupported("power state")
    }
    /// Suspends the kernel now (without powerd), optionally arming the RTC to wake after
    /// `wake_after`. Returns after resume, or with an error when the kernel refuses
    /// (`EBUSY` while a wakeup source such as a USB cable is active).
    /// See `docs/ARCHITECTURE.md` (sleep) before using it: powerd is bypassed.
    fn suspend(&mut self, _wake_after: Option<Duration>) -> Result<()> {
        unsupported("suspend")
    }
    /// What to show while asleep (current page, a PNG, or a cover the app rendered).
    fn set_screensaver(&mut self, _s: crate::screensaver::Screensaver) -> Result<()> {
        unsupported("screensaver")
    }
    /// Inactivity before sleeping (`None` = never).
    fn set_auto_sleep(&mut self, _after: Option<Duration>) -> Result<()> {
        unsupported("auto sleep")
    }
    /// Current inactivity limit.
    fn auto_sleep(&self) -> Option<Duration> {
        None
    }
    /// Sleeps now, as if the power key was pressed (screensaver, then suspend).
    fn sleep_now(&mut self) -> Result<()> {
        unsupported("sleep")
    }
    /// The device layer's sleep state.
    fn sleep_state(&self) -> crate::sleep::SleepState {
        crate::sleep::SleepState::Awake
    }
}

/// Wi-Fi connection state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiState {
    /// The radio is on.
    pub enabled: bool,
    /// Connection manager state (`CONNECTED`, `NA`, ...).
    pub state: String,
    /// Network name when connected.
    pub essid: Option<String>,
    /// Signal strength, 0..=5 bars as wifid reports it.
    pub signal: Option<u8>,
}

/// A network found by a scan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiNetwork {
    /// Network name.
    pub essid: String,
    /// Signal 0..=5.
    pub signal: u8,
    /// Needs a password.
    pub secured: bool,
    /// A saved profile exists.
    pub known: bool,
}

/// Wi-Fi (`com.lab126.wifid`, `com.lab126.cmd`).
pub trait Wifi {
    /// Current state.
    fn state(&self) -> Result<WifiState> {
        unsupported("wifi")
    }
    /// Radio on or off.
    fn set_enabled(&mut self, _on: bool) -> Result<()> {
        unsupported("wifi")
    }
    /// Starts a scan and returns the results.
    fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
        unsupported("wifi scan")
    }
    /// Joins a network, creating a profile with `password` when given.
    fn connect(&mut self, _essid: &str, _password: Option<&str>) -> Result<()> {
        unsupported("wifi connect")
    }
    /// Disconnects.
    fn disconnect(&mut self) -> Result<()> {
        unsupported("wifi disconnect")
    }
    /// The last scan results, without scanning again.
    fn scan_list(&self) -> Result<Vec<WifiNetwork>> {
        unsupported("wifi scan list")
    }
    /// Saved profiles.
    fn profiles(&self) -> Result<Vec<crate::wifi::WifiProfile>> {
        unsupported("wifi profiles")
    }
    /// Saves a profile (`password` `None` = open network) without connecting.
    fn create_profile(&mut self, _essid: &str, _password: Option<&str>) -> Result<()> {
        unsupported("wifi profile")
    }
    /// Deletes a saved profile.
    fn delete_profile(&mut self, _essid: &str) -> Result<()> {
        unsupported("wifi profile")
    }
    /// Airplane mode (all radios off).
    fn airplane_mode(&self) -> Result<bool> {
        unsupported("airplane mode")
    }
    /// Turns airplane mode on or off.
    fn set_airplane_mode(&mut self, _on: bool) -> Result<()> {
        unsupported("airplane mode")
    }
    /// What the network is doing ([`crate::netman`]). Backends with a network worker
    /// answer its last status without reading the device; the others read
    /// [`Wifi::state`].
    fn status(&self) -> Result<crate::netman::NetStatus> {
        self.state().map(|s| crate::netman::NetStatus::from_state(&s))
    }
    /// The app started; `restore`: Wi-Fi was on when it last ran (kept in the settings
    /// from [`crate::netman::NetEvent::WasOn`]). Starts the network worker's work.
    fn start_network(&mut self, _restore: bool) {}
    /// The device left (`true`) or entered (`false`) the screensaver.
    fn set_awake(&mut self, _awake: bool) {}
    /// The platform's connection state changed (wifid's `cmStateChange`).
    fn link_changed(&mut self, _state: &str) {}
    /// Joins a network in the background when the backend can: `Ok(true)` means it
    /// started and [`crate::PowerEvent::Network`] events follow; `Ok(false)` means it
    /// joined already (the default runs [`Wifi::connect`]).
    fn connect_in_background(&mut self, essid: &str, password: Option<&str>) -> Result<bool> {
        self.connect(essid, password).map(|()| false)
    }
    /// Scans in the background when the backend can: `Ok(true)` means a
    /// [`crate::netman::NetEvent::Scanned`] follows; `Ok(false)` means the scan finished
    /// ([`Wifi::scan_list`] has the networks).
    fn scan_in_background(&mut self) -> Result<bool> {
        self.scan().map(|_| false)
    }
}

/// USB state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsbState {
    /// No cable.
    #[default]
    Unplugged,
    /// Cable present, userstore mounted locally (charge only).
    Charging,
    /// Userstore exported to the computer (USB drive mode).
    Exported,
}

/// USB (`com.lab126.volumd`).
pub trait Usb {
    /// Current state.
    fn state(&self) -> Result<UsbState> {
        unsupported("usb")
    }
    /// Exports the userstore to the computer (`true`) or takes it back (`false`).
    fn set_drive_mode(&mut self, _on: bool) -> Result<()> {
        unsupported("usb drive mode")
    }
    /// What the cable is connected to.
    fn cable(&self) -> Result<crate::usb::UsbCable> {
        unsupported("usb cable")
    }
    /// Charge only for the current cable (no export).
    fn set_charge_only(&mut self, _on: bool) -> Result<()> {
        unsupported("usb charge only")
    }
}

/// Wall clock and monotonic time.
pub trait Clock {
    /// Wall-clock time.
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    /// Sets the wall clock.
    fn set_time(&mut self, _t: SystemTime) -> Result<()> {
        unsupported("set time")
    }
    /// Local date and time (timezone from `/etc/localtime` or `TZ`).
    fn local_now(&self) -> crate::clock::DateTime {
        let tz = crate::clock::TimeZone::local();
        tz.at(self.now())
    }
    /// Queries an NTP server (`host[:port]`; `None` = the system's configured one) and
    /// sets the clock when it is off by more than a second.
    fn sync_ntp(&mut self, _server: Option<&str>) -> Result<crate::clock::NtpSample> {
        unsupported("ntp")
    }
}

/// A backend without this service: every call returns [`DeviceError::Unsupported`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Unsupported;

impl Light for Unsupported {}
impl Battery for Unsupported {}
impl Power for Unsupported {}
impl Wifi for Unsupported {}
impl Usb for Unsupported {}

/// The system clock (read-only).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_services_report_errors() {
        let mut u = Unsupported;
        assert!(matches!(u.set_brightness(3), Err(DeviceError::Unsupported("frontlight"))));
        assert!(Battery::state(&u).is_err());
        assert!(u.scan().is_err());
        assert_eq!(u.brightness_range(), (0, 24));
        assert!(SystemClock.now() > SystemTime::UNIX_EPOCH);
    }
}
