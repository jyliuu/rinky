//! Wi-Fi on a Kobo without Nickel: the radio, the driver modules, `wpa_supplicant` and
//! DHCP, as KOReader's `enable-wifi.sh`, `disable-wifi.sh` and `obtain-ip.sh` do.
//!
//! - Power and driver by `WIFI_MODULE` (from Nickel's environment, which the launcher
//!   passes on): `moal` (NXP, Libra 2 second revision) powers the radio with
//!   `ioctl(/dev/ntx_io, 208, 1)` and loads `mlan` first; `wlan_drv_gen4m` (MediaTek)
//!   loads `wmt_drv`, `wmt_chrdev_wifi`, `wmt_cdev_bt`, pokes `/proc/driver/wmt_dbg` and
//!   writes `1` to `/dev/wmtWifi`; the others load `sdio_wifi_pwr` (or the ntx_io
//!   ioctl when that module is missing). Modules live in `/drivers/$PLATFORM/wifi`
//!   (`mt66xx` on MediaTek).
//! - `wpa_supplicant` runs on Nickel's own `/mnt/onboard/.kobo/wpa_supplicant.conf`,
//!   so the networks Nickel knows are known here and the other way round. It is driven
//!   with `wpa_cli`.
//! - DHCP: `dhcpcd` when the firmware has it, else busybox `udhcpc`.

use crate::ereader::system::processes_named;
use crate::services::{WifiNetwork, WifiState};
use crate::{DeviceError, Result};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const WPA_CONF: &str = "/mnt/onboard/.kobo/wpa_supplicant.conf";
/// Limit for a tool that should answer at once (`wpa_cli`, `insmod`, `ifconfig`).
const TOOL_TIMEOUT: Duration = Duration::from_secs(15);
/// `CM_WIFI_CTRL` on `/dev/ntx_io`.
const NTX_WIFI_CTRL: libc::c_ulong = 208;

/// One Wi-Fi operation at a time (the UI thread, the connect thread, suspend).
static BUSY: Mutex<()> = Mutex::new(());
/// The last [`state`] read and when; cleared by every change rinky makes.
static STATE: Mutex<Option<(Instant, WifiState)>> = Mutex::new(None);
/// How long a state read is reused (each one runs `wpa_cli`; the control center and the
/// settings pages read it on every build).
const STATE_FOR: Duration = Duration::from_secs(10);
/// The radio is being turned on ([`start_in_background`]).
static STARTING: AtomicBool = AtomicBool::new(false);

fn changed() {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    BUSY.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the firmware's environment says about the Wi-Fi chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiEnv {
    /// Driver module (`moal`, `8821cs`, `wlan_drv_gen4m`, …).
    pub module: String,
    /// Network interface (`wlan0`, `eth0` on old models).
    pub interface: String,
    /// `PLATFORM` (`mx6sll-ntx`, `mt8113t-ntx`), the folder under `/drivers`.
    pub platform: String,
}

/// Where the launcher saves Nickel's environment (`$RINKY_RUN/nickel.env`), for rinky
/// commands run after Nickel stopped.
fn saved_env_path() -> std::path::PathBuf {
    ink_platform::run_dir().join("nickel.env")
}

/// The value in `NAME=value` when it is not empty.
fn value_of<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.strip_prefix(name)?.strip_prefix('=').filter(|v| !v.is_empty())
}

/// A variable from rinky's environment (the launcher copies Nickel's), else from the
/// launcher's saved copy, else from the environment of a firmware process that has it
/// (`udevd`, `nickel`).
fn firmware_env(name: &str) -> Option<String> {
    if let Some(v) = std::env::var(name).ok().filter(|v| !v.is_empty()) {
        return Some(v);
    }
    let saved = std::fs::read_to_string(saved_env_path()).unwrap_or_default();
    if let Some(v) = saved.lines().find_map(|l| value_of(l, name)) {
        return Some(v.to_string());
    }
    for dir in processes_named(&["udevd", "nickel", "systemd-udevd"]) {
        let env = std::fs::read(dir.join("environ")).unwrap_or_default();
        for kv in env.split(|b| *b == 0) {
            if let Some(v) = value_of(&String::from_utf8_lossy(kv), name) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// The chip's module, interface and platform, worked out once ([`read_env`]).
pub fn env() -> &'static WifiEnv {
    static ENV: OnceLock<WifiEnv> = OnceLock::new();
    ENV.get_or_init(read_env)
}

/// The chip's module, interface and platform. Without Nickel's environment: the one
/// folder in `/drivers` is the platform, and a MediaTek one (`mt66xx`) means
/// `wlan_drv_gen4m`.
fn read_env() -> WifiEnv {
    let platform = firmware_env("PLATFORM").unwrap_or_else(|| {
        let dirs: Vec<String> =
            std::fs::read_dir("/drivers").into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        if dirs.len() == 1 { dirs[0].clone() } else { String::new() }
    });
    let module = firmware_env("WIFI_MODULE").unwrap_or_else(|| {
        if std::path::Path::new(&format!("/drivers/{platform}/mt66xx")).is_dir() {
            "wlan_drv_gen4m".into()
        } else {
            log::warn!("wifi: WIFI_MODULE unknown (start rinky from its launcher)");
            "sdio_wifi".into()
        }
    });
    WifiEnv { module, interface: firmware_env("INTERFACE").unwrap_or_else(|| "wlan0".into()), platform }
}

fn run(prog: &str, args: &[&str]) -> Result<String> {
    crate::lipc::run_tool(prog, args, TOOL_TIMEOUT)
}

/// Runs a tool that leaves a daemon behind (`wpa_supplicant -B`, a DHCP client) with its
/// output to /dev/null, so the daemon holds no pipe of ours; kills it after `limit`.
fn run_daemon(prog: &str, args: &[&str], limit: Duration) -> Result<()> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| DeviceError::Backend(format!("{prog}: {e}")))?;
    let end = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() { Ok(()) } else { Err(DeviceError::Backend(format!("{prog} {}: {status}", args.join(" ")))) };
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(DeviceError::Timeout(format!("{prog} after {limit:?}")));
        }
        pause(100);
    }
}

fn module_loaded(name: &str) -> bool {
    std::fs::read_to_string("/proc/modules").unwrap_or_default().lines().any(|l| l.split(' ').next() == Some(name))
}

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

impl WifiEnv {
    fn mtk(&self) -> bool {
        self.module == "wlan_drv_gen4m"
    }

    fn kmod_dir(&self) -> String {
        if self.mtk() {
            format!("/drivers/{}/mt66xx", self.platform)
        } else {
            format!("/drivers/{}/wifi", self.platform)
        }
    }

    fn insmod(&self, name: &str, params: &[String]) -> Result<()> {
        if module_loaded(name) {
            return Ok(());
        }
        let mut path = format!("{}/{name}.ko", self.kmod_dir());
        if !std::path::Path::new(&path).exists() {
            path = format!("/drivers/{}/{name}.ko", self.platform);
        }
        let mut args = vec![path.as_str()];
        args.extend(params.iter().map(String::as_str));
        run("insmod", &args)?;
        pause(250);
        Ok(())
    }

    fn rmmod(&self, name: &str) {
        if module_loaded(name) {
            pause(250);
            if let Err(e) = run("rmmod", &[name]) {
                log::warn!("wifi: {e}");
            }
        }
    }

    fn wpa(&self, args: &[&str]) -> Result<String> {
        let mut a = vec!["-i", self.interface.as_str()];
        a.extend_from_slice(args);
        let out = run("wpa_cli", &a)?;
        let t = out.trim();
        if t == "FAIL" || t.starts_with("Failed to connect") {
            return Err(DeviceError::Backend(format!("wpa_cli {}: {t}", args.join(" "))));
        }
        Ok(out)
    }
}

/// The interface exists and is up (or is being brought up).
pub fn is_up() -> bool {
    STARTING.load(Ordering::Relaxed) || iface_up()
}

/// The interface exists and is up (`IFF_UP`).
pub fn iface_up() -> bool {
    std::fs::read_to_string(format!("/sys/class/net/{}/flags", env().interface))
        .ok()
        .and_then(|f| u32::from_str_radix(f.trim().trim_start_matches("0x"), 16).ok())
        .is_some_and(|f| f & 1 != 0)
}

fn wpa_running() -> bool {
    !processes_named(&["wpa_supplicant"]).is_empty()
}

/// Powers the radio, loads the driver, brings the interface up and starts
/// `wpa_supplicant` (which joins a known network by itself). Each step is skipped when
/// it was done already (a radio left on by Nickel).
pub fn start() -> Result<()> {
    let _g = lock();
    changed();
    let e = env();
    log::info!("wifi: on ({} on {}, {})", e.module, e.interface, e.platform);
    match e.module.as_str() {
        "moal" => super::ntx_io(NTX_WIFI_CTRL, 1)?,
        "wlan_drv_gen4m" => {
            for m in ["wmt_drv", "wmt_chrdev_wifi", "wmt_cdev_bt", e.module.as_str()] {
                e.insmod(m, &[])?;
            }
            let dbg = "/proc/driver/wmt_dbg";
            let _ = std::fs::write(dbg, "0xDB9DB9");
            let _ = std::fs::write(dbg, "7 9 0");
            pause(1000);
            let _ = std::fs::write(dbg, "0xDB9DB9");
            let _ = std::fs::write(dbg, "7 9 1");
            std::fs::write("/dev/wmtWifi", "1")?;
        }
        _ => {
            if std::path::Path::new(&format!("{}/sdio_wifi_pwr.ko", e.kmod_dir())).exists() {
                e.insmod("sdio_wifi_pwr", &[])?;
            } else {
                super::ntx_io(NTX_WIFI_CTRL, 1)?;
            }
        }
    }
    pause(250);
    if !e.mtk() {
        let mut params = Vec::new();
        if let Some(cc) = super::nickel_setting("WifiRegulatoryDomain") {
            match e.module.as_str() {
                "8821cs" => params.push(format!("rtw_country_code={cc}")),
                "moal" => params.push(format!("reg_alpha2={cc}")),
                _ => {}
            }
        }
        if e.module == "moal" {
            params.push("mod_para=nxp/wifi_mod_para_sd8987.conf".into());
            e.insmod("mlan", &[])?;
        }
        e.insmod(&e.module, &params)?;
    }
    pause(if e.module == "moal" { 2000 } else { 1000 });
    run("ifconfig", &[&e.interface, "up"])?;
    if !wpa_running() {
        let driver = if matches!(e.module.as_str(), "moal" | "wlan_drv_gen4m") { "nl80211" } else { "wext" };
        let (conf, extra): (&str, &[&str]) =
            if std::path::Path::new(WPA_CONF).exists() { (WPA_CONF, &[]) } else { ("/etc/wpa_supplicant/wpa_supplicant.conf", &["-s"]) };
        let mut args = vec!["-D", driver];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["-i", &e.interface, "-c", conf, "-C", "/var/run/wpa_supplicant", "-B"]);
        run_daemon("wpa_supplicant", &args, TOOL_TIMEOUT)?;
    }
    Ok(())
}

/// [`start`], then joins the saved network wpa_supplicant picks and gets an address, on
/// a thread (the radio takes seconds to come up); `then` runs once there is an address.
/// While it runs, [`state`] says `PENDING` and a second call does nothing.
pub fn start_in_background(then: impl FnOnce() + Send + 'static) {
    if STARTING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("wifi-on".into()).spawn(move || {
        let joined = start().and_then(|()| wait_associated(Duration::from_secs(15))).and_then(|()| obtain_ip());
        STARTING.store(false, Ordering::SeqCst);
        match joined {
            Ok(()) => {
                log::info!("wifi: connected");
                then();
            }
            Err(e) => log::info!("wifi: no saved network joined: {e}"),
        }
    });
    if let Err(e) = spawned {
        STARTING.store(false, Ordering::SeqCst);
        log::warn!("wifi: no thread: {e}");
    }
}

/// Releases the lease, stops `wpa_supplicant`, takes the interface down, unloads the
/// driver (not on MediaTek, as KOReader) and powers the radio off.
pub fn stop() -> Result<()> {
    let _g = lock();
    changed();
    if !is_up() && !wpa_running() {
        return Ok(());
    }
    let e = env();
    log::info!("wifi: off");
    release_ip(e);
    let _ = e.wpa(&["terminate"]);
    let _ = run("ifconfig", &[&e.interface, "down"]);
    match e.module.as_str() {
        "moal" => {
            e.rmmod("moal");
            e.rmmod("mlan");
            pause(250);
            super::ntx_io(NTX_WIFI_CTRL, 0)?;
        }
        "wlan_drv_gen4m" => std::fs::write("/dev/wmtWifi", "0")?,
        m => {
            e.rmmod(m);
            if module_loaded("sdio_wifi_pwr") {
                e.rmmod("sdio_wifi_pwr");
            } else {
                pause(250);
                let _ = super::ntx_io(NTX_WIFI_CTRL, 0);
            }
        }
    }
    Ok(())
}

fn release_ip(e: &WifiEnv) {
    let resolv = std::fs::read("/etc/resolv.conf").ok();
    if std::path::Path::new("/sbin/dhcpcd").exists() {
        let _ = run("dhcpcd", &["-d", "-k", &e.interface]);
    }
    let _ = run("killall", &["-q", "-TERM", "udhcpc", "default.script"]);
    // The lease script rewrites resolv.conf on release; keep the old one (KOReader).
    if let Some(old) = resolv {
        if std::fs::read("/etc/resolv.conf").ok().as_ref() != Some(&old) {
            let _ = std::fs::write("/etc/resolv.conf", old);
        }
    }
}

/// Gets an address (blocks up to 30 s). Not with the interface down: the radio was
/// turned off meanwhile (before a suspend), and a DHCP client would stay behind.
pub fn obtain_ip() -> Result<()> {
    changed();
    if !iface_up() {
        return Err(DeviceError::Backend("DHCP: the interface is down".into()));
    }
    let e = env();
    release_ip(e);
    if std::path::Path::new("/sbin/dhcpcd").exists() {
        run_daemon("dhcpcd", &["-d", "-t", "30", "-w", &e.interface], Duration::from_secs(35))?;
    } else {
        let host = run("hostname", &["-s"]).map(|h| h.trim().to_string()).unwrap_or_default();
        let host = if host.is_empty() || host == "(none)" { "kobo".to_string() } else { host };
        let hn = format!("hostname:{host}");
        // -q: quit once leased; -b: keep trying in the background otherwise.
        run_daemon("udhcpc", &["-S", "-i", &e.interface, "-s", "/etc/udhcpc.d/default.script", "-x", &hn, "-b", "-q"], Duration::from_secs(35))?;
    }
    Ok(())
}

/// `wpa_cli status` as key/value pairs.
fn status(e: &WifiEnv) -> Vec<(String, String)> {
    e.wpa(&["status"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect()
}

fn get<'a>(kv: &'a [(String, String)], key: &str) -> Option<&'a str> {
    kv.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Signal bars 0..=5 from an RSSI in dBm.
fn bars(rssi: i32) -> u8 {
    match rssi {
        r if r >= -55 => 5,
        r if r >= -65 => 4,
        r if r >= -72 => 3,
        r if r >= -80 => 2,
        r if r >= -88 => 1,
        _ => 0,
    }
}

/// The state in wifid's words: `CONNECTED` (associated and an address), `PENDING`
/// (associating or waiting for DHCP), `NA` (radio on, not connected).
pub fn state() -> WifiState {
    if STARTING.load(Ordering::Relaxed) {
        return WifiState { enabled: true, state: "PENDING".into(), essid: None, signal: None };
    }
    let mut cached = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, s)) = cached.as_ref().filter(|(at, _)| at.elapsed() < STATE_FOR) {
        return s.clone();
    }
    let s = read_state();
    *cached = Some((Instant::now(), s.clone()));
    s
}

/// [`state`] read from the interface and `wpa_cli`.
fn read_state() -> WifiState {
    if !is_up() {
        return WifiState { enabled: false, state: "NA".into(), essid: None, signal: None };
    }
    let e = env();
    let kv = status(e);
    let wpa = get(&kv, "wpa_state").unwrap_or("");
    let ip = get(&kv, "ip_address").is_some();
    let essid = get(&kv, "ssid").map(str::to_string);
    let state = match wpa {
        "COMPLETED" if ip => "CONNECTED",
        "COMPLETED" | "ASSOCIATING" | "ASSOCIATED" | "AUTHENTICATING" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE" => "PENDING",
        _ => "NA",
    };
    let signal = (state == "CONNECTED")
        .then(|| e.wpa(&["signal_poll"]).ok())
        .flatten()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("RSSI=")).and_then(|v| v.trim().parse().ok()))
        .map(bars);
    WifiState { enabled: true, state: state.into(), essid: essid.filter(|_| state == "CONNECTED"), signal }
}

/// What the network worker needs to know about the link: the interface, the address
/// (sysfs and `getifaddrs`), and `wpa_supplicant`'s association.
pub fn probe() -> crate::netman::Probe {
    use crate::netman::{link, Probe};
    if !iface_up() {
        return Probe::default();
    }
    let e = env();
    let sys = std::path::Path::new("/sys");
    if !link::may_be_associated(link::operstate(sys, &e.interface).as_deref()) {
        // Not associated, sysfs says: no wpa_cli (this runs every 10 s while no
        // network is found).
        return Probe { radio: true, ..Probe::default() };
    }
    let address = link::has_address(sys, &e.interface);
    let kv = status(e);
    let associated = get(&kv, "wpa_state") == Some("COMPLETED");
    let signal = address
        .then(|| e.wpa(&["signal_poll"]).ok())
        .flatten()
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("RSSI=")).and_then(|v| v.trim().parse().ok()))
        .map(bars);
    Probe { radio: true, address, associated, essid: get(&kv, "ssid").map(str::to_string).filter(|s| !s.is_empty()), signal }
}

/// Parses `wpa_cli scan_results` (`bssid / frequency / signal level / flags / ssid`),
/// strongest first, one entry per name.
fn parse_scan(text: &str, known: &[String]) -> Vec<WifiNetwork> {
    let mut v: Vec<(i32, WifiNetwork)> = Vec::new();
    for l in text.lines().filter(|l| l.contains('\t')) {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 5 || f[4].is_empty() {
            continue;
        }
        let rssi: i32 = f[2].trim().parse().unwrap_or(-100);
        let essid = f[4].to_string();
        let secured = f[3].contains("WPA") || f[3].contains("WEP") || f[3].contains("SAE");
        if let Some(i) = v.iter().position(|(_, n)| n.essid == essid) {
            if v[i].0 >= rssi {
                continue;
            }
            v.remove(i);
        }
        let known = known.contains(&essid);
        v.push((rssi, WifiNetwork { essid, signal: bars(rssi), secured, known }));
    }
    v.sort_by_key(|(rssi, _)| std::cmp::Reverse(*rssi));
    v.into_iter().map(|(_, n)| n).collect()
}

/// Saved networks: `(id, ssid)` from `wpa_cli list_networks`.
fn parse_networks(text: &str) -> Vec<(i64, String)> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split('\t');
            let id = f.next()?.trim().parse().ok()?;
            Some((id, f.next()?.to_string()))
        })
        .collect()
}

/// The saved networks.
pub fn networks() -> Result<Vec<(i64, String)>> {
    Ok(parse_networks(&env().wpa(&["list_networks"])?))
}

/// Starts a scan without waiting for it (wpa_supplicant joins a saved network it finds).
pub fn request_scan() -> Result<()> {
    env().wpa(&["scan"]).map(|_| ())
}

/// Scans and returns what was found (waits up to 8 s for the results).
pub fn scan() -> Result<Vec<WifiNetwork>> {
    let e = env();
    request_scan()?;
    let start = Instant::now();
    let mut raw = String::new();
    while start.elapsed() < Duration::from_secs(8) {
        pause(1000);
        raw = e.wpa(&["scan_results"])?;
        if raw.lines().count() > 1 && start.elapsed() >= Duration::from_secs(3) {
            break;
        }
    }
    Ok(parse_scan(&raw, &known_names()))
}

/// The last scan's results.
pub fn scan_results() -> Result<Vec<WifiNetwork>> {
    Ok(parse_scan(&env().wpa(&["scan_results"])?, &known_names()))
}

fn known_names() -> Vec<String> {
    networks().unwrap_or_default().into_iter().map(|(_, s)| s).collect()
}

/// `"text"` as wpa_supplicant wants a string value.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Removes every saved network with this name.
fn remove_named(e: &WifiEnv, essid: &str) -> Result<()> {
    for (id, s) in networks()? {
        if s == essid {
            e.wpa(&["remove_network", &id.to_string()])?;
        }
    }
    Ok(())
}

/// Saves a network (replacing one of the same name) and returns its id.
pub fn save_network(essid: &str, password: Option<&str>) -> Result<i64> {
    let e = env();
    remove_named(e, essid)?;
    let id: i64 = e.wpa(&["add_network"])?.trim().parse().map_err(|_| DeviceError::Backend("add_network: no id".into()))?;
    let ids = id.to_string();
    e.wpa(&["set_network", &ids, "ssid", &quoted(essid)])?;
    match password.filter(|p| !p.is_empty()) {
        Some(p) => e.wpa(&["set_network", &ids, "psk", &quoted(p)])?,
        None => e.wpa(&["set_network", &ids, "key_mgmt", "NONE"])?,
    };
    e.wpa(&["enable_network", &ids])?;
    if let Err(err) = e.wpa(&["save_config"]) {
        log::warn!("wifi: {err} (network kept until wpa_supplicant stops)");
    }
    Ok(id)
}

/// Forgets a saved network.
pub fn forget(essid: &str) -> Result<()> {
    let e = env();
    remove_named(e, essid)?;
    let _ = e.wpa(&["save_config"]);
    Ok(())
}

/// The id of the saved network with this name.
pub fn network_id(essid: &str) -> Result<Option<i64>> {
    Ok(networks()?.into_iter().find(|(_, s)| s == essid).map(|(id, _)| id))
}

/// Joins saved network `id`, waits for the association (20 s) and gets an address.
pub fn connect(id: i64) -> Result<()> {
    select(id)?;
    let r = wait_associated(Duration::from_secs(20));
    // select_network disables the others; enable them again (in memory only) so
    // wpa_supplicant and Nickel still join them later.
    let _ = env().wpa(&["enable_network", "all"]);
    r?;
    obtain_ip()
}

/// Has `wpa_supplicant` join saved network `id` (returns at once).
pub fn select(id: i64) -> Result<()> {
    changed();
    env().wpa(&["select_network", &id.to_string()]).map(|_| ())
}

/// Waits until `wpa_supplicant` is associated.
fn wait_associated(limit: Duration) -> Result<()> {
    let e = env();
    let start = Instant::now();
    loop {
        if get(&status(e), "wpa_state") == Some("COMPLETED") {
            return Ok(());
        }
        if start.elapsed() >= limit {
            return Err(DeviceError::Timeout(format!("wifi association after {limit:?}")));
        }
        pause(500);
    }
}

/// Disconnects (the radio stays on).
pub fn disconnect() -> Result<()> {
    changed();
    let e = env();
    release_ip(e);
    e.wpa(&["disconnect"]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_results_strongest_first_one_per_name() {
        let text = "bssid / frequency / signal level / flags / ssid\n\
            aa:bb:cc:00:00:01\t2412\t-70\t[WPA2-PSK-CCMP][ESS]\tHome\n\
            aa:bb:cc:00:00:02\t5180\t-50\t[WPA2-PSK-CCMP][ESS]\tHome\n\
            aa:bb:cc:00:00:03\t2437\t-85\t[ESS]\tCafe\n\
            aa:bb:cc:00:00:04\t2437\t-60\t[ESS]\t\n";
        let v = parse_scan(text, &["Home".to_string()]);
        assert_eq!(
            v,
            [
                WifiNetwork { essid: "Home".into(), signal: 5, secured: true, known: true },
                WifiNetwork { essid: "Cafe".into(), signal: 1, secured: false, known: false },
            ]
        );
    }

    #[test]
    fn saved_networks() {
        let text = "network id / ssid / bssid / flags\n0\tHome\tany\t[CURRENT]\n3\tCafe Wifi\tany\t\n";
        assert_eq!(parse_networks(text), [(0, "Home".to_string()), (3, "Cafe Wifi".to_string())]);
        assert_eq!(quoted(r#"a"b"#), r#""a\"b""#);
    }
}
