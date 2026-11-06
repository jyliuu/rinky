//! Kobo backend (Nickel stopped by the launcher). The screen, touch, keys and sleep
//! are the Kindle backend's, which reads the Kobo's model from the shared table
//! ([`crate::kindle::model::KOBO_MODELS`]): refresh interface, upright rotation, touch
//! transforms, page and cover keys. Kobo has no `lipc` services: light, battery, Wi-Fi
//! and suspend talk to sysfs, `ntx_io` and `wpa_supplicant` directly ([`services`],
//! [`wifi`]), as KOReader does. USB storage is Nickel's: rinky does not export the
//! drive (exit rinky, then plug in).

pub mod services;
pub mod wifi;

use crate::journal::Journal;
use crate::kindle::{input, KindleOptions, KindleScreen, Shared};
use crate::sleep::SleepState;
use crate::usb::{DriveModeState, UsbCable};
use crate::{Device, DeviceInfo, Result};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// The firmware version from `/mnt/onboard/.kobo/version` (third field).
pub fn firmware() -> String {
    let v = std::fs::read_to_string("/mnt/onboard/.kobo/version").unwrap_or_default();
    v.trim().split(',').nth(2).unwrap_or("").to_string()
}

/// Opens the whole device. `opts` as for the Kindle; the lipc events, the USB drive
/// policy and powerd's warmth schedule do not apply.
pub fn open(opts: KindleOptions) -> Result<Device> {
    let journal = Journal::at(&opts.journal);
    let screen = crate::kindle::fb::KindleFb::open_with(Some(journal.clone()))?;
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
        usb_enabled: false,
    }));
    let ntp_server = if std::env::var("RINKY_NTP").as_deref() == Ok("0") {
        None
    } else {
        Some(std::env::var("RINKY_NTP_SERVER").unwrap_or_else(|_| "pool.ntp.org".into()))
    };
    let loop_opts = input::LoopOptions {
        grab_touch: opts.grab_touch,
        lipc_events: false,
        sleep: opts.sleep.clone(),
        usb: None,
        low_battery: opts.low_battery,
        ntp_server: None,
        amber: false,
        heartbeat: opts.heartbeat.clone(),
    };
    if let Some(dir) = opts.heartbeat.as_ref().and_then(|p| p.parent()) {
        let _ = std::fs::create_dir_all(dir);
    }
    let input = input::KindleInput::open(shared.clone(), opts.lipc.clone(), opts.sys.clone(), loop_opts)?;
    let svc = services::KoboServices::new(Some(shared.clone()), opts.sys.clone(), ntp_server.clone());
    let model = shared.borrow().screen.model.name.to_string();
    Ok(Device {
        info: DeviceInfo { backend: "kobo", model, firmware: firmware(), page_wave: false },
        screen: Box::new(KindleScreen(shared)),
        input: Box::new(input),
        light: Box::new(svc.clone()),
        battery: Box::new(svc.clone()),
        power: Box::new(svc.clone()),
        wifi: Box::new(svc.clone()),
        usb: Box::new(svc),
        clock: Box::new(crate::kindle::services::KindleClock { lipc: opts.lipc, ntp_server }),
    })
}
