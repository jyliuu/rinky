//! Verifier (cycle 1) attacks on ink-device, written against the public API only:
//! hostile timing for the sleep policy, a flapping lipc watcher, rapid and odd touch
//! streams, hostile parser input and hostile screensaver images.
//!
//! A test marked `#[ignore = "verifier: ..."]` shows a defect the verifier found; it
//! fails until the product code is fixed (run with `cargo test -- --ignored`).
//! Linux-only and 32-bit-only tests run in the cross-built test binary on the guest.

use ink_device::canvas::Canvas;
use ink_device::evdev::{
    RawEvent, TouchDecoder, TouchMapping, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID,
    EV_ABS, EV_SYN, SYN_REPORT,
};
use ink_device::input::{PowerEvent, Timestamp, TouchContact, TouchPhase};
use ink_device::lipc::{EventHub, Lipc};
use ink_device::screensaver::{self, Screensaver};
use ink_device::sleep::{Resume, SleepConfig, SleepController, SleepPlatform, SleepState};
use ink_device::{DeviceError, Result, Size};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-verify1-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------------------------
// Sleep policy

/// Records every platform call; suspend is refused (EBUSY, like the emulator) unless
/// `refuse` is false, then it "resumes" at `clock`.
#[derive(Default)]
struct Plat {
    calls: Vec<String>,
    refuse: bool,
    rtc: bool,
    clock: Option<Instant>,
    /// How far `clock` moves during `restore_screen` (a GC16 flash that waits for the
    /// panel, as `MXCFB_WAIT_FOR_UPDATE_COMPLETE` does on the device).
    restore_takes: Duration,
    /// How far `clock` moves during `suspend` (a refused attempt first waits up to 2 s
    /// for `/sys/power/wakeup_count`).
    suspend_takes: Duration,
}

impl SleepPlatform for Plat {
    fn show_screensaver(&mut self) -> Result<()> {
        self.calls.push("screensaver".into());
        Ok(())
    }
    fn restore_screen(&mut self) -> Result<()> {
        self.calls.push("restore".into());
        if let Some(c) = self.clock.as_mut() {
            *c += self.restore_takes;
        }
        Ok(())
    }
    fn set_light(&mut self, on: bool) -> Result<()> {
        self.calls.push(format!("light {on}"));
        Ok(())
    }
    fn suspend(&mut self, _rtc: Option<Duration>) -> Result<Resume> {
        self.calls.push("suspend".into());
        if let Some(c) = self.clock.as_mut() {
            *c += self.suspend_takes;
        }
        if self.refuse {
            return Err(DeviceError::Io(std::io::Error::from_raw_os_error(16)));
        }
        Ok(Resume { slept: Duration::from_secs(30), by_rtc: self.rtc })
    }
    fn after_resume(&mut self) -> Result<()> {
        self.calls.push("regrab".into());
        Ok(())
    }
    fn now(&self) -> Instant {
        self.clock.unwrap_or_else(Instant::now)
    }
}

fn count(p: &Plat, what: &str) -> usize {
    p.calls.iter().filter(|c| c.as_str() == what).count()
}

/// Ten power presses at mixed intervals (bounces, presses right at the debounce edge,
/// slow presses) with refused suspend attempts in between: every accepted press
/// toggles the state, screensaver/restore and light off/on stay paired and
/// alternate, and the final state matches the last screen call.
#[test]
fn power_key_burst_keeps_screen_and_light_calls_paired() {
    let t0 = Instant::now();
    let mut c = SleepController::new(SleepConfig::default(), t0);
    let mut p = Plat { refuse: true, ..Default::default() };
    let gaps = [50u64, 2500, 120, 900, 30, 610, 3000, 590, 10, 650];
    let mut t = t0;
    let mut accepted = 0;
    for g in gaps {
        // Timers run right before each press (a suspend attempt once the screensaver
        // has been up for the 2 s delay; refused, as on the emulator).
        let _ = c.tick(t + ms(g - 1), &mut p);
        t += ms(g);
        let before = c.state();
        let ev = c.power_key(t, &mut p);
        if ev.is_empty() {
            assert_eq!(c.state(), before, "an ignored press changed the state");
        } else {
            accepted += 1;
            assert_ne!(c.state(), before, "an accepted press did not toggle the state");
        }
    }
    assert!(accepted >= 4, "only {accepted} of 10 presses were accepted");
    let (shows, restores) = (count(&p, "screensaver"), count(&p, "restore"));
    let (off, on) = (count(&p, "light false"), count(&p, "light true"));
    match c.state() {
        SleepState::Asleep => assert!(shows == restores + 1 && off == on + 1, "{:?}", p.calls),
        SleepState::Awake => assert!(shows == restores && off == on, "{:?}", p.calls),
    }
    let screen: Vec<&str> = p.calls.iter().map(String::as_str).filter(|c| *c == "screensaver" || *c == "restore").collect();
    assert!(screen.windows(2).all(|w| w[0] != w[1]), "screen calls do not alternate: {screen:?}");
    assert!(count(&p, "suspend") >= 1, "no suspend attempt was made while asleep");
}

/// The press that wakes the device from a real suspend reaches the input loop only
/// after `wake()` has re-grabbed touch, restored the page with a GC16 flash
/// (`MXCFB_WAIT_FOR_UPDATE_COMPLETE`) and turned the light on: the loop reads the
/// power fd after `tick()` returns and stamps the press with `Instant::now()`. The
/// controller starts its 600 ms debounce at the resume time read *before* that work
/// (`last_change = Some(after)`), so a restore that takes 700 ms lets the wake press
/// put the device straight back to sleep. Here the fake restore advances the clock by
/// 700 ms and the press arrives 5 ms after the restore finished.
#[test]
fn wake_press_delivered_after_a_slow_restore_is_ignored() {
    let t0 = Instant::now();
    let resumed_at = t0 + Duration::from_secs(3);
    let mut c = SleepController::new(SleepConfig::default(), t0);
    let mut p = Plat { clock: Some(resumed_at), restore_takes: ms(700), ..Default::default() };
    c.power_key(t0, &mut p);
    let ev = c.tick(t0 + Duration::from_secs(2), &mut p);
    assert_eq!(ev.last(), Some(&PowerEvent::OutOfScreenSaver), "resume by a key should wake: {ev:?}");
    let restore_done = p.now();
    assert_eq!(restore_done, resumed_at + ms(700));
    let ev = c.power_key(restore_done + ms(5), &mut p);
    assert!(ev.is_empty() && c.state() == SleepState::Awake, "the wake press slept the device again: {ev:?}");
}

/// A refused suspend blocks the input loop for up to 2 s (the `wakeup_count` read
/// limit in `kernel_suspend`). The retry is counted from `now` *before* that attempt
/// (`next_suspend = now + retry`), so with `RINKY_SUSPEND_RETRY` 1 or 2 (the
/// setting accepts 1) the next attempt is already due when the refused one returns:
/// `next_event` returns the queued `Suspending`, ticks again and never reaches
/// `poll`, so the power key and touch are not read (e2e:
/// `device-verify1-suspend-retry-wake`). The next attempt must lie after the moment
/// the refused attempt returned.
#[test]
fn refused_suspend_retry_counts_from_the_end_of_the_attempt() {
    let t0 = Instant::now();
    let cfg = SleepConfig { retry: Duration::from_secs(1), ..SleepConfig::default() };
    let mut c = SleepController::new(cfg, t0);
    let mut p = Plat { refuse: true, clock: Some(t0), suspend_takes: Duration::from_secs(2), ..Default::default() };
    c.power_key(t0, &mut p);
    let t = t0 + Duration::from_secs(2);
    p.clock = Some(t);
    assert_eq!(c.tick(t, &mut p), vec![PowerEvent::Suspending]);
    let returned = p.now();
    assert_eq!(returned, t + Duration::from_secs(2));
    let next = c.deadline().expect("a retry is scheduled");
    assert!(next > returned, "the next attempt was due {:?} before the refused attempt returned", returned - next);
}

/// A deferral that ends while the screensaver is up: no suspend before the end, one
/// attempt right after it.
#[test]
fn deferral_ending_while_asleep_suspends_at_the_end() {
    let t0 = Instant::now();
    let mut c = SleepController::new(SleepConfig::default(), t0);
    let mut p = Plat { refuse: true, ..Default::default() };
    c.defer_suspend(Duration::from_secs(30), t0, &mut p);
    c.power_key(t0 + Duration::from_secs(1), &mut p);
    for s in [3u64, 10, 20, 29] {
        assert!(c.tick(t0 + Duration::from_secs(s), &mut p).is_empty(), "suspend at {s} s while deferred");
    }
    assert_eq!(count(&p, "suspend"), 0);
    assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(30)));
    let ev = c.tick(t0 + Duration::from_secs(30), &mut p);
    assert_eq!(ev, vec![PowerEvent::Suspending]);
    assert_eq!(count(&p, "suspend"), 1);
}

// ---------------------------------------------------------------------------------
// Touch decoder

fn ev(t: u64, ty: u16, code: u16, value: i32) -> RawEvent {
    RawEvent { time: Timestamp::from_millis(t), ty, code, value }
}

fn decode(events: &[RawEvent]) -> Vec<TouchContact> {
    let mut d = TouchDecoder::new(TouchMapping::identity(1272, 1696));
    events.iter().filter_map(|e| d.feed(*e)).flat_map(|f| f.contacts).collect()
}

/// 2000 very short taps (down and up in one SYN frame, as the PW12 chip reports them)
/// at alternating places, each with a new tracking id; the kernel omits a position
/// that equals the slot's cached value. Every tap must come out once, where it was.
#[test]
fn rapid_short_taps_each_come_out_once_where_tapped() {
    let spots = [(100, 200), (1100, 1500), (1100, 200), (100, 1500)];
    let mut evs = Vec::new();
    let mut last = (-1, -1);
    for i in 0..2000u64 {
        let (x, y) = spots[(i % 4) as usize];
        let t = i * 30;
        evs.push(ev(t, EV_ABS, ABS_MT_TRACKING_ID, i as i32 + 1));
        if x != last.0 {
            evs.push(ev(t, EV_ABS, ABS_MT_POSITION_X, x));
        }
        if y != last.1 {
            evs.push(ev(t, EV_ABS, ABS_MT_POSITION_Y, y));
        }
        last = (x, y);
        evs.push(ev(t, EV_ABS, ABS_MT_TRACKING_ID, -1));
        evs.push(ev(t, EV_SYN, SYN_REPORT, 0));
    }
    let got = decode(&evs);
    assert_eq!(got.len(), 2000);
    for (i, c) in got.iter().enumerate() {
        assert_eq!((c.phase, c.x, c.y), (TouchPhase::DownUp, spots[i % 4].0, spots[i % 4].1), "tap {i}");
    }
}

/// Protocol B lets one slot's contact end and a new one start in the same frame
/// (`TRACKING_ID -1`, then a new id): fast taps with two fingers. Expected: the old
/// contact lifts where it was and the new one goes down (and later up) where it is.
#[test]
fn lift_and_new_touch_in_one_frame_are_two_contacts() {
    let got = decode(&[
        ev(0, EV_ABS, ABS_MT_TRACKING_ID, 5),
        ev(0, EV_ABS, ABS_MT_POSITION_X, 100),
        ev(0, EV_ABS, ABS_MT_POSITION_Y, 200),
        ev(0, EV_SYN, SYN_REPORT, 0),
        ev(40, EV_ABS, ABS_MT_TRACKING_ID, -1),
        ev(40, EV_ABS, ABS_MT_TRACKING_ID, 6),
        ev(40, EV_ABS, ABS_MT_POSITION_X, 1100),
        ev(40, EV_ABS, ABS_MT_POSITION_Y, 1500),
        ev(40, EV_SYN, SYN_REPORT, 0),
        ev(80, EV_ABS, ABS_MT_TRACKING_ID, -1),
        ev(80, EV_SYN, SYN_REPORT, 0),
    ]);
    let at = |phases: &[TouchPhase], x: i32, y: i32| got.iter().any(|c| phases.contains(&c.phase) && (c.x, c.y) == (x, y));
    assert!(at(&[TouchPhase::Up], 100, 200), "first touch must lift at 100,200: {got:?}");
    assert!(at(&[TouchPhase::Down, TouchPhase::DownUp], 1100, 1500), "second touch must go down at 1100,1500: {got:?}");
    assert!(at(&[TouchPhase::Up, TouchPhase::DownUp], 1100, 1500), "second touch must lift at 1100,1500: {got:?}");
}

/// Slot numbers outside 0..10 (negative, huge) and position values far outside the
/// panel: no panic, positions clamped to the panel.
#[test]
fn hostile_slot_numbers_and_positions_are_clamped() {
    let got = decode(&[
        ev(0, EV_ABS, ABS_MT_SLOT, 1_000_000),
        ev(0, EV_ABS, ABS_MT_TRACKING_ID, 1),
        ev(0, EV_ABS, ABS_MT_POSITION_X, i32::MAX),
        ev(0, EV_ABS, ABS_MT_POSITION_Y, i32::MIN),
        ev(0, EV_ABS, ABS_MT_SLOT, -7),
        ev(0, EV_ABS, ABS_MT_TRACKING_ID, 2),
        ev(0, EV_ABS, ABS_MT_POSITION_X, -5),
        ev(0, EV_ABS, ABS_MT_POSITION_Y, 99_999),
        ev(0, EV_SYN, SYN_REPORT, 0),
    ]);
    assert_eq!(got.len(), 2, "{got:?}");
    for c in &got {
        assert!((0..1272).contains(&c.x) && (0..1696).contains(&c.y), "{c:?}");
    }
}

// ---------------------------------------------------------------------------------
// lipc

fn write_tool(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A `lipc-wait-event -m` that prints one line and exits (lipc tools print their
/// errors on stdout, see `Lipc::hash_call`) must be respawned with the growing
/// back-off the module promises (1, 2, 4 s ...), not every second forever: the reset
/// on "delivered an event" makes such a watcher respawn once a second and feeds its
/// error line to the app as an event each time.
#[test]
fn hub_backs_off_a_watcher_that_dies_after_each_line() {
    let dir = temp_dir("hub-flap");
    write_tool(&dir, "lipc-wait-event", r#"echo run >> "$(dirname "$0")/runs"; echo 'Failed to open lipc handle'; exit 1"#);
    let mut hub = EventHub::new(Lipc::new().with_bin_dir(&dir), || {});
    hub.subscribe("com.lab126.powerd", &[]);
    let start = Instant::now();
    let mut events = 0;
    // 4.5 s: with the back-off the tool runs at about 0, 1 and 3 s (the next at 7 s);
    // respawned every second it runs 4 or 5 times.
    while start.elapsed() < ms(4500) {
        if hub.next_timeout(ms(200)).is_some() {
            events += 1;
        }
    }
    drop(hub);
    let runs = std::fs::read_to_string(dir.join("runs")).unwrap_or_default().lines().count();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(runs <= 3, "{runs} spawns and {events} error-line events in 4.5 s");
}

/// A `lipc-wait-event` that ignores its own `-s` limit and hangs: `wait_event` returns
/// `Ok(None)` within its deadline (timeout + 2 s) and kills the child.
#[test]
fn wait_event_with_a_hung_tool_returns_in_time() {
    let dir = temp_dir("wait-hung");
    write_tool(&dir, "lipc-wait-event", "echo $$ > \"$(dirname \"$0\")/pid\"; exec sleep 60");
    let lipc = Lipc::new().with_bin_dir(&dir);
    let t = Instant::now();
    let r = lipc.wait_event("com.lab126.powerd", &["x"], Duration::from_secs(1)).unwrap();
    assert_eq!(r, None);
    assert!(t.elapsed() < Duration::from_secs(5), "took {:?}", t.elapsed());
    let pid: i32 = std::fs::read_to_string(dir.join("pid")).unwrap().trim().parse().unwrap();
    // SAFETY: probing a pid with signal 0.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!alive, "the hung child {pid} is still running");
}

// ---------------------------------------------------------------------------------
// Parsers with hostile input

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn bytes(&mut self, max: usize, alphabet: &[u8]) -> Vec<u8> {
        let n = (self.next() as usize) % (max + 1);
        (0..n).map(|_| alphabet[(self.next() as usize) % alphabet.len()]).collect()
    }
}

/// Every text/byte parser the input loop feeds from lipc, netlink, `/proc`, the
/// journal or the environment survives 20000 random inputs built from the characters
/// those formats use (no panic). TZif data has its own tests below.
#[test]
fn parsers_survive_random_input() {
    let alpha: &[u8] = b"abcdefgHIJKLMNOPQRSTUVWXYZTZif0123456789 \t\n\"'=:,./<>[]{}()-+@\0\xff\xc3MJ";
    let mut r = Lcg(0x5eed);
    for _ in 0..20_000 {
        let b = r.bytes(120, alpha);
        let s = String::from_utf8_lossy(&b).into_owned();
        let _ = ink_device::lipc::LipcEvent::parse(&s);
        let _ = ink_device::lipc::parse_hash_output(&s);
        let _ = ink_device::events::Uevent::parse(&b);
        let _ = ink_device::journal::Entry::parse(&s);
        let _ = ink_device::evdev::TouchOverride::parse(&s);
        let _ = ink_device::evdev::parse_devices(&s);
        let _ = RawEvent::parse_evdump(&s);
        let _ = PowerEvent::parse(&s);
        let _ = ink_device::light::DayTime::parse(&s);
        let _ = ink_device::light::AmberMode::parse(&s);
        let _ = ink_device::clock::ntp_server_from_config(&s);
        let _ = Screensaver::from_setting(&s);
    }
}

/// POSIX TZ strings and `signalStrength` values with numbers that overflow i32/u32
/// arithmetic (a hostile `TZ` or a broken wifid reply): no panic (debug builds check
/// overflow; release builds would silently compute a wrong offset).
#[test]
fn numeric_overflow_in_tz_and_signal_parsing() {
    for tz in ["UTC999999", "AAA2147483647", "CET-1CEST,M3.5.0/999999,M10.5.0/3", "<+99999>-99999"] {
        let r = std::panic::catch_unwind(|| ink_device::clock::PosixTz::parse(tz));
        assert!(r.is_ok(), "PosixTz::parse({tz:?}) panicked");
    }
    let r = std::panic::catch_unwind(|| ink_device::wifi::parse_signal("4294967295/5"));
    assert!(r.is_ok(), "parse_signal panicked");
}

/// A 4-byte `/etc/localtime` (`TZif` and nothing else, e.g. a truncated write):
/// `TimeZone::local()` must fall back to UTC, not panic in the input loop.
#[test]
fn four_byte_tzif_is_rejected_without_panic() {
    let r = std::panic::catch_unwind(|| ink_device::clock::TimeZone::from_tzif(b"TZif"));
    assert!(matches!(r, Ok(None)), "from_tzif panicked or accepted 4 bytes");
}

/// Corrupted TZif data (`/etc/localtime` -> `/var/local/system/tz`) on the 32-bit
/// device: a header whose counts claim 2^31-1 transitions, and 5000 random headers.
/// `from_tzif` sums the counts in `usize` (`v1_len`, overflows on 32-bit: a panic in
/// debug, a wrapped length in release) and calls `Vec::with_capacity(timecnt)` before
/// reading a single transition ("capacity overflow" panic). `TimeZone::local()` runs in
/// the input loop (warmth schedule) and in `Clock::local_now`. Runs on the guest
/// (ARM32 test binary); not on 64-bit hosts, where such a header asks for gigabytes.
#[cfg(target_pointer_width = "32")]
#[test]
fn corrupted_tzif_is_rejected_without_panic() {
    let mut z = b"TZif2".to_vec();
    z.resize(44, 0);
    z[32..36].copy_from_slice(&0x7fff_ffffu32.to_be_bytes()); // timecnt
    let r = std::panic::catch_unwind(|| ink_device::clock::TimeZone::from_tzif(&z));
    assert!(matches!(r, Ok(None)), "from_tzif panicked on a header with timecnt 2^31-1");
    let mut rng = Lcg(0x7a1f);
    let alpha: Vec<u8> = (0u8..=255).collect();
    let mut panics = 0;
    for _ in 0..5000 {
        let mut z = b"TZif".to_vec();
        z.extend_from_slice(&rng.bytes(200, &alpha));
        if std::panic::catch_unwind(|| ink_device::clock::TimeZone::from_tzif(&z)).is_err() {
            panics += 1;
        }
    }
    assert_eq!(panics, 0, "{panics} of 5000 random TZif headers panicked");
}

// ---------------------------------------------------------------------------------
// Screensaver images

/// Garbage, empty, truncated and extreme-aspect PNG files as the chosen screensaver:
/// always a full panel canvas, never a panic.
#[test]
fn hostile_screensaver_images_fall_back_to_a_panel_canvas() {
    let dir = temp_dir("saver");
    let panel = Size::new(1272, 1696);
    let garbage = dir.join("garbage.png");
    std::fs::write(&garbage, b"\x89PNG\r\n\x1a\nthis is not a png at all").unwrap();
    let empty = dir.join("empty.png");
    std::fs::write(&empty, b"").unwrap();
    let thin = dir.join("thin.png");
    let mut c = Canvas::new(1, 5000);
    c.clear(0);
    c.save_png(&thin).unwrap();
    let wide = dir.join("wide.png");
    Canvas::new(5000, 1).save_png(&wide).unwrap();
    let truncated = dir.join("truncated.png");
    let mut t = std::fs::read(&thin).unwrap();
    t.truncate(t.len() / 2);
    std::fs::write(&truncated, t).unwrap();
    for p in [&garbage, &empty, &thin, &wide, &truncated, &dir.join("missing.png"), &dir] {
        let out = screensaver::render(&Screensaver::Image(p.clone()), None, panel);
        assert_eq!((out.width, out.height), (1272, 1696), "{}", p.display());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------
// Linux: kernel suspend helpers (run in the ARM test binary on the guest)

/// `/sys/power/wakeup_count` blocks while a wakeup source is active (USB cable, the
/// emulator's USB networking). `read_wakeup_count` gives up after its limit but leaves
/// the reading thread (and its open file) behind; the input loop retries a refused
/// suspend every `RINKY_SUSPEND_RETRY` (60 s), so a device asleep on a charger
/// piles up one blocked thread per minute. A FIFO with no writer stands in for the
/// blocking sysfs file here.
#[cfg(target_os = "linux")]
#[test]
fn blocked_wakeup_count_reads_do_not_pile_up_threads() {
    use ink_device::ereader::system::read_wakeup_count;
    use ink_device::sysfs::SysRoot;
    // Only the reader threads (named `wakeup_count`): other tests of this binary run in
    // parallel and start threads of their own, which made a count of all threads flaky.
    fn threads() -> usize {
        std::fs::read_dir("/proc/self/task")
            .unwrap()
            .flatten()
            .filter(|t| std::fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.trim() == "wakeup_count"))
            .count()
    }
    let dir = temp_dir("wakeup");
    std::fs::create_dir_all(dir.join("sys/power")).unwrap();
    let fifo = dir.join("sys/power/wakeup_count");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: mkfifo with a valid C path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let sys = SysRoot::at(&dir);
    let before = threads();
    for _ in 0..20 {
        assert_eq!(read_wakeup_count(&sys, ms(50)), None);
    }
    std::thread::sleep(ms(200));
    let left = threads().saturating_sub(before);
    // Let the blocked readers finish: open the FIFO for writing and close it.
    for _ in 0..40 {
        // SAFETY: open/close of our own FIFO.
        unsafe {
            let fd = libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK);
            if fd >= 0 {
                libc::close(fd);
            }
        }
        std::thread::sleep(ms(5));
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(left <= 2, "{left} threads left blocked after 20 refused suspends");
}
