//! `rinky device ...`: test and diagnostic commands for the device layer, run on
//! the Kindle (or the emulator) and used for the phase 5 evidence. They draw with the
//! built-in bitmap font, so they need nothing above `ink-device`.

/// Usage text.
pub const USAGE: &str = "usage: rinky device COMMAND
  info                              input devices, touch mapping, fb flags, USB, battery, wakeup sources
  events [SECONDS]                  print every input event (touch, keys, lipc, hal, uevents) with times
  touch [--seconds N]               test page; log taps (first touch after start included)
  rotate [portrait|cw|ud|ccw|all] [--seconds N]   rotated test page; taps marked in logical coordinates
  night test [--out DIR] | on | off hardware night mode (grayscale flag + night waveforms)
  screenshot FILE.png [--logical]   what the panel shows, as PNG
  sleep [--seconds N] [--screensaver page,image:PATH,cover,blank] [--defer S]
                                    test page under the device sleep policy (power key, cover, auto-sleep);
                                    the screensaver list is cycled after each wake; --defer: no
                                    auto-sleep or suspend for S seconds from the start (Power::defer_suspend)
  usb [--seconds N] [--drive|--charge|--plan drive,charge]
                                    test page under the USB policy (cable, drive mode, lock screen);
                                    the plan is cycled over successive plug-ins
  wifi status|scan|list|profiles|enable 0|1|airplane 0|1|delete ESSID
  wifi connect|create ESSID [--password-stdin]
  light [BRIGHTNESS [WARMTH]]       frontlight get/set
  amber [show | set HH:MM HH:MM LEVEL on|off]   warmth schedule (powerd) and today's curve
  battery                           battery and charger
  clock [now | ntp [SERVER] | set UNIX_SECONDS]
  restore                           undo journaled settings after a crash (launcher)
  crash|hang [--after S] [--night]  launcher/watchdog tests (abort / stop processing input)
  selftest [--write] [--ntp SERVER] subsystem checks (PASS/FAIL lines; exit status = failures)
environment: RINKY_TOUCH, RINKY_SLEEP, RINKY_AUTOSLEEP, RINKY_SUSPEND,
  RINKY_RTC_WAKE, RINKY_SCREENSAVER, RINKY_USB, RINKY_NTP_SERVER, RINKY_RUN";

/// Runs `rinky device ARGS`; returns the exit status.
pub fn main(args: &[String]) -> i32 {
    #[cfg(target_os = "linux")]
    {
        match kindle::run(args) {
            Ok(code) => code,
            Err(e) => {
                say!("rinky device: {e}");
                1
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        say!("rinky device: needs the Kindle (Linux framebuffer and evdev)\n{USAGE}");
        2
    }
}

/// Option value after `name` (`--seconds 30`).
pub fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// True when the flag is present.
pub fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

#[cfg(target_os = "linux")]
mod kindle {
    use super::{flag, opt, USAGE};
    use crate::canvas::Canvas;
    use crate::clock::{stamp, TimeZone};
    use crate::font;
    use crate::geom::{Point, Rect, Rotation};
    use crate::input::{InputEvent, PowerEvent, TouchPhase};
    use crate::journal::{Entry, Journal};
    use crate::kindle::{self, fb, input, services::LipcServices, system, KindleOptions};
    use crate::lipc::Lipc;
    use crate::screen::{RefreshClass, Screen};
    use crate::services::{Battery, Clock, Light, Wifi};
    use crate::sysfs::SysRoot;
    use crate::{Device, DeviceError, Result};
    use std::time::{Duration, Instant};

    fn seconds(args: &[String], default: u64) -> Duration {
        Duration::from_secs(opt(args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(default))
    }

    fn page(canvas: &mut Canvas, title: &str, lines: &[String]) {
        canvas.clear(255);
        let w = canvas.width as i32;
        canvas.fill_rect(0, 0, w, 150, 0);
        font::draw_centered(canvas, 40, 8, 255, title);
        let mut y = 220;
        for l in lines {
            let mut s = 5;
            while s > 2 && font::text_width(l, s) > w - 60 {
                s -= 1;
            }
            font::draw_text(canvas, 30, y, s, 0, l);
            y += font::CELL_H * s + 16;
        }
        // Gray steps along the bottom (dark on the left).
        let step = w / 16;
        for i in 0..16 {
            canvas.fill_rect(i * step, canvas.height as i32 - 160, step, 120, (i * 17) as u8);
        }
    }

    fn open_full() -> Result<Device> {
        let opts = KindleOptions { power_events: true, ..Default::default() };
        if kobo() {
            return crate::kobo::open(opts);
        }
        kindle::open(opts)
    }

    fn kobo() -> bool {
        crate::platform::Platform::detect() == crate::platform::Platform::Kobo
    }

    /// Light, battery and Wi-Fi of this e-reader without the input loop.
    trait Services: Light + Battery + Wifi {}
    impl<T: Light + Battery + Wifi> Services for T {}

    fn services(lipc: Lipc) -> Box<dyn Services> {
        if kobo() {
            Box::new(crate::kobo::services::KoboServices::new(None, SysRoot::system(), None))
        } else {
            Box::new(LipcServices::standalone(lipc))
        }
    }

    fn mark(canvas: &mut Canvas, p: Point, n: u32) -> Rect {
        let r = Rect::new(p.x - 30, p.y - 30, 60, 60);
        canvas.fill_rect(r.x, r.y, 60, 60, 0);
        font::draw_text(canvas, p.x + 40, p.y - 16, 4, 0, &n.to_string());
        Rect::new(r.x, r.y, 60 + 40 + font::text_width(&n.to_string(), 4) as u32 + 10, 60)
    }

    pub fn run(args: &[String]) -> Result<i32> {
        let cmd = args.first().map(String::as_str).unwrap_or("");
        let rest = if args.len() > 1 { &args[1..] } else { &[] };
        match cmd {
            "info" => info().map(|_| 0),
            "events" => events(Duration::from_secs(rest.first().and_then(|s| s.parse().ok()).unwrap_or(30))).map(|_| 0),
            "touch" => touch(seconds(rest, 60)).map(|_| 0),
            "rotate" => rotate(rest).map(|_| 0),
            "night" => night(rest),
            "screenshot" => screenshot(rest).map(|_| 0),
            "sleep" => sleep(rest).map(|_| 0),
            "usb" => usb(rest).map(|_| 0),
            "wifi" => wifi(rest),
            "light" => light(rest).map(|_| 0),
            "amber" => amber(rest).map(|_| 0),
            "battery" => battery().map(|_| 0),
            "clock" => clock(rest),
            "restore" => restore().map(|_| 0),
            "crash" => crash(rest),
            "hang" => hang(rest),
            "selftest" => Ok(crate::selftest::print(&crate::selftest::run(&crate::selftest::Options {
                write: flag(rest, "--write"),
                ntp: opt(rest, "--ntp").map(str::to_string),
            })) as i32),
            _ => {
                say!("{USAGE}");
                Ok(2)
            }
        }
    }

    fn info() -> Result<()> {
        let sys = SysRoot::system();
        let model = crate::kindle::model::Model::detect(&sys);
        if kobo() {
            out!("model: {} (Kobo {:?}, firmware {})", model.describe(), model.codes.first(), crate::kobo::firmware());
            let w = crate::kobo::wifi::env();
            out!("wifi: module {} interface {} platform {}", w.module, w.interface, w.platform);
        } else {
            let serial = sys.read("/proc/usid").unwrap_or_default();
            out!("model: {} (code {:?})", model.describe(), crate::kindle::model::device_code(&serial));
        }
        let devs = input::list_devices();
        for d in &devs {
            out!("input: {:<16} {:<7} abs {} mt {} keys {}", d.name, d.event, d.has_abs, d.has_mt, d.key_bits);
        }
        if let Some(d) = input::power_key_device(&devs) {
            out!("power key: {} ({})", d.name, d.event);
        }
        if let Some(d) = model.page_keys.and_then(|c| input::page_key_device(&devs, c)) {
            out!("page keys: {} ({})", d.name, d.event);
        }
        let ov = crate::evdev::TouchOverride::parse(&std::env::var("RINKY_TOUCH").unwrap_or_default());
        let fbs = fb::KindleFb::open_passive()?;
        let size = fbs.panel_size();
        match input::TouchDev::open(size.w, size.h, false, &ov) {
            Ok(mut t) => {
                out!("touch: {} {} profile {} mapping {}", t.name, t.path, t.profile.name, t.mapping().describe());
                out!("touch: kernel slot cache {:?}", t.seed());
            }
            Err(e) => out!("touch: {e}"),
        }
        out!("fb: {}x{} grayscale {} rotate {}", size.w, size.h, fbs.grayscale()?, fbs.hw_rotate()?);
        out!("udc: {:?}", crate::usb::read_udc(&sys));
        out!("transport: {:?}", input::detect_transport(&Lipc::new()));
        out!("battery: {:?}", crate::battery::read_sysfs(&sys));
        out!("charger online: {}", crate::battery::external_power(&sys));
        out!("wakeup sources active: {:?}", system::active_wakeup_sources(&sys));
        out!("suspend states: {}", sys.read("/sys/power/state").unwrap_or_default());
        out!("timezone: {} (local {})", std::env::var("TZ").unwrap_or_default(), TimeZone::local().at(std::time::SystemTime::now()).format());
        Ok(())
    }

    fn describe(ev: &InputEvent) -> String {
        match ev {
            InputEvent::Touch(f) => {
                let c: Vec<String> = f.contacts.iter().map(|c| format!("slot {} {:?} {},{}", c.slot, c.phase, c.x, c.y)).collect();
                format!("touch {}", c.join("; "))
            }
            other => format!("{other:?}"),
        }
    }

    fn events(limit: Duration) -> Result<()> {
        let mut dev = open_full()?;
        let size = dev.screen.size();
        let mut c = Canvas::new(size.w as usize, size.h as usize);
        page(&mut c, "device events", &["every event is logged".into(), format!("for {} s", limit.as_secs())]);
        dev.screen.update_all(&c, RefreshClass::Gc16)?;
        out!("{} events: started", stamp());
        let start = Instant::now();
        while start.elapsed() < limit {
            if let Some(ev) = dev.input.next_event(Some(limit.saturating_sub(start.elapsed())))? {
                out!("{} event: {}", stamp(), describe(&ev));
            }
        }
        out!("{} events: done", stamp());
        Ok(())
    }

    fn touch(limit: Duration) -> Result<()> {
        let mut dev = open_full()?;
        let size = dev.screen.size();
        let mut c = Canvas::new(size.w as usize, size.h as usize);
        page(&mut c, "touch test", &["taps are marked and logged".into(), "top right corner: exit".into()]);
        dev.screen.update_all(&c, RefreshClass::Gc16)?;
        out!("{} touch: started", stamp());
        let start = Instant::now();
        let mut n = 0;
        while start.elapsed() < limit {
            let Some(ev) = dev.input.next_event(Some(Duration::from_millis(500)))? else { continue };
            if let InputEvent::Touch(f) = &ev {
                for ct in &f.contacts {
                    if matches!(ct.phase, TouchPhase::Up | TouchPhase::DownUp) {
                        n += 1;
                        out!("{} tap {n} at {},{} ({:?})", stamp(), ct.x, ct.y, ct.phase);
                        if ct.x > size.w as i32 * 3 / 4 && ct.y < 150 {
                            out!("{} touch: exit", stamp());
                            return Ok(());
                        }
                        let r = mark(&mut c, Point::new(ct.x, ct.y), n);
                        dev.screen.update(&c, r, RefreshClass::Du)?;
                    }
                }
            } else {
                out!("{} event: {}", stamp(), describe(&ev));
            }
        }
        Ok(())
    }

    fn parse_rotation(s: &str) -> Option<Rotation> {
        Some(match s {
            "portrait" | "0" => Rotation::Portrait,
            "cw" | "90" | "clockwise" => Rotation::Clockwise,
            "ud" | "180" => Rotation::UpsideDown,
            "ccw" | "270" => Rotation::CounterClockwise,
            _ => return None,
        })
    }

    fn rotate(args: &[String]) -> Result<()> {
        let which = args.first().map(String::as_str).unwrap_or("all");
        let list: Vec<Rotation> = if which == "all" { Rotation::ALL.to_vec() } else { parse_rotation(which).into_iter().collect() };
        if list.is_empty() {
            return Err(DeviceError::Backend(format!("unknown rotation {which}")));
        }
        let per = seconds(args, 20);
        let mut dev = open_full()?;
        for rot in list {
            dev.screen.set_rotation(rot)?;
            let size = dev.screen.size();
            let mut c = Canvas::new(size.w as usize, size.h as usize);
            page(&mut c, &format!("rotation {rot:?}"), &[
                format!("logical {}x{}", size.w, size.h),
                "this text is upright".into(),
                "taps are marked where touched".into(),
            ]);
            // Corner labels in logical coordinates.
            font::draw_text(&mut c, 10, 160, 5, 0, "TL");
            font::draw_text(&mut c, size.w as i32 - 80, size.h as i32 - 220, 5, 0, "BR");
            dev.screen.update_all(&c, RefreshClass::Gc16)?;
            out!("{} rotate: {rot:?} logical {}x{} drawn", stamp(), size.w, size.h);
            let space_panel = dev.screen.panel_size();
            let start = Instant::now();
            let mut n = 0;
            while start.elapsed() < per {
                let Some(ev) = dev.input.next_event(Some(Duration::from_millis(500)))? else { continue };
                if let InputEvent::Touch(f) = ev {
                    for ct in f.contacts.iter().filter(|c| matches!(c.phase, TouchPhase::Up | TouchPhase::DownUp)) {
                        n += 1;
                        let l = rot.from_panel(Point::new(ct.x, ct.y), space_panel);
                        out!("{} rotate {rot:?}: tap {n} panel {},{} -> logical {},{}", stamp(), ct.x, ct.y, l.x, l.y);
                        let r = mark(&mut c, l, n);
                        dev.screen.update(&c, r, RefreshClass::Du)?;
                    }
                }
            }
        }
        dev.screen.set_rotation(Rotation::Portrait)?;
        Ok(())
    }

    fn night(args: &[String]) -> Result<i32> {
        match args.first().map(String::as_str) {
            Some("test") => {
                let out = opt(args, "--out").unwrap_or("/tmp/rinky/night");
                std::fs::create_dir_all(out)?;
                let mut s = fb::KindleFb::open()?;
                let size = s.size();
                let mut c = Canvas::new(size.w as usize, size.h as usize);
                page(&mut c, "night mode test", &["day: black on white".into()]);
                s.update_all(&c, RefreshClass::Gc16)?;
                out!("day: grayscale {} waveform {:?}", s.grayscale()?, s.last_waveform);
                s.screenshot()?.save_png(&std::path::Path::new(out).join("day.png"))?;
                s.set_night_mode(true)?;
                page(&mut c, "night mode test", &["night: hardware inverted".into(), format!("hw flag {}", s.hw_night())]);
                s.update_all(&c, RefreshClass::Gc16)?;
                out!("night full: grayscale {} hw {} waveform {:?} (GCK16 = 8)", s.grayscale()?, s.hw_night(), s.last_waveform);
                c.fill_rect(30, 600, 400, 100, 0);
                s.update(&c, Rect::new(30, 600, 400, 100), RefreshClass::Reagl)?;
                out!("night partial: waveform {:?} (GLKW16 = 9, FULL = 1)", s.last_waveform);
                s.update(&c, Rect::new(30, 600, 400, 100), RefreshClass::Du)?;
                out!("night DU: waveform {:?}", s.last_waveform);
                s.screenshot()?.save_png(&std::path::Path::new(out).join("night-visible.png"))?;
                s.snapshot().save_png(&std::path::Path::new(out).join("night-raw.png"))?;
                std::thread::sleep(Duration::from_secs(opt(args, "--hold").and_then(|v| v.parse().ok()).unwrap_or(3)));
                s.set_night_mode(false)?;
                s.update_all(&c, RefreshClass::Gc16)?;
                out!("day again: grayscale {}", s.grayscale()?);
                Ok(0)
            }
            Some(v @ ("on" | "off")) => {
                let mut s = fb::KindleFb::open()?;
                s.set_night_mode(v == "on")?;
                s.refresh_all()?;
                out!("night {v}: grayscale {} (exits now; the flag is restored on exit)", s.grayscale()?);
                Ok(0)
            }
            _ => {
                say!("{USAGE}");
                Ok(2)
            }
        }
    }

    fn screenshot(args: &[String]) -> Result<()> {
        let file = args.first().ok_or_else(|| DeviceError::Backend("screenshot needs FILE.png".into()))?;
        let s = fb::KindleFb::open_passive()?;
        s.screenshot()?.save_png(std::path::Path::new(file))?;
        out!("{file}: {}x{}", s.size().w, s.size().h);
        Ok(())
    }

    /// A self-drawn book cover (600 x 900) for the `cover` screensaver test: no image
    /// file, so nothing copyrighted.
    fn test_cover() -> Canvas {
        let (w, h) = (600i32, 900i32);
        let mut c = Canvas::new(w as usize, h as usize);
        c.clear(0);
        c.fill_rect(20, 20, w - 40, h - 40, 220);
        c.fill_rect(20, 330, w - 40, 240, 40);
        font::draw_centered(&mut c, 120, 6, 0, "COVER");
        font::draw_centered(&mut c, 400, 5, 255, "rinky");
        font::draw_centered(&mut c, 480, 3, 255, "screensaver test");
        font::draw_centered(&mut c, 720, 3, 0, "a book the app opened");
        c
    }

    /// `page`, `blank`, `image:PATH` or `cover` (a [`test_cover`]).
    fn screensaver_from_spec(spec: &str) -> crate::screensaver::Screensaver {
        if spec == "cover" {
            crate::screensaver::Screensaver::Cover(test_cover())
        } else {
            crate::screensaver::Screensaver::from_setting(spec)
        }
    }

    fn sleep(args: &[String]) -> Result<()> {
        let limit = seconds(args, 600);
        // `--screensaver page,image:/mnt/us/x.png,cover`: the first is used at once, the
        // next after each wake (one run shows every source).
        let specs: Vec<String> = opt(args, "--screensaver")
            .map(|s| s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
            .unwrap_or_default();
        let mut spec_i = 0;
        let mut dev = open_full()?;
        if let Some(s) = specs.first() {
            let saver = screensaver_from_spec(s);
            out!("{} sleep: screensaver {}", stamp(), saver.describe());
            dev.power.set_screensaver(saver)?;
        }
        if let Some(d) = opt(args, "--defer").and_then(|v| v.parse::<u64>().ok()) {
            out!("{} sleep: defer_suspend({d} s)", stamp());
            dev.power.defer_suspend(Duration::from_secs(d))?;
        }
        let size = dev.screen.size();
        let mut c = Canvas::new(size.w as usize, size.h as usize);
        let lines = |wakes: u32, taps: u32| -> Vec<String> {
            vec![
                "sleep test (device layer)".into(),
                format!("awake at {} UTC", stamp()),
                format!("wakes {wakes}  taps {taps}"),
                "power key: sleep / wake".into(),
                "top right corner: exit".into(),
            ]
        };
        page(&mut c, "rinky", &lines(0, 0));
        dev.screen.update_all(&c, RefreshClass::Gc16)?;
        out!("{} sleep: started, auto-sleep {:?}", stamp(), dev.power.auto_sleep());
        let (mut wakes, mut taps) = (0, 0);
        let start = Instant::now();
        while start.elapsed() < limit {
            let Some(ev) = dev.input.next_event(Some(Duration::from_secs(1)))? else { continue };
            match &ev {
                InputEvent::Touch(f) => {
                    for ct in f.contacts.iter().filter(|c| matches!(c.phase, TouchPhase::Up | TouchPhase::DownUp)) {
                        taps += 1;
                        out!("{} tap at {},{}", stamp(), ct.x, ct.y);
                        if ct.x > size.w as i32 * 3 / 4 && ct.y < 150 {
                            out!("{} sleep: exit", stamp());
                            return Ok(());
                        }
                        let r = mark(&mut c, Point::new(ct.x, ct.y), taps);
                        dev.screen.update(&c, r, RefreshClass::Du)?;
                    }
                }
                InputEvent::Power(p) => {
                    out!("{} event: {p:?}", stamp());
                    if *p == PowerEvent::OutOfScreenSaver {
                        wakes += 1;
                        // The device layer restored the page; refresh the status lines.
                        page(&mut c, "rinky", &lines(wakes, taps));
                        dev.screen.update(&c, Rect::new(0, 150, size.w, 500), RefreshClass::Gl16)?;
                        if specs.len() > 1 {
                            spec_i = (spec_i + 1) % specs.len();
                            let saver = screensaver_from_spec(&specs[spec_i]);
                            out!("{} sleep: next screensaver {}", stamp(), saver.describe());
                            dev.power.set_screensaver(saver)?;
                        }
                    }
                }
                other => out!("{} event: {}", stamp(), describe(other)),
            }
        }
        Ok(())
    }

    fn usb(args: &[String]) -> Result<()> {
        let limit = seconds(args, 300);
        // What to choose on successive plug-ins: `--plan drive,charge` (cycled);
        // `--charge` / `--drive` are one-entry plans.
        let plan: Vec<bool> = match opt(args, "--plan") {
            Some(p) => p.split(',').map(|s| s.trim() == "drive").collect(),
            None => vec![!flag(args, "--charge")],
        };
        let mut plugs = 0usize;
        let mut dev = open_full()?;
        let size = dev.screen.size();
        let mut c = Canvas::new(size.w as usize, size.h as usize);
        let plan_text: Vec<&str> = plan.iter().map(|d| if *d { "drive mode" } else { "charge only" }).collect();
        page(&mut c, "USB test", &[format!("cable {:?}", dev.usb.cable()), format!("on plug: {}", plan_text.join(", then "))]);
        dev.screen.update_all(&c, RefreshClass::Gc16)?;
        out!("{} usb: started, cable {:?}, state {:?}", stamp(), dev.usb.cable(), dev.usb.state());
        let start = Instant::now();
        while start.elapsed() < limit {
            let Some(ev) = dev.input.next_event(Some(Duration::from_secs(1)))? else { continue };
            out!("{} event: {}", stamp(), describe(&ev));
            match ev {
                InputEvent::Touch(f) if f.contacts.iter().any(|c| c.x > size.w as i32 * 3 / 4 && c.y < 150 && matches!(c.phase, TouchPhase::Up | TouchPhase::DownUp)) => {
                    out!("{} usb: exit", stamp());
                    return Ok(());
                }
                InputEvent::Usb(true) => {
                    let drive = plan.get(plugs % plan.len().max(1)).copied().unwrap_or(true);
                    plugs += 1;
                    if drive {
                        dev.usb.set_drive_mode(true)?;
                        out!("{} usb: drive mode chosen", stamp());
                    } else {
                        dev.usb.set_charge_only(true)?;
                        out!("{} usb: charge only chosen (state {:?})", stamp(), dev.usb.state());
                        page(&mut c, "USB test", &["charge only".into(), "the files stay on the Kindle".into()]);
                        dev.screen.update_all(&c, RefreshClass::Gc16)?;
                    }
                }
                InputEvent::Usb(false) => out!("{} usb: unplugged", stamp()),
                InputEvent::Power(PowerEvent::LibraryRescan) => {
                    out!("{} usb: library rescan signal", stamp());
                    page(&mut c, "USB test", &["back from USB drive mode".into(), "library rescan requested".into()]);
                    dev.screen.update_all(&c, RefreshClass::Gc16)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn wifi(args: &[String]) -> Result<i32> {
        let mut w = services(Lipc::new().with_timeout(Duration::from_secs(10)));
        let password = || -> Result<Option<String>> {
            if flag(args, "--password-stdin") {
                let mut s = String::new();
                std::io::stdin().read_line(&mut s)?;
                Ok(Some(s.trim_end_matches(['\n', '\r']).to_string()))
            } else {
                Ok(None)
            }
        };
        match args.first().map(String::as_str) {
            Some("status") | None => out!("{:?}", Wifi::state(w.as_ref())?),
            Some("scan") => {
                for n in w.scan()? {
                    out!("{n:?}");
                }
            }
            Some("list") => {
                for n in w.scan_list()? {
                    out!("{n:?}");
                }
            }
            Some("profiles") => {
                for p in w.profiles()? {
                    out!("{p:?}");
                }
            }
            Some("enable") => w.set_enabled(args.get(1).map(String::as_str) == Some("1"))?,
            Some("airplane") => {
                if let Some(v) = args.get(1) {
                    w.set_airplane_mode(v == "1")?;
                }
                out!("airplane {}", w.airplane_mode()?);
            }
            Some("connect") => {
                let essid = args.get(1).ok_or_else(|| DeviceError::Backend("connect needs ESSID".into()))?;
                let t = Instant::now();
                w.connect(essid, password()?.as_deref())?;
                out!("connected to {essid} in {} ms: {:?}", t.elapsed().as_millis(), Wifi::state(w.as_ref())?);
            }
            Some("create") => {
                let essid = args.get(1).ok_or_else(|| DeviceError::Backend("create needs ESSID".into()))?;
                w.create_profile(essid, password()?.as_deref())?;
                out!("profile {essid} saved");
            }
            Some("delete") => {
                let essid = args.get(1).ok_or_else(|| DeviceError::Backend("delete needs ESSID".into()))?;
                w.delete_profile(essid)?;
                out!("profile {essid} deleted");
            }
            Some(_) => {
                say!("{USAGE}");
                return Ok(2);
            }
        }
        Ok(0)
    }

    fn light(args: &[String]) -> Result<()> {
        let mut l = services(Lipc::new());
        if let Some(b) = args.first().and_then(|v| v.parse().ok()) {
            l.set_brightness(b)?;
        }
        if let Some(w) = args.get(1).and_then(|v| v.parse().ok()) {
            l.set_warmth(w)?;
        }
        out!("brightness {} of {:?}, warmth {} of {:?}", l.brightness()?, l.brightness_range(), l.warmth()?, l.warmth_range());
        let sys = SysRoot::system();
        for b in system::backlights(&sys) {
            out!("sysfs {b}: {}", sys.read(&format!("/sys/class/backlight/{b}/brightness")).unwrap_or_default());
        }
        Ok(())
    }

    fn amber(args: &[String]) -> Result<()> {
        let mut l = LipcServices::standalone(Lipc::new());
        if args.first().map(String::as_str) == Some("set") {
            let parse = |i: usize| args.get(i).and_then(|v| crate::light::DayTime::parse(v));
            let mut s = l.amber_schedule()?;
            s.start = parse(1).ok_or_else(|| DeviceError::Backend("start HH:MM".into()))?;
            s.end = parse(2).ok_or_else(|| DeviceError::Backend("end HH:MM".into()))?;
            s.level = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(s.level);
            s.enabled = args.get(4).map(String::as_str) != Some("off");
            l.set_amber_schedule(&s)?;
        }
        let s = l.amber_schedule()?;
        out!("schedule: {s:?}");
        let base = l.warmth()?;
        let curve: Vec<String> = (0..24).map(|h| format!("{h:02}h={}", s.warmth_at(crate::light::DayTime::hm(h, 0), base))).collect();
        out!("warmth by hour (base {base}): {}", curve.join(" "));
        Ok(())
    }

    fn battery() -> Result<()> {
        let b = services(Lipc::new());
        out!("{:?}", b.info()?);
        if kobo() {
            return Ok(());
        }
        let lipc = Lipc::new();
        out!("powerd battLevel {:?} isCharging {:?}", lipc.get_int("com.lab126.powerd", "battLevel"), lipc.get_int("com.lab126.powerd", "isCharging"));
        Ok(())
    }

    fn clock(args: &[String]) -> Result<i32> {
        let ntp_server = if kobo() { Some("pool.ntp.org".to_string()) } else { KindleOptions::default().ntp_server };
        let mut c = kindle::services::KindleClock { lipc: Lipc::new(), ntp_server };
        match args.first().map(String::as_str) {
            Some("now") | None => out!("local {} (utc {})", c.local_now().format(), crate::clock::DateTime::from_unix(crate::clock::unix_secs(c.now()), 0).format()),
            Some("ntp") => {
                let s = c.sync_ntp(args.get(1).map(String::as_str))?;
                out!("ntp {}: offset {} ms, delay {} ms, stratum {}; clock now {}", s.server, s.offset_ms, s.delay_ms, s.stratum, c.local_now().format());
            }
            Some("set") => {
                let secs: u64 = args.get(1).and_then(|v| v.parse().ok()).ok_or_else(|| DeviceError::Backend("set UNIX_SECONDS".into()))?;
                c.set_time(std::time::UNIX_EPOCH + Duration::from_secs(secs))?;
                out!("clock set: {}", c.local_now().format());
            }
            Some(_) => {
                say!("{USAGE}");
                return Ok(2);
            }
        }
        Ok(0)
    }

    fn restore() -> Result<()> {
        let path = std::env::var("RINKY_RUN").map(|r| format!("{r}/restore")).unwrap_or_else(|_| crate::journal::DEFAULT_PATH.into());
        let j = Journal::at(&path);
        let entries = j.entries();
        let lipc = Lipc::new();
        for e in &entries {
            let r = match e {
                Entry::LipcInt { .. } | Entry::LipcStr { .. } if kobo() => continue,
                Entry::LipcInt { publisher, prop, value } => lipc.set_int(publisher, prop, *value),
                Entry::LipcStr { publisher, prop, value } => lipc.set_str(publisher, prop, value),
                Entry::Fb { .. } => continue,
            };
            out!("restore {}: {}", e.line(), if r.is_ok() { "ok".to_string() } else { format!("{r:?}") });
        }
        match fb::restore_after_crash(&entries) {
            Ok(done) => out!("restore fb: {}", done.join(", ")),
            Err(e) => out!("restore fb: {e}"),
        }
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    fn crash(args: &[String]) -> Result<i32> {
        let after = Duration::from_secs(opt(args, "--after").and_then(|v| v.parse().ok()).unwrap_or(10));
        let mut dev = open_full()?;
        if flag(args, "--night") {
            dev.screen.set_night_mode(true)?;
        }
        let size = dev.screen.size();
        let mut c = Canvas::new(size.w as usize, size.h as usize);
        page(&mut c, "crash test", &[format!("rinky aborts in {} s", after.as_secs()), "the launcher must bring the framework back".into(), format!("night mode {}", dev.screen.night_mode())]);
        dev.screen.update_all(&c, RefreshClass::Gc16)?;
        let start = Instant::now();
        while start.elapsed() < after {
            let _ = dev.input.next_event(Some(Duration::from_millis(500)))?;
        }
        out!("{} crash: abort()", stamp());
        std::process::abort();
    }

    fn hang(args: &[String]) -> Result<i32> {
        let after = Duration::from_secs(opt(args, "--after").and_then(|v| v.parse().ok()).unwrap_or(10));
        let mut dev = open_full()?;
        if flag(args, "--night") {
            dev.screen.set_night_mode(true)?;
        }
        let size = dev.screen.size();
        let mut c = Canvas::new(size.w as usize, size.h as usize);
        page(&mut c, "hang test", &[format!("rinky stops in {} s", after.as_secs()), "the watchdog must notice".into(), format!("night mode {}", dev.screen.night_mode())]);
        dev.screen.update_all(&c, RefreshClass::Gc16)?;
        let start = Instant::now();
        while start.elapsed() < after {
            let _ = dev.input.next_event(Some(Duration::from_millis(500)))?;
        }
        out!("{} hang: no more input processing (heartbeat stops)", stamp());
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}
