//! USB network: the Kindle's USB port as a USB Ethernet device (CDC ECM), so a computer
//! on the cable can SSH to the Kindle. A port of `out/usbnet-reference.sh`.
//!
//! - A control-center tile "USB network" (icon `usb`; tap: on or off). The line under
//!   the name is `192.168.15.244` while on, `Off` while off, `Not connected` while off
//!   with no USB cable in ([`gadget::UsbNet::cable`]: the controller's `state` and the
//!   charger supplies), and a short error after a failure (the full text is a toast). A
//!   bind failure without a cable is `Not connected`, not `USB port busy`. Binding is
//!   tried again for about 1.5 s (the controller can refuse for a moment after MTP lets
//!   go, and MTP may bind itself again); a port held by another, non-MTP gadget is
//!   `USB port in use`, naming it. A legacy gadget module on the port (`g_ether`, the
//!   "RNDIS/Ethernet Gadget" a computer sees) is unloaded first.
//! - Settings page "USB network": the status, Turn on / Turn off, "Turn on when
//!   rinky starts" (setting `autostart`, default off) and "How to connect a
//!   computer" (a Markdown page: addresses, macOS, Linux, Windows, `ssh root@...`).
//! - On: a configfs gadget `usbnet` (vendor 0x1d6b, product 0x0103) with one function
//!   `ecm.usb0` (fixed MAC addresses, so macOS keeps one network port), bound to the
//!   first USB device controller, interface up as 192.168.15.244/24. The stock MTP
//!   gadget is unbound first if it holds the port; that is remembered (setting
//!   `mtp_udc`) and MTP is bound again at off. Off undoes everything in reverse order.
//! - The real state is read at the first tick after start, before and after every
//!   action and at wake-up: a KUAL script can switch it too. A gadget left on (crash,
//!   script) is only shown, not changed, unless `autostart` asks for on.
//! - At exit ([`Plugin::on_exit`]) USB network is turned off, so the Kindle UI gets MTP
//!   back (on the next replug). Not on a restart (`rinky-ctl.sh restart`, deploys):
//!   the Kindle UI stays stopped, and a deploy over the cable keeps its link.
//! - Failures are toasts and the tile's error text, never `Err` (five `Err`s disable a
//!   plugin).

pub mod gadget;

use gadget::{UsbNet, KINDLE_IP};
use ink_plugin::serde_json::{json, Value};
use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Registrar, Result, TileState};
use std::time::Duration;

/// Plugin name.
pub const NAME: &str = "usbnet";
/// The computer's address on the cable (set by hand on the computer).
pub const COMPUTER_IP: &str = "192.168.15.201";

const MENU: u32 = 1;
const HELP: u32 = 2;

/// The first tick comes this soon after start.
const FIRST_TICK: Duration = Duration::from_millis(100);

/// The page behind "How to connect a computer".
pub const HELP_TEXT: &str = "\
# Connect a computer by USB

USB network makes the Kindle's USB port a network card. The cable then carries a small \
network with two addresses:

| | Address |
| --- | --- |
| Kindle | `192.168.15.244` |
| Computer | `192.168.15.201`, netmask `255.255.255.0` (/24) |

The computer's address is set by hand once.

## macOS

A network port **Kindle USB network** appears in System Settings > Network. Give it the \
address in Terminal:

```
networksetup -setmanual \"Kindle USB network\" 192.168.15.201 255.255.255.0
```

## Linux

Find the new interface with `ip link` (often `usb0` or `enx...`), then:

```
sudo ip addr add 192.168.15.201/24 dev <iface>
sudo ip link set <iface> up
```

## Windows

Windows needs a driver for this kind of USB network (ECM). Not supported for now.

## SSH

Turn on the SSH server (the control-center tile **SSH server**), then on the computer:

```
ssh root@192.168.15.244
```

## While it is on

MTP file transfer (the Kindle as a drive) is off. Turning USB network off does not \
bring it back while rinky runs: the Kindle home screen provides file transfer. \
Exit to the Kindle, then plug the cable in again.
";

/// The USB network plugin.
pub struct UsbNetPlugin {
    manifest: PluginManifest,
    net: UsbNet,
    /// The last state read: on, with the interface name.
    on: Option<String>,
    /// The last failure (short text for the tile, cleared by the next success).
    error: Option<gadget::Error>,
    /// The first tick (state read, autostart) has run.
    started: bool,
}

impl Default for UsbNetPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbNetPlugin {
    /// The plugin on the real device.
    pub fn new() -> Self {
        Self::with_gadget(UsbNet::system())
    }

    /// The plugin with this gadget (tests use a fake configfs).
    pub fn with_gadget(net: UsbNet) -> Self {
        UsbNetPlugin { manifest: PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Ui, Permission::Storage]), net, on: None, error: None, started: false }
    }

    /// USB network was on at the last look.
    pub fn is_on(&self) -> bool {
        self.on.is_some()
    }

    /// The last failure.
    pub fn error(&self) -> Option<&gadget::Error> {
        self.error.as_ref()
    }

    /// Reads the real state.
    fn refresh(&mut self) {
        let st = self.net.state();
        self.on = st.udc.map(|_| st.ifname.unwrap_or_else(|| "usb0".into()));
    }

    /// The last failure, unless the cable made it stale: "no cable" once a cable is in,
    /// "port busy" once it is out (then "Not connected" says more).
    fn current_error(&self, cable: Option<bool>) -> Option<&gadget::Error> {
        self.error.as_ref().filter(|e| match e {
            gadget::Error::NoCable => cable == Some(false),
            gadget::Error::Bind(..) => cable != Some(false),
            _ => true,
        })
    }

    /// The status row of the settings page.
    fn status_text(&self) -> String {
        let cable = self.net.cable();
        match (&self.on, self.current_error(cable)) {
            (Some(ifname), _) if cable == Some(false) => format!("On · {KINDLE_IP} ({ifname}) · no cable"),
            (Some(ifname), _) => format!("On · {KINDLE_IP} ({ifname})"),
            (None, Some(gadget::Error::NoCable)) | (None, None) if cable == Some(false) => "Off · no USB cable".into(),
            (None, Some(e)) => format!("Off · {e}"),
            (None, None) => "Off".into(),
        }
    }

    /// Turns USB network on; a failure is a toast.
    pub fn turn_on(&mut self, cx: &mut HostCtx) -> Result<()> {
        match self.net.on() {
            Ok(on) => {
                self.error = None;
                if let Some(udc) = &on.took_mtp {
                    cx.storage()?.set("mtp_udc", json!(udc))?;
                }
                self.refresh();
                cx.toast(&format!("USB network on: {KINDLE_IP}"))?;
            }
            Err(e) => {
                self.refresh();
                cx.toast(&format!("USB network: {e}"))?;
                self.error = Some(e);
            }
        }
        Ok(())
    }

    /// Turns USB network off (and gives the port back to MTP if it was taken from it).
    pub fn turn_off(&mut self, cx: &mut HostCtx) -> Result<()> {
        let mtp = cx.setting_str("mtp_udc");
        let r = self.net.off(mtp.as_deref());
        self.refresh();
        match r {
            Ok(()) => {
                self.error = None;
                if mtp.is_some() {
                    cx.storage()?.set("mtp_udc", Value::Null)?;
                }
                cx.toast("USB network off")?;
            }
            Err(e) => {
                cx.toast(&format!("USB network: {e}"))?;
                self.error = Some(e);
            }
        }
        Ok(())
    }

    fn show_menu(&mut self, cx: &mut HostCtx) -> Result<()> {
        let auto = cx.setting_bool("autostart", false);
        let switch = if self.is_on() { ("Turn off", "off") } else { ("Turn on", "on") };
        let auto_label = format!("Turn on when rinky starts\t{}", if auto { "on" } else { "off" });
        let status = self.status_text();
        cx.menu(MENU, "USB network", [(status.as_str(), "status"), switch, (auto_label.as_str(), "autostart"), ("How to connect a computer", "help")])
    }
}

impl Plugin for UsbNetPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.settings_page("usbnet", "USB network");
        reg.control_tile("toggle", "USB network", "usb").state("Off").on_hold("settings:usbnet");
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        self.refresh();
        match id {
            "settings:usbnet" => self.show_menu(cx)?,
            "toggle" if self.is_on() => self.turn_off(cx)?,
            "toggle" => self.turn_on(cx)?,
            _ => {}
        }
        Ok(())
    }

    fn tile_state(&self, id: &str) -> Option<TileState> {
        (id == "toggle").then(|| {
            // The cable is read on every call (two small sysfs reads), so the line
            // follows a plug or unplug while the control center is open.
            let cable = self.net.cable();
            let text = match (&self.on, self.current_error(cable)) {
                (Some(_), _) => KINDLE_IP.to_string(),
                (None, Some(e)) => e.short().to_string(),
                (None, None) if cable == Some(false) => gadget::Error::NoCable.short().to_string(),
                (None, None) => "Off".to_string(),
            };
            TileState { text, on: Some(self.is_on()) }
        })
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        match (id, result) {
            (MENU, "status") => {
                self.refresh();
                self.show_menu(cx)?;
            }
            (MENU, "on" | "off") => {
                self.refresh();
                match (result, self.is_on()) {
                    ("on", false) => self.turn_on(cx)?,
                    ("off", true) => self.turn_off(cx)?,
                    _ => {}
                }
                self.show_menu(cx)?;
            }
            (MENU, "autostart") => {
                let auto = !cx.setting_bool("autostart", false);
                cx.storage()?.set("autostart", json!(auto))?;
                self.show_menu(cx)?;
            }
            (MENU, "help") => cx.view_markdown(HELP, "How to connect a computer", HELP_TEXT)?,
            // Back from the help page: the settings page again.
            (HELP, "closed") => self.show_menu(cx)?,
            _ => {}
        }
        Ok(())
    }

    fn on_resume(&mut self, _cx: &mut HostCtx) -> Result<()> {
        self.refresh();
        Ok(())
    }

    fn on_exit(&mut self, cx: &mut HostCtx, restarting: bool) -> Result<()> {
        self.refresh();
        if self.is_on() && restarting {
            log::info!("usbnet: exit for a restart: USB network stays on");
        } else if self.is_on() {
            log::info!("usbnet: exit: turning USB network off");
            self.turn_off(cx)?;
        }
        Ok(())
    }

    fn tick_interval(&self) -> Option<Duration> {
        (!self.started).then_some(FIRST_TICK)
    }

    fn on_tick(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        self.refresh();
        log::info!("usbnet: start: USB network is {}", if self.is_on() { "on" } else { "off" });
        if !self.is_on() && cx.setting_bool("autostart", false) {
            log::info!("usbnet: autostart");
            self.turn_on(cx)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
