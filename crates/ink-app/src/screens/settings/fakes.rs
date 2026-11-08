//! Host fakes of the device services the settings screen uses (light, battery, power,
//! Wi-Fi, clock). Headless and sim devices report every service as unsupported; tests
//! and host screenshots put these in their place with [`install`].
//!
//! All fakes share one [`FakeState`], so a test can read what the screen did.

use ink_device::screensaver::Screensaver;
use ink_device::wifi::WifiProfile;
use ink_device::{
    Battery, BatteryState, Clock, Device, DeviceError, Light, Power, Result, Usb, Wifi, WifiNetwork, WifiState,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// What the fakes hold; tests read and change it.
#[derive(Clone, Debug)]
pub struct FakeState {
    /// Frontlight level.
    pub brightness: u32,
    /// Warmth level.
    pub warmth: u32,
    /// Battery.
    pub battery: BatteryState,
    /// Auto-sleep timeout last set.
    pub auto_sleep: Option<Duration>,
    /// Screensaver last set.
    pub screensaver: Option<Screensaver>,
    /// Radio on.
    pub wifi_on: bool,
    /// Airplane mode.
    pub airplane: bool,
    /// Connected network.
    pub connected: Option<String>,
    /// What a scan finds.
    pub networks: Vec<WifiNetwork>,
    /// Saved profiles.
    pub profiles: Vec<WifiProfile>,
    /// The password a secured network accepts (any password when `None`).
    pub password: Option<String>,
    /// Saved networks whose saved password is wrong: a connect without a password
    /// fails, one with the right password succeeds and fixes the profile.
    pub stale: Vec<String>,
    /// Every connect call: (essid, password).
    pub connects: Vec<(String, Option<String>)>,
    /// NTP syncs requested.
    pub ntp_syncs: u32,
    /// Joins and scans "run in the background": they happen at once, but the screen is
    /// told to wait for network events (a script's `net` steps).
    pub background: bool,
}

impl Default for FakeState {
    fn default() -> Self {
        let net = |essid: &str, signal, secured, known| WifiNetwork { essid: essid.into(), signal, secured, known };
        FakeState {
            brightness: 12,
            warmth: 4,
            battery: BatteryState { percent: 81, charging: false },
            auto_sleep: None,
            screensaver: None,
            wifi_on: true,
            airplane: false,
            connected: None,
            networks: vec![net("Home", 5, true, true), net("Cafe", 3, false, false), net("Office", 2, true, false)],
            profiles: vec![WifiProfile { essid: "Home".into(), netid: 1, secured: true }],
            password: Some("secret123".into()),
            stale: Vec::new(),
            connects: Vec::new(),
            ntp_syncs: 0,
            background: false,
        }
    }
}

/// One fake implementing the services; clones share the state.
#[derive(Clone, Debug, Default)]
pub struct Fake(pub Rc<RefCell<FakeState>>);

/// Replaces the light, battery, power, Wi-Fi, USB and clock services of `dev` with fakes
/// sharing one state, and returns that state.
pub fn install(dev: &mut Device) -> Rc<RefCell<FakeState>> {
    let f = Fake::default();
    dev.light = Box::new(f.clone());
    dev.battery = Box::new(f.clone());
    dev.power = Box::new(f.clone());
    dev.wifi = Box::new(f.clone());
    dev.usb = Box::new(f.clone());
    dev.clock = Box::new(f.clone());
    f.0
}

impl Light for Fake {
    fn brightness(&self) -> Result<u32> {
        Ok(self.0.borrow().brightness)
    }
    fn set_brightness(&mut self, level: u32) -> Result<()> {
        self.0.borrow_mut().brightness = level.min(24);
        Ok(())
    }
    fn warmth(&self) -> Result<u32> {
        Ok(self.0.borrow().warmth)
    }
    fn set_warmth(&mut self, level: u32) -> Result<()> {
        self.0.borrow_mut().warmth = level.min(24);
        Ok(())
    }
}

impl Battery for Fake {
    fn state(&self) -> Result<BatteryState> {
        Ok(self.0.borrow().battery)
    }
}

impl Power for Fake {
    fn set_screensaver(&mut self, s: Screensaver) -> Result<()> {
        self.0.borrow_mut().screensaver = Some(s);
        Ok(())
    }
    fn set_auto_sleep(&mut self, after: Option<Duration>) -> Result<()> {
        self.0.borrow_mut().auto_sleep = after;
        Ok(())
    }
    fn auto_sleep(&self) -> Option<Duration> {
        self.0.borrow().auto_sleep
    }
}

impl Wifi for Fake {
    fn state(&self) -> Result<WifiState> {
        let s = self.0.borrow();
        let signal = s.connected.as_ref().and_then(|c| s.networks.iter().find(|n| &n.essid == c)).map(|n| n.signal);
        Ok(WifiState {
            enabled: s.wifi_on,
            state: if s.connected.is_some() { "CONNECTED".into() } else { "NA".into() },
            essid: s.connected.clone(),
            signal,
        })
    }
    fn set_enabled(&mut self, on: bool) -> Result<()> {
        let mut s = self.0.borrow_mut();
        s.wifi_on = on;
        if !on {
            s.connected = None;
        }
        Ok(())
    }
    fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
        self.scan_list()
    }
    fn scan_list(&self) -> Result<Vec<WifiNetwork>> {
        let s = self.0.borrow();
        if !s.wifi_on {
            return Err(DeviceError::Backend("Wi-Fi is off".into()));
        }
        Ok(s.networks.iter().map(|n| WifiNetwork { known: s.profiles.iter().any(|p| p.essid == n.essid), ..n.clone() }).collect())
    }
    fn connect_in_background(&mut self, essid: &str, password: Option<&str>) -> Result<bool> {
        let background = self.0.borrow().background;
        self.connect(essid, password).map(|()| background)
    }
    fn scan_in_background(&mut self) -> Result<bool> {
        let background = self.0.borrow().background;
        self.scan().map(|_| background)
    }
    fn connect(&mut self, essid: &str, password: Option<&str>) -> Result<()> {
        let mut s = self.0.borrow_mut();
        s.connects.push((essid.to_string(), password.map(str::to_string)));
        let Some(net) = s.networks.iter().find(|n| n.essid == essid).cloned() else {
            return Err(DeviceError::Backend(format!("{essid}: not in range")));
        };
        let known = s.profiles.iter().any(|p| p.essid == essid);
        if known && s.stale.iter().any(|e| e == essid) {
            if password.is_none() || password != s.password.as_deref() {
                return Err(DeviceError::Backend("wrong password".into()));
            }
            s.stale.retain(|e| e != essid);
        }
        if net.secured && !known {
            match (&s.password, password) {
                (Some(want), Some(got)) if want != got => return Err(DeviceError::Backend("wrong password".into())),
                (_, None) => return Err(DeviceError::Backend("password needed".into())),
                _ => {}
            }
        }
        if !known {
            let netid = s.profiles.len() as i64 + 1;
            s.profiles.push(WifiProfile { essid: essid.into(), netid, secured: net.secured });
        }
        s.connected = Some(essid.to_string());
        Ok(())
    }
    fn disconnect(&mut self) -> Result<()> {
        self.0.borrow_mut().connected = None;
        Ok(())
    }
    fn profiles(&self) -> Result<Vec<WifiProfile>> {
        Ok(self.0.borrow().profiles.clone())
    }
    fn delete_profile(&mut self, essid: &str) -> Result<()> {
        self.0.borrow_mut().profiles.retain(|p| p.essid != essid);
        Ok(())
    }
    fn airplane_mode(&self) -> Result<bool> {
        Ok(self.0.borrow().airplane)
    }
    fn set_airplane_mode(&mut self, on: bool) -> Result<()> {
        let mut s = self.0.borrow_mut();
        s.airplane = on;
        if on {
            s.wifi_on = false;
            s.connected = None;
        }
        Ok(())
    }
}

impl Usb for Fake {}

impl Clock for Fake {
    fn local_now(&self) -> ink_device::clock::DateTime {
        // A fixed time keeps screenshots stable.
        ink_device::clock::DateTime::from_unix(1_793_436_000, 3600)
    }
    fn sync_ntp(&mut self, server: Option<&str>) -> Result<ink_device::clock::NtpSample> {
        self.0.borrow_mut().ntp_syncs += 1;
        Ok(ink_device::clock::NtpSample {
            server: server.unwrap_or("pool.ntp.org").into(),
            offset_ms: 120,
            delay_ms: 30,
            stratum: 2,
            time: std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_436_000),
        })
    }
}
