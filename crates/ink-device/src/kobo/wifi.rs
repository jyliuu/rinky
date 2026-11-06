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

use crate::services::{WifiNetwork, WifiState};
use crate::{DeviceError, Result};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WPA_CONF: &str = "/mnt/onboard/.kobo/wpa_supplicant.conf";
const NICKEL_CONF: &str = "/mnt/onboard/.kobo/Kobo/Kobo eReader.conf";
/// `CM_WIFI_CTRL` on `/dev/ntx_io`.
const NTX_WIFI_CTRL: libc::c_ulong = 208;

/// One Wi-Fi operation at a time (the UI thread, the connect thread, suspend).
static BUSY: Mutex<()> = Mutex::new(());

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
fn saved_env_path() -> String {
    format!("{}/nickel.env", std::env::var("RINKY_RUN").unwrap_or_else(|_| "/tmp/rinky".into()))
}

/// A variable from rinky's environment (the launcher copies Nickel's), else from the
/// launcher's saved copy, else from the environment of a firmware process that has it
/// (`udevd`, `nickel`).
pub fn firmware_env(name: &str) -> Option<String> {
    if let Some(v) = std::env::var(name).ok().filter(|v| !v.is_empty()) {
        return Some(v);
    }
    let saved = std::fs::read_to_string(saved_env_path()).unwrap_or_default();
    if let Some(v) = saved.lines().find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix('='))).filter(|v| !v.is_empty()) {
        return Some(v.to_string());
    }
    let procs = std::fs::read_dir("/proc").ok()?;
    for p in procs.flatten() {
        let comm = std::fs::read_to_string(p.path().join("comm")).unwrap_or_default();
        if !matches!(comm.trim(), "udevd" | "nickel" | "systemd-udevd") {
            continue;
        }
        let env = std::fs::read(p.path().join("environ")).unwrap_or_default();
        for kv in env.split(|b| *b == 0) {
            let kv = String::from_utf8_lossy(kv);
            if let Some(v) = kv.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// The chip's module, interface and platform. Without Nickel's environment: the one
/// folder in `/drivers` is the platform, and a MediaTek one (`mt66xx`) means
/// `wlan_drv_gen4m`.
pub fn env() -> WifiEnv {
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
    let out = Command::new(prog).args(args).output().map_err(|e| DeviceError::Backend(format!("{prog}: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(DeviceError::Backend(format!("{prog} {}: {} {}", args.join(" "), out.status, err.trim())));
    }
    Ok(text)
}

fn module_loaded(name: &str) -> bool {
    std::fs::read_to_string("/proc/modules").unwrap_or_default().lines().any(|l| l.split(' ').next() == Some(name))
}

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// `ioctl(/dev/ntx_io, cmd, arg)`, the argument passed by value (KOReader `ntx_io.lua`).
pub fn ntx_io(cmd: libc::c_ulong, arg: libc::c_int) -> Result<()> {
    let path = c"/dev/ntx_io";
    // SAFETY: plain open/ioctl/close on a NUL-terminated path; the ioctl takes an int.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let r = libc::ioctl(fd, cmd as _, arg);
        let e = std::io::Error::last_os_error();
        libc::close(fd);
        if r < 0 {
            return Err(DeviceError::Backend(format!("ntx_io {cmd} {arg}: {e}")));
        }
    }
    Ok(())
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

/// The interface exists and is up.
pub fn is_up() -> bool {
    let e = env();
    std::fs::read_to_string(format!("/sys/class/net/{}/flags", e.interface))
        .ok()
        .and_then(|f| u32::from_str_radix(f.trim().trim_start_matches("0x"), 16).ok())
        .is_some_and(|f| f & 1 != 0)
}

fn wpa_running() -> bool {
    std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|p| {
        std::fs::read_to_string(p.path().join("comm")).is_ok_and(|c| c.trim() == "wpa_supplicant")
    })
}

/// The regulatory domain Nickel uses (`WifiRegulatoryDomain=` in its settings).
fn country() -> Option<String> {
    let conf = std::fs::read_to_string(NICKEL_CONF).ok()?;
    conf.lines().find_map(|l| l.strip_prefix("WifiRegulatoryDomain=")).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Powers the radio, loads the driver, brings the interface up and starts
/// `wpa_supplicant` (which joins a known network by itself).
pub fn start() -> Result<()> {
    let _g = lock();
    let e = env();
    log::info!("wifi: on ({} on {}, {})", e.module, e.interface, e.platform);
    match e.module.as_str() {
        "moal" => ntx_io(NTX_WIFI_CTRL, 1)?,
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
                ntx_io(NTX_WIFI_CTRL, 1)?;
            }
        }
    }
    pause(250);
    if !e.mtk() {
        let mut params = Vec::new();
        if let Some(cc) = country() {
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
        run("wpa_supplicant", &args)?;
    }
    Ok(())
}

/// Releases the lease, stops `wpa_supplicant`, takes the interface down, unloads the
/// driver (not on MediaTek, as KOReader) and powers the radio off.
pub fn stop() -> Result<()> {
    let _g = lock();
    if !is_up() && !wpa_running() {
        return Ok(());
    }
    let e = env();
    log::info!("wifi: off");
    release_ip(&e);
    let _ = e.wpa(&["terminate"]);
    let _ = run("ifconfig", &[&e.interface, "down"]);
    match e.module.as_str() {
        "moal" => {
            e.rmmod("moal");
            e.rmmod("mlan");
            pause(250);
            ntx_io(NTX_WIFI_CTRL, 0)?;
        }
        "wlan_drv_gen4m" => std::fs::write("/dev/wmtWifi", "0")?,
        m => {
            e.rmmod(m);
            if module_loaded("sdio_wifi_pwr") {
                e.rmmod("sdio_wifi_pwr");
            } else {
                pause(250);
                let _ = ntx_io(NTX_WIFI_CTRL, 0);
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

/// Gets an address (blocks up to 30 s).
pub fn obtain_ip() -> Result<()> {
    let e = env();
    release_ip(&e);
    if std::path::Path::new("/sbin/dhcpcd").exists() {
        run("dhcpcd", &["-d", "-t", "30", "-w", &e.interface])?;
    } else {
        let host = run("hostname", &["-s"]).map(|h| h.trim().to_string()).unwrap_or_default();
        let host = if host.is_empty() || host == "(none)" { "kobo".to_string() } else { host };
        let hn = format!("hostname:{host}");
        // -q: quit once leased; -b: keep trying in the background otherwise.
        run("udhcpc", &["-S", "-i", &e.interface, "-s", "/etc/udhcpc.d/default.script", "-x", &hn, "-b", "-q"])?;
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
pub fn bars(rssi: i32) -> u8 {
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
    if !is_up() {
        return WifiState { enabled: false, state: "NA".into(), essid: None, signal: None };
    }
    let e = env();
    let kv = status(&e);
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
    WifiState { enabled: true, state: state.into(), essid: essid.filter(|_| state != "NA"), signal }
}

/// Parses `wpa_cli scan_results` (`bssid / frequency / signal level / flags / ssid`),
/// strongest first, one entry per name.
pub fn parse_scan(text: &str, known: &[String]) -> Vec<WifiNetwork> {
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
pub fn parse_networks(text: &str) -> Vec<(i64, String)> {
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

/// Scans and returns what was found (waits up to 8 s for the results).
pub fn scan() -> Result<Vec<WifiNetwork>> {
    let e = env();
    e.wpa(&["scan"])?;
    let start = Instant::now();
    let mut last = Vec::new();
    while start.elapsed() < Duration::from_secs(8) {
        pause(1000);
        last = scan_results()?;
        if !last.is_empty() && start.elapsed() >= Duration::from_secs(3) {
            break;
        }
    }
    Ok(last)
}

/// The last scan's results.
pub fn scan_results() -> Result<Vec<WifiNetwork>> {
    let e = env();
    let known: Vec<String> = networks().unwrap_or_default().into_iter().map(|(_, s)| s).collect();
    Ok(parse_scan(&e.wpa(&["scan_results"])?, &known))
}

/// `"text"` as wpa_supplicant wants a string value.
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Saves a network (replacing one of the same name) and returns its id.
pub fn save_network(essid: &str, password: Option<&str>) -> Result<i64> {
    let e = env();
    for (id, s) in networks()? {
        if s == essid {
            e.wpa(&["remove_network", &id.to_string()])?;
        }
    }
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
    for (id, s) in networks()? {
        if s == essid {
            e.wpa(&["remove_network", &id.to_string()])?;
        }
    }
    let _ = e.wpa(&["save_config"]);
    Ok(())
}

/// Joins a saved network, waits for the association (20 s) and gets an address.
pub fn connect(essid: &str) -> Result<()> {
    let e = env();
    let id = networks()?
        .into_iter()
        .find(|(_, s)| s == essid)
        .map(|(id, _)| id)
        .ok_or_else(|| DeviceError::Backend(format!("no saved network {essid}")))?;
    e.wpa(&["select_network", &id.to_string()])?;
    let r = wait_associated(Duration::from_secs(20));
    // select_network disables the others; enable them again (in memory only) so
    // wpa_supplicant and Nickel still join them later.
    let _ = e.wpa(&["enable_network", "all"]);
    r?;
    obtain_ip()
}

/// Waits until `wpa_supplicant` is associated.
pub fn wait_associated(limit: Duration) -> Result<()> {
    let e = env();
    let start = Instant::now();
    loop {
        if get(&status(&e), "wpa_state") == Some("COMPLETED") {
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
    let e = env();
    release_ip(&e);
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
