//! Turning system notifications into [`InputEvent`]s: lipc events from the subscribed
//! publishers and kernel uevents (netlink), both host-testable.
//!
//! Publishers (5.19.5): `com.lab126.powerd` (screensaver/suspend/charging/battery),
//! `com.lab126.wifid` (`cmStateChange "CONNECTED"`, `scanComplete`, `signalStrength`,
//! `cmConnectionFailed`), `com.lab126.cmd` (`connectionAvailable`, `interfaceChange`,
//! `InternetConnected`), `com.lab126.volumd` (`userstoreAvailable`,
//! `userstoreUnAvailable`), `com.lab126.hal` (sent by udev scripts:
//! `usbPlugIn/usbPlugOut`, `usbConfigured/usbUnconfigured`, `powerButtonPressed`,
//! `magSensorOpened/magSensorClosed` = the cover's hall sensor), and
//! `com.lab126.time` (`timeChanged DELTA`).

use crate::input::{InputEvent, PowerEvent};
use crate::lipc::PublishedEvent;
use std::collections::BTreeMap;

/// The publishers the Kindle backend subscribes to.
pub const PUBLISHERS: [&str; 6] =
    ["com.lab126.powerd", "com.lab126.wifid", "com.lab126.cmd", "com.lab126.volumd", "com.lab126.hal", "com.lab126.time"];

/// Maps one lipc event to input events (most events map to one).
pub fn map_published(e: &PublishedEvent) -> Vec<InputEvent> {
    let name = e.event.name.as_str();
    let arg0 = e.event.args.first().map(String::as_str).unwrap_or("");
    let generic = || {
        InputEvent::Power(PowerEvent::Lipc { publisher: e.publisher.clone(), name: name.to_string(), args: e.event.args.clone() })
    };
    match e.publisher.as_str() {
        "com.lab126.powerd" => PowerEvent::parse(&e.event.line()).map(InputEvent::Power).into_iter().collect(),
        "com.lab126.hal" => match name {
            "usbPlugIn" => vec![InputEvent::Usb(true)],
            "usbPlugOut" => vec![InputEvent::Usb(false)],
            "magSensorClosed" => vec![InputEvent::Power(PowerEvent::Cover(true))],
            "magSensorOpened" => vec![InputEvent::Power(PowerEvent::Cover(false))],
            _ => vec![generic()],
        },
        "com.lab126.wifid" => match name {
            "cmStateChange" => vec![InputEvent::Power(PowerEvent::WifiState(arg0.to_string()))],
            _ => vec![generic()],
        },
        "com.lab126.time" if name == "timeChanged" => {
            vec![InputEvent::Power(PowerEvent::TimeChanged(arg0.parse().unwrap_or(0)))]
        }
        _ => vec![generic()],
    }
}

/// Where a cover (hall sensor) event came from. All three can fire for one movement of
/// the cover; the sleep controller ignores the repeats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverSource {
    /// Kernel uevent `HALLSENSOR=closed|opened` from `drivers/char/eink_hall.c`, read
    /// from our own netlink socket.
    Uevent,
    /// `com.lab126.hal magSensorClosed|magSensorOpened`, sent by udev
    /// (`/etc/udev/rules.d/10-hw-wakeup.rules` -> `/lib/udev/bin/notifyUserTrigger`).
    LipcHal,
    /// powerd `goingToScreenSaver 4` (HALL_SUSPEND) / `outOfScreenSaver 6`
    /// (HALL_WAKEUP), the events KOReader uses.
    Powerd(u32),
    /// A cover key on the key device (Kobo `KEY_F1`: pressed = closed).
    Key,
}

impl std::fmt::Display for CoverSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoverSource::Uevent => write!(f, "uevent"),
            CoverSource::LipcHal => write!(f, "lipc-hal"),
            CoverSource::Powerd(n) => write!(f, "powerd({n})"),
            CoverSource::Key => write!(f, "key"),
        }
    }
}

/// powerd's screensaver source for a cover close (KOReader `POWERD_EVENT_SOURCES`).
pub const POWERD_HALL_SUSPEND: u32 = 4;
/// powerd's screensaver source for a cover open.
pub const POWERD_HALL_WAKEUP: u32 = 6;

/// The cover state a lipc event reports: `Some(closed)` for `com.lab126.hal
/// magSensorClosed/Opened` and powerd `goingToScreenSaver 4` / `outOfScreenSaver 6`.
pub fn cover_from_published(e: &PublishedEvent) -> Option<(bool, CoverSource)> {
    let name = e.event.name.as_str();
    match e.publisher.as_str() {
        "com.lab126.hal" => match name {
            "magSensorClosed" => Some((true, CoverSource::LipcHal)),
            "magSensorOpened" => Some((false, CoverSource::LipcHal)),
            _ => None,
        },
        "com.lab126.powerd" => {
            let src: u32 = e.event.args.first()?.trim_matches('"').parse().ok()?;
            match (name, src) {
                ("goingToScreenSaver", POWERD_HALL_SUSPEND) => Some((true, CoverSource::Powerd(src))),
                ("outOfScreenSaver", POWERD_HALL_WAKEUP) => Some((false, CoverSource::Powerd(src))),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The eink_hall driver's sysfs directory (KOReader's `hall_file` is `hall_enable` in it).
pub const HALL_DIR: &str = "/sys/devices/platform/eink_hall";

/// The hall sensor's sysfs files at start-up (log only: the meaning of `hall_detect`
/// and `hall_hw_state` values is not documented, so they never drive sleep).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HallInfo {
    /// The folder read (the model's; [`HALL_DIR`] by default).
    pub dir: String,
    /// The folder exists.
    pub present: bool,
    /// `hall_enable` (`1` = the driver sends events; `0` = muted). Never written by us.
    pub enable: Option<String>,
    /// `hall_detect`, raw.
    pub detect: Option<String>,
    /// `hall_hw_state`, raw.
    pub hw_state: Option<String>,
}

impl HallInfo {
    /// Reads the files under [`HALL_DIR`].
    pub fn read(sys: &crate::sysfs::SysRoot) -> HallInfo {
        HallInfo::read_at(sys, HALL_DIR)
    }

    /// Reads the files under `dir` (older Kindles: `wario_hall0`, `rex_hall`).
    pub fn read_at(sys: &crate::sysfs::SysRoot, dir: &str) -> HallInfo {
        let get = |f: &str| sys.read(&format!("{dir}/{f}")).ok().map(|s| s.trim().to_string());
        HallInfo {
            dir: dir.to_string(),
            present: sys.exists(dir),
            enable: get("hall_enable"),
            detect: get("hall_detect"),
            hw_state: get("hall_hw_state"),
        }
    }

    /// `hall_enable` reads `0`: the kernel sends no cover events.
    pub fn disabled(&self) -> bool {
        self.enable.as_deref() == Some("0")
    }

    /// One log line.
    pub fn describe(&self) -> String {
        if !self.present {
            return format!("cover: no hall sensor at {}", self.dir);
        }
        let v = |o: &Option<String>| o.clone().unwrap_or_else(|| "?".into());
        format!(
            "cover: hall sensor {}: hall_enable={} hall_detect={} hall_hw_state={}{}",
            self.dir,
            v(&self.enable),
            v(&self.detect),
            v(&self.hw_state),
            if self.disabled() { " (DISABLED in the kernel: no cover events will arrive; rinky does not change it)" } else { "" }
        )
    }
}

/// A kernel uevent (`ACTION@DEVPATH` header and `KEY=VALUE` fields).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Uevent {
    /// `add`, `remove`, `change`, ...
    pub action: String,
    /// `/devices/...`.
    pub devpath: String,
    /// All `KEY=VALUE` fields.
    pub env: BTreeMap<String, String>,
}

impl Uevent {
    /// Parses one netlink message (NUL-separated). Messages from udevd (`libudev`
    /// header) are ignored.
    pub fn parse(buf: &[u8]) -> Option<Uevent> {
        let mut parts = buf.split(|b| *b == 0).filter(|p| !p.is_empty());
        let head = std::str::from_utf8(parts.next()?).ok()?;
        let (action, devpath) = head.split_once('@')?;
        let mut env = BTreeMap::new();
        for p in parts {
            if let Ok(s) = std::str::from_utf8(p) {
                if let Some((k, v)) = s.split_once('=') {
                    env.insert(k.to_string(), v.to_string());
                }
            }
        }
        Some(Uevent { action: action.to_string(), devpath: devpath.to_string(), env })
    }

    /// A field.
    pub fn get(&self, k: &str) -> Option<&str> {
        self.env.get(k).map(String::as_str)
    }
}

/// What a uevent means for rinky.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UeventSignal {
    /// The hall sensor: cover closed (`true`) or opened.
    Cover(bool),
    /// A power supply changed (charger plugged, battery level): re-read sysfs.
    PowerSupply(String),
    /// A USB gadget or controller changed: re-read the cable state.
    Usb,
    /// The power key's PMIC (`rohm-bd71828`) reported a press (`notifyUserTrigger`).
    PowerButton,
}

/// Classifies a uevent (rules from `/lib/udev/rules.d`: `KERNEL=="*eink_hall*"` with
/// `HALLSENSOR=opened|closed`; `DRIVER=="rohm-bd71828"` for the power button).
pub fn classify(u: &Uevent) -> Option<UeventSignal> {
    if let Some(h) = u.get("HALLSENSOR") {
        return match h {
            "closed" => Some(UeventSignal::Cover(true)),
            "opened" => Some(UeventSignal::Cover(false)),
            _ => None,
        };
    }
    if u.get("SUBSYSTEM") == Some("power_supply") {
        return Some(UeventSignal::PowerSupply(u.get("POWER_SUPPLY_NAME").unwrap_or("").to_string()));
    }
    if matches!(u.get("SUBSYSTEM"), Some("udc") | Some("usb") | Some("android_usb")) || u.devpath.contains(".usb") {
        return Some(UeventSignal::Usb);
    }
    if u.get("DRIVER") == Some("rohm-bd71828") {
        return Some(UeventSignal::PowerButton);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lipc::LipcEvent;

    fn pe(publisher: &str, line: &str) -> PublishedEvent {
        PublishedEvent { publisher: publisher.into(), event: LipcEvent::parse(line).unwrap() }
    }

    #[test]
    fn lipc_events_map_to_input() {
        assert_eq!(map_published(&pe("com.lab126.powerd", "goingToScreenSaver 2")), vec![InputEvent::Power(PowerEvent::GoingToScreenSaver)]);
        assert_eq!(map_published(&pe("com.lab126.powerd", "battLevelChanged 57")), vec![InputEvent::Power(PowerEvent::BatteryLevel(57))]);
        assert_eq!(map_published(&pe("com.lab126.hal", "usbPlugIn")), vec![InputEvent::Usb(true)]);
        assert_eq!(map_published(&pe("com.lab126.hal", "usbPlugOut")), vec![InputEvent::Usb(false)]);
        assert_eq!(map_published(&pe("com.lab126.hal", "magSensorClosed")), vec![InputEvent::Power(PowerEvent::Cover(true))]);
        assert_eq!(map_published(&pe("com.lab126.hal", "magSensorOpened")), vec![InputEvent::Power(PowerEvent::Cover(false))]);
        assert_eq!(
            map_published(&pe("com.lab126.wifid", r#"[18:43:49.320945] cmStateChange "CONNECTED" "#)),
            vec![InputEvent::Power(PowerEvent::WifiState("CONNECTED".into()))]
        );
        assert_eq!(map_published(&pe("com.lab126.time", "timeChanged -90")), vec![InputEvent::Power(PowerEvent::TimeChanged(-90))]);
        assert_eq!(
            map_published(&pe("com.lab126.cmd", r#"connectionAvailable "wifi" "internet""#)),
            vec![InputEvent::Power(PowerEvent::Lipc {
                publisher: "com.lab126.cmd".into(),
                name: "connectionAvailable".into(),
                args: vec!["wifi".into(), "internet".into()]
            })]
        );
        assert!(matches!(&map_published(&pe("com.lab126.volumd", "userstoreAvailable"))[0], InputEvent::Power(PowerEvent::Lipc { name, .. }) if name == "userstoreAvailable"));
    }

    #[test]
    fn cover_events_from_hal_and_powerd() {
        assert_eq!(cover_from_published(&pe("com.lab126.hal", "magSensorClosed")), Some((true, CoverSource::LipcHal)));
        assert_eq!(cover_from_published(&pe("com.lab126.hal", "magSensorOpened")), Some((false, CoverSource::LipcHal)));
        assert_eq!(cover_from_published(&pe("com.lab126.powerd", "goingToScreenSaver 4")), Some((true, CoverSource::Powerd(4))));
        assert_eq!(cover_from_published(&pe("com.lab126.powerd", "outOfScreenSaver 6")), Some((false, CoverSource::Powerd(6))));
        // Button and timeout sources, other events and publishers are not the cover.
        assert_eq!(cover_from_published(&pe("com.lab126.powerd", "goingToScreenSaver 2")), None);
        assert_eq!(cover_from_published(&pe("com.lab126.powerd", "outOfScreenSaver 1")), None);
        assert_eq!(cover_from_published(&pe("com.lab126.powerd", "goingToScreenSaver")), None);
        assert_eq!(cover_from_published(&pe("com.lab126.powerd", "readyToSuspend 4")), None);
        assert_eq!(cover_from_published(&pe("com.lab126.hal", "powerButtonPressed")), None);
        assert_eq!(cover_from_published(&pe("com.lab126.wifid", "magSensorClosed")), None);
        assert_eq!(CoverSource::Powerd(4).to_string(), "powerd(4)");
        assert_eq!(CoverSource::Uevent.to_string(), "uevent");
        assert_eq!(CoverSource::LipcHal.to_string(), "lipc-hal");
    }

    #[test]
    fn hall_sysfs_info() {
        let dir = crate::sysfs::test_dir("hall");
        let sys = crate::sysfs::SysRoot::at(&dir);
        let none = HallInfo::read(&sys);
        assert!(!none.present && !none.disabled());
        assert!(none.describe().contains("no hall sensor"));
        crate::sysfs::write_tree(&dir, &[("sys/devices/platform/eink_hall/hall_enable", "1\n"), ("sys/devices/platform/eink_hall/hall_detect", "0\n")]).unwrap();
        let on = HallInfo::read(&sys);
        assert_eq!((on.present, on.enable.as_deref(), on.detect.as_deref(), on.hw_state.as_deref()), (true, Some("1"), Some("0"), None));
        assert!(!on.disabled());
        assert_eq!(on.describe(), "cover: hall sensor /sys/devices/platform/eink_hall: hall_enable=1 hall_detect=0 hall_hw_state=?");
        crate::sysfs::write_tree(&dir, &[("sys/devices/platform/eink_hall/hall_enable", "0\n")]).unwrap();
        let off = HallInfo::read(&sys);
        assert!(off.disabled() && off.describe().contains("DISABLED"));
        // Reading never writes: the flag is still 0.
        assert_eq!(std::fs::read_to_string(dir.join("sys/devices/platform/eink_hall/hall_enable")).unwrap(), "0\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hall_uevent_opened() {
        let u = Uevent::parse(b"change@/devices/platform/eink_hall\0ACTION=change\0HALLSENSOR=opened\0").unwrap();
        assert_eq!(classify(&u), Some(UeventSignal::Cover(false)));
        assert_eq!(classify(&Uevent::parse(b"change@/devices/platform/eink_hall\0HALLSENSOR=weird\0").unwrap()), None);
    }

    #[test]
    fn uevents() {
        let hall = b"change@/devices/platform/eink_hall\0ACTION=change\0DEVPATH=/devices/platform/eink_hall\0SUBSYSTEM=platform\0HALLSENSOR=closed\0SEQNUM=1234\0";
        let u = Uevent::parse(hall).unwrap();
        assert_eq!(u.action, "change");
        assert_eq!(classify(&u), Some(UeventSignal::Cover(true)));
        let psy = b"change@/devices/platform/bd71827-power.4.auto/power_supply/bd71827_ac\0ACTION=change\0SUBSYSTEM=power_supply\0POWER_SUPPLY_NAME=bd71827_ac\0POWER_SUPPLY_ONLINE=1\0";
        assert_eq!(classify(&Uevent::parse(psy).unwrap()), Some(UeventSignal::PowerSupply("bd71827_ac".into())));
        let key = b"change@/devices/platform/10019000.i2c/i2c-1/1-004b\0ACTION=change\0DRIVER=rohm-bd71828\0";
        assert_eq!(classify(&Uevent::parse(key).unwrap()), Some(UeventSignal::PowerButton));
        let udc = b"change@/devices/platform/11211000.usb/udc/11211000.usb\0ACTION=change\0SUBSYSTEM=udc\0";
        assert_eq!(classify(&Uevent::parse(udc).unwrap()), Some(UeventSignal::Usb));
        assert_eq!(Uevent::parse(b"libudev\0\xfe\xed"), None);
        assert_eq!(classify(&Uevent::parse(b"add@/devices/x\0SUBSYSTEM=block\0").unwrap()), None);
    }
}
