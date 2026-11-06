//! Kindle backend (firmware 5.x; tested on the PW12, MT8113, 5.19.x): `/dev/fb0` with
//! the model's refresh ioctls ([`model`]), evdev touch, power and page keys, lipc
//! services and events, kernel uevents. Linux only.
//!
//! The screen, the input loop and the services share one [`Shared`] state, so the
//! input loop can draw the screensaver or the USB lock screen and keep the app's
//! drawing in a shadow buffer meanwhile, and the services can queue requests
//! (sleep now, drive mode) that the input loop carries out.

pub mod fb;
pub mod input;
pub mod model;
pub mod services;
pub mod system;

use crate::canvas::Canvas;
use crate::geom::{Rect, Rotation, Size};
use crate::journal::Journal;
use crate::lipc::Lipc;
use crate::screen::{copy_to_panel, RefreshClass, Screen};
use crate::screensaver::Screensaver;
use crate::sleep::{SleepConfig, SleepState};
use crate::sysfs::SysRoot;
use crate::usb::{DriveModeState, UsbCable, UsbPolicy};
use crate::{Device, DeviceInfo, Result};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Options for [`open`]. `Default` reads the `RINKY_*` environment variables
/// documented on each field.
#[derive(Clone, Debug)]
pub struct KindleOptions {
    /// Grab the touchscreen (`EVIOCGRAB`) so the framework does not see taps.
    pub grab_touch: bool,
    /// Subscribe to powerd, wifid, cmd, volumd, hal and time events
    /// (`InputEvent::Power`, `InputEvent::Usb`).
    pub power_events: bool,
    /// lipc runner for services and events.
    pub lipc: Lipc,
    /// rinky handles the power key, the cover, inactivity and suspend
    /// (`RINKY_SLEEP=0` turns it off: the app then gets `Key::Power`).
    pub sleep: Option<SleepConfig>,
    /// What to show while asleep (`RINKY_SCREENSAVER=page|blank|image:PATH`).
    pub screensaver: Screensaver,
    /// USB policy (`RINKY_USB=ask|charge|drive|off`; `off` leaves volumd alone).
    pub usb: Option<UsbPolicy>,
    /// Low-battery warning in percent (`RINKY_LOW_BATTERY`, default 10).
    pub low_battery: u8,
    /// NTP server queried when Wi-Fi connects (`RINKY_NTP_SERVER`, default
    /// `NTP_SERVER` from `/etc/sysconfig/ntp`; `RINKY_NTP=0` disables).
    pub ntp_server: Option<String>,
    /// Apply powerd's warmth schedule while the framework is stopped
    /// (`RINKY_AMBER=0` disables).
    pub amber: bool,
    /// Heartbeat file for the watchdog (`/tmp/rinky/heartbeat`).
    pub heartbeat: Option<PathBuf>,
    /// Restore journal (`/tmp/rinky/restore`).
    pub journal: PathBuf,
    /// Root for `/sys` reads.
    pub sys: SysRoot,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl Default for KindleOptions {
    fn default() -> Self {
        let run = env("RINKY_RUN").unwrap_or_else(|| "/tmp/rinky".into());
        let ntp_server = if env("RINKY_NTP").as_deref() == Some("0") {
            None
        } else {
            env("RINKY_NTP_SERVER").or_else(|| {
                std::fs::read_to_string("/etc/sysconfig/ntp").ok().and_then(|t| crate::clock::ntp_server_from_config(&t))
            })
        };
        KindleOptions {
            grab_touch: true,
            power_events: false,
            lipc: Lipc::new(),
            sleep: (env("RINKY_SLEEP").as_deref() != Some("0")).then(SleepConfig::from_env),
            screensaver: env("RINKY_SCREENSAVER").map(|s| Screensaver::from_setting(&s)).unwrap_or_default(),
            usb: match env("RINKY_USB").as_deref() {
                Some("off") => None,
                Some("charge") => Some(UsbPolicy::ChargeOnly),
                Some("drive") => Some(UsbPolicy::DriveMode),
                _ => Some(UsbPolicy::Ask),
            },
            low_battery: env("RINKY_LOW_BATTERY").and_then(|v| v.parse().ok()).unwrap_or(10),
            ntp_server,
            amber: env("RINKY_AMBER").as_deref() != Some("0"),
            heartbeat: Some(PathBuf::from(format!("{run}/heartbeat"))),
            journal: PathBuf::from(format!("{run}/restore")),
            sys: SysRoot::system(),
        }
    }
}

/// State shared by the screen, the input loop and the services.
pub(crate) struct Shared {
    pub(crate) screen: fb::KindleFb,
    /// Raw panel content under the screensaver or lock screen (app updates go here).
    pub(crate) shadow: Option<Canvas>,
    pub(crate) asleep: bool,
    pub(crate) locked: bool,
    pub(crate) screensaver: Screensaver,
    pub(crate) requests: VecDeque<input::Request>,
    pub(crate) sleep_state: SleepState,
    pub(crate) auto_sleep: Option<Duration>,
    pub(crate) drive_state: DriveModeState,
    pub(crate) cable: UsbCable,
    pub(crate) journal: Journal,
    pub(crate) sleep_enabled: bool,
    pub(crate) usb_enabled: bool,
}

/// The screen as the app sees it: while the screensaver or the USB lock screen is up,
/// updates go to the shadow buffer and appear when it is removed.
pub struct KindleScreen(pub(crate) Rc<RefCell<Shared>>);

impl Screen for KindleScreen {
    fn panel_size(&self) -> Size {
        self.0.borrow().screen.panel_size()
    }

    fn rotation(&self) -> Rotation {
        self.0.borrow().screen.rotation()
    }

    fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
        self.0.borrow_mut().screen.set_rotation(rotation)
    }

    fn night_mode(&self) -> bool {
        self.0.borrow().screen.night_mode()
    }

    fn set_night_mode(&mut self, on: bool) -> Result<()> {
        let mut sh = self.0.borrow_mut();
        let was_sw = sh.screen.sw_invert();
        sh.screen.set_night_mode(on)?;
        let now_sw = sh.screen.sw_invert();
        if was_sw != now_sw {
            if let Some(shadow) = sh.shadow.as_mut() {
                for p in &mut shadow.pixels {
                    *p = 255 - *p;
                }
            }
        }
        Ok(())
    }

    fn update(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass) -> Result<()> {
        let mut sh = self.0.borrow_mut();
        let sh = &mut *sh;
        if sh.asleep || sh.locked {
            let (rotation, invert, panel) = (sh.screen.rotation(), sh.screen.sw_invert(), sh.screen.panel_size());
            if let Some(shadow) = sh.shadow.as_mut() {
                let stride = shadow.width;
                copy_to_panel(canvas, rect, rotation, invert, panel, &mut shadow.pixels, stride);
            }
            return Ok(());
        }
        sh.screen.update(canvas, rect, class)
    }

    fn update_wave(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass, wave: crate::screen::Wave) -> Result<()> {
        if self.0.borrow().asleep || self.0.borrow().locked {
            return self.update(canvas, rect, class);
        }
        self.0.borrow_mut().screen.update_wave(canvas, rect, class, wave)
    }

    fn refresh_all(&mut self) -> Result<()> {
        let mut sh = self.0.borrow_mut();
        if sh.asleep || sh.locked {
            return Ok(());
        }
        sh.screen.refresh_all()
    }

    fn screenshot(&self) -> Result<Canvas> {
        self.0.borrow().screen.screenshot()
    }
}

/// Opens the screen only (for `rinky refresh` and screenshots).
pub fn open_screen() -> Result<fb::KindleFb> {
    fb::KindleFb::open()
}

/// Opens the whole device.
pub fn open(opts: KindleOptions) -> Result<Device> {
    let journal = Journal::at(&opts.journal);
    let screen = fb::KindleFb::open_with(Some(journal.clone()))?;
    let shared = Rc::new(RefCell::new(Shared {
        screen,
        shadow: None,
        asleep: false,
        locked: false,
        screensaver: opts.screensaver.clone(),
        requests: VecDeque::new(),
        sleep_state: SleepState::Awake,
        auto_sleep: None,
        drive_state: DriveModeState::Off,
        cable: UsbCable::None,
        journal,
        sleep_enabled: opts.sleep.is_some(),
        usb_enabled: opts.usb.is_some(),
    }));
    let loop_opts = input::LoopOptions {
        grab_touch: opts.grab_touch,
        lipc_events: opts.power_events,
        sleep: opts.sleep.clone(),
        usb: if opts.power_events { opts.usb } else { None },
        low_battery: opts.low_battery,
        ntp_server: opts.ntp_server.clone(),
        amber: opts.amber,
        heartbeat: opts.heartbeat.clone(),
    };
    if let Some(dir) = opts.heartbeat.as_ref().and_then(|p| p.parent()) {
        let _ = std::fs::create_dir_all(dir);
    }
    let input = input::KindleInput::open(shared.clone(), opts.lipc.clone(), opts.sys.clone(), loop_opts)?;
    let svc = services::LipcServices::new(opts.lipc.clone(), Some(shared.clone()), opts.sys.clone());
    // Light levels and Wi-Fi state for the control center, read once off the UI
    // thread after start-up (the first book page comes first).
    svc.refresh_after(Duration::from_secs(3));
    let firmware = std::fs::read_to_string("/etc/version.txt").unwrap_or_default().lines().next().unwrap_or("").to_string();
    let (model, page_wave) = {
        let sh = shared.borrow();
        (sh.screen.model.name.to_string(), sh.screen.page_wave())
    };
    Ok(Device {
        info: DeviceInfo { backend: "kindle", model, firmware, page_wave },
        screen: Box::new(KindleScreen(shared)),
        input: Box::new(input),
        light: Box::new(svc.clone()),
        battery: Box::new(svc.clone()),
        power: Box::new(svc.clone()),
        wifi: Box::new(svc.clone()),
        usb: Box::new(svc),
        clock: Box::new(services::KindleClock { lipc: opts.lipc, ntp_server: opts.ntp_server }),
    })
}
