//! The gadget against a fake configfs tree in a temp folder (it behaves like configfs
//! where it matters: attribute files appear on mkdir, rmdir needs the folder free of
//! subfolders and links, a controller binds to one gadget at a time), and the plugin's
//! tile, settings page, autostart and exit.

use super::*;
use gadget::{Error, Paths, Sys};
use ink_plugin::testing::TempDir;
use ink_plugin::{MemoryStorage, Registrar, UiQueue, UiRequest};
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const UDC: &str = "ci_hdrc.0";

fn tmp(tag: &str) -> TempDir {
    TempDir::new(&format!("usbnet-{tag}"))
}

/// The fake device's switches and the commands run.
#[derive(Default)]
struct Log {
    calls: Vec<String>,
    /// Modules whose modprobe fails.
    bad_modules: Vec<&'static str>,
    /// `ifconfig ... up` fails.
    bad_ifconfig: bool,
    /// Modules loaded.
    loaded: Vec<String>,
    /// The controller refuses this many binds (EBUSY) even when free, as right after
    /// MTP let go or without a cable.
    refuse_binds: u32,
    /// MTP binds itself again this many times right after being unbound (`mtp.sh`
    /// reacting to the computer seeing the port go away).
    mtp_rebinds: u32,
    /// The waits between bind tries.
    sleeps: Vec<std::time::Duration>,
}

struct FakeSys {
    gadgets: PathBuf,
    udc: PathBuf,
    log: Rc<RefCell<Log>>,
}

fn err(kind: io::ErrorKind, what: &str) -> io::Error {
    io::Error::new(kind, what.to_string())
}

impl Sys for FakeSys {
    fn run(&mut self, program: &str, args: &[&str]) -> std::result::Result<(), String> {
        let mut log = self.log.borrow_mut();
        log.calls.push(format!("{program} {}", args.join(" ")));
        match (program, args) {
            ("modprobe", [m]) if log.bad_modules.contains(m) => Err(format!("modprobe: FATAL: Module {m} not found")),
            ("modprobe", ["-r", m]) if log.bad_modules.contains(m) => Err(format!("modprobe: FATAL: Module {m} is builtin.")),
            ("modprobe", ["-r", m]) => {
                // The legacy gadget lets go of the controller.
                for d in std::fs::read_dir(&self.udc).unwrap() {
                    let f = d.unwrap().path().join("function");
                    if std::fs::read_to_string(&f).is_ok_and(|s| s.trim() == *m) {
                        std::fs::remove_file(f).unwrap();
                        log.refuse_binds = 0;
                    }
                }
                Ok(())
            }
            ("modprobe", [m]) => {
                log.loaded.push(m.to_string());
                Ok(())
            }
            ("ifconfig", [.., "up"]) if log.bad_ifconfig => Err("ifconfig: SIOCSIFADDR: No such device".into()),
            _ => Ok(()),
        }
    }

    fn mkdir(&mut self, path: &Path) -> io::Result<()> {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let parent = path.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        if parent == "functions" && name.starts_with("ecm.") && !self.log.borrow().loaded.iter().any(|m| m == "usb_f_ecm") {
            return Err(err(io::ErrorKind::NotFound, "No such file or directory"));
        }
        if path.is_dir() {
            return Ok(());
        }
        std::fs::create_dir_all(path)?;
        // The kernel's attribute files.
        let attrs: &[(&str, &str)] = match (parent.as_str(), name.as_str()) {
            ("usb_gadget", _) => &[("UDC", "\n"), ("idVendor", "0x0000\n"), ("idProduct", "0x0000\n"), ("bcdUSB", "0x0000\n"), ("bcdDevice", "0x0000\n")],
            ("functions", _) => &[("ifname", "usb0\n"), ("dev_addr", "00:00:00:00:00:00\n"), ("host_addr", "00:00:00:00:00:00\n"), ("qmult", "5\n")],
            ("configs", _) => &[("MaxPower", "2\n"), ("bmAttributes", "0x80\n")],
            _ => &[],
        };
        for (f, v) in attrs {
            std::fs::write(path.join(f), v)?;
        }
        for g in default_groups(path) {
            std::fs::create_dir_all(path.join(g))?;
        }
        Ok(())
    }

    fn rmdir(&mut self, path: &Path) -> io::Result<()> {
        // Only attributes and the kernel's own (empty) default groups may be inside.
        let groups = default_groups(path);
        for e in std::fs::read_dir(path)? {
            let e = e?;
            let t = e.file_type()?;
            let group = t.is_dir() && groups.iter().any(|g| e.file_name() == *g) && std::fs::read_dir(e.path())?.next().is_none();
            if (t.is_dir() && !group) || t.is_symlink() {
                return Err(err(io::ErrorKind::Other, "Directory not empty"));
            }
        }
        std::fs::remove_dir_all(path)
    }

    fn sleep(&mut self, d: std::time::Duration) {
        self.log.borrow_mut().sleeps.push(d);
    }

    fn write(&mut self, path: &Path, value: &str) -> io::Result<()> {
        let mtp = path.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "mtpgadget");
        if mtp && path.file_name().is_some_and(|n| n == "UDC") && value.is_empty() {
            let mut log = self.log.borrow_mut();
            if log.mtp_rebinds > 0 {
                log.mtp_rebinds -= 1;
                return Ok(()); // unbound and at once bound again
            }
        }
        if path.file_name().is_some_and(|n| n == "UDC") && !value.is_empty() {
            {
                let mut log = self.log.borrow_mut();
                if log.refuse_binds > 0 && !mtp {
                    log.refuse_binds -= 1;
                    return Err(err(io::ErrorKind::ResourceBusy, "Device or resource busy"));
                }
            }
            if !self.udc.join(value).exists() {
                return Err(err(io::ErrorKind::NotFound, "No such device"));
            }
            for g in std::fs::read_dir(&self.gadgets)? {
                let f = g?.path().join("UDC");
                if f != path && std::fs::read_to_string(&f).is_ok_and(|s| s.trim() == value) {
                    return Err(err(io::ErrorKind::ResourceBusy, "Device or resource busy"));
                }
            }
        }
        std::fs::write(path, format!("{value}\n"))
    }
}

/// The folders configfs creates inside a new gadget or configuration (and removes
/// with it).
fn default_groups(path: &Path) -> &'static [&'static str] {
    match path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) {
        Some("usb_gadget") => &["configs", "functions", "strings", "os_desc"],
        Some("configs") => &["strings"],
        _ => &[],
    }
}

/// A fake device: configfs with or without the MTP gadget, one controller.
struct Device {
    root: TempDir,
    paths: Paths,
    log: Rc<RefCell<Log>>,
}

impl Device {
    fn new(tag: &str) -> Self {
        let root = tmp(tag);
        let paths = Paths { gadgets: root.join("usb_gadget"), udc: root.join("udc"), usid: root.join("usid"), power_supply: root.join("power_supply") };
        std::fs::create_dir_all(&paths.gadgets).unwrap();
        std::fs::create_dir_all(paths.udc.join(UDC)).unwrap();
        std::fs::write(&paths.usid, "G0001234567890AB\n").unwrap();
        Device { root, paths, log: Rc::default() }
    }

    /// The stock MTP gadget, bound to `udc` (or unbound for "").
    fn with_mtp(self, udc: &str) -> Self {
        let m = self.paths.gadgets.join("mtpgadget");
        std::fs::create_dir_all(m.join("functions/ffs.mtp")).unwrap();
        std::fs::write(m.join("UDC"), format!("{udc}\n")).unwrap();
        self
    }

    /// The controller's `state` and the PW12's supplies: the charger `bd71827_ac`
    /// (online with the cable in), the battery and the moisture sensor (always online).
    fn cable(&self, udc_state: &str, ac_online: bool) {
        std::fs::write(self.paths.udc.join(UDC).join("state"), format!("{udc_state}\n")).unwrap();
        for (name, ty, online) in [("bd71827_ac", "Mains", ac_online as u8), ("bd71827_bat", "Battery", 1), ("max20342_moisture", "USB_WET", 1), ("Wireless", "Wireless", 0)] {
            let d = self.paths.power_supply.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("type"), format!("{ty}\n")).unwrap();
            std::fs::write(d.join("online"), format!("{online}\n")).unwrap();
        }
    }

    /// The controller refuses the next `n` binds.
    fn refuse_binds(&self, n: u32) {
        self.log.borrow_mut().refuse_binds = n;
    }

    fn net(&self) -> UsbNet {
        let sys = FakeSys { gadgets: self.paths.gadgets.clone(), udc: self.paths.udc.clone(), log: self.log.clone() };
        UsbNet::new(self.paths.clone(), Box::new(sys))
    }

    fn file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.paths.gadgets.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut self.log.borrow_mut().calls)
    }

    fn gadget(&self) -> PathBuf {
        self.paths.gadgets.join("usbnet")
    }
}


const UP: &str = "ifconfig usb0 192.168.15.244 netmask 255.255.255.0 up";

// -- Gadget ------------------------------------------------------------------------------------

#[test]
fn on_creates_the_gadget_and_off_removes_it() {
    let dev = Device::new("onoff");
    let mut net = dev.net();
    assert!(!net.state().on());

    let on = net.on().unwrap();
    assert_eq!(on, gadget::On { udc: UDC.into(), ifname: "usb0".into(), took_mtp: None });
    assert_eq!(dev.calls(), ["modprobe libcomposite", "modprobe usb_f_ecm", UP]);
    for (f, v) in [
        ("usbnet/idVendor", "0x1d6b"),
        ("usbnet/idProduct", "0x0103"),
        ("usbnet/bcdUSB", "0x0200"),
        ("usbnet/bcdDevice", "0x0100"),
        ("usbnet/strings/0x409/manufacturer", "Amazon"),
        ("usbnet/strings/0x409/product", "Kindle USB network"),
        ("usbnet/strings/0x409/serialnumber", "G0001234567890AB"),
        ("usbnet/functions/ecm.usb0/dev_addr", "ee:19:00:00:00:01"),
        ("usbnet/functions/ecm.usb0/host_addr", "ee:19:00:00:00:02"),
        ("usbnet/configs/c.1/strings/0x409/configuration", "ECM"),
        ("usbnet/configs/c.1/MaxPower", "250"),
        ("usbnet/UDC", UDC),
    ] {
        assert_eq!(dev.file(f), format!("{v}\n"), "{f}");
    }
    let link = dev.gadget().join("configs/c.1/ecm.usb0");
    assert_eq!(std::fs::read_link(&link).unwrap(), dev.gadget().join("functions/ecm.usb0"));
    assert_eq!(net.state(), gadget::State { udc: Some(UDC.into()), ifname: Some("usb0".into()), mtp_udc: None });

    // On again: nothing is run or changed.
    assert_eq!(net.on().unwrap().took_mtp, None);
    assert_eq!(dev.calls(), Vec::<String>::new());

    // Off: unbound, interface down, link and folders gone (the fake's rmdir refuses a
    // folder with subfolders or links, so this also checks the order).
    net.off(None).unwrap();
    assert_eq!(dev.calls(), ["ifconfig usb0 down"]);
    assert!(!dev.gadget().exists());
    assert!(!net.state().on());
    // Off again: nothing to do.
    net.off(None).unwrap();
    assert_eq!(dev.calls(), Vec::<String>::new());
}

#[test]
fn on_takes_the_port_from_mtp_and_off_gives_it_back() {
    let dev = Device::new("mtp").with_mtp(UDC);
    let mut net = dev.net();
    assert_eq!(net.state().mtp_udc.as_deref(), Some(UDC));
    let on = net.on().unwrap();
    assert_eq!(on.took_mtp.as_deref(), Some(UDC));
    assert_eq!(dev.file("mtpgadget/UDC"), "\n", "MTP unbound");
    assert_eq!(dev.file("usbnet/UDC"), format!("{UDC}\n"));

    net.off(Some(UDC)).unwrap();
    assert_eq!(dev.file("mtpgadget/UDC"), format!("{UDC}\n"), "MTP bound again");
    assert!(!dev.gadget().exists() && dev.paths.gadgets.join("mtpgadget/functions/ffs.mtp").is_dir());

    // An unbound MTP gadget is left alone at on; off binds it only when asked.
    let dev = Device::new("mtp-unbound").with_mtp("");
    let mut net = dev.net();
    assert_eq!(net.on().unwrap().took_mtp, None);
    net.off(None).unwrap();
    assert_eq!(dev.file("mtpgadget/UDC"), "\n");
    // Rebinding without an MTP gadget (or with one bound) is a no-op, not an error.
    let dev = Device::new("no-mtp");
    let mut net = dev.net();
    net.on().unwrap();
    net.off(Some(UDC)).unwrap();
    assert!(!dev.paths.gadgets.join("mtpgadget").exists());
}

#[test]
fn failures_are_reported_and_undone() {
    // No ECM module: nothing is created and MTP keeps the port.
    let dev = Device::new("nomod").with_mtp(UDC);
    dev.log.borrow_mut().bad_modules.push("usb_f_ecm");
    let mut net = dev.net();
    assert!(matches!(net.on(), Err(Error::Module(m)) if m.contains("not found")));
    assert!(!dev.gadget().exists());
    assert_eq!(dev.file("mtpgadget/UDC"), format!("{UDC}\n"));

    // libcomposite failing is only logged (it may be built in).
    let dev = Device::new("nocomposite");
    dev.log.borrow_mut().bad_modules.push("libcomposite");
    assert!(dev.net().on().is_ok());

    // No controller.
    let dev = Device::new("noudc").with_mtp("");
    std::fs::remove_dir(dev.paths.udc.join(UDC)).unwrap();
    let mut net = dev.net();
    assert_eq!(net.on(), Err(Error::NoController));
    assert_eq!(Error::NoController.short(), "No USB controller");
    assert!(!dev.gadget().exists());

    // The address cannot be set: the gadget is removed again and MTP gets the port back.
    let dev = Device::new("noif").with_mtp(UDC);
    dev.log.borrow_mut().bad_ifconfig = true;
    let mut net = dev.net();
    assert!(matches!(net.on(), Err(Error::Ifconfig(_))));
    assert!(!dev.gadget().exists());
    assert_eq!(dev.file("mtpgadget/UDC"), format!("{UDC}\n"));
    assert!(!net.state().on());

    // Another gadget holds the controller: it is not taken, the error names it, and
    // everything is undone.
    let dev = Device::new("held");
    let other = dev.paths.gadgets.join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("UDC"), format!("{UDC}\n")).unwrap();
    let mut net = dev.net();
    let e = net.on().unwrap_err();
    assert_eq!(e, Error::Held(UDC.into(), "other".into()));
    assert_eq!(e.short(), "USB port in use");
    assert!(e.to_string().contains("\"other\". Turn that off first"), "{e}");
    assert_eq!(dev.file("other/UDC"), format!("{UDC}\n"), "left alone");
    assert!(!dev.gadget().exists());
    assert!(!dev.calls().iter().any(|c| c.ends_with(" up")));
}

/// The reported "USB port busy" with a computer on the cable: right after MTP lets go,
/// the controller refuses the new gadget for a moment (the shell script this is a port
/// of was slow enough not to notice). Binding is tried again after short waits.
#[test]
fn bind_waits_for_the_controller_after_mtp() {
    let dev = Device::new("settle").with_mtp(UDC);
    dev.cable("configured", true);
    dev.refuse_binds(2);
    let mut net = dev.net();
    let on = net.on().unwrap();
    assert_eq!(on.took_mtp.as_deref(), Some(UDC));
    assert_eq!(dev.file("usbnet/UDC"), format!("{UDC}\n"));
    assert_eq!(dev.log.borrow().sleeps, gadget::BIND_RETRY[..2]);
}

/// MTP binds itself again right after being unbound: it is taken again before the next
/// try, and given back at off.
#[test]
fn bind_takes_the_port_from_mtp_again() {
    let dev = Device::new("mtprace").with_mtp(UDC);
    dev.cable("configured", true);
    // Rebound after the first unbind and after the one before the first try.
    dev.log.borrow_mut().mtp_rebinds = 2;
    let mut net = dev.net();
    let on = net.on().unwrap();
    assert_eq!(on.took_mtp.as_deref(), Some(UDC));
    assert_eq!(dev.file("mtpgadget/UDC"), "\n");
    assert_eq!(dev.log.borrow().sleeps.len(), 1);
    net.off(on.took_mtp.as_deref()).unwrap();
    assert_eq!(dev.file("mtpgadget/UDC"), format!("{UDC}\n"));
}

/// What the user's PW12 showed: the Mac saw "RNDIS/Ethernet Gadget" (the kernel's
/// legacy `g_ether`) on the cable, and every bind failed. The module is unloaded first.
#[test]
fn legacy_gadget_module_is_unloaded() {
    let dev = Device::new("gether").with_mtp("");
    dev.cable("configured", true);
    dev.refuse_binds(u32::MAX);
    std::fs::write(dev.paths.udc.join(UDC).join("function"), "g_ether\n").unwrap();
    let mut net = dev.net();
    let on = net.on().unwrap();
    assert_eq!(on.ifname, "usb0");
    assert!(dev.calls().contains(&"modprobe -r g_ether".to_string()));
    assert!(dev.log.borrow().sleeps.is_empty());

    // Built in (or in use): it cannot be unloaded, and the error says which module.
    let dev = Device::new("gether-builtin").with_mtp("");
    dev.cable("configured", true);
    dev.refuse_binds(u32::MAX);
    dev.log.borrow_mut().bad_modules.push("g_ether");
    std::fs::write(dev.paths.udc.join(UDC).join("function"), "g_ether\n").unwrap();
    let e = dev.net().on().unwrap_err();
    assert!(matches!(&e, Error::Legacy(u, m, _) if u == UDC && m == "g_ether"), "{e:?}");
    assert_eq!(e.short(), "USB port in use");
    assert!(!dev.gadget().exists());

    // A configfs gadget's name in `function` is not a module.
    let dev = Device::new("function-ours").with_mtp(UDC);
    std::fs::write(dev.paths.udc.join(UDC).join("function"), "mtpgadget\n").unwrap();
    dev.net().on().unwrap();
    assert!(!dev.calls().iter().any(|c| c.starts_with("modprobe -r")));
}

/// A controller that never lets go: after all tries the error says what to do and
/// which kernel driver holds the port.
#[test]
fn bind_gives_up_with_advice() {
    let dev = Device::new("stuckport").with_mtp(UDC);
    dev.cable("configured", true);
    dev.refuse_binds(u32::MAX);
    std::fs::write(dev.paths.udc.join(UDC).join("function"), "fastboot\n").unwrap();
    let mut net = dev.net();
    let e = net.on().unwrap_err();
    assert_eq!(e.short(), "USB port busy");
    let text = e.to_string();
    assert!(text.contains("the driver fastboot") && text.contains("Unplug the cable, plug it in again"), "{text}");
    assert_eq!(dev.log.borrow().sleeps, gadget::BIND_RETRY);
    assert!(!dev.gadget().exists(), "undone");
    assert_eq!(dev.file("mtpgadget/UDC"), format!("{UDC}\n"), "MTP given back");
}

#[test]
fn off_reports_a_gadget_it_cannot_remove() {
    let dev = Device::new("stuck");
    let mut net = dev.net();
    net.on().unwrap();
    // Something else put a folder in the gadget: rmdir of the gadget fails.
    std::fs::create_dir_all(dev.gadget().join("functions/rndis.usb0")).unwrap();
    assert!(matches!(net.off(None), Err(Error::Remove(_))));
    assert!(!net.state().on(), "still unbound");
}

#[test]
fn cable_detection() {
    let dev = Device::new("cable");
    // Nothing readable: unknown, never "no cable".
    assert_eq!(dev.net().cable(), None);
    // The PW12 unplugged: controller not attached, charger offline; the moisture sensor's
    // online 1 does not count.
    dev.cable("not attached", false);
    assert_eq!(dev.net().cable(), Some(false));
    // A computer: the controller is configured (MTP bound) or only VBUS is seen.
    dev.cable("configured", true);
    assert_eq!(dev.net().cable(), Some(true));
    dev.cable("not attached", true);
    assert_eq!(dev.net().cable(), Some(true), "VBUS through the charger with no gadget bound");
    dev.cable("powered", false);
    assert_eq!(dev.net().cable(), Some(true));
    // Only the controller says something.
    std::fs::remove_dir_all(&dev.paths.power_supply).unwrap();
    dev.cable("not attached", false);
    std::fs::remove_dir_all(&dev.paths.power_supply).unwrap();
    assert_eq!(dev.net().cable(), Some(false));
}

#[test]
fn bind_failure_without_a_cable_is_not_connected() {
    let dev = Device::new("nocable");
    dev.cable("not attached", false);
    dev.refuse_binds(u32::MAX);
    let e = dev.net().on().unwrap_err();
    assert_eq!(e, Error::NoCable);
    assert_eq!(e.short(), "Not connected");
    assert!(!dev.gadget().exists(), "undone");
    // With the cable in, the same failure is the real one.
    dev.cable("configured", true);
    assert!(matches!(dev.net().on(), Err(Error::Bind(..))));
}

#[test]
fn tile_without_a_cable() {
    let dev = Device::new("tilecable").with_mtp("");
    dev.cable("not attached", false);
    let mut h = Host::new(&dev.root);
    let mut p = UsbNetPlugin::with_gadget(dev.net());
    assert_eq!(tile(&p), TileState { text: "Not connected".into(), on: Some(false) });
    p.on_action(&mut h.cx(), "settings:usbnet").unwrap();
    assert_eq!(h.menu()[0].0, "Off · no USB cable");

    // A tap that fails to bind without a cable: "Not connected" and a toast that says
    // what to do, not "USB port busy".
    dev.refuse_binds(u32::MAX);
    h.ui.0.clear();
    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert_eq!(tile(&p), TileState { text: "Not connected".into(), on: Some(false) });
    assert_eq!(h.toasts(), ["USB network: no USB cable. Connect the Kindle to a computer, then turn it on"]);

    // Plugged in: the stale "no cable" error is dropped at once.
    dev.cable("configured", true);
    assert_eq!(tile(&p).text, "Off");
    // A real bind failure with the cable in keeps its own text.
    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert_eq!(tile(&p).text, "USB port busy");
    // Unplugged again: "Not connected" says more than the old busy error.
    dev.cable("not attached", false);
    assert_eq!(tile(&p).text, "Not connected");

    // On (binding works) and then unplugged: still the address, the page adds "no cable".
    dev.refuse_binds(0);
    dev.cable("configured", true);
    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert_eq!(tile(&p).text, "192.168.15.244");
    dev.cable("not attached", false);
    assert_eq!(tile(&p).text, "192.168.15.244");
    h.ui.0.clear();
    p.on_action(&mut h.cx(), "settings:usbnet").unwrap();
    assert_eq!(h.menu()[0].0, "On · 192.168.15.244 (usb0) · no cable");
}

// -- Plugin ------------------------------------------------------------------------------------

struct Host {
    ui: UiQueue,
    store: MemoryStorage,
}

const PERMS: [Permission; 2] = [Permission::Ui, Permission::Storage];

impl Host {
    fn new(dir: &Path) -> Self {
        Host { ui: UiQueue::default(), store: MemoryStorage { dir: dir.join("data"), ..Default::default() } }
    }

    fn cx(&mut self) -> HostCtx<'_> {
        HostCtx::new(&mut self.ui, &mut self.store, &PERMS)
    }

    fn toasts(&mut self) -> Vec<String> {
        let t = self.ui.0.iter().filter_map(|r| if let UiRequest::Toast { text } = r { Some(text.clone()) } else { None }).collect();
        self.ui.0.clear();
        t
    }

    /// The last menu's items.
    fn menu(&self) -> Vec<(String, String)> {
        match self.ui.0.iter().rev().find(|r| matches!(r, UiRequest::Menu { .. })) {
            Some(UiRequest::Menu { id: MENU, items, .. }) => items.clone(),
            other => panic!("no menu: {other:?}"),
        }
    }
}

fn tile(p: &UsbNetPlugin) -> TileState {
    p.tile_state("toggle").unwrap()
}

#[test]
fn tile_toggles_and_shows_the_state() {
    let dev = Device::new("tile").with_mtp(UDC);
    let mut h = Host::new(&dev.root);
    let mut p = UsbNetPlugin::with_gadget(dev.net());
    let mut reg = ink_plugin::Registry::default();
    p.on_load(&mut Registrar { plugin: NAME, registry: &mut reg }).unwrap();
    let t = reg.control_tiles.iter().find(|t| t.id == "toggle").expect("tile");
    assert_eq!((t.label.as_str(), t.icon.as_str(), t.needs_wifi.is_none()), ("USB network", "usb", true));
    assert_eq!((reg.settings_pages[0].id.as_str(), reg.settings_pages[0].title.as_str()), ("usbnet", "USB network"));
    assert_eq!(p.manifest().permissions, PERMS);
    assert_eq!(tile(&p), TileState { text: "Off".into(), on: Some(false) });
    assert_eq!(p.tile_state("other"), None);

    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert_eq!(tile(&p), TileState { text: "192.168.15.244".into(), on: Some(true) });
    assert_eq!(h.toasts(), ["USB network on: 192.168.15.244"]);
    assert_eq!(h.store.settings.get("mtp_udc"), Some(&json!(UDC)), "taken from MTP: remembered");

    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert_eq!(tile(&p), TileState { text: "Off".into(), on: Some(false) });
    assert_eq!(h.toasts(), ["USB network off"]);
    assert_eq!(dev.file("mtpgadget/UDC"), format!("{UDC}\n"));
    assert_eq!(h.store.settings.get("mtp_udc"), None);

    // A failure: a toast with the reason and a short tile text; no Err. The next
    // success clears it.
    dev.log.borrow_mut().bad_modules.push("usb_f_ecm");
    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert_eq!(tile(&p), TileState { text: "No ECM support".into(), on: Some(false) });
    let t = h.toasts();
    assert!(t.len() == 1 && t[0].starts_with("USB network: the kernel has no USB Ethernet"), "{t:?}");
    dev.log.borrow_mut().bad_modules.clear();
    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert_eq!(tile(&p).text, "192.168.15.244");

    // Switched off outside rinky (a KUAL script): the next action sees it, so a tap
    // turns it on rather than off.
    dev.net().off(None).unwrap();
    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert!(p.is_on());
}

#[test]
fn settings_page() {
    let dev = Device::new("menu");
    let mut h = Host::new(&dev.root);
    let mut p = UsbNetPlugin::with_gadget(dev.net());
    p.on_action(&mut h.cx(), "settings:usbnet").unwrap();
    let labels = |h: &Host| h.menu().into_iter().map(|(l, a)| format!("{l}|{a}")).collect::<Vec<_>>();
    assert_eq!(labels(&h), ["Off|status", "Turn on|on", "Turn on when rinky starts\toff|autostart", "How to connect a computer|help"]);

    p.on_ui_result(&mut h.cx(), MENU, "on").unwrap();
    assert!(p.is_on());
    assert_eq!(&labels(&h)[..2], ["On · 192.168.15.244 (usb0)|status", "Turn off|off"]);

    p.on_ui_result(&mut h.cx(), MENU, "autostart").unwrap();
    assert_eq!(h.store.settings.get("autostart"), Some(&json!(true)));
    assert_eq!(labels(&h)[2], "Turn on when rinky starts\ton|autostart");

    p.on_ui_result(&mut h.cx(), MENU, "off").unwrap();
    assert!(!p.is_on());
    assert_eq!(labels(&h)[0], "Off|status");

    h.ui.0.clear();
    p.on_ui_result(&mut h.cx(), MENU, "help").unwrap();
    let Some(UiRequest::TextViewer { id: HELP, markdown, .. }) = h.ui.0.last() else { panic!("{:?}", h.ui.0) };
    for want in ["192.168.15.244", "192.168.15.201", "networksetup -setmanual \"Kindle USB network\" 192.168.15.201 255.255.255.0", "sudo ip addr add 192.168.15.201/24 dev <iface>", "Windows", "MTP", "ssh root@192.168.15.244", "SSH server"] {
        assert!(markdown.contains(want), "{want}");
    }
    assert!(ink_plugin::markdown::parse(markdown).len() > 5);
    // Closing the help page shows the settings page again; a cancelled menu shows nothing.
    h.ui.0.clear();
    p.on_ui_result(&mut h.cx(), HELP, "closed").unwrap();
    assert_eq!(h.menu().len(), 4);
    h.ui.0.clear();
    p.on_ui_result(&mut h.cx(), MENU, "").unwrap();
    assert!(h.ui.0.is_empty());
}

#[test]
fn start_reflects_the_state_and_autostart_turns_on() {
    // Left on by a crash, autostart off: shown as on, not changed.
    let dev = Device::new("start");
    dev.net().on().unwrap();
    dev.calls();
    let mut h = Host::new(&dev.root);
    let mut p = UsbNetPlugin::with_gadget(dev.net());
    assert!(p.tick_interval().is_some(), "one tick at start");
    p.on_tick(&mut h.cx()).unwrap();
    assert!(p.tick_interval().is_none());
    assert!(p.is_on());
    assert_eq!(dev.calls(), Vec::<String>::new());

    // Off, autostart on: the first tick turns it on, later ticks do nothing.
    dev.net().off(None).unwrap();
    dev.calls();
    h.store.settings.insert("autostart".into(), json!(true));
    let mut p = UsbNetPlugin::with_gadget(dev.net());
    p.on_tick(&mut h.cx()).unwrap();
    assert!(p.is_on() && dev.gadget().exists());
    assert_eq!(dev.calls().last().map(String::as_str), Some(UP));
    p.on_tick(&mut h.cx()).unwrap();
    assert_eq!(dev.calls(), Vec::<String>::new());
}

#[test]
fn exit_turns_it_off_and_gives_mtp_back() {
    let dev = Device::new("exit").with_mtp(UDC);
    let mut h = Host::new(&dev.root);
    let mut p = UsbNetPlugin::with_gadget(dev.net());
    p.on_action(&mut h.cx(), "toggle").unwrap();
    assert!(p.is_on());
    p.on_exit(&mut h.cx(), false).unwrap();
    assert!(!p.is_on() && !dev.gadget().exists());
    assert_eq!(dev.file("mtpgadget/UDC"), format!("{UDC}\n"));

    // Already off: exit changes nothing.
    dev.calls();
    let mut q = UsbNetPlugin::with_gadget(dev.net());
    q.on_exit(&mut h.cx(), false).unwrap();
    assert_eq!(dev.calls(), Vec::<String>::new());
    assert_eq!(dev.file("mtpgadget/UDC"), format!("{UDC}\n"));
}

#[test]
fn a_restart_keeps_it_on() {
    let dev = Device::new("restart").with_mtp(UDC);
    let mut h = Host::new(&dev.root);
    let mut p = UsbNetPlugin::with_gadget(dev.net());
    p.on_action(&mut h.cx(), "toggle").unwrap();
    dev.calls();
    p.on_exit(&mut h.cx(), true).unwrap();
    assert!(p.is_on() && dev.gadget().exists());
    assert_eq!(dev.calls(), Vec::<String>::new());
}
