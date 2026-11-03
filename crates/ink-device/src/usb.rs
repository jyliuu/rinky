//! USB cable detection and the USB drive-mode state machine (platform independent;
//! the Kindle backend feeds it events and executes its actions).
//!
//! Facts (5.19.5; emulator and firmware files, `out/evidence/device/`):
//! - udev sends `com.lab126.hal usbPlugIn/usbPlugOut` (`/lib/udev/bin/notifyusb`) and the
//!   gadget scripts `usbConfigured/usbUnconfigured` (`/usr/bin/mtp.sh`); these work with
//!   the framework stopped (lipc-daemon/udev, not the framework).
//! - The PW12 exports the userstore over **MTP** (`tizen-mtp`, started with `lab126`,
//!   not `lab126_gui`); volumd starts/stops MTP on plug events ("mtp mode:
//!   prv_start_mtp_mode") and reports `driveModeState` (OFF/STARTING/ON/STOPPING). Its
//!   `disableMounting` property makes it skip that ("plugin: mounting disabled
//!   skipping") and turns MTP off: that is charge-only. Older firmware unmounts the
//!   userstore for mass storage after `userstoreReadyToUnMount`; `userstoreAvailable`
//!   / `userstoreUnAvailable` events and `userstoreIsAvailable` report it.
//! - The emulator uses the USB port for networking (`volumd useUsbForNetwork 1`, MTP
//!   and usbnetd jobs disabled). QMP `set_link usb off/on` changes only
//!   `/sys/class/udc/11211000.usb/state` (`not attached` / `configured`): no hal,
//!   powerd or volumd events, and the PMIC sees no VBUS change.
//!
//! So the cable is detected from several sources ([`cable_from`]), and drive mode is a
//! state machine over volumd ([`UsbManager`]) whose real export is a device test.

use crate::sysfs::SysRoot;
use std::time::{Duration, Instant};

/// What is at the other end of the cable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsbCable {
    /// No cable.
    #[default]
    None,
    /// Power only (wall charger, or a computer that did not configure the gadget).
    Charger,
    /// A computer configured the USB gadget.
    Computer,
}

impl UsbCable {
    /// Any cable.
    pub fn present(self) -> bool {
        self != UsbCable::None
    }
}

/// The first UDC (`/sys/class/udc/NAME`) and its `state`.
pub fn read_udc(root: &SysRoot) -> Option<(String, String)> {
    let name = root.list("/sys/class/udc").into_iter().next()?;
    let state = root.read(&format!("/sys/class/udc/{name}/state")).ok()?;
    Some((name, state))
}

/// Combines the sources: UDC gadget state (`configured`, `addressed`, `default`,
/// `powered`, `attached`, `not attached`, `suspended`), a charger supply `online`, and
/// the last hal plug event.
pub fn cable_from(udc_state: Option<&str>, external_power: bool, hal_plugged: Option<bool>) -> UsbCable {
    let udc = udc_state.map(str::trim);
    match udc {
        Some("configured") | Some("addressed") => UsbCable::Computer,
        Some("default") | Some("powered") | Some("attached") | Some("suspended") => UsbCable::Charger,
        _ if external_power || hal_plugged == Some(true) => UsbCable::Charger,
        _ => UsbCable::None,
    }
}

/// volumd's `driveModeState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DriveModeState {
    /// The userstore is ours (`DRIVE_MODE_STATE_OFF`, 0).
    #[default]
    Off,
    /// Export starting.
    Starting,
    /// The computer has the userstore (MTP session or mass storage).
    On,
    /// Export ending.
    Stopping,
}

impl DriveModeState {
    /// From volumd's integer. Only 0 = off was observed; the order of the others is
    /// assumed from volumd's strings (OFF, ON, STARTING, STOPPING), and any non-zero
    /// value is treated as "the computer may write files".
    pub fn from_volumd(v: i64) -> DriveModeState {
        match v {
            0 => DriveModeState::Off,
            2 => DriveModeState::Starting,
            3 => DriveModeState::Stopping,
            _ => DriveModeState::On,
        }
    }

    /// The computer may change files now.
    pub fn active(self) -> bool {
        self != DriveModeState::Off
    }
}

/// How the userstore is exported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbTransport {
    /// MTP (`tizen-mtp`; PW12 and current firmware): the userstore stays mounted.
    Mtp,
    /// USB mass storage (older firmware): the userstore is unmounted while exported.
    MassStorage,
    /// The port is used for USB networking (`useUsbForNetwork 1`, e.g. the emulator):
    /// no export possible.
    Network,
}

/// What to do when a computer is connected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsbPolicy {
    /// Keep the userstore and let the app ask (charge only until it chooses).
    #[default]
    Ask,
    /// Never export.
    ChargeOnly,
    /// Export at once (the stock behaviour).
    DriveMode,
}

/// The volumd operations the state machine needs (fake in tests, lipc on the Kindle).
pub trait Volumd {
    /// `disableMounting` (1 = no export on plug-in; turns MTP off).
    fn set_disable_mounting(&mut self, on: bool) -> crate::Result<()>;
    /// Current `disableMounting`.
    fn disable_mounting(&self) -> crate::Result<bool>;
    /// `driveModeState`.
    fn drive_mode_state(&self) -> crate::Result<i64>;
    /// `userstoreIsAvailable`.
    fn userstore_available(&self) -> crate::Result<bool>;
    /// `userstoreReadyToUnMount 1` (our files are closed; mass storage may proceed).
    fn ready_to_unmount(&mut self) -> crate::Result<()>;
}

/// Something the backend must do for the state machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsbAction {
    /// Show the drive-mode lock screen (the computer owns the files).
    ShowLockScreen,
    /// Remove it and redraw what was there.
    HideLockScreen,
    /// Deliver this event to the app.
    Emit(crate::input::PowerEvent),
    /// Deliver a cable change to the app (`InputEvent::Usb`).
    Cable(bool),
    /// Record a property change for the launcher to undo after a crash
    /// (`publisher property value`).
    Journal(String, String, i64),
    /// A message for the log.
    Log(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Exporting(Instant),
    Exported,
    Returning(Instant),
}

/// How long volumd may take to start or stop an export.
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(20);

/// The USB state machine.
pub struct UsbManager<V: Volumd> {
    volumd: V,
    /// What to do on plug-in.
    pub policy: UsbPolicy,
    transport: UsbTransport,
    cable: UsbCable,
    phase: Phase,
    original_disable: Option<bool>,
    charge_only: bool,
}

impl<V: Volumd> UsbManager<V> {
    /// Takes control: unless the policy is `DriveMode`, sets `disableMounting 1` so
    /// volumd does not export before the user chooses (the old value is journaled for
    /// the launcher and restored by [`UsbManager::release`]).
    pub fn new(mut volumd: V, policy: UsbPolicy, transport: UsbTransport, cable: UsbCable) -> (Self, Vec<UsbAction>) {
        let mut acts = Vec::new();
        let original = volumd.disable_mounting().ok();
        let want = policy != UsbPolicy::DriveMode;
        if original.is_some() && original != Some(want) {
            match volumd.set_disable_mounting(want) {
                Ok(()) => {
                    acts.push(UsbAction::Journal(
                        "com.lab126.volumd".into(),
                        "disableMounting".into(),
                        original.unwrap_or(false) as i64,
                    ));
                    acts.push(UsbAction::Log(format!("usb: disableMounting {} (was {})", want as i64, original.unwrap_or(false) as i64)));
                }
                Err(e) => acts.push(UsbAction::Log(format!("usb: cannot set disableMounting: {e}"))),
            }
        }
        let mut m = UsbManager {
            volumd,
            policy,
            transport,
            cable,
            phase: Phase::Idle,
            original_disable: original,
            charge_only: policy == UsbPolicy::ChargeOnly,
        };
        acts.push(UsbAction::Log(format!("usb: transport {transport:?}, cable {cable:?}, policy {policy:?}")));
        if let Ok(v) = m.volumd.drive_mode_state() {
            if DriveModeState::from_volumd(v).active() {
                // Already exported when we started (e.g. stock UI left it on).
                m.phase = Phase::Exported;
                acts.push(UsbAction::ShowLockScreen);
                acts.push(UsbAction::Emit(crate::input::PowerEvent::DriveMode(DriveModeState::On)));
            }
        }
        (m, acts)
    }

    /// The transport.
    pub fn transport(&self) -> UsbTransport {
        self.transport
    }

    /// The cable as last seen.
    pub fn cable(&self) -> UsbCable {
        self.cable
    }

    /// True while the computer owns the files (lock screen shown).
    pub fn exported(&self) -> bool {
        matches!(self.phase, Phase::Exporting(_) | Phase::Exported | Phase::Returning(_))
    }

    /// The drive-mode state as the app should see it.
    pub fn state(&self) -> DriveModeState {
        match self.phase {
            Phase::Idle => DriveModeState::Off,
            Phase::Exporting(_) => DriveModeState::Starting,
            Phase::Exported => DriveModeState::On,
            Phase::Returning(_) => DriveModeState::Stopping,
        }
    }

    /// Charge-only was chosen for this cable.
    pub fn charge_only(&self) -> bool {
        self.charge_only
    }

    /// Access to the volumd implementation (tests).
    pub fn volumd(&mut self) -> &mut V {
        &mut self.volumd
    }

    /// A new cable reading.
    pub fn cable_changed(&mut self, cable: UsbCable, now: Instant) -> Vec<UsbAction> {
        let mut acts = Vec::new();
        if cable == self.cable {
            return acts;
        }
        let was = self.cable;
        self.cable = cable;
        acts.push(UsbAction::Log(format!("usb: cable {was:?} -> {cable:?}")));
        if was.present() != cable.present() {
            acts.push(UsbAction::Cable(cable.present()));
        }
        if !cable.present() {
            // Unplugged: the export ends whatever volumd says; reset the choice.
            self.charge_only = self.policy == UsbPolicy::ChargeOnly;
            if self.exported() {
                acts.extend(self.finish(now, "cable unplugged"));
            }
        } else if cable == UsbCable::Computer && self.policy == UsbPolicy::DriveMode && !self.exported() {
            acts.extend(self.set_drive_mode(true, now));
        }
        acts
    }

    /// The app chose drive mode (`true`) or asks to end it (`false`).
    pub fn set_drive_mode(&mut self, on: bool, now: Instant) -> Vec<UsbAction> {
        let mut acts = Vec::new();
        if on {
            if self.exported() {
                return acts;
            }
            if self.transport == UsbTransport::Network {
                acts.push(UsbAction::Log("usb: drive mode unavailable: the USB port is used for networking (useUsbForNetwork 1)".into()));
            }
            self.charge_only = false;
            if let Err(e) = self.volumd.set_disable_mounting(false) {
                acts.push(UsbAction::Log(format!("usb: disableMounting 0 failed: {e}")));
            }
            if self.transport == UsbTransport::MassStorage {
                if let Err(e) = self.volumd.ready_to_unmount() {
                    acts.push(UsbAction::Log(format!("usb: userstoreReadyToUnMount failed: {e}")));
                }
            }
            self.phase = Phase::Exporting(now);
            acts.push(UsbAction::ShowLockScreen);
            acts.push(UsbAction::Emit(crate::input::PowerEvent::DriveMode(DriveModeState::Starting)));
            acts.push(UsbAction::Log("usb: drive mode requested (disableMounting 0)".into()));
        } else if self.exported() {
            if let Err(e) = self.volumd.set_disable_mounting(true) {
                acts.push(UsbAction::Log(format!("usb: disableMounting 1 failed: {e}")));
            }
            self.phase = Phase::Returning(now);
            acts.push(UsbAction::Emit(crate::input::PowerEvent::DriveMode(DriveModeState::Stopping)));
            acts.push(UsbAction::Log("usb: drive mode end requested (disableMounting 1)".into()));
        }
        acts
    }

    /// The app chose charge only for this cable.
    pub fn set_charge_only(&mut self, on: bool, now: Instant) -> Vec<UsbAction> {
        self.charge_only = on;
        let mut acts = Vec::new();
        if on && self.exported() {
            acts.extend(self.set_drive_mode(false, now));
        }
        if on {
            match self.volumd.set_disable_mounting(true) {
                Ok(()) => acts.push(UsbAction::Log("usb: charge only (disableMounting 1, no export)".into())),
                Err(e) => acts.push(UsbAction::Log(format!("usb: disableMounting 1 failed: {e}"))),
            }
        }
        acts
    }

    /// A volumd or hal event (`userstoreAvailable`, `userstoreUnAvailable`,
    /// `usbConfigured`, `usbUnconfigured`, ...).
    pub fn lipc_event(&mut self, name: &str, now: Instant) -> Vec<UsbAction> {
        match name {
            "userstoreUnAvailable" if matches!(self.phase, Phase::Exporting(_)) => {
                self.phase = Phase::Exported;
                vec![UsbAction::Emit(crate::input::PowerEvent::DriveMode(DriveModeState::On))]
            }
            "userstoreAvailable" | "usbUnconfigured" if self.exported() && !matches!(self.phase, Phase::Exporting(_)) => {
                self.finish(now, name)
            }
            _ => Vec::new(),
        }
    }

    /// Polls volumd while an export starts or ends (call about once a second while
    /// [`UsbManager::busy`]).
    pub fn tick(&mut self, now: Instant) -> Vec<UsbAction> {
        let state = self.volumd.drive_mode_state().map(DriveModeState::from_volumd);
        let available = self.volumd.userstore_available().unwrap_or(true);
        match self.phase {
            Phase::Exporting(since) => match state {
                Ok(s) if s == DriveModeState::On || (self.transport == UsbTransport::MassStorage && !available) => {
                    self.phase = Phase::Exported;
                    vec![
                        UsbAction::Emit(crate::input::PowerEvent::DriveMode(DriveModeState::On)),
                        UsbAction::Log(format!("usb: exported (driveModeState {s:?}, userstoreIsAvailable {available})")),
                    ]
                }
                _ if now.duration_since(since) >= EXPORT_TIMEOUT => {
                    let mut a = vec![UsbAction::Log(format!(
                        "usb: export did not start within {EXPORT_TIMEOUT:?} (driveModeState {state:?}); back to charge only"
                    ))];
                    let _ = self.volumd.set_disable_mounting(true);
                    a.extend(self.finish(now, "export timeout"));
                    a
                }
                _ => Vec::new(),
            },
            Phase::Exported => match state {
                Ok(DriveModeState::Off) if available => self.finish(now, "volumd drive mode off"),
                _ => Vec::new(),
            },
            Phase::Returning(since) => match state {
                Ok(DriveModeState::Off) if available => self.finish(now, "drive mode stopped"),
                _ if now.duration_since(since) >= EXPORT_TIMEOUT && available => self.finish(now, "stop timeout"),
                _ => Vec::new(),
            },
            Phase::Idle => Vec::new(),
        }
    }

    /// True while waiting for volumd (poll with [`UsbManager::tick`]).
    pub fn busy(&self) -> bool {
        self.exported()
    }

    fn finish(&mut self, _now: Instant, why: &str) -> Vec<UsbAction> {
        self.phase = Phase::Idle;
        if self.policy != UsbPolicy::DriveMode {
            let _ = self.volumd.set_disable_mounting(true);
        }
        vec![
            UsbAction::HideLockScreen,
            UsbAction::Emit(crate::input::PowerEvent::DriveMode(DriveModeState::Off)),
            UsbAction::Emit(crate::input::PowerEvent::LibraryRescan),
            UsbAction::Log(format!("usb: drive mode ended ({why}); library rescan")),
        ]
    }

    /// Gives `disableMounting` back its value from before [`UsbManager::new`].
    pub fn release(&mut self) -> Vec<UsbAction> {
        let mut acts = Vec::new();
        if let Some(orig) = self.original_disable.take() {
            match self.volumd.set_disable_mounting(orig) {
                Ok(()) => acts.push(UsbAction::Log(format!("usb: disableMounting restored to {}", orig as i64))),
                Err(e) => acts.push(UsbAction::Log(format!("usb: restoring disableMounting failed: {e}"))),
            }
        }
        acts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::PowerEvent;
    use crate::sysfs::{test_dir, write_tree};

    #[derive(Default)]
    struct FakeVolumd {
        disable: bool,
        drive: i64,
        available: bool,
        ready_calls: u32,
        log: Vec<String>,
    }

    impl Volumd for FakeVolumd {
        fn set_disable_mounting(&mut self, on: bool) -> crate::Result<()> {
            self.disable = on;
            self.log.push(format!("disableMounting {}", on as i64));
            Ok(())
        }
        fn disable_mounting(&self) -> crate::Result<bool> {
            Ok(self.disable)
        }
        fn drive_mode_state(&self) -> crate::Result<i64> {
            Ok(self.drive)
        }
        fn userstore_available(&self) -> crate::Result<bool> {
            Ok(self.available)
        }
        fn ready_to_unmount(&mut self) -> crate::Result<()> {
            self.ready_calls += 1;
            Ok(())
        }
    }

    fn fake() -> FakeVolumd {
        FakeVolumd { available: true, ..Default::default() }
    }

    fn emits(a: &[UsbAction]) -> Vec<PowerEvent> {
        a.iter().filter_map(|x| if let UsbAction::Emit(e) = x { Some(e.clone()) } else { None }).collect()
    }

    #[test]
    fn cable_sources() {
        assert_eq!(cable_from(Some("configured"), false, None), UsbCable::Computer);
        assert_eq!(cable_from(Some("not attached"), false, None), UsbCable::None);
        assert_eq!(cable_from(Some("not attached"), true, None), UsbCable::Charger);
        assert_eq!(cable_from(None, false, Some(true)), UsbCable::Charger);
        assert_eq!(cable_from(Some("powered"), false, None), UsbCable::Charger);
        assert_eq!(cable_from(None, false, Some(false)), UsbCable::None);
        let d = test_dir("udc");
        write_tree(&d, &[("/sys/class/udc/11211000.usb/state", "configured\n")]).unwrap();
        assert_eq!(read_udc(&SysRoot::at(&d)), Some(("11211000.usb".into(), "configured".into())));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn startup_takes_and_release_restores_disable_mounting() {
        let (mut m, acts) = UsbManager::new(fake(), UsbPolicy::Ask, UsbTransport::Mtp, UsbCable::None);
        assert!(m.volumd().disable);
        assert!(acts.contains(&UsbAction::Journal("com.lab126.volumd".into(), "disableMounting".into(), 0)));
        m.release();
        assert!(!m.volumd().disable);
        // Policy DriveMode leaves volumd alone.
        let (mut m, acts) = UsbManager::new(fake(), UsbPolicy::DriveMode, UsbTransport::Mtp, UsbCable::None);
        assert!(!m.volumd().disable);
        assert!(!acts.iter().any(|a| matches!(a, UsbAction::Journal(..))));
    }

    #[test]
    fn mtp_drive_mode_cycle_with_lock_screen_and_rescan() {
        let t0 = Instant::now();
        let (mut m, _) = UsbManager::new(fake(), UsbPolicy::Ask, UsbTransport::Mtp, UsbCable::None);
        let a = m.cable_changed(UsbCable::Computer, t0);
        assert!(a.contains(&UsbAction::Cable(true)));
        assert!(!m.exported(), "Ask waits for the app");
        let a = m.set_drive_mode(true, t0);
        assert!(a.contains(&UsbAction::ShowLockScreen));
        assert_eq!(emits(&a), vec![PowerEvent::DriveMode(DriveModeState::Starting)]);
        assert!(!m.volumd().disable);
        assert!(m.tick(t0 + Duration::from_secs(1)).is_empty());
        m.volumd().drive = 1; // volumd started MTP
        let a = m.tick(t0 + Duration::from_secs(2));
        assert_eq!(emits(&a), vec![PowerEvent::DriveMode(DriveModeState::On)]);
        assert_eq!(m.state(), DriveModeState::On);
        // Unplug: lock screen off, rescan, charge-only again.
        let a = m.cable_changed(UsbCable::None, t0 + Duration::from_secs(30));
        assert!(a.contains(&UsbAction::Cable(false)) && a.contains(&UsbAction::HideLockScreen));
        assert_eq!(
            emits(&a),
            vec![PowerEvent::DriveMode(DriveModeState::Off), PowerEvent::LibraryRescan]
        );
        assert!(m.volumd().disable);
        assert!(!m.exported());
    }

    #[test]
    fn mass_storage_waits_for_unmount_and_remount() {
        let t0 = Instant::now();
        let (mut m, _) = UsbManager::new(fake(), UsbPolicy::Ask, UsbTransport::MassStorage, UsbCable::Computer);
        m.set_drive_mode(true, t0);
        assert_eq!(m.volumd().ready_calls, 1, "userstoreReadyToUnMount sent");
        let a = m.lipc_event("userstoreUnAvailable", t0);
        assert_eq!(emits(&a), vec![PowerEvent::DriveMode(DriveModeState::On)]);
        assert!(m.lipc_event("somethingElse", t0).is_empty());
        let a = m.lipc_event("userstoreAvailable", t0 + Duration::from_secs(60));
        assert!(emits(&a).contains(&PowerEvent::LibraryRescan));
    }

    #[test]
    fn app_can_end_drive_mode_and_choose_charge_only() {
        let t0 = Instant::now();
        let (mut m, _) = UsbManager::new(fake(), UsbPolicy::Ask, UsbTransport::Mtp, UsbCable::Computer);
        m.set_drive_mode(true, t0);
        m.volumd().drive = 1;
        m.tick(t0);
        let a = m.set_drive_mode(false, t0);
        assert_eq!(emits(&a), vec![PowerEvent::DriveMode(DriveModeState::Stopping)]);
        assert!(m.volumd().disable);
        assert!(m.tick(t0).is_empty(), "still on in volumd");
        m.volumd().drive = 0;
        assert!(emits(&m.tick(t0)).contains(&PowerEvent::LibraryRescan));
        m.set_charge_only(true, t0);
        assert!(m.charge_only() && m.volumd().disable);
    }

    #[test]
    fn export_that_never_starts_times_out() {
        let t0 = Instant::now();
        let (mut m, _) = UsbManager::new(fake(), UsbPolicy::Ask, UsbTransport::Network, UsbCable::Computer);
        let a = m.set_drive_mode(true, t0);
        assert!(a.iter().any(|x| matches!(x, UsbAction::Log(s) if s.contains("networking"))));
        assert!(m.tick(t0 + Duration::from_secs(5)).is_empty());
        let a = m.tick(t0 + EXPORT_TIMEOUT);
        assert!(a.contains(&UsbAction::HideLockScreen));
        assert!(!m.exported());
        assert!(m.volumd().disable);
    }

    #[test]
    fn policy_drive_mode_exports_on_computer_plug() {
        let t0 = Instant::now();
        let (mut m, _) = UsbManager::new(fake(), UsbPolicy::DriveMode, UsbTransport::Mtp, UsbCable::None);
        assert!(m.cable_changed(UsbCable::Charger, t0).iter().all(|a| !matches!(a, UsbAction::ShowLockScreen)));
        let a = m.cable_changed(UsbCable::Computer, t0);
        assert!(a.contains(&UsbAction::ShowLockScreen));
        assert_eq!(DriveModeState::from_volumd(0), DriveModeState::Off);
        assert!(DriveModeState::from_volumd(7).active());
    }
}
