//! Device layer for rinky.
//!
//! A [`Device`] bundles one implementation of each service trait:
//!
//! | Trait | Kindle and Kobo (`ereader`) | Host window (`sim`) | Tests (`headless`) |
//! | --- | --- | --- | --- |
//! | [`Screen`] | `/dev/fb0` + the model's refresh ioctls | minifb window, e-ink look | PNG frames |
//! | [`Input`] | evdev touch and keys; powerd events on a Kindle | mouse + keys | script |
//! | [`Light`], [`Battery`], [`Power`], [`Wifi`], [`Usb`] | Kindle: `lipc` properties; Kobo: sysfs, `wpa_supplicant` | unsupported | unsupported |
//! | [`Clock`] | system clock and RTC | system clock | system clock |
//!
//! Fields are public so the app can borrow the screen and the input at the same time.
//! Shared building blocks live here too: [`Canvas`] (8-bit gray pixels), [`geom`]
//! (points, rectangles, rotation), [`evdev`] decoding and the [`lipc`] runner.

#![warn(missing_docs)]

/// `eprintln!` that never panics. rinky's stderr is a file on the userstore, which
/// can turn read-only (ext4 error), fill up or go away (USB export); `eprintln!` then
/// panics, and with `panic = "abort"` that kills the process. A failed write is dropped.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// `println!` that never panics (see `say!`).
macro_rules! out {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

pub mod battery;
pub mod canvas;
pub mod clock;
pub mod devtool;
pub mod eink;
pub mod events;
pub mod evdev;
pub mod font;
pub mod geom;
pub mod headless;
pub mod input;
pub mod journal;
pub mod light;
pub mod lipc;
pub mod netman;
pub use ink_platform as platform;
pub mod screen;
pub mod screensaver;
pub mod script;
pub mod selftest;
pub mod services;
pub mod sleep;
pub mod sysfs;
pub mod trace;
pub mod usb;
pub mod wifi;

#[cfg(target_os = "linux")]
pub mod ereader;
#[cfg(all(feature = "sim", not(target_arch = "arm")))]
pub mod sim;

pub use canvas::Canvas;
pub use geom::{Point, Rect, Rotation, Size};
pub use input::{Input, InputEvent, Key, KeyEvent, PowerEvent, Timestamp, TouchContact, TouchFrame, TouchPhase};
pub use screen::{RefreshClass, Screen, Wave};
pub use services::{Battery, BatteryState, Clock, Light, Power, Usb, UsbState, Wifi, WifiNetwork, WifiState};

/// Errors from the device layer.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    /// The backend does not provide this service (yet).
    #[error("not supported by this device backend: {0}")]
    Unsupported(&'static str),
    /// A system call or file operation failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A `lipc` tool failed or printed something unexpected.
    #[error("lipc: {0}")]
    Lipc(String),
    /// A child process or wait exceeded its deadline.
    #[error("timed out: {0}")]
    Timeout(String),
    /// A backend-specific failure (ioctl, window system).
    #[error("{0}")]
    Backend(String),
    /// A malformed input script.
    #[error("bad input script: {0}")]
    Script(String),
}

/// Result alias for this crate.
pub type Result<T, E = DeviceError> = std::result::Result<T, E>;

/// Which backend a [`Device`] uses and what it runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// `kindle`, `kobo`, `sim` or `headless`.
    pub backend: &'static str,
    /// Model name (`Kindle Paperwhite 4 (10th gen)`, `Kobo Libra 2`).
    pub model: String,
    /// Firmware version (Kindle `/etc/version.txt`, Kobo `.kobo/version`); empty off the
    /// device.
    pub firmware: String,
    /// The panel sweeps a page turn in ([`Screen::update_wave`]); without it the
    /// "Page turn wave" setting is hidden.
    pub page_wave: bool,
}

/// One implementation of every device service.
pub struct Device {
    /// What this is.
    pub info: DeviceInfo,
    /// Display.
    pub screen: Box<dyn Screen>,
    /// Touch, keys and power events.
    pub input: Box<dyn Input>,
    /// Frontlight.
    pub light: Box<dyn Light>,
    /// Battery.
    pub battery: Box<dyn Battery>,
    /// Suspend and screensaver.
    pub power: Box<dyn Power>,
    /// Wi-Fi.
    pub wifi: Box<dyn Wifi>,
    /// USB.
    pub usb: Box<dyn Usb>,
    /// Clock.
    pub clock: Box<dyn Clock>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device").field("info", &self.info).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_device_has_every_service() {
        let (dev, _probe) = headless::open(headless::HeadlessOptions::default());
        assert_eq!(dev.info.backend, "headless");
        assert_eq!(dev.screen.size(), Size::new(1272, 1696));
        assert!(matches!(dev.light.brightness(), Err(DeviceError::Unsupported(_))));
        assert!(format!("{dev:?}").contains("headless"));
    }
}
