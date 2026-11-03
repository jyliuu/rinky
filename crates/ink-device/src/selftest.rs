//! Device subsystem checks for `rinky selftest --device` and
//! `rinky device selftest`: daemons and lipc properties (also with the framework
//! stopped), lipc robustness (timeouts, watcher restart, injected events), input,
//! framebuffer flags, light, battery, Wi-Fi, USB, clock and suspend readiness.
//! Read-only unless `write` is set (then it changes and restores brightness, the
//! night flag and the RTC alarm).

use std::time::Instant;

/// What to check.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Also run checks that change and restore settings.
    pub write: bool,
    /// Query this NTP server (`host[:port]`).
    pub ntp: Option<String>,
}

/// Result of one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Worked.
    Pass,
    /// Failed.
    Fail,
    /// Not run (with the reason as detail).
    Skip,
}

/// One check.
#[derive(Clone, Debug)]
pub struct Check {
    /// `area.name`.
    pub name: String,
    /// Result.
    pub outcome: Outcome,
    /// What was seen.
    pub detail: String,
    /// Time taken.
    pub ms: u128,
}

#[allow(dead_code)]
struct Runner {
    checks: Vec<Check>,
}

#[allow(dead_code)]
impl Runner {
    fn check(&mut self, name: &str, f: impl FnOnce() -> Result<String, String>) {
        let t = Instant::now();
        let (outcome, detail) = match f() {
            Ok(d) => (Outcome::Pass, d),
            Err(d) => (Outcome::Fail, d),
        };
        self.checks.push(Check { name: name.into(), outcome, detail, ms: t.elapsed().as_millis() });
    }

    fn skip(&mut self, name: &str, why: &str) {
        self.checks.push(Check { name: name.into(), outcome: Outcome::Skip, detail: why.into(), ms: 0 });
    }
}

/// Prints `PASS|FAIL|SKIP name: detail (ms)` lines and a summary; returns failures.
pub fn print(checks: &[Check]) -> u32 {
    let mut failed = 0;
    for c in checks {
        let tag = match c.outcome {
            Outcome::Pass => "PASS",
            Outcome::Fail => {
                failed += 1;
                "FAIL"
            }
            Outcome::Skip => "SKIP",
        };
        out!("{tag} {}: {} ({} ms)", c.name, c.detail, c.ms);
    }
    out!("device selftest: {} checks, {failed} failed", checks.len());
    failed
}

/// Runs the checks.
pub fn run(opts: &Options) -> Vec<Check> {
    let mut r = Runner { checks: Vec::new() };
    #[cfg(target_os = "linux")]
    kindle::all(&mut r, opts);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = opts;
        r.skip("device", "not a Kindle");
    }
    r.checks
}

#[cfg(target_os = "linux")]
mod kindle {
    use super::{Options, Runner};
    use crate::kindle_mtk::{fb, input, services::LipcServices, system};
    use crate::lipc::{EventHub, Lipc, LipcValue};
    use crate::services::{Battery, Light, Wifi};
    use crate::sysfs::SysRoot;
    use std::time::{Duration, Instant};

    fn running(names: &[&str]) -> Vec<(String, bool)> {
        let mut comms = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(c) = std::fs::read_to_string(e.path().join("comm")) {
                        comms.push(c.trim().to_string());
                    }
                }
            }
        }
        names.iter().map(|n| (n.to_string(), comms.iter().any(|c| c == &n[..n.len().min(15)]))).collect()
    }

    pub(super) fn all(r: &mut Runner, opts: &Options) {
        let lipc = Lipc::new().with_timeout(Duration::from_secs(5));
        let sys = SysRoot::system();

        // Daemons (phase 5: which keep running after `stop lab126_gui`).
        r.check("daemons.framework", || {
            let st = std::process::Command::new("status").arg("lab126_gui").output();
            let st = st.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|e| e.to_string());
            let procs = running(&["KPPMainAppV2", "KPPMainApp", "pillowd", "kb", "cvm", "awesome", "Xorg"]);
            let up: Vec<String> = procs.iter().filter(|(_, r)| *r).map(|(n, _)| n.clone()).collect();
            // com.lab126.kaf (the Java framework) is what powerd asks before handling the
            // power button; it is gone while the framework is stopped.
            let probe = lipc.run("lipc-probe", &["-l"], Duration::from_secs(15)).unwrap_or_default();
            let names = ["com.lab126.kaf", "com.lab126.pillow", "com.lab126.KPPMainApp", "com.lab126.powerd", "com.lab126.wifid", "com.lab126.cmd", "com.lab126.volumd"];
            let listed: Vec<&str> = names.iter().copied().filter(|n| probe.lines().any(|l| l.trim() == *n)).collect();
            Ok(format!("lab126_gui: {st}; running: {}; lipc publishers listed: {}", up.join(" "), listed.join(" ")))
        });
        r.check("daemons.system", || {
            let procs = running(&["powerd", "wifid", "cmd", "volumd", "lipc-daemon", "deviced", "blanket", "appmgrd", "tizen-mtp", "wpa_supplicant"]);
            let up: Vec<&str> = procs.iter().filter(|(_, r)| *r).map(|(n, _)| n.as_str()).collect();
            let down: Vec<&str> = procs.iter().filter(|(_, r)| !*r).map(|(n, _)| n.as_str()).collect();
            let needed = ["powerd", "wifid", "cmd", "volumd"];
            let missing: Vec<&str> = needed.iter().copied().filter(|n| down.contains(n)).collect();
            if missing.is_empty() {
                Ok(format!("running: {}; not running: {}", up.join(" "), down.join(" ")))
            } else {
                Err(format!("missing {missing:?}; running: {}", up.join(" ")))
            }
        });

        // lipc properties of each daemon.
        let props: [(&str, &[(&str, bool)]); 4] = [
            ("com.lab126.powerd", &[("state", false), ("flIntensity", true), ("currentAmberLevel", true), ("battLevel", true), ("isCharging", true), ("preventScreenSaver", true)]),
            ("com.lab126.wifid", &[("cmState", false), ("enable", true), ("signalStrength", false), ("scanState", false), ("profileCount", true)]),
            ("com.lab126.cmd", &[("wirelessEnable", true), ("activeInterface", false)]),
            ("com.lab126.volumd", &[("driveModeState", true), ("userstoreIsAvailable", true), ("disableMounting", true), ("useUsbForNetwork", true)]),
        ];
        for (publisher, list) in props {
            let short = publisher.rsplit('.').next().unwrap_or(publisher);
            r.check(&format!("lipc.{short}"), || {
                let mut out = Vec::new();
                for (p, int) in list {
                    let t = Instant::now();
                    let v = if *int { lipc.get_int(publisher, p).map(|v| v.to_string()) } else { lipc.get_str(publisher, p) };
                    match v {
                        Ok(v) => out.push(format!("{p}={v:?} ({} ms)", t.elapsed().as_millis())),
                        Err(e) => return Err(format!("{p}: {e}")),
                    }
                }
                Ok(out.join(", "))
            });
        }
        r.check("lipc.hash_props", || {
            let scan = lipc.hash_get("com.lab126.wifid", "scanList").map_err(|e| e.to_string())?;
            let cur = lipc.hash_get("com.lab126.wifid", "currentEssid").map_err(|e| e.to_string())?;
            let prof = lipc.hash_get("com.lab126.wifid", "profileData").map_err(|e| e.to_string())?;
            Ok(format!("scanList {} entries, currentEssid {:?}, profileData {} profiles", scan.len(), cur.first().and_then(|h| h.get("essid")), prof.len()))
        });
        r.check("lipc.timeout_kills_child", || {
            let t = Instant::now();
            match lipc.run("lipc-wait-event", &["-s", "30", "com.lab126.powerd", "noSuchEvent"], Duration::from_millis(500)) {
                Err(crate::DeviceError::Timeout(_)) if t.elapsed() < Duration::from_secs(3) => {
                    Ok(format!("child killed after {} ms", t.elapsed().as_millis()))
                }
                other => Err(format!("expected a timeout within 3 s, got {other:?} after {:?}", t.elapsed())),
            }
        });
        r.check("lipc.bad_property", || match lipc.get_int("com.lab126.powerd", "noSuchProperty") {
            Err(crate::DeviceError::Lipc(m)) => Ok(format!("error as expected: {}", m.chars().take(70).collect::<String>())),
            other => Err(format!("expected a lipc error, got {other:?}")),
        });
        r.check("lipc.subscriptions", || {
            let mut hub = EventHub::new(Lipc::new(), || {});
            for p in crate::events::PUBLISHERS {
                hub.subscribe(p, &[]);
            }
            std::thread::sleep(Duration::from_millis(800));
            let st = hub.status();
            if let Some(bad) = st.iter().find(|s| !s.running) {
                return Err(format!("{} watcher not running: {st:?}", bad.publisher));
            }
            // An injected hal event (udev scripts send these with the framework stopped).
            let _ = Lipc::new().send_event("com.lab126.hal", "rinkySelftest", &[LipcValue::Int(42)]);
            let mut got = None;
            let end = Instant::now() + Duration::from_secs(5);
            while Instant::now() < end && got.is_none() {
                if let Some(e) = hub.next_timeout(Duration::from_millis(200)) {
                    if e.publisher == "com.lab126.hal" && e.event.name == "rinkySelftest" {
                        got = Some(e.event.line());
                    }
                }
            }
            let got = got.ok_or("injected com.lab126.hal event not received within 5 s")?;
            // kill -9 one watcher: the hub must start it again.
            let pid = st.iter().find(|s| s.publisher == "com.lab126.cmd").and_then(|s| s.pid).ok_or("no cmd watcher pid")?;
            // SAFETY: kill(2) on our own child.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            let end = Instant::now() + Duration::from_secs(6);
            loop {
                hub.maintain();
                let s = hub.status();
                let cmd = s.iter().find(|s| s.publisher == "com.lab126.cmd").cloned();
                if let Some(c) = cmd.filter(|c| c.running && c.restarts >= 1) {
                    return Ok(format!(
                        "{} watchers running ({}); hal event '{got}' received; cmd watcher {pid} killed and restarted as {:?}",
                        s.len(),
                        s.iter().map(|x| x.publisher.rsplit('.').next().unwrap_or("")).collect::<Vec<_>>().join(","),
                        c.pid
                    ));
                }
                if Instant::now() > end {
                    return Err(format!("cmd watcher not restarted: {s:?}"));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });

        // Input.
        r.check("input.touch", || {
            let devs = input::list_devices();
            let ov = crate::evdev::TouchOverride::parse(&std::env::var("RINKY_TOUCH").unwrap_or_default());
            let s = fb::MtkScreen::open_passive().map_err(|e| e.to_string())?;
            let size = crate::Screen::panel_size(&s);
            let mut t = input::TouchDev::open(size.w, size.h, false, &ov).map_err(|e| format!("{e}; devices {devs:?}"))?;
            let seed = t.seed();
            Ok(format!("{} ({}) profile {} mapping {}; kernel slot cache {:?}", t.name, t.path, t.profile.name, t.mapping().describe(), seed))
        });
        r.check("input.power_key", || {
            let devs = input::list_devices();
            devs.iter().find(|d| d.name.contains("pwrkey")).map(|d| format!("{} ({})", d.name, d.event)).ok_or_else(|| format!("no pwrkey in {devs:?}"))
        });

        // Framebuffer.
        r.check("fb.flags", || {
            let s = fb::MtkScreen::open_passive().map_err(|e| e.to_string())?;
            let size = crate::Screen::panel_size(&s);
            Ok(format!("{}x{} grayscale {} rotate {}", size.w, size.h, s.grayscale().map_err(|e| e.to_string())?, s.hw_rotate().map_err(|e| e.to_string())?))
        });
        if opts.write {
            r.check("fb.night_flag", || {
                let mut s = fb::MtkScreen::open().map_err(|e| e.to_string())?;
                crate::Screen::set_night_mode(&mut s, true).map_err(|e| e.to_string())?;
                let on = s.grayscale().map_err(|e| e.to_string())?;
                let hw = s.hw_night();
                crate::Screen::set_night_mode(&mut s, false).map_err(|e| e.to_string())?;
                let off = s.grayscale().map_err(|e| e.to_string())?;
                if hw && on == fb::GRAYSCALE_8BIT_INVERTED && off != fb::GRAYSCALE_8BIT_INVERTED {
                    Ok(format!("grayscale {on} in night mode (hardware), {off} after"))
                } else {
                    Err(format!("hardware flag not kept: night {on} (hw {hw}), day {off}"))
                }
            });
        } else {
            r.skip("fb.night_flag", "pass --write");
        }

        // Light.
        let mut svc = LipcServices::standalone(lipc.clone());
        r.check("light.get", || {
            let b = svc.brightness().map_err(|e| e.to_string())?;
            let w = svc.warmth().map_err(|e| e.to_string())?;
            let sched = svc.amber_schedule().map_err(|e| e.to_string())?;
            Ok(format!("flIntensity {b} of {:?}, currentAmberLevel {w}; schedule {} {}..{} level {} mode {:?}", svc.brightness_range(), if sched.enabled { "on" } else { "off" }, sched.start, sched.end, sched.level, sched.mode))
        });
        if opts.write {
            r.check("light.set", || {
                let b = svc.brightness().map_err(|e| e.to_string())?;
                let other = if b > 0 { b - 1 } else { b + 1 };
                svc.set_brightness(other).map_err(|e| e.to_string())?;
                let got = svc.brightness().map_err(|e| e.to_string())?;
                let sysfs = SysRoot::system().read("/sys/class/backlight/fp9967-bl1/brightness").unwrap_or_default();
                svc.set_brightness(b).map_err(|e| e.to_string())?;
                let w = svc.warmth().map_err(|e| e.to_string())?;
                svc.set_warmth(if w > 0 { w - 1 } else { w + 1 }).map_err(|e| e.to_string())?;
                let w2 = svc.warmth().map_err(|e| e.to_string())?;
                svc.set_warmth(w).map_err(|e| e.to_string())?;
                (got == other && w2 != w)
                    .then(|| format!("flIntensity {b} -> {got} (sysfs bl1 {sysfs}) -> {b}; warmth {w} -> {w2} -> {w}"))
                    .ok_or(format!("brightness set {other} read {got}; warmth {w} -> {w2}"))
            });
        } else {
            r.skip("light.set", "pass --write");
        }

        // Battery.
        r.check("battery", || {
            let i = svc.info().map_err(|e| e.to_string())?;
            let p = lipc.get_int("com.lab126.powerd", "battLevel").map_err(|e| e.to_string())?;
            Ok(format!("{}% charging {} status {:?} temp {:?} C, {:?} mV, charger {} ({}); powerd battLevel {p}", i.percent, i.charging, i.status, i.temperature_c, i.voltage_mv, i.external_power, i.source))
        });

        // Wi-Fi.
        r.check("wifi.state", || {
            let s = Wifi::state(&svc).map_err(|e| e.to_string())?;
            let air = svc.airplane_mode().map_err(|e| e.to_string())?;
            let list = svc.scan_list().map_err(|e| e.to_string())?;
            let prof = svc.profiles().map_err(|e| e.to_string())?;
            Ok(format!("{s:?}; airplane {air}; scan list {:?}; {} saved profiles", list.iter().map(|n| (&n.essid, n.signal, n.secured)).collect::<Vec<_>>(), prof.len()))
        });

        // USB.
        r.check("usb", || {
            let udc = crate::usb::read_udc(&sys);
            let cable = crate::usb::cable_from(udc.as_ref().map(|u| u.1.as_str()), crate::battery::external_power(&sys), None);
            let transport = input::detect_transport(&lipc);
            Ok(format!("udc {udc:?}, cable {cable:?}, transport {transport:?}"))
        });

        // Clock.
        r.check("clock", || {
            let tz = crate::clock::TimeZone::local();
            let now = std::time::SystemTime::now();
            Ok(format!("local {} (TZ {:?}, /etc/localtime {})", tz.at(now).format(), std::env::var("TZ").ok(), std::fs::read_link("/etc/localtime").map(|p| p.display().to_string()).unwrap_or_else(|e| e.to_string())))
        });
        match &opts.ntp {
            Some(server) => r.check("clock.ntp", || {
                let s = crate::clock::sntp_query(server, Duration::from_secs(5)).map_err(|e| e.to_string())?;
                Ok(format!("{}: offset {} ms, delay {} ms, stratum {}", s.server, s.offset_ms, s.delay_ms, s.stratum))
            }),
            None => r.skip("clock.ntp", "pass --ntp SERVER"),
        }

        // Suspend readiness.
        r.check("power.suspend_ready", || {
            let states = sys.read("/sys/power/state").map_err(|e| e.to_string())?;
            let wc = system::read_wakeup_count(&sys, Duration::from_secs(2));
            let active = system::active_wakeup_sources(&sys);
            let busy: Vec<&(String, u64)> = active.iter().filter(|(n, _)| n.contains("usb")).collect();
            if !states.contains("mem") {
                return Err(format!("/sys/power/state = {states:?}"));
            }
            Ok(format!(
                "states {states:?}; wakeup_count {}; usb wakeup sources {busy:?}",
                wc.map(|c| format!("readable ({c})")).unwrap_or_else(|| "blocked (a wakeup source is active: suspend will be refused)".into())
            ))
        });
        if opts.write {
            r.check("power.rtc_alarm", || {
                let rtc0 = system::rtc_now(&sys).ok_or("no /sys/class/rtc/rtc0/since_epoch")?;
                let got = system::arm_rtc(&sys, Duration::from_secs(3600)).map_err(|e| e.to_string())?;
                system::clear_rtc(&sys);
                let after = sys.read("/sys/class/rtc/rtc0/wakealarm").unwrap_or_default();
                let sysclock = crate::clock::unix_secs(std::time::SystemTime::now());
                ((got - rtc0 - 3600).abs() <= 2 && after.is_empty())
                    .then(|| format!("wakealarm armed at RTC {got} (RTC now {rtc0} + 3600; system clock {sysclock}) and cleared"))
                    .ok_or(format!("wakealarm {got} (RTC now {rtc0}), after clear {after:?}"))
            });
        } else {
            r.skip("power.rtc_alarm", "pass --write");
        }
    }
}
