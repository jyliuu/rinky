//! The e-reader backend: Kindles (firmware 5.x) and Kobos, one binary. Linux only.
//!
//! What both have in common is here: `/dev/fb0` with the model's refresh ioctls
//! ([`fb`], [`model`]), evdev touch, power, page and cover keys, sleep with the
//! screensaver and kernel suspend ([`input`], [`system`]). What differs is the system
//! software around rinky, behind the service traits: on a Kindle Amazon's `lipc`
//! services ([`kindle`]), on a Kobo sysfs, `ntx_io` and `wpa_supplicant` ([`kobo`]).
//! [`open`] picks them by [`Platform`].
//!
//! The screen, the input loop and the services share one [`Shared`] state, so the
//! input loop can draw the screensaver or the USB lock screen and keep the app's
//! drawing in a shadow buffer meanwhile, and the services can queue requests
//! (sleep now, drive mode) that the input loop carries out.

pub mod fb;
pub mod input;
pub mod kindle;
pub mod kobo;
pub mod model;
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
use crate::platform::Platform;
use crate::services::{Battery, Clock, Light, Power, Usb, Wifi};
use crate::{Device, DeviceInfo, Result};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Options for [`open`]. `Default` reads the `RINKY_*` environment variables
/// documented on each field. The lipc events, the USB drive policy and powerd's
/// warmth schedule exist only on a Kindle; a Kobo ignores them.
#[derive(Clone, Debug)]
pub struct ReaderOptions {
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
    /// NTP server queried when Wi-Fi connects (`RINKY_NTP_SERVER`, default `NTP_SERVER`
    /// from `/etc/sysconfig/ntp` on a Kindle, `pool.ntp.org` on a Kobo; `RINKY_NTP=0`
    /// disables).
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

impl Default for ReaderOptions {
    fn default() -> Self {
        let run = env("RINKY_RUN").unwrap_or_else(|| "/tmp/rinky".into());
        let ntp_server = if env("RINKY_NTP").as_deref() == Some("0") {
            None
        } else {
            env("RINKY_NTP_SERVER").or_else(|| match crate::platform::current() {
                Platform::Kindle => {
                    std::fs::read_to_string("/etc/sysconfig/ntp").ok().and_then(|t| crate::clock::ntp_server_from_config(&t))
                }
                Platform::Kobo => Some("pool.ntp.org".into()),
            })
        };
        ReaderOptions {
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
    pub(crate) screen: fb::Framebuffer,
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

/// What the services of either platform need from [`Shared`]: the requests they queue
/// for the input loop and the state it keeps. `None` without the input loop (`rinky
/// device`).
#[derive(Clone, Default)]
pub(crate) struct SharedHandle(pub(crate) Option<Rc<RefCell<Shared>>>);

impl SharedHandle {
    /// Queues a request for the input loop.
    pub(crate) fn request(&self, r: input::Request) -> Result<()> {
        let s = self.0.as_ref().ok_or(crate::DeviceError::Unsupported("needs the running input loop"))?;
        s.borrow_mut().requests.push_back(r);
        Ok(())
    }

    /// rinky's own sleep controller runs.
    pub(crate) fn sleep_enabled(&self) -> bool {
        self.0.as_ref().is_some_and(|s| s.borrow().sleep_enabled)
    }

    /// The model (none without the input loop).
    pub(crate) fn model(&self) -> Option<model::Model> {
        self.0.as_ref().map(|s| s.borrow().screen.model)
    }

    /// Records a restore journal entry.
    pub(crate) fn journal(&self, e: crate::journal::Entry) {
        if let Some(s) = &self.0 {
            let _ = s.borrow().journal.record(e);
        }
    }

    pub(crate) fn set_screensaver(&self, saver: Screensaver) -> Result<()> {
        let s = self.0.as_ref().ok_or(crate::DeviceError::Unsupported("screensaver needs the running input loop"))?;
        log::info!("screensaver: {}", saver.describe());
        s.borrow_mut().screensaver = saver;
        Ok(())
    }

    pub(crate) fn auto_sleep(&self) -> Option<Duration> {
        self.0.as_ref().and_then(|s| s.borrow().auto_sleep)
    }

    pub(crate) fn sleep_state(&self) -> SleepState {
        self.0.as_ref().map(|s| s.borrow().sleep_state).unwrap_or_default()
    }

    /// The USB cable as the input loop saw it last, else read now.
    pub(crate) fn cable(&self, sys: &SysRoot) -> UsbCable {
        match &self.0 {
            Some(s) => s.borrow().cable,
            None => {
                let udc = crate::usb::read_udc(sys).map(|(_, s)| s);
                crate::usb::cable_from(udc.as_deref(), crate::battery::external_power(sys), None)
            }
        }
    }

    /// The drive mode state the input loop keeps.
    pub(crate) fn drive_state(&self) -> Option<DriveModeState> {
        self.0.as_ref().map(|s| s.borrow().drive_state)
    }

    pub(crate) fn usb_enabled(&self) -> bool {
        self.0.as_ref().is_some_and(|s| s.borrow().usb_enabled)
    }
}

/// Runs a suspend routine and logs its progress lines.
pub(crate) fn logged_suspend(suspend: system::SuspendFn, sys: &SysRoot, wake_after: Option<Duration>) -> Result<()> {
    let mut lines = Vec::new();
    let r = suspend(sys, wake_after, &mut |s| lines.push(s));
    for l in lines {
        log::info!("suspend: {l}");
    }
    r.map(|_| ())
}

/// The screen as the app sees it: while the screensaver or the USB lock screen is up,
/// updates go to the shadow buffer and appear when it is removed.
pub struct ReaderScreen(pub(crate) Rc<RefCell<Shared>>);

impl Screen for ReaderScreen {
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
pub fn open_screen() -> Result<fb::Framebuffer> {
    fb::Framebuffer::open()
}

/// Opens the whole device: the shared screen and input loop, and the platform's
/// services.
pub fn open(opts: ReaderOptions) -> Result<Device> {
    let journal = Journal::at(&opts.journal);
    let screen = fb::Framebuffer::open_with(Some(journal.clone()))?;
    let platform = screen.model.platform;
    // Amazon's services: lipc events, volumd's USB drive mode, powerd's warmth schedule.
    let lipc = (platform == Platform::Kindle).then(|| opts.lipc.clone());
    let lipc_events = lipc.is_some() && opts.power_events;
    let usb = opts.usb.filter(|_| lipc_events);
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
        usb_enabled: usb.is_some(),
    }));
    let loop_opts = input::LoopOptions {
        grab_touch: opts.grab_touch,
        lipc_events,
        sleep: opts.sleep.clone(),
        usb,
        low_battery: opts.low_battery,
        // On a Kobo the Wi-Fi service syncs the clock itself when it connects.
        ntp_server: opts.ntp_server.clone().filter(|_| lipc_events),
        amber: lipc.is_some() && opts.amber,
        heartbeat: opts.heartbeat.clone(),
        suspend: match platform {
            Platform::Kindle => system::kernel_suspend,
            Platform::Kobo => kobo::suspend,
        },
    };
    if let Some(dir) = opts.heartbeat.as_ref().and_then(|p| p.parent()) {
        let _ = std::fs::create_dir_all(dir);
    }
    let input = Box::new(input::ReaderInput::open(shared.clone(), lipc.clone(), opts.sys.clone(), loop_opts)?);
    let (model, page_wave) = {
        let sh = shared.borrow();
        (sh.screen.model.name.to_string(), sh.screen.page_wave())
    };
    let info = DeviceInfo { backend: platform.id(), model, firmware: firmware(platform), page_wave };
    let clock = Box::new(system::DeviceClock { lipc, ntp_server: opts.ntp_server.clone() });
    let screen = Box::new(ReaderScreen(shared.clone()));
    Ok(match platform {
        Platform::Kindle => {
            let svc = kindle::LipcServices::new(opts.lipc, Some(shared), opts.sys);
            // Light levels and Wi-Fi state for the control center, read once off the UI
            // thread after start-up (the first book page comes first).
            svc.refresh_after(Duration::from_secs(3));
            with_services(info, screen, input, clock, svc)
        }
        Platform::Kobo => with_services(info, screen, input, clock, kobo::KoboServices::new(Some(shared), opts.sys, opts.ntp_server)),
    })
}

/// A [`Device`] whose light, battery, power, Wi-Fi and USB are all `svc`.
fn with_services<S>(info: DeviceInfo, screen: Box<ReaderScreen>, input: Box<input::ReaderInput>, clock: Box<dyn Clock>, svc: S) -> Device
where
    S: Light + Battery + Power + Wifi + Usb + Clone + 'static,
{
    Device {
        info,
        screen,
        input,
        light: Box::new(svc.clone()),
        battery: Box::new(svc.clone()),
        power: Box::new(svc.clone()),
        wifi: Box::new(svc.clone()),
        usb: Box::new(svc),
        clock,
    }
}

/// The firmware version: the first line of `/etc/version.txt` on a Kindle, the third
/// field of `/mnt/onboard/.kobo/version` on a Kobo; empty when missing.
pub fn firmware(platform: Platform) -> String {
    let (file, field) = match platform {
        Platform::Kindle => ("/etc/version.txt", None),
        Platform::Kobo => ("/mnt/onboard/.kobo/version", Some(2)),
    };
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let line = text.lines().next().unwrap_or("").trim();
    match field {
        Some(i) => line.split(',').nth(i).unwrap_or("").to_string(),
        None => line.to_string(),
    }
}
