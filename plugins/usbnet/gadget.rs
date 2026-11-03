//! The configfs USB gadget `usbnet` (one CDC ECM function), a port of
//! `out/usbnet-reference.sh`.
//!
//! Every file operation and command goes through [`Sys`], so tests run against a fake
//! configfs tree in a temp folder and record the commands instead of running them.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The Kindle's address on the cable.
pub const KINDLE_IP: &str = "192.168.15.244";
/// The netmask of [`KINDLE_IP`] (a /24).
pub const NETMASK: &str = "255.255.255.0";
/// Our gadget's folder name under the configfs `usb_gadget` folder.
pub const GADGET: &str = "usbnet";
/// The stock MTP gadget (created by the Kindle UI's `/usr/bin/mtp.sh`).
pub const MTP_GADGET: &str = "mtpgadget";
/// Fixed MAC addresses, so macOS keeps one network port across sessions.
pub const DEV_ADDR: &str = "ee:19:00:00:00:01";
/// The computer's side of the link.
pub const HOST_ADDR: &str = "ee:19:00:00:00:02";
/// The waits between tries to bind the gadget (about 1.5 s in all): right after MTP
/// lets go the controller can refuse a new gadget for a moment, and the Kindle's
/// `mtp.sh` may bind MTP again when the computer sees the port go away.
pub const BIND_RETRY: [Duration; 5] = [Duration::from_millis(50), Duration::from_millis(100), Duration::from_millis(200), Duration::from_millis(400), Duration::from_millis(800)];

/// Files and commands, as the device has them (tests use a fake).
pub trait Sys {
    /// Runs `program args`; `Err` carries its stderr (or why it could not start).
    fn run(&mut self, program: &str, args: &[&str]) -> Result<(), String>;

    /// `mkdir -p`. In configfs the kernel fills a new folder with its attribute files.
    fn mkdir(&mut self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    /// `rmdir`. In configfs a folder that holds only attributes can be removed.
    fn rmdir(&mut self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir(path)
    }

    /// `echo value > path` (the value ends in a newline, as from the shell).
    fn write(&mut self, path: &Path, value: &str) -> io::Result<()> {
        std::fs::write(path, format!("{value}\n"))
    }

    /// The file's content without surrounding white space; `None` when it cannot be read.
    fn read(&mut self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    }

    /// Waits (between bind tries).
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d)
    }
}

/// The real device: `std::fs` and `std::process::Command`.
pub struct RealSys;

impl RealSys {
    /// The program's full path when it is in a system folder (rinky may run with a
    /// short `PATH`), else the bare name.
    fn locate(program: &str) -> PathBuf {
        ["/sbin", "/usr/sbin", "/bin", "/usr/bin"].iter().map(|d| Path::new(d).join(program)).find(|p| p.exists()).unwrap_or_else(|| PathBuf::from(program))
    }
}

impl Sys for RealSys {
    fn run(&mut self, program: &str, args: &[&str]) -> Result<(), String> {
        let out = Command::new(Self::locate(program)).args(args).output().map_err(|e| format!("{program}: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { format!("{program}: {}", out.status) } else { err })
    }
}

/// Where the kernel's folders are.
#[derive(Clone, Debug)]
pub struct Paths {
    /// configfs gadgets: `/sys/kernel/config/usb_gadget`.
    pub gadgets: PathBuf,
    /// USB device controllers: `/sys/class/udc`.
    pub udc: PathBuf,
    /// The device serial number: `/proc/usid`.
    pub usid: PathBuf,
    /// Power supplies (a charger sees the cable's VBUS): `/sys/class/power_supply`.
    pub power_supply: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Paths { gadgets: "/sys/kernel/config/usb_gadget".into(), udc: "/sys/class/udc".into(), usid: "/proc/usid".into(), power_supply: "/sys/class/power_supply".into() }
    }
}

/// What is bound now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The controller our gadget is bound to (`None`: USB network is off).
    pub udc: Option<String>,
    /// The network interface (`usb0`) while on.
    pub ifname: Option<String>,
    /// The controller the MTP gadget is bound to.
    pub mtp_udc: Option<String>,
}

impl State {
    /// USB network is on.
    pub fn on(&self) -> bool {
        self.udc.is_some()
    }
}

/// A failure to turn USB network on or off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// `modprobe usb_f_ecm` failed (the kernel has no ECM function).
    Module(String),
    /// No entry in `/sys/class/udc`.
    NoController,
    /// `mkdir functions/ecm.usb0` failed.
    NoFunction(String),
    /// Writing the controller name to `UDC` still failed after [`BIND_RETRY`] while a
    /// cable is in (or its presence is unknown), with no other gadget in configfs
    /// holding the controller: the controller name and the last error (with the kernel
    /// driver that holds it, when the controller says).
    Bind(String, String),
    /// Another configfs gadget (not MTP, which is taken over) holds the controller: the
    /// controller and the gadget's name.
    Held(String, String),
    /// Binding failed and no USB cable is in.
    NoCable,
    /// No interface name after binding.
    NoInterface,
    /// `ifconfig <if> 192.168.15.244 ... up` failed.
    Ifconfig(String),
    /// Off: the gadget is still bound or its folder is still there.
    Remove(String),
}

impl Error {
    /// A short text for the control tile's state line.
    pub fn short(&self) -> &'static str {
        match self {
            Error::Module(_) | Error::NoFunction(_) => "No ECM support",
            Error::NoController => "No USB controller",
            Error::Bind(..) => "USB port busy",
            Error::Held(..) => "USB port in use",
            Error::NoCable => "Not connected",
            Error::NoInterface | Error::Ifconfig(_) => "Network failed",
            Error::Remove(_) => "Off failed",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Module(e) => write!(f, "the kernel has no USB Ethernet (modprobe usb_f_ecm: {e})"),
            Error::NoController => write!(f, "no USB device controller in /sys/class/udc"),
            Error::NoFunction(e) => write!(f, "no ecm function in configfs ({e})"),
            Error::Bind(udc, e) => write!(f, "the USB port {udc} stays busy ({e}). Unplug the cable, plug it in again, then turn USB network on"),
            Error::Held(udc, g) => write!(f, "the USB port {udc} is used by the USB gadget \"{g}\". Turn that off first (or restart the Kindle)"),
            Error::NoCable => write!(f, "no USB cable. Connect the Kindle to a computer, then turn it on"),
            Error::NoInterface => write!(f, "the gadget has no network interface"),
            Error::Ifconfig(e) => write!(f, "cannot set the address {KINDLE_IP} ({e})"),
            Error::Remove(e) => write!(f, "cannot remove the USB network gadget ({e})"),
        }
    }
}

/// The result of a successful [`UsbNet::on`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct On {
    /// The controller bound to.
    pub udc: String,
    /// The network interface.
    pub ifname: String,
    /// The controller taken from the MTP gadget (to give back at off).
    pub took_mtp: Option<String>,
}

/// The `usbnet` gadget.
pub struct UsbNet {
    /// Kernel folders.
    pub paths: Paths,
    sys: Box<dyn Sys>,
    /// The controller taken from the MTP gadget during this [`UsbNet::on`].
    took_mtp: Option<String>,
}

impl UsbNet {
    /// The gadget on the real device.
    pub fn system() -> Self {
        Self::new(Paths::default(), Box::new(RealSys))
    }

    /// The gadget with these folders and this [`Sys`].
    pub fn new(paths: Paths, sys: Box<dyn Sys>) -> Self {
        UsbNet { paths, sys, took_mtp: None }
    }

    fn dir(&self) -> PathBuf {
        self.paths.gadgets.join(GADGET)
    }

    fn mtp_dir(&self) -> PathBuf {
        self.paths.gadgets.join(MTP_GADGET)
    }

    fn nonempty(&mut self, path: &Path) -> Option<String> {
        self.sys.read(path).filter(|s| !s.is_empty())
    }

    /// The first USB device controller (`ls /sys/class/udc | head -n 1`).
    pub fn controller(&self) -> Option<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.paths.udc).ok()?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names.into_iter().next()
    }

    /// Whether a USB cable is in. `Some(true)`: the controller's
    /// `/sys/class/udc/<udc>/state` is anything but `not attached`, or a charger supply
    /// (not the battery, not wireless, not the `USB_WET` moisture sensor) is `online`.
    /// `Some(false)`: the controller says `not attached` or a charger says offline, and
    /// nothing says otherwise. `None`: nothing readable (then nobody claims the cable is
    /// out). The same sources as `ink_device::usb::cable_from`; plain file reads, so the
    /// tile can ask on every repaint.
    pub fn cable(&self) -> Option<bool> {
        let read = |p: PathBuf| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string());
        let mut known = false;
        if let Some(state) = self.controller().and_then(|u| read(self.paths.udc.join(u).join("state"))) {
            match state.as_str() {
                "" => {}
                "not attached" => known = true,
                _ => return Some(true),
            }
        }
        if let Ok(dir) = std::fs::read_dir(&self.paths.power_supply) {
            for e in dir.filter_map(|e| e.ok()) {
                let p = e.path();
                let charger = read(p.join("type")).is_some_and(|t| matches!(t.as_str(), "Mains" | "USB" | "USB_DCP" | "USB_CDP" | "USB_ACA" | "USB_PD"));
                match read(p.join("online")).as_deref() {
                    Some("1") if charger => return Some(true),
                    Some(_) if charger => known = true,
                    _ => {}
                }
            }
        }
        known.then_some(false)
    }

    /// What is bound now (a few small reads).
    pub fn state(&mut self) -> State {
        let e = self.dir();
        let udc = self.nonempty(&e.join("UDC"));
        let ifname = if udc.is_some() { self.nonempty(&e.join("functions/ecm.usb0/ifname")) } else { None };
        let mtp_udc = self.nonempty(&self.mtp_dir().join("UDC"));
        State { udc, ifname, mtp_udc }
    }

    /// Writes a gadget attribute; failures are logged, as the script ignores them.
    fn attr(&mut self, path: &Path, value: &str) {
        if let Err(e) = self.sys.write(path, value) {
            log::warn!("usbnet: cannot write {value:?} to {}: {e}", path.display());
        }
    }

    /// Creates the gadget, takes the USB port (from the MTP gadget if it holds it) and
    /// brings the interface up as [`KINDLE_IP`]. On a failure after the port was taken
    /// everything is undone (and MTP rebound).
    pub fn on(&mut self) -> Result<On, Error> {
        let st = self.state();
        if let Some(udc) = st.udc {
            log::info!("usbnet: already on ({udc})");
            return Ok(On { udc, ifname: st.ifname.unwrap_or_default(), took_mtp: None });
        }
        if let Err(e) = self.sys.run("modprobe", &["libcomposite"]) {
            log::warn!("usbnet: modprobe libcomposite: {e}");
        }
        self.sys.run("modprobe", &["usb_f_ecm"]).map_err(Error::Module)?;
        let udc = self.controller().ok_or(Error::NoController)?;
        self.took_mtp = None;
        if let Some(m) = st.mtp_udc {
            self.unbind_mtp(m);
        }
        let r = self.create_and_bind(&udc);
        let took_mtp = self.took_mtp.take();
        match r {
            Ok(ifname) => {
                log::info!("usbnet: on: {ifname} {KINDLE_IP} on {udc}");
                Ok(On { udc, ifname, took_mtp })
            }
            Err(e) => {
                log::warn!("usbnet: on failed: {e}");
                let _ = self.off(took_mtp.as_deref());
                // Without a cable the controller can refuse the gadget (EBUSY): say what
                // is missing rather than that the port is taken.
                match e {
                    Error::Bind(..) if self.cable() == Some(false) => Err(Error::NoCable),
                    e => Err(e),
                }
            }
        }
    }

    fn unbind_mtp(&mut self, udc: String) {
        log::info!("usbnet: unbinding {MTP_GADGET} from {udc}");
        let p = self.mtp_dir().join("UDC");
        self.attr(&p, "");
        self.took_mtp = Some(udc);
    }

    /// The other configfs gadget bound to `udc`, if any.
    fn holder(&mut self, udc: &str) -> Option<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.paths.gadgets).ok()?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n != GADGET).collect();
        names.sort();
        names.into_iter().find(|n| self.nonempty(&self.paths.gadgets.join(n).join("UDC")).as_deref() == Some(udc))
    }

    /// Binds the gadget to `udc`, trying again after each wait of [`BIND_RETRY`] and
    /// taking the port from MTP again whenever it got it back. A port held by any other
    /// gadget is not taken ([`Error::Held`]).
    fn bind(&mut self, udc: &str) -> Result<(), Error> {
        let path = self.dir().join("UDC");
        let mut last = String::new();
        for try_ in 0..=BIND_RETRY.len() {
            if try_ > 0 {
                self.sys.sleep(BIND_RETRY[try_ - 1]);
            }
            match self.holder(udc) {
                Some(g) if g == MTP_GADGET => self.unbind_mtp(udc.into()),
                Some(g) => return Err(Error::Held(udc.into(), g)),
                None => {}
            }
            match self.sys.write(&path, udc) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    log::warn!("usbnet: bind to {udc} (try {}): {err}", try_ + 1);
                    last = err.to_string();
                }
            }
        }
        // A kernel driver outside configfs (a legacy gadget module) shows only here.
        if let Some(f) = self.nonempty(&self.paths.udc.join(udc).join("function")) {
            last = format!("{last}; the port has the driver {f}");
        }
        Err(Error::Bind(udc.into(), last))
    }

    fn create_and_bind(&mut self, udc: &str) -> Result<String, Error> {
        let e = self.dir();
        let mkdir = |s: &mut Self, p: &Path| {
            if let Err(err) = s.sys.mkdir(p) {
                log::warn!("usbnet: mkdir {}: {err}", p.display());
            }
        };
        mkdir(self, &e);
        self.attr(&e.join("idVendor"), "0x1d6b"); // Linux Foundation
        self.attr(&e.join("idProduct"), "0x0103"); // NCM/ECM gadget
        self.attr(&e.join("bcdUSB"), "0x0200");
        self.attr(&e.join("bcdDevice"), "0x0100");
        let strings = e.join("strings/0x409");
        mkdir(self, &strings);
        self.attr(&strings.join("manufacturer"), "Amazon");
        self.attr(&strings.join("product"), "Kindle USB network");
        let usid = self.sys.read(&self.paths.usid).unwrap_or_default();
        self.attr(&strings.join("serialnumber"), &usid);
        let func = e.join("functions/ecm.usb0");
        self.sys.mkdir(&func).map_err(|err| Error::NoFunction(err.to_string()))?;
        self.attr(&func.join("dev_addr"), DEV_ADDR);
        self.attr(&func.join("host_addr"), HOST_ADDR);
        let config = e.join("configs/c.1");
        mkdir(self, &config.join("strings/0x409"));
        self.attr(&config.join("strings/0x409/configuration"), "ECM");
        self.attr(&config.join("MaxPower"), "250");
        let link = config.join("ecm.usb0");
        if link.symlink_metadata().is_err() {
            if let Err(err) = std::os::unix::fs::symlink(&func, &link) {
                log::warn!("usbnet: ln -s {}: {err}", link.display());
            }
        }
        self.bind(udc)?;
        // The interface gets its name when the function binds.
        let ifname = self.nonempty(&func.join("ifname")).ok_or(Error::NoInterface)?;
        if ifname.contains(char::is_whitespace) || ifname.starts_with('(') {
            return Err(Error::NoInterface);
        }
        self.sys.run("ifconfig", &[&ifname, KINDLE_IP, "netmask", NETMASK, "up"]).map_err(Error::Ifconfig)?;
        Ok(ifname)
    }

    /// Unbinds the gadget, takes the interface down and removes the gadget (reverse
    /// order). Then binds the MTP gadget to `rebind_mtp` again if it is unbound (a
    /// failure is only logged; MTP also comes back at the next replug while the Kindle UI
    /// runs).
    pub fn off(&mut self, rebind_mtp: Option<&str>) -> Result<(), Error> {
        let e = self.dir();
        if e.is_dir() {
            if self.nonempty(&e.join("UDC")).is_some() {
                self.attr(&e.join("UDC"), "");
            }
            if let Some(ifname) = self.nonempty(&e.join("functions/ecm.usb0/ifname")) {
                if let Err(err) = self.sys.run("ifconfig", &[&ifname, "down"]) {
                    log::warn!("usbnet: ifconfig {ifname} down: {err}");
                }
            }
            let _ = std::fs::remove_file(e.join("configs/c.1/ecm.usb0"));
            for d in ["configs/c.1/strings/0x409", "configs/c.1", "functions/ecm.usb0", "strings/0x409", ""] {
                let p = if d.is_empty() { e.clone() } else { e.join(d) };
                if p.is_dir() {
                    if let Err(err) = self.sys.rmdir(&p) {
                        log::warn!("usbnet: rmdir {}: {err}", p.display());
                    }
                }
            }
        }
        if let Some(udc) = rebind_mtp {
            let m = self.mtp_dir();
            if m.is_dir() && self.nonempty(&m.join("UDC")).is_none() {
                match self.sys.write(&m.join("UDC"), udc) {
                    Ok(()) => log::info!("usbnet: {MTP_GADGET} bound to {udc} again"),
                    Err(err) => log::warn!("usbnet: cannot bind {MTP_GADGET} to {udc}: {err}"),
                }
            }
        }
        let result = if self.state().on() {
            Err(Error::Remove("the gadget is still bound".into()))
        } else if e.exists() {
            Err(Error::Remove(format!("{} is still there", e.display())))
        } else {
            Ok(())
        };
        log::info!("usbnet: off{}", if result.is_ok() { "" } else { " (incomplete)" });
        result
    }
}
