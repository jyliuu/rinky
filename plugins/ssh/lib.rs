//! SSH server: runs dropbear on the Kindle while rinky runs, so a computer can log in
//! as root over Wi-Fi, or over the cable with the `usbnet` plugin (the Kindle is
//! `192.168.15.244` on `usb0`).
//!
//! - The server is `/mnt/us/rinky/bin/dropbearmulti` from the package, started as
//!   `dropbearmulti dropbear ...` ([`dropbear_args`] is the whole argument list) in the
//!   foreground with its log on stderr, which goes to `ssh/dropbear.log` (trimmed when
//!   large). The plugin keeps the [`Child`]; [`Plugin::tick_interval`] asks for a tick
//!   every 2 s while it runs, so an exit is noticed (toast with the last log line, the
//!   tile shows the error). Stop: SIGTERM, a short wait, then SIGKILL.
//! - Keys: `/mnt/us/rinky/ssh/authorized_keys` (created with instructions when
//!   missing). `/mnt/us` is vfat (everything 0777), and dropbear wants every folder above
//!   `authorized_keys` up to `/` owned by root and not writable by others (`/tmp` is
//!   1777), so each start copies the file into the first root-only folder of
//!   `/var/run/rinky-ssh`, `/var/local/rinky-ssh`, `/dev/rinky-ssh`
//!   ([`choose_key_dir`], 0700, file 0600) and passes it with `-D`. A key added to the
//!   file counts from the next start. Password logins are off (`-s`) unless "login
//!   without password" is on (`-n`).
//! - Host keys `ssh/dropbear_ed25519_host_key` and `ssh/dropbear_ecdsa_host_key`: made
//!   with `dropbearkey` before a start when missing (dropbear exits on a missing `-r`
//!   key) and kept, so the computer's `known_hosts` stays valid.
//! - Firewall: the Kindle's iptables may drop incoming connections; a start adds
//!   `INPUT -p tcp --dport <port> -j ACCEPT` when `iptables -C` does not find it, a stop
//!   deletes the rule it added (as KOReader's SSH plugin does). iptables is
//!   `/usr/sbin/iptables`, `/sbin/iptables` or the one in `PATH`; failures are logged,
//!   never fatal.
//! - Every stop (also on exit, in `Drop` and after the server died) removes the key
//!   folder again, so nothing stays outside `/mnt/us/rinky` after an uninstall.
//! - Control-center tile `toggle` "SSH server" (tap: start or stop; the line shows the
//!   address to use, `Off`, or a short error) and settings page `ssh` "SSH server"
//!   (status, start/stop, port, login without password, autostart, "How to log in").
//! - Settings: `port` (22; KOReader's own SSH server uses 2222, so the host keys of the
//!   two never meet on one port), `no_password` (false; dropbear `-n`, patched in: any
//!   password, or none, logs in), `autostart` (false: start once after rinky starts).
//! - Every external call goes through [`System`], so tests run without dropbear,
//!   iptables or network interfaces.

use ink_plugin::serde_json::{json, Value};
use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Registrar, Result, TileState};
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Plugin name.
pub const NAME: &str = "ssh";
/// Default port (KOReader's SSH server uses 2222).
pub const DEFAULT_PORT: u16 = 22;
/// The Kindle's address on `usb0` with the `usbnet` plugin.
pub const USB_ADDRESS: &str = "192.168.15.244";

const MENU: u32 = 1;
const IN_PORT: u32 = 2;
const DLG_NO_PASSWORD: u32 = 3;
const VIEW_HELP: u32 = 4;

/// Tick interval while the server runs.
const TICK: Duration = Duration::from_secs(2);
/// Tick interval until the autostart setting was looked at.
const FIRST_TICK: Duration = Duration::from_millis(500);
/// Internal setting: the server ran when rinky exited for a restart.
const RESUME: &str = "resume";
/// How long a stop waits after SIGTERM before SIGKILL.
const STOP_WAIT: Duration = Duration::from_millis(800);
/// The log is cut to its last [`LOG_KEEP`] bytes before a start when it is larger.
const LOG_MAX: u64 = 256 * 1024;
const LOG_KEEP: u64 = 32 * 1024;

/// What a missing `authorized_keys` is created with.
pub const KEYS_TEMPLATE: &str = "\
# rinky SSH server: public keys that may log in as root, one per line.
# Add the contents of your computer's ~/.ssh/id_ed25519.pub (or id_rsa.pub) as a new
# line below, then restart the SSH server (the file is read when the server starts).
# Lines starting with # are ignored.
";

/// Where things are. [`Paths::kindle`] is the real layout; tests use a temp folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// The dropbear multi-call binary (also `dropbearkey`).
    pub dropbear: PathBuf,
    /// The user's folder: `authorized_keys`, the host keys, the log.
    pub ssh_dir: PathBuf,
    /// Candidates for the root-only folder with the copied `authorized_keys` and the pid
    /// file, best first ([`choose_key_dir`]).
    pub key_dirs: Vec<PathBuf>,
    /// iptables programs to try; the first that exists is used, else `iptables` from
    /// `PATH`.
    pub iptables: Vec<PathBuf>,
}

impl Paths {
    /// `$RINKY_HOME` (default `/mnt/us/rinky`): `bin/dropbearmulti` and `ssh/`;
    /// key folder `/var/run`, `/var/local` or `/dev` + `/rinky-ssh`.
    pub fn kindle() -> Self {
        let home = std::env::var_os("RINKY_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/mnt/us/rinky"));
        Paths {
            dropbear: home.join("bin/dropbearmulti"),
            ssh_dir: home.join("ssh"),
            key_dirs: ["/var/run/rinky-ssh", "/var/local/rinky-ssh", "/dev/rinky-ssh"].iter().map(PathBuf::from).collect(),
            iptables: ["/usr/sbin/iptables", "/sbin/iptables"].iter().map(PathBuf::from).collect(),
        }
    }
    /// The user's `authorized_keys`.
    pub fn user_keys(&self) -> PathBuf {
        self.ssh_dir.join("authorized_keys")
    }
    /// The persistent ed25519 host key.
    pub fn host_key(&self) -> PathBuf {
        self.ssh_dir.join("dropbear_ed25519_host_key")
    }
    /// The persistent ECDSA (P-256) host key, for clients without ed25519.
    pub fn ecdsa_key(&self) -> PathBuf {
        self.ssh_dir.join("dropbear_ecdsa_host_key")
    }
    /// The host keys with the `dropbearkey` arguments that make each.
    pub fn host_keys(&self) -> Vec<(PathBuf, Vec<&'static str>)> {
        vec![(self.host_key(), vec!["-t", "ed25519"]), (self.ecdsa_key(), vec!["-t", "ecdsa", "-s", "256"])]
    }
    /// dropbear's log (its stderr).
    pub fn log(&self) -> PathBuf {
        self.ssh_dir.join("dropbear.log")
    }
    /// The iptables program: the first of [`Paths::iptables`] that exists, else `iptables`.
    pub fn iptables_program(&self) -> String {
        self.iptables.iter().find(|p| p.is_file()).map_or_else(|| "iptables".into(), |p| p.to_string_lossy().into_owned())
    }
}

/// The arguments after the binary: `dropbear -F -E -p <port> -r <ed25519> -r <ecdsa> -D
/// <keydir> -P <keydir>/dropbear.pid (-s | -n)`. `-F` foreground (the plugin keeps the
/// child), `-E` log to stderr, `-r` the host keys (they must exist, see
/// [`SshPlugin::start`]), `-D` the folder with `authorized_keys`; `-s` keys only, or
/// `-n` (a patch in the package's build) any password logs in.
pub fn dropbear_args(paths: &Paths, key_dir: &Path, port: u16, no_password: bool) -> Vec<String> {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut a: Vec<String> = vec!["dropbear".into(), "-F".into(), "-E".into(), "-p".into(), port.to_string()];
    for (key, _) in paths.host_keys() {
        a.extend(["-r".into(), s(&key)]);
    }
    a.extend(["-D".into(), s(key_dir), "-P".into(), s(&key_dir.join("dropbear.pid"))]);
    a.push(if no_password { "-n" } else { "-s" }.into());
    a
}

/// The `dropbearkey` arguments (after the binary) that make `key`.
pub fn keygen_args(key: &Path, kind: &[&str]) -> Vec<String> {
    let mut a = vec!["dropbearkey".to_string()];
    a.extend(kind.iter().map(|s| s.to_string()));
    a.extend(["-f".into(), key.to_string_lossy().into_owned()]);
    a
}

/// The iptables arguments for the rule (`op` is `-C`, `-A` or `-D`).
pub fn firewall_args(op: &str, port: u16) -> Vec<String> {
    [op, "INPUT", "-p", "tcp", "--dport", &port.to_string(), "-j", "ACCEPT"].iter().map(|s| s.to_string()).collect()
}

/// Keys in an `authorized_keys` text: lines that are neither empty nor comments.
pub fn count_keys(text: &str) -> usize {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).count()
}

/// Owned by root and not writable by group or others (what dropbear's
/// `checkpubkeyperms` asks of every folder above `authorized_keys`). Follows symlinks.
pub fn root_only(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0)
}

/// The first of `paths.key_dirs` dropbear will accept: its parent exists and the parent
/// and every folder above it (after resolving symlinks) pass `trusted`. The folder is
/// created (a file or symlink in its place is removed), made 0700, and must then pass
/// `trusted` too. `None` when no candidate qualifies.
pub fn choose_key_dir(paths: &Paths, trusted: &mut dyn FnMut(&Path) -> bool) -> Option<PathBuf> {
    for cand in &paths.key_dirs {
        let (Some(parent), Some(name)) = (cand.parent(), cand.file_name()) else { continue };
        let Ok(parent) = fs::canonicalize(parent) else {
            log::info!("ssh: key folder {}: no parent folder", cand.display());
            continue;
        };
        if let Some(bad) = parent.ancestors().find(|a| !trusted(a)) {
            log::info!("ssh: key folder {}: {} is not root-only", cand.display(), bad.display());
            continue;
        }
        let dir = parent.join(name);
        let made = (|| -> io::Result<()> {
            if fs::symlink_metadata(&dir).is_ok_and(|m| !m.is_dir()) {
                fs::remove_file(&dir)?;
            }
            if !dir.is_dir() {
                fs::create_dir(&dir)?;
            }
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        })();
        match made {
            Ok(()) if trusted(&dir) => return Some(dir),
            Ok(()) => log::info!("ssh: key folder {} is not root-only", dir.display()),
            Err(e) => log::info!("ssh: key folder {}: {e}", dir.display()),
        }
    }
    None
}

/// Creates the user's folder and a commented `authorized_keys` when missing. Returns the
/// text.
pub fn user_keys_text(paths: &Paths) -> io::Result<String> {
    fs::create_dir_all(&paths.ssh_dir)?;
    let src = paths.user_keys();
    if !src.exists() {
        fs::write(&src, KEYS_TEMPLATE)?;
        log::info!("ssh: created {}", src.display());
    }
    fs::read_to_string(&src)
}

/// Copies the user's `authorized_keys` (made by [`user_keys_text`] when missing) to
/// `key_dir/authorized_keys`, 0600, written beside it and renamed. Returns the number of
/// keys.
pub fn install_keys(paths: &Paths, key_dir: &Path) -> io::Result<usize> {
    let text = user_keys_text(paths)?;
    let tmp = key_dir.join("authorized_keys.new");
    let _ = fs::remove_file(&tmp);
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, key_dir.join("authorized_keys"))?;
    Ok(count_keys(&text))
}

/// Cuts the log to its last part when it grew past [`LOG_MAX`] (at a line start).
pub fn trim_log(path: &Path) {
    let Ok(meta) = fs::metadata(path) else { return };
    if meta.len() <= LOG_MAX {
        return;
    }
    let tail = read_tail(path, LOG_KEEP);
    let start = tail.find('\n').map_or(0, |i| i + 1);
    if let Err(e) = fs::write(path, &tail[start..]) {
        log::warn!("ssh: cannot trim {}: {e}", path.display());
    }
}

fn read_tail(path: &Path, bytes: u64) -> String {
    let Ok(mut f) = File::open(path) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(bytes)));
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// The last non-empty line of the log (at most 200 characters), for the exit toast.
pub fn last_log_line(path: &Path) -> Option<String> {
    let tail = read_tail(path, 8 * 1024);
    let line = tail.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    Some(line.chars().take(200).collect())
}

/// IPv4 addresses of the network interfaces (`getifaddrs`), as `(interface, address)`.
pub fn interface_addrs() -> Vec<(String, Ipv4Addr)> {
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list that freeifaddrs releases below; the
    // entries are only read in between.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut p = head;
        while !p.is_null() {
            let ifa = &*p;
            if !ifa.ifa_addr.is_null() && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                out.push((CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned(), ip));
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

/// Addresses a computer can use, best first: `usb0` (USB networking), then `wlan0`.
pub fn usable_addrs(all: &[(String, Ipv4Addr)]) -> Vec<(&'static str, Ipv4Addr)> {
    let mut out = Vec::new();
    for (iface, label) in [("usb0", "USB"), ("wlan0", "Wi-Fi")] {
        out.extend(all.iter().filter(|(n, ip)| n == iface && !ip.is_loopback() && !ip.is_unspecified()).map(|(_, ip)| (label, *ip)));
    }
    out
}

/// The outside world: processes, iptables, file ownership and interface addresses.
pub trait System {
    /// Starts `program args` with stdin from `/dev/null` and stdout and stderr to `log`.
    fn spawn(&mut self, program: &Path, args: &[String], log: File) -> io::Result<Child>;
    /// Runs a short command (iptables, dropbearkey) and tells whether it exited with 0.
    fn run(&mut self, program: &str, args: &[String]) -> io::Result<bool>;
    /// IPv4 addresses of the interfaces.
    fn addresses(&mut self) -> Vec<(String, Ipv4Addr)>;
    /// Whether dropbear accepts `path` as a folder above `authorized_keys` ([`root_only`]).
    fn trusted(&mut self, path: &Path) -> bool;
}

/// [`System`] with std::process and getifaddrs.
pub struct RealSystem;

/// Starts `program args` as [`System::spawn`] describes.
pub fn spawn_logged(program: &Path, args: &[String], log: File) -> io::Result<Child> {
    use std::os::unix::process::CommandExt;
    let err = log.try_clone()?;
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(log).stderr(err);
    // rinky blocks SIGHUP/SIGINT/SIGTERM for its signal thread and ignores some
    // others; a child inherits both, so dropbear would never see the stop's SIGTERM and
    // its login shells would start with those signals blocked.
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe { cmd.pre_exec(reset_signals) };
    cmd.spawn()
}

/// Empty signal mask and default handlers for the signals rinky blocks or ignores.
fn reset_signals() -> io::Result<()> {
    // SAFETY: sigemptyset/sigprocmask/signal on a local set; async-signal-safe.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
        for sig in [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGPIPE] {
            libc::signal(sig, libc::SIG_DFL);
        }
    }
    Ok(())
}

impl System for RealSystem {
    fn spawn(&mut self, program: &Path, args: &[String], log: File) -> io::Result<Child> {
        spawn_logged(program, args, log)
    }
    fn run(&mut self, program: &str, args: &[String]) -> io::Result<bool> {
        Ok(Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()?.success())
    }
    fn addresses(&mut self) -> Vec<(String, Ipv4Addr)> {
        interface_addrs()
    }
    fn trusted(&mut self, path: &Path) -> bool {
        root_only(path)
    }
}

/// Removes what a start put into the key folder, and the folder itself (only that
/// folder, never a parent), so nothing stays outside `/mnt/us/rinky`.
pub fn remove_key_dir(dir: &Path) {
    for f in ["authorized_keys", "authorized_keys.new", "dropbear.pid"] {
        let _ = fs::remove_file(dir.join(f));
    }
    match fs::remove_dir(dir) {
        Ok(()) => log::info!("ssh: removed {}", dir.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("ssh: cannot remove {}: {e}", dir.display()),
    }
}

/// The running server.
struct Server {
    child: Child,
    port: u16,
    no_password: bool,
    /// The firewall rule was added by this start (and is deleted by the stop).
    firewall: bool,
    keys: usize,
    /// The root-only folder with `authorized_keys` and the pid file.
    key_dir: PathBuf,
}

/// The last start or run failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Short text for the tile (`Not installed`, `Start failed`, `Stopped`).
    pub short: String,
    /// The whole message (toast, settings page).
    pub message: String,
}

/// The SSH server plugin.
pub struct SshPlugin {
    manifest: PluginManifest,
    paths: Paths,
    sys: Box<dyn System>,
    server: Option<Server>,
    failure: Option<Failure>,
    /// Interface addresses as of the last start or tick.
    addrs: Vec<(String, Ipv4Addr)>,
    /// The `autostart` setting is still to be looked at (first tick after load).
    autostart_pending: bool,
    /// The settings menu is open and shows this status in its first row.
    menu_status: Option<String>,
}

impl Default for SshPlugin {
    /// The plugin with the Kindle's paths.
    fn default() -> Self {
        Self::with_system(Paths::kindle(), Box::new(RealSystem))
    }
}

impl SshPlugin {
    /// The plugin with other paths and a fake [`System`] (tests).
    pub fn with_system(paths: Paths, sys: Box<dyn System>) -> Self {
        SshPlugin {
            manifest: PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Ui, Permission::Storage]),
            paths,
            sys,
            server: None,
            failure: None,
            addrs: Vec::new(),
            autostart_pending: true,
            menu_status: None,
        }
    }

    /// True while the server process runs.
    pub fn running(&self) -> bool {
        self.server.is_some()
    }

    /// The last failure (cleared by a start or a stop).
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }

    /// The server's pid while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.server.as_ref().map(|s| s.child.id())
    }

    fn port(cx: &mut HostCtx) -> u16 {
        cx.setting_u64("port").and_then(|p| u16::try_from(p).ok()).filter(|&p| p > 0).unwrap_or(DEFAULT_PORT)
    }

    /// Stores a setting; a failed write is a toast, not an error.
    fn set(cx: &mut HostCtx, key: &str, value: Value) {
        let r = cx.storage().and_then(|s| s.set(key, value));
        if let Err(e) = r {
            log::warn!("ssh: cannot save {key}: {e}");
            toast(cx, &format!("Cannot save the SSH setting: {e}"));
        }
    }

    /// `address:port` of the best interface, or `Port N`.
    fn address_text(&self, port: u16) -> String {
        match usable_addrs(&self.addrs).first() {
            Some((_, ip)) => format!("{ip}:{port}"),
            None => format!("Port {port}"),
        }
    }

    fn fail(&mut self, cx: &mut HostCtx, short: &str, message: String) {
        log::warn!("ssh: {message}");
        toast(cx, &message);
        self.failure = Some(Failure { short: short.into(), message });
    }

    /// Starts the server with the stored settings (nothing when it runs). Failures are
    /// toasts and [`SshPlugin::failure`].
    pub fn start(&mut self, cx: &mut HostCtx) {
        if self.server.is_some() {
            return;
        }
        let port = Self::port(cx);
        let no_password = cx.setting_bool("no_password", false);
        self.failure = None;
        if !self.paths.dropbear.is_file() {
            return self.fail(cx, "Not installed", format!("The SSH server is not installed ({} is missing)", self.paths.dropbear.display()));
        }
        if let Err(e) = user_keys_text(&self.paths) {
            return self.fail(cx, "Start failed", format!("Cannot read {}: {e}", self.paths.user_keys().display()));
        }
        if let Err(m) = self.make_host_keys() {
            return self.fail(cx, "No host key", m);
        }
        let sys = &mut self.sys;
        let Some(key_dir) = choose_key_dir(&self.paths, &mut |p| sys.trusted(p)) else {
            let tried: Vec<String> = self.paths.key_dirs.iter().map(|d| d.display().to_string()).collect();
            return self.fail(cx, "No safe folder for keys", format!("No safe folder for keys: none of {} is root-only", tried.join(", ")));
        };
        let (keys, child) = match self.launch(&key_dir, port, no_password) {
            Ok(started) => started,
            Err(m) => {
                remove_key_dir(&key_dir);
                return self.fail(cx, "Start failed", m);
            }
        };
        let firewall = self.open_firewall(port);
        self.addrs = self.sys.addresses();
        self.server = Some(Server { child, port, no_password, firewall, keys, key_dir });
        let mut msg = format!("SSH server on {}", self.address_text(port));
        if no_password {
            msg.push_str(" · login without password");
        } else if keys == 0 {
            msg.push_str(" · no keys in ssh/authorized_keys yet");
        }
        log::info!("ssh: {msg} (pid {:?}, {keys} keys)", self.pid());
        toast(cx, &msg);
    }

    /// Puts the keys into `key_dir` and starts dropbear with its log; returns the number
    /// of keys and the process, or the message of the step that failed.
    fn launch(&mut self, key_dir: &Path, port: u16, no_password: bool) -> std::result::Result<(usize, Child), String> {
        let keys = install_keys(&self.paths, key_dir).map_err(|e| format!("Cannot prepare the SSH keys: {e}"))?;
        let log_path = self.paths.log();
        trim_log(&log_path);
        let log = OpenOptions::new().create(true).append(true).open(&log_path).map_err(|e| format!("Cannot open {}: {e}", log_path.display()))?;
        let args = dropbear_args(&self.paths, key_dir, port, no_password);
        log::info!("ssh: starting {} {}", self.paths.dropbear.display(), args.join(" "));
        let child = self.sys.spawn(&self.paths.dropbear, &args, log).map_err(|e| format!("Cannot start the SSH server: {e}"))?;
        Ok((keys, child))
    }

    /// Makes each missing host key with `dropbearkey` (the first start takes a few
    /// seconds). The message says which key failed.
    fn make_host_keys(&mut self) -> std::result::Result<(), String> {
        let prog = self.paths.dropbear.to_string_lossy().into_owned();
        for (key, kind) in self.paths.host_keys() {
            if key.exists() {
                continue;
            }
            log::info!("ssh: making host key {}", key.display());
            match self.sys.run(&prog, &keygen_args(&key, &kind)) {
                Ok(true) if key.exists() => {}
                Ok(_) => return Err(format!("Cannot make the SSH host key {}", key.display())),
                Err(e) => return Err(format!("Cannot make the SSH host key {}: {e}", key.display())),
            }
        }
        Ok(())
    }

    /// Adds the firewall rule when missing; true when this call added it.
    fn open_firewall(&mut self, port: u16) -> bool {
        let ipt = self.paths.iptables_program();
        let added = match self.sys.run(&ipt, &firewall_args("-C", port)) {
            Ok(true) => return false,
            Ok(false) => self.sys.run(&ipt, &firewall_args("-A", port)),
            Err(e) => {
                log::warn!("ssh: iptables not available ({e}); not changing the firewall");
                return false;
            }
        };
        match &added {
            Ok(true) => log::info!("ssh: firewall: accepting tcp port {port}"),
            Ok(false) => log::warn!("ssh: iptables could not add the rule for port {port}"),
            Err(e) => log::warn!("ssh: iptables: {e}"),
        }
        matches!(added, Ok(true))
    }

    fn close_firewall(&mut self, port: u16) {
        let ipt = self.paths.iptables_program();
        match self.sys.run(&ipt, &firewall_args("-D", port)) {
            Ok(true) => log::info!("ssh: firewall: rule for tcp port {port} removed"),
            Ok(false) => log::warn!("ssh: iptables could not delete the rule for port {port}"),
            Err(e) => log::warn!("ssh: iptables: {e}"),
        }
    }

    /// Ends the server process (SIGTERM, a short wait, SIGKILL), removes the firewall
    /// rule it added and the key folder. Sessions already open are separate processes and
    /// keep running until they log out. No UI.
    fn halt(&mut self) {
        let Some(mut s) = self.server.take() else { return };
        if alive(&mut s.child) {
            log::info!("ssh: stopping the server (pid {})", s.child.id());
            if let Ok(pid) = libc::pid_t::try_from(s.child.id()) {
                // SAFETY: plain kill(2) on our own child, which has not been reaped yet.
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            let deadline = Instant::now() + STOP_WAIT;
            while alive(&mut s.child) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if alive(&mut s.child) {
                log::warn!("ssh: the server did not stop; killing it");
                let _ = s.child.kill();
                let _ = s.child.wait();
            }
        }
        self.release(&s);
    }

    /// After the process ended: the firewall rule this start added and the key folder go.
    fn release(&mut self, s: &Server) {
        if s.firewall {
            self.close_firewall(s.port);
        }
        remove_key_dir(&s.key_dir);
    }

    /// Stops the server (toast), nothing when it is not running.
    pub fn stop(&mut self, cx: &mut HostCtx) {
        self.failure = None;
        if self.server.is_some() {
            self.halt();
            toast(cx, "SSH server stopped");
        }
    }

    /// After a port or password setting changed: restarts a running server.
    fn restart_if_running(&mut self, cx: &mut HostCtx, why: &str) {
        let Some(s) = &self.server else { return };
        if s.port == Self::port(cx) && s.no_password == cx.setting_bool("no_password", false) {
            return;
        }
        log::info!("ssh: {why}; restarting");
        toast(cx, &format!("{why}: restarting the SSH server"));
        self.halt();
        self.start(cx);
    }

    /// Notices a server that exited by itself.
    fn check_exit(&mut self, cx: &mut HostCtx) {
        let Some(s) = self.server.as_mut() else { return };
        let status = match s.child.try_wait() {
            Ok(Some(st)) => st,
            Ok(None) => return,
            Err(e) => {
                log::warn!("ssh: cannot check the server: {e}");
                return;
            }
        };
        if let Some(s) = self.server.take() {
            self.release(&s);
        }
        let why = last_log_line(&self.paths.log()).unwrap_or_else(|| status.to_string());
        self.fail(cx, "Stopped", format!("SSH server stopped: {why}"));
    }

    /// The first row of the settings page.
    fn status_text(&self) -> String {
        if let Some(s) = &self.server {
            let addrs = usable_addrs(&self.addrs);
            let at = if addrs.is_empty() {
                format!("port {} (no Wi-Fi or USB address yet)", s.port)
            } else {
                addrs.iter().map(|(l, ip)| format!("{ip}:{} ({l})", s.port)).collect::<Vec<_>>().join(", ")
            };
            format!("Running on {at} · {}", keys_text(s.keys))
        } else if let Some(f) = &self.failure {
            f.message.clone()
        } else {
            "Off".into()
        }
    }

    fn show_menu(&mut self, cx: &mut HostCtx) -> Result<()> {
        let port = Self::port(cx);
        let no_password = cx.setting_bool("no_password", false);
        let autostart = cx.setting_bool("autostart", false);
        let status = self.status_text();
        self.menu_status = Some(status.clone());
        let mut items: Vec<(String, &str)> = vec![(status, "status")];
        if self.server.is_none() {
            let keys = fs::read_to_string(self.paths.user_keys()).map(|t| count_keys(&t)).unwrap_or(0);
            items.push((format!("Keys in ssh/authorized_keys\t{keys}"), "status"));
        }
        if no_password {
            items.push(("Warning: login without password is on".into(), "status"));
        }
        items.push((if self.server.is_some() { "Stop the server" } else { "Start the server" }.into(), if self.server.is_some() { "stop" } else { "start" }));
        items.push((format!("Port\t{port}"), "port"));
        items.push((format!("Login without password\t{}", on_off(no_password)), "no_password"));
        items.push((format!("Start when rinky starts\t{}", on_off(autostart)), "autostart"));
        items.push(("How to log in".into(), "help"));
        cx.menu(MENU, "SSH server", items)
    }

    /// While the settings menu is open, shows a changed status in its first row.
    fn refresh_status_row(&mut self, cx: &mut HostCtx) -> Result<()> {
        let text = self.status_text();
        if self.menu_status.as_ref().is_some_and(|shown| *shown != text) {
            cx.ui()?.menu_label(MENU, "status", &text);
            self.menu_status = Some(text);
        }
        Ok(())
    }

    fn help_markdown(&self, port: u16) -> String {
        let best = usable_addrs(&self.addrs).first().map(|(_, ip)| ip.to_string());
        let addr = best.clone().unwrap_or_else(|| format!("{USB_ADDRESS} (USB) or the Wi-Fi address"));
        let host = best.unwrap_or_else(|| "ADDRESS".into());
        let keys = self.paths.user_keys();
        let hostkey = self.paths.host_key();
        let p = format!("-p {port} ");
        format!(
            "# Log in\n\n\
             Start the server, then on the computer:\n\n\
             ```\nssh {p}root@{host}\n```\n\n\
             The Kindle's address: {addr}. Over the cable, turn on USB networking (the \
             `usbnet` plugin); the Kindle is then `{USB_ADDRESS}`.\n\n\
             # Add your key\n\n\
             Add your public key (for example `~/.ssh/id_ed25519.pub`) as one line of \
             `{keys}`, then stop and start the server: the file is read when the server \
             starts.\n\n\
             - Over USB file transfer: open `rinky/ssh/authorized_keys` on the Kindle \
             drive with a text editor and paste the key.\n\
             - With login without password on (`ssh-copy-id` does not work here, the file \
             is copied at start):\n\n\
             ```\ncat ~/.ssh/id_ed25519.pub | ssh {p}root@{host} 'cat >> {keys}'\n```\n\n\
             Then turn login without password off again. Without it, only keys log in \
             (password logins are off).\n\n\
             # Copy files\n\n\
             There is no SFTP server; `scp` needs `-O` (OpenSSH 9 and later):\n\n\
             ```\nscp -O -P {port} book.epub root@{host}:/mnt/us/\n```\n\n\
             # Host key\n\n\
             The first login asks whether to trust the Kindle's host key; answer yes. The \
             server keeps its host keys in `{hostkey}` and `{ecdsa}`. The fingerprint the \
             computer remembers changes only if those files are deleted.\n",
            keys = keys.display(),
            hostkey = hostkey.display(),
            ecdsa = self.paths.ecdsa_key().display(),
        )
    }

    fn action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        match id {
            "settings:ssh" => self.show_menu(cx),
            "toggle" if self.server.is_some() => {
                self.stop(cx);
                Ok(())
            }
            "toggle" => {
                self.start(cx);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// The process has not exited (as far as `try_wait` can tell).
fn alive(child: &mut Child) -> bool {
    child.try_wait().ok().flatten().is_none()
}

fn toast(cx: &mut HostCtx, text: &str) {
    if let Ok(ui) = cx.ui() {
        ui.toast(text);
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn keys_text(n: usize) -> String {
    match n {
        1 => "1 key".into(),
        n => format!("{n} keys"),
    }
}

impl Drop for SshPlugin {
    /// Fallback for an exit without `on_exit`.
    fn drop(&mut self) {
        self.halt();
    }
}

impl Plugin for SshPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.settings_page("ssh", "SSH server");
        reg.control_tile("toggle", "SSH server", "terminal").state("Off").on_hold("settings:ssh");
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        let r = self.action(cx, id);
        self.refresh_status_row(cx)?;
        r
    }

    fn tile_state(&self, id: &str) -> Option<TileState> {
        if id != "toggle" {
            return None;
        }
        let text = match (&self.server, &self.failure) {
            (Some(s), _) => self.address_text(s.port),
            (None, Some(f)) => f.short.clone(),
            (None, None) => "Off".into(),
        };
        Some(TileState { text, on: Some(self.server.is_some()) })
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        if id == MENU {
            // Any answer closes the menu; `show_menu` opens it again.
            self.menu_status = None;
        }
        match id {
            MENU => match result {
                "status" => self.show_menu(cx),
                "start" => {
                    self.start(cx);
                    self.show_menu(cx)
                }
                "stop" => {
                    self.stop(cx);
                    self.show_menu(cx)
                }
                "port" => {
                    let port = Self::port(cx);
                    cx.input(IN_PORT, "SSH port (1-65535)", &port.to_string())
                }
                "no_password" if cx.setting_bool("no_password", false) => {
                    Self::set(cx, "no_password", json!(false));
                    self.restart_if_running(cx, "Login without password is off");
                    self.show_menu(cx)
                }
                "no_password" => cx.dialog(
                    DLG_NO_PASSWORD,
                    "Login without password",
                    "Anyone who can reach the Kindle over Wi-Fi or USB gets a root shell, with any password or none. Turn it on only to add your key, and off again afterwards.",
                    &[("Cancel", "cancel"), ("Turn on", "on")],
                ),
                "autostart" => {
                    let on = !cx.setting_bool("autostart", false);
                    Self::set(cx, "autostart", json!(on));
                    self.show_menu(cx)
                }
                "help" => {
                    let port = self.server.as_ref().map_or_else(|| Self::port(cx), |s| s.port);
                    let md = self.help_markdown(port);
                    cx.view_markdown(VIEW_HELP, "How to log in", &md)
                }
                _ => Ok(()),
            },
            IN_PORT => {
                let t = result.trim();
                if !t.is_empty() {
                    match t.parse::<u16>() {
                        Ok(p) if p > 0 => {
                            Self::set(cx, "port", json!(p));
                            self.restart_if_running(cx, &format!("Port changed to {p}"));
                        }
                        _ => toast(cx, "The port is a number from 1 to 65535, for example 22 or 2222"),
                    }
                }
                self.show_menu(cx)
            }
            DLG_NO_PASSWORD => {
                if result == "on" {
                    Self::set(cx, "no_password", json!(true));
                    self.restart_if_running(cx, "Login without password is on");
                }
                self.show_menu(cx)
            }
            _ => Ok(()),
        }
    }

    // The server keeps running while the device sleeps: Wi-Fi goes down anyway and the
    // USB network with it, and a server that is still there after wake-up is what the
    // user expects (sessions resume or time out on their own).

    fn on_exit(&mut self, cx: &mut HostCtx, restarting: bool) -> Result<()> {
        // The server is rinky's child, so it stops either way; after a restart
        // (`rinky-ctl.sh restart`, deploys) the next rinky starts it again.
        if restarting && self.server.is_some() {
            log::info!("ssh: exit for a restart: the next start resumes the server");
            Self::set(cx, RESUME, Value::Bool(true));
        }
        self.halt();
        Ok(())
    }

    fn tick_interval(&self) -> Option<Duration> {
        if self.server.is_some() {
            Some(TICK)
        } else if self.autostart_pending {
            Some(FIRST_TICK)
        } else {
            None
        }
    }

    fn on_tick(&mut self, cx: &mut HostCtx) -> Result<()> {
        if std::mem::take(&mut self.autostart_pending) {
            let resume = cx.setting_bool(RESUME, false);
            if resume {
                Self::set(cx, RESUME, Value::Null);
            }
            if (resume || cx.setting_bool("autostart", false)) && self.server.is_none() {
                log::info!("ssh: {}", if resume { "resuming after a restart" } else { "autostart" });
                self.start(cx);
            }
        }
        self.check_exit(cx);
        if self.server.is_some() {
            self.addrs = self.sys.addresses();
        }
        self.refresh_status_row(cx)
    }
}

#[cfg(test)]
mod tests;
