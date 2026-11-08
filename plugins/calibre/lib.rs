//! Calibre wireless: calibre's Smart Device App protocol over TCP, like KOReader's
//! calibre plugin (`calibre.koplugin/wireless.lua`).
//!
//! - A control-center tile "Calibre" (tap: connect or disconnect; the line under the name
//!   is the status) and a library entry "Connect to calibre" (overflow menu, or the
//!   navigation bar when chosen in Settings).
//! - Settings page "Calibre wireless" and reader menu entry "Connect to calibre". The
//!   page shows the status (`Connected to <host> · receiving N/M`), Connect/Disconnect,
//!   the server (empty = find calibre by UDP broadcast on its ports, or `host:port`), the
//!   password and the books folder. While the page is open its first row follows the
//!   status ([`ink_plugin::UiRequest::MenuLabel`]: only that row is repainted).
//! - The connection runs on a worker thread ([`session`]); the plugin collects its
//!   events in [`Plugin::on_tick`] (asked for only while connected) and shows a toast per
//!   received or deleted book. After each batch the library is rescanned.
//! - Books go to `books_dir` (default: the first library folder, `/mnt/us/Books`) under
//!   calibre's `lpath` (`Author/Title.epub`), through `<file>.part` and a rename. The
//!   book list calibre asks for is kept in `metadata.calibre.json` in the plugin's data
//!   folder (KOReader keeps `metadata.calibre` in the inbox folder).
//! - Settings: `server` (`host:port`), `password`, `books_dir`, `idle_minutes` (5).

pub mod cache;
pub mod discovery;
pub mod protocol;
pub mod session;

use ink_plugin::serde_json::{json, Value};
use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Registrar, Result, TileState};
use session::{Config, End, Event};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

/// Plugin name.
pub const NAME: &str = "calibre";
/// Default books folder (the app passes its first library folder as the default).
pub const DEFAULT_DIR: &str = "/mnt/us/Books";
/// calibre's default port for wireless devices.
pub const DEFAULT_PORT: u16 = 9090;
/// Default idle limit in minutes.
pub const DEFAULT_IDLE_MINUTES: u64 = 5;
/// Cache file names in the plugin's data folder.
pub const CACHE_FILE: &str = "metadata.calibre.json";
/// calibre's drive info.
pub const DRIVE_FILE: &str = "driveinfo.calibre.json";

const MENU: u32 = 1;
const IN_SERVER: u32 = 2;
const IN_PASSWORD: u32 = 3;
const IN_DIR: u32 = 4;
const IN_RETRY_PASSWORD: u32 = 5;

/// Tick interval while connected.
const TICK: Duration = Duration::from_millis(250);

/// Connection state shown on the settings page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// No connection.
    Idle,
    /// Broadcasting for calibre.
    Searching,
    /// Opening the connection to this server.
    Connecting(String),
    /// Connected.
    Connected {
        /// Server name or address.
        host: String,
        /// Book `this` of `total` is being received (0 = none).
        this: usize,
        /// Books in the batch.
        total: usize,
    },
    /// The user asked to disconnect; waiting for the worker.
    Disconnecting,
    /// The last connection ended (reason).
    Ended(String),
}

impl Status {
    /// The short state under the control-center tile's name.
    pub fn tile_text(&self) -> String {
        match self {
            Status::Idle => "Tap to connect".into(),
            Status::Searching => "Searching…".into(),
            Status::Connecting(_) => "Connecting…".into(),
            Status::Connected { this, total, .. } if *this > 0 => format!("Receiving {this}/{total}"),
            Status::Connected { .. } => "Connected".into(),
            Status::Disconnecting => "Disconnecting…".into(),
            Status::Ended(_) => "Not connected".into(),
        }
    }

    /// The status line.
    pub fn text(&self) -> String {
        match self {
            Status::Idle => "Not connected".into(),
            Status::Searching => "Searching for calibre…".into(),
            Status::Connecting(h) => format!("Connecting to {h}…"),
            Status::Connected { host, this, total } if *this > 0 => format!("Connected to {host} · receiving {this}/{total}"),
            Status::Connected { host, .. } => format!("Connected to {host}"),
            Status::Disconnecting => "Disconnecting…".into(),
            Status::Ended(why) => format!("Not connected ({why})"),
        }
    }
}

/// Messages from the worker thread.
#[derive(Debug)]
enum Msg {
    Status(Status),
    Event(Event),
    End(End),
}

struct Conn {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    /// Server name or address once known.
    host: String,
}

/// `host[:port]` from the `server` setting: `(host, port)`; port 9090 when missing.
pub fn parse_server(text: &str) -> Option<(String, u16)> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    // [v6]:port, host:port, host
    if let Some(rest) = t.strip_prefix('[') {
        let (h, p) = rest.split_once(']')?;
        let port = match p.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if p.is_empty() => DEFAULT_PORT,
            None => return None,
        };
        return Some((h.to_string(), port));
    }
    match t.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Some((h.to_string(), p.parse().ok()?)),
        Some(_) => Some((t.to_string(), DEFAULT_PORT)), // a bare IPv6 address
        None => Some((t.to_string(), DEFAULT_PORT)),
    }
}

/// Plain words for a socket error.
pub fn net_message(e: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::ConnectionRefused => "connection refused: start the wireless device connection in calibre".into(),
        K::TimedOut | K::WouldBlock => "no answer (timed out)".into(),
        K::NetworkUnreachable | K::HostUnreachable | K::NetworkDown | K::AddrNotAvailable => format!("network unreachable; is Wi-Fi on? ({e})"),
        _ => e.to_string(),
    }
}

/// Worker thread: find or resolve the server, connect, run the session.
fn worker(server: Option<(String, u16)>, cfg: Config, tx: Sender<Msg>, cancel: Arc<AtomicBool>, discovery_targets: Vec<SocketAddr>) {
    let cancelled = || cancel.load(Ordering::Relaxed);
    let fail = |m: String| {
        log::warn!("calibre: {m}");
        let _ = tx.send(Msg::End(End::Failed(m)));
    };
    let (label, addrs): (String, Vec<SocketAddr>) = match server {
        Some((h, p)) => {
            let label = format!("{h}:{p}");
            let _ = tx.send(Msg::Status(Status::Connecting(label.clone())));
            match (h.as_str(), p).to_socket_addrs() {
                Ok(a) => (label, a.collect()),
                Err(e) => return fail(format!("cannot find {h}: {e}")),
            }
        }
        None => {
            let _ = tx.send(Msg::Status(Status::Searching));
            match discovery::discover(&discovery_targets, Duration::from_secs(3), &cancelled) {
                Ok(Some(f)) => (f.name, vec![f.addr]),
                Ok(None) if cancelled() => return drop(tx.send(Msg::End(End::Cancelled))),
                Ok(None) => return fail("no calibre found on this network; in calibre use Connect/share > Start wireless device connection, or set the server address".into()),
                Err(e) => return fail(format!("cannot search for calibre: {}", net_message(&e))),
            }
        }
    };
    let _ = tx.send(Msg::Status(Status::Connecting(label.clone())));
    let mut last = None;
    let mut stream = None;
    for a in &addrs {
        log::info!("calibre: connecting to {a} ({label})");
        match TcpStream::connect_timeout(a, Duration::from_secs(5)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let Some(stream) = stream else {
        let why = last.map(|e| net_message(&e)).unwrap_or_else(|| "no address".into());
        return fail(format!("cannot connect to calibre at {label}: {why}"));
    };
    let setup = stream.set_read_timeout(Some(Duration::from_secs(1))).and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(30))));
    if let Err(e) = setup {
        return fail(format!("socket setup: {e}"));
    }
    let _ = stream.set_nodelay(true);
    let _ = tx.send(Msg::Status(Status::Connected { host: label, this: 0, total: 0 }));
    let end = session::run(&stream, &cfg, &|ev| drop(tx.send(Msg::Event(ev))), &cancelled);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = tx.send(Msg::End(end));
}

/// The calibre wireless plugin.
pub struct CalibrePlugin {
    manifest: PluginManifest,
    conn: Option<Conn>,
    status: Status,
    /// Books received since the last library rescan.
    unscanned: usize,
    /// Where discovery datagrams go (tests point it at localhost).
    pub discovery_targets: Vec<SocketAddr>,
    /// The settings menu is open and shows this status text in its first row.
    menu_status: Option<String>,
}

impl Default for CalibrePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl CalibrePlugin {
    /// The plugin.
    pub fn new() -> Self {
        CalibrePlugin {
            manifest: PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Ui, Permission::Storage, Permission::Network]),
            conn: None,
            status: Status::Idle,
            unscanned: 0,
            discovery_targets: discovery::broadcast_targets(),
            menu_status: None,
        }
    }

    /// The connection state.
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// True while a worker thread runs.
    pub fn connected(&self) -> bool {
        self.conn.is_some()
    }

    fn setting(cx: &mut HostCtx, key: &str) -> String {
        cx.setting_str(key).unwrap_or_default()
    }

    fn books_dir(cx: &mut HostCtx) -> PathBuf {
        let d = Self::setting(cx, "books_dir");
        PathBuf::from(if d.trim().is_empty() { DEFAULT_DIR.to_string() } else { d })
    }

    fn config(cx: &mut HostCtx) -> Result<Config> {
        let idle = cx.setting_u64("idle_minutes").unwrap_or(DEFAULT_IDLE_MINUTES).max(1);
        Ok(Config {
            books_dir: Self::books_dir(cx),
            password: Self::setting(cx, "password"),
            device_name: "rinky (Kindle)".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            cache_file: cx.data_file(CACHE_FILE)?,
            drive_file: cx.data_file(DRIVE_FILE)?,
            idle_timeout: Duration::from_secs(idle * 60),
        })
    }

    /// Starts a connection (nothing when one runs).
    pub fn connect(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.conn.is_some() {
            return Ok(());
        }
        // The plugin opens its own sockets under the network permission.
        if !cx.permissions.contains(&Permission::Network) {
            return Err(ink_plugin::PluginError::Permission("network".into()));
        }
        let cfg = Self::config(cx)?;
        let server = parse_server(&Self::setting(cx, "server"));
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (c2, targets) = (cancel.clone(), self.discovery_targets.clone());
        let host = server.as_ref().map(|(h, p)| format!("{h}:{p}")).unwrap_or_default();
        log::info!("calibre: connect ({}), books in {}", if host.is_empty() { "search the network" } else { &host }, cfg.books_dir.display());
        std::thread::Builder::new()
            .name("calibre".into())
            .stack_size(256 * 1024)
            .spawn(move || worker(server, cfg, tx, c2, targets))
            .map_err(|e| ink_plugin::PluginError::Failed(format!("cannot start the calibre thread: {e}")))?;
        self.status = if host.is_empty() { Status::Searching } else { Status::Connecting(host.clone()) };
        self.conn = Some(Conn { rx, cancel, host });
        cx.ui()?.toast(&self.status.text());
        Ok(())
    }

    /// Asks the worker to stop (it sends a parting NOOP and ends within about a second).
    pub fn disconnect(&mut self) {
        if let Some(c) = &self.conn {
            log::info!("calibre: disconnect requested");
            c.cancel.store(true, Ordering::Relaxed);
            self.status = Status::Disconnecting;
        }
    }

    fn show_menu(&mut self, cx: &mut HostCtx) -> Result<()> {
        let server = Self::setting(cx, "server");
        let password = Self::setting(cx, "password");
        let dir = Self::books_dir(cx);
        self.menu_status = Some(self.status.text());
        let mut items = vec![(self.status.text(), "status")];
        if self.conn.is_some() {
            items.push(("Disconnect".into(), "disconnect"));
        } else {
            items.push(("Connect to calibre".into(), "connect"));
        }
        let server = server.trim();
        items.push((format!("Server\t{}", if server.is_empty() { "find automatically" } else { server }), "server"));
        if !server.is_empty() {
            items.push(("Find the server automatically".into(), "auto"));
        }
        items.push((format!("Password\t{}", if password.is_empty() { "none" } else { "set" }), "password"));
        if !password.is_empty() {
            items.push(("Remove the password".into(), "nopassword"));
        }
        items.push((format!("Books folder\t{}", dir.display()), "dir"));
        cx.menu(MENU, "Calibre wireless", items)
    }

    fn handle(&mut self, cx: &mut HostCtx, m: Msg) -> Result<()> {
        match m {
            Msg::Status(st) => {
                if let (Status::Connecting(h), Some(c)) = (&st, self.conn.as_mut()) {
                    c.host = h.clone();
                }
                if self.status != Status::Disconnecting {
                    self.status = st;
                }
            }
            Msg::Event(ev) => self.event(cx, ev)?,
            Msg::End(end) => {
                self.conn = None;
                self.rescan(cx)?;
                let ui = cx.ui()?;
                match &end {
                    End::Cancelled => {
                        self.status = Status::Idle;
                        ui.toast("Disconnected from calibre");
                    }
                    End::WrongPassword => {
                        self.status = Status::Ended(end.to_string());
                        ui.toast("calibre refused the password");
                        ui.input(IN_RETRY_PASSWORD, "calibre password", "");
                    }
                    End::Ejected => {
                        self.status = Status::Idle;
                        ui.toast("calibre disconnected the reader");
                    }
                    other => {
                        self.status = Status::Ended(other.to_string());
                        ui.toast(&format!("Disconnected from calibre: {other}"));
                    }
                }
            }
        }
        Ok(())
    }

    /// A registered entry was triggered (see [`Plugin::on_action`]).
    fn action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        match id {
            "settings:calibre" | "menu" => self.show_menu(cx),
            "connect" if self.conn.is_some() => self.show_menu(cx),
            "connect" => self.connect(cx),
            // The control-center tile.
            "toggle" if self.conn.is_some() => {
                self.disconnect();
                Ok(())
            }
            "toggle" => self.connect(cx),
            "disconnect" => {
                self.disconnect();
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Handles the worker's messages that arrived.
    fn drain(&mut self, cx: &mut HostCtx) -> Result<()> {
        loop {
            let Some(c) = self.conn.as_ref() else { return Ok(()) };
            match c.rx.try_recv() {
                Ok(m) => self.handle(cx, m)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    // The thread ended without an End message (panic).
                    return self.handle(cx, Msg::End(End::Failed("the connection thread stopped".into())));
                }
            }
        }
    }

    /// Asks the library to rescan when books arrived since the last rescan.
    fn rescan(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.unscanned > 0 {
            self.unscanned = 0;
            cx.ui()?.rescan_library();
        }
        Ok(())
    }

    /// While the settings menu is open, shows a changed status in its first row (one row
    /// repainted, no new menu).
    fn refresh_status_row(&mut self, cx: &mut HostCtx) -> Result<()> {
        let text = self.status.text();
        if self.menu_status.as_ref().is_some_and(|shown| *shown != text) {
            cx.ui()?.menu_label(MENU, "status", &text);
            self.menu_status = Some(text);
        }
        Ok(())
    }

    fn event(&mut self, cx: &mut HostCtx, ev: Event) -> Result<()> {
        let host = self.conn.as_ref().map(|c| c.host.clone()).unwrap_or_default();
        match ev {
            Event::Connected { calibre_version } => {
                log::info!("calibre: connected to {host} (calibre {calibre_version})");
                self.status = Status::Connected { host: host.clone(), this: 0, total: 0 };
                cx.ui()?.toast(&format!("Connected to calibre on {host}"));
            }
            Event::Library(name) => log::info!("calibre: library {name:?}"),
            Event::Receiving { this, total, .. } => {
                if self.status != Status::Disconnecting {
                    self.status = Status::Connected { host, this, total };
                }
            }
            Event::Received { this, total, title, .. } => {
                self.unscanned += 1;
                cx.ui()?.toast(&format!("Received {title} ({this}/{total})"));
            }
            Event::ReceiveFailed { title, why } => cx.ui()?.toast(&format!("Could not receive {title}: {why}")),
            Event::BatchDone { .. } => {
                if self.status != Status::Disconnecting {
                    self.status = Status::Connected { host, this: 0, total: 0 };
                }
                self.rescan(cx)?;
            }
            Event::Deleted(titles) => {
                let ui = cx.ui()?;
                match &titles[..] {
                    [one] => ui.toast(&format!("calibre deleted {one}")),
                    many => ui.toast(&format!("calibre deleted {} books", many.len())),
                }
                ui.rescan_library();
            }
            Event::Message(m) if !m.trim().is_empty() => cx.ui()?.notify(&format!("calibre: {m}")),
            Event::Message(_) => {}
        }
        Ok(())
    }
}

impl Plugin for CalibrePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.settings_page("calibre", "Calibre wireless");
        reg.menu_item("connect", "Connect to calibre", 65);
        reg.control_tile("toggle", "Calibre", "library").needs_wifi("Turn on Wi-Fi to connect to calibre").state("Tap to connect");
        reg.library_entry("connect", "Connect to calibre", "download");
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        // A tile tap or a menu entry can change the status while the settings menu is open.
        let r = self.action(cx, id);
        self.refresh_status_row(cx)?;
        r
    }

    fn tile_state(&self, id: &str) -> Option<TileState> {
        (id == "toggle").then(|| TileState { text: self.status.tile_text(), on: Some(self.conn.is_some()) })
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        if id == MENU {
            // Any answer closes the menu; the next `show_menu` opens it again.
            self.menu_status = None;
        }
        match id {
            MENU => match result {
                "status" => self.show_menu(cx),
                "connect" => self.connect(cx),
                "disconnect" => {
                    self.disconnect();
                    Ok(())
                }
                "server" => {
                    let cur = Self::setting(cx, "server");
                    cx.ui()?.input(IN_SERVER, "calibre address (host:port)", &cur);
                    Ok(())
                }
                "auto" => {
                    cx.storage()?.set("server", Value::Null)?;
                    self.show_menu(cx)
                }
                "password" => {
                    cx.ui()?.input(IN_PASSWORD, "calibre password", "");
                    Ok(())
                }
                "nopassword" => {
                    cx.storage()?.set("password", Value::Null)?;
                    self.show_menu(cx)
                }
                "dir" => {
                    let cur = Self::books_dir(cx);
                    cx.ui()?.input(IN_DIR, "Books folder", &cur.to_string_lossy());
                    Ok(())
                }
                _ => Ok(()),
            },
            IN_SERVER => {
                let t = result.trim();
                if !t.is_empty() {
                    if parse_server(t).is_some() {
                        cx.storage()?.set("server", json!(t))?;
                    } else {
                        cx.ui()?.toast("Write the address as host:port, for example 192.168.1.20:9090");
                    }
                }
                self.show_menu(cx)
            }
            IN_PASSWORD => {
                if !result.is_empty() {
                    cx.storage()?.set("password", json!(result))?;
                }
                self.show_menu(cx)
            }
            IN_RETRY_PASSWORD => {
                if result.is_empty() {
                    return Ok(());
                }
                cx.storage()?.set("password", json!(result))?;
                self.connect(cx)
            }
            IN_DIR => {
                let t = result.trim();
                if !t.is_empty() {
                    if t.starts_with('/') {
                        cx.storage()?.set("books_dir", json!(t.trim_end_matches('/')))?;
                    } else {
                        cx.ui()?.toast("The books folder must be a full path, for example /mnt/us/Books");
                    }
                }
                self.show_menu(cx)
            }
            _ => Ok(()),
        }
    }

    fn on_suspend(&mut self, cx: &mut HostCtx) -> Result<()> {
        // Wi-Fi goes down while the device sleeps; end the session cleanly first.
        if self.conn.is_some() {
            self.disconnect();
            self.conn = None;
            self.status = Status::Ended("the reader went to sleep".into());
            self.rescan(cx)?;
        }
        Ok(())
    }

    fn tick_interval(&self) -> Option<Duration> {
        self.conn.as_ref().map(|_| TICK)
    }

    fn on_tick(&mut self, cx: &mut HostCtx) -> Result<()> {
        let r = self.drain(cx);
        self.refresh_status_row(cx)?;
        r
    }
}

#[cfg(test)]
mod tests;
