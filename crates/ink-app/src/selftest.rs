//! `rinky selftest`: device, lipc, store, network, Lua and document checks.
//!
//! Prints one `PASS`/`FAIL`/`SKIP` line per check and a summary; the exit status is the
//! number of failures (0 = all passed). Runs on the host too (device checks skip).

use crate::app::providers;
use ink_store::BookState;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What to check.
#[derive(Clone, Debug, Default)]
pub struct SelftestOptions {
    /// Draw the gray test pattern on the screen (Kindle only).
    pub screen: bool,
    /// A book to open and hash.
    pub book: Option<PathBuf>,
    /// An http(s) URL to GET (e.g. a host server at `http://10.0.2.2:PORT/` from the emulator).
    pub url: Option<String>,
    /// Run the lipc checks (Kindle only).
    pub lipc: bool,
    /// Run the device subsystem checks (`ink_device::selftest`).
    pub device: bool,
    /// Let the device checks change and restore settings.
    pub device_write: bool,
    /// NTP server for the device clock check.
    pub ntp: Option<String>,
    /// Base URL of the ink-net mock server (`net-mock-server` example): runs
    /// `ink_net::selftest` (HTTP, SSE, downloads, OPDS, KOSync; with `--lipc` also the
    /// Wi-Fi and deferSuspend hooks through ink-device).
    pub net: Option<String>,
    /// HTTPS base URL of the same mock server.
    pub net_tls: Option<String>,
    /// PEM file with the CA of the HTTPS mock server.
    pub ca: Option<PathBuf>,
    /// Directory for the net checks' downloads (default: the temp dir). On the Kindle,
    /// `/mnt/us/...` tests the userstore (`fuse.fsp` over `/mnt/base-us` on 5.19.5).
    pub net_scratch: Option<PathBuf>,
}

struct Report {
    passed: u32,
    failed: u32,
}

impl Report {
    fn check(&mut self, name: &str, f: impl FnOnce() -> anyhow::Result<String>) {
        let t = Instant::now();
        match f() {
            Ok(detail) => {
                self.passed += 1;
                println!("PASS {name}: {detail} ({} ms)", t.elapsed().as_millis());
            }
            Err(e) => {
                self.failed += 1;
                println!("FAIL {name}: {e:#} ({} ms)", t.elapsed().as_millis());
            }
        }
    }

    fn skip(&self, name: &str, why: &str) {
        println!("SKIP {name}: {why}");
    }

    fn net(&mut self, res: &ink_net::selftest::CheckResult) {
        println!("{}", res.line());
        match &res.outcome {
            Some(Ok(_)) => self.passed += 1,
            Some(Err(_)) => self.failed += 1,
            None => {}
        }
    }
}

/// Runs the checks; returns the number of failures.
pub fn run(opts: &SelftestOptions) -> u32 {
    let mut r = Report { passed: 0, failed: 0 };
    println!("rinky {} selftest", env!("CARGO_PKG_VERSION"));

    // Device.
    #[cfg(target_os = "linux")]
    {
        if opts.screen {
            r.check("device.screen", || {
                use ink_device::{Canvas, RefreshClass, Screen};
                let mut fb = ink_device::ereader::open_screen()?;
                let size = fb.panel_size();
                let mut canvas = Canvas::new(size.w as usize, size.h as usize);
                let mut fonts = ink_text::Fonts::load();
                ink_ui::message::draw_message(&mut canvas, &mut fonts, &["rinky selftest", "16 gray steps below, black on the left"]);
                let step_w = size.w as i32 / 16;
                for i in 0..16 {
                    canvas.fill_rect(i * step_w, size.h as i32 * 2 / 3, step_w, 200, (i * 17) as u8);
                }
                fb.update_all(&canvas, RefreshClass::Gc16)?;
                let shot = fb.screenshot()?;
                let probe = shot.get(step_w / 2, size.h as i32 * 2 / 3 + 100).unwrap_or(255);
                anyhow::ensure!(probe < 32, "framebuffer readback {probe}, expected black");
                Ok(format!("{}x{} framebuffer, pattern drawn", size.w, size.h))
            });
        } else {
            r.skip("device.screen", "pass --screen to draw the test pattern");
        }
        r.check("device.input", || {
            use ink_device::evdev::{choose_touch_device, TouchOverride};
            let devs = ink_device::ereader::input::list_devices();
            let touch = choose_touch_device(&devs, &TouchOverride::default()).map(|d| d.name.clone());
            let power = ink_device::ereader::input::power_key_device(&devs).map(|d| d.name.clone());
            anyhow::ensure!(touch.is_some(), "no touch device in {devs:?}");
            Ok(format!("touch {:?}, power key {:?}", touch.unwrap_or_default(), power))
        });
    }
    #[cfg(not(target_os = "linux"))]
    r.skip("device", "not an e-reader");

    // lipc through child processes.
    if opts.lipc {
        lipc_checks(&mut r);
    } else {
        r.skip("lipc", "pass --lipc on the Kindle");
    }

    // Device subsystems (ink-device): daemons, lipc, input, fb, light, battery,
    // Wi-Fi, USB, clock, suspend readiness.
    if opts.device {
        let o = ink_device::selftest::Options { write: opts.device_write, ntp: opts.ntp.clone() };
        for c in ink_device::selftest::run(&o) {
            let name = format!("device.{}", c.name);
            match c.outcome {
                ink_device::selftest::Outcome::Skip => r.skip(&name, &c.detail),
                ink_device::selftest::Outcome::Pass => r.check(&name, || Ok(format!("{} [{} ms]", c.detail, c.ms))),
                ink_device::selftest::Outcome::Fail => r.check(&name, || anyhow::bail!("{} [{} ms]", c.detail, c.ms)),
            }
        }
    } else {
        r.skip("device.*", "pass --device");
    }

    // Store.
    let tmp = std::env::temp_dir().join(format!("rinky-selftest-{}", std::process::id()));
    r.check("store.sidecar", || {
        std::fs::create_dir_all(&tmp)?;
        let book = tmp.join("book.epub");
        std::fs::write(&book, b"not really a book")?;
        let s = BookState { position: Some("flow:1.2.3".into()), ..Default::default() };
        s.save(&book)?;
        let back = BookState::load(&book)?;
        anyhow::ensure!(back.position == s.position, "read back {:?}", back.position);
        Ok(format!("{}", ink_store::sidecar::sidecar_path(&book).display()))
    });
    // ink-store's own checks (partial MD5 vectors, sidecar recovery, library benchmark,
    // settings, KOReader import, statistics); owner: the store agent.
    for c in ink_store::selftest::run(&ink_store::selftest::SelftestOptions { scratch: tmp.join("store"), ..Default::default() }) {
        r.check(&c.name, || if c.ok { Ok(format!("{} (store: {} ms)", c.detail, c.millis)) } else { anyhow::bail!("{}", c.detail) });
    }
    let _ = std::fs::remove_dir_all(&tmp);
    match &opts.book {
        Some(book) => {
            r.check("store.partial_md5", || Ok(ink_store::partial_md5(book)?));
            r.check("doc.open", || {
                let doc = crate::screens::reader::open_book(&providers(), &ink_store::Settings::default(), book, 1272, 1696)?;
                Ok(format!("'{}', {} pages", doc.metadata().title, doc.page_count().pages))
            });
        }
        None => r.skip("store.partial_md5", "pass --book PATH"),
    }

    // Network.
    r.check("net.tls_config", || {
        let _client = ink_net::UreqClient::new();
        Ok("ureq + rustls client built".into())
    });
    match &opts.url {
        Some(url) => r.check("net.get", || {
            use ink_net::HttpClient;
            let resp = ink_net::UreqClient::new().send(ink_net::Request::get(url.clone()).timeout(Duration::from_secs(10)))?;
            let status = resp.status;
            let body = resp.bytes(16 << 20)?;
            anyhow::ensure!((200..300).contains(&status), "status {status}");
            Ok(format!("{url}: status {status}, {} bytes", body.len()))
        }),
        None => r.skip("net.get", "pass --url URL"),
    }
    match &opts.net {
        Some(base) => net_checks(&mut r, opts, base),
        None => r.skip("net.selftest", "pass --net URL (ink-net mock server)"),
    }

    // Lua.
    r.check("plugin.lua", || {
        let h = ink_plugin::lua::LuaHost::new()?;
        let v = h.eval("return 6 * 7")?;
        anyhow::ensure!(v == "42", "6*7 = {v}");
        Ok(format!("{} sandbox, 6*7 = {v}", h.version()))
    });

    println!("selftest: {} passed, {} failed", r.passed, r.failed);
    r.failed
}

/// ink-net's checks against the mock server. With `--lipc` on the Kindle the Wi-Fi hook
/// reads ink-device's Wi-Fi state and the keep-awake hook calls powerd `deferSuspend`.
fn net_checks(r: &mut Report, opts: &SelftestOptions, base: &str) {
    use ink_net::connectivity::{Connectivity, KeepAwake};
    let ca_pem = match &opts.ca {
        Some(p) => match std::fs::read(p) {
            Ok(b) => Some(b),
            Err(e) => {
                r.check("net.ca", || anyhow::bail!("{}: {e}", p.display()));
                None
            }
        },
        None => None,
    };
    #[cfg(target_os = "linux")]
    let svc = std::cell::RefCell::new(ink_device::ereader::kindle::LipcServices::default());
    #[cfg(target_os = "linux")]
    let wifi = || {
        use ink_device::Wifi;
        match svc.borrow().state() {
            Ok(s) => ink_net::connectivity::wifi_rule(s.enabled, &s.state),
            Err(e) => Err(ink_net::NetError::Offline(format!("wifi state: {e}"))),
        }
    };
    // powerd takes deferSuspend only during its readyToSuspend countdown; in state
    // `active` it answers lipcPropErrInvalidState, which means "nothing to defer".
    #[cfg(target_os = "linux")]
    let keep = |d: Duration| {
        use ink_device::Power;
        svc.borrow_mut().defer_suspend(d).map_err(|e| ink_net::connectivity::defer_error(&e.to_string()))
    };
    #[allow(unused_mut)]
    let (mut wifi_hook, mut keep_hook): (Option<&dyn Connectivity>, Option<&dyn KeepAwake>) = (None, None);
    #[cfg(target_os = "linux")]
    if opts.lipc {
        wifi_hook = Some(&wifi);
        keep_hook = Some(&keep);
    }
    let t = ink_net::selftest::NetSelftest {
        http_base: base.to_string(),
        https_base: opts.net_tls.clone(),
        ca_pem,
        wifi: wifi_hook,
        keep_awake: keep_hook,
        scratch: opts.net_scratch.clone().unwrap_or_else(std::env::temp_dir).join(format!("rinky-net-selftest-{}", std::process::id())),
    };
    t.run(|res| r.net(res));
}

fn lipc_checks(r: &mut Report) {
    use ink_device::lipc::Lipc;
    use ink_device::DeviceError;
    let lipc = Lipc::new();
    r.check("lipc.get_int", || {
        let v = lipc.get_int("com.lab126.powerd", "flIntensity")?;
        Ok(format!("powerd flIntensity = {v}"))
    });
    r.check("lipc.get_str", || Ok(format!("powerd state = {:?}", lipc.get_str("com.lab126.powerd", "state")?)));
    r.check("lipc.set_int", || {
        let v = lipc.get_int("com.lab126.powerd", "flIntensity")?;
        let other = if v > 0 { v - 1 } else { v + 1 };
        lipc.set_int("com.lab126.powerd", "flIntensity", other)?;
        let got = lipc.get_int("com.lab126.powerd", "flIntensity")?;
        lipc.set_int("com.lab126.powerd", "flIntensity", v)?;
        let back = lipc.get_int("com.lab126.powerd", "flIntensity")?;
        anyhow::ensure!(got == other && back == v, "set {other} read {got}, restored {back} (want {v})");
        Ok(format!("flIntensity {v} -> {got} -> {back}"))
    });
    r.check("lipc.bad_property", || match lipc.get_int("com.lab126.powerd", "noSuchProperty") {
        Err(DeviceError::Lipc(msg)) => Ok(format!("error as expected: {}", msg.chars().take(80).collect::<String>())),
        other => anyhow::bail!("expected a lipc error, got {other:?}"),
    });
    r.check("lipc.wait_event_timeout", || {
        let t = Instant::now();
        let ev = lipc.wait_event("com.lab126.powerd", &["noSuchEvent"], Duration::from_secs(1))?;
        anyhow::ensure!(ev.is_none(), "unexpected event {ev:?}");
        Ok(format!("no event, returned after {} ms", t.elapsed().as_millis()))
    });
    r.check("lipc.kill_on_timeout", || {
        let t = Instant::now();
        match lipc.run("lipc-wait-event", &["-s", "30", "com.lab126.powerd", "noSuchEvent"], Duration::from_millis(500)) {
            Err(DeviceError::Timeout(_)) if t.elapsed() < Duration::from_secs(3) => {
                Ok(format!("child killed after {} ms", t.elapsed().as_millis()))
            }
            other => anyhow::bail!("expected a timeout within 3 s, got {other:?} after {:?}", t.elapsed()),
        }
    });
    r.check("lipc.watch", || {
        let mut w = lipc.watch("com.lab126.powerd", &[])?;
        std::thread::sleep(Duration::from_millis(300));
        anyhow::ensure!(w.is_running(), "lipc-wait-event -m exited");
        let ev = w.try_next();
        Ok(format!("watcher running, first event {ev:?}"))
    });
}

/// The book next to the binary (`juliet.epub`), if present.
pub fn default_book(exe: &Path) -> Option<PathBuf> {
    let p = exe.parent()?.join("juliet.epub");
    p.exists().then_some(p)
}
