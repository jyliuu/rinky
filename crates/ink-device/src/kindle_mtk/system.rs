//! Kernel suspend with an RTC alarm, wakeup sources, frontlight on/off through sysfs,
//! and setting the system clock.
//!
//! Suspend sequence (powerd is bypassed; see `docs/ARCHITECTURE.md`):
//! 1. arm the RTC: `0` then `+SECONDS` to `/sys/class/rtc/rtc0/wakealarm`;
//! 2. read `/sys/power/wakeup_count` on a helper thread with a 2 s limit (the read
//!    blocks while a wakeup event is in progress: it hung on the emulator with the USB
//!    wakeup source active; at most one such reader is kept, see
//!    [`read_wakeup_count_ex`]) and write it back, so a wake event that arrives
//!    meanwhile makes the kernel refuse the suspend instead of losing it;
//! 3. `sync()`, then write `mem` to `/sys/power/state`: returns after resume, or fails
//!    at once with `EBUSY` while a wakeup source is active (USB cable);
//! 4. clear the alarm; the RTC fired when the wall clock passed the alarm time.

use crate::sleep::Resume;
use crate::sysfs::SysRoot;
use crate::{DeviceError, Result};
use std::path::PathBuf;
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const WAKEALARM: &str = "/sys/class/rtc/rtc0/wakealarm";

/// Result of a `/sys/power/wakeup_count` read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeupCount {
    /// The count (write it back before suspending).
    Value(String),
    /// The read blocked longer than the limit (a wakeup event is in progress).
    Blocked,
    /// The read of an earlier call is still blocked; no new reader was started.
    StillBlocked,
    /// The file could not be read.
    Failed(String),
}

/// The one `wakeup_count` read that may be in flight: its path and result channel.
type PendingRead = (PathBuf, mpsc::Receiver<std::io::Result<String>>);
static PENDING: Mutex<Option<PendingRead>> = Mutex::new(None);

/// Reads `/sys/power/wakeup_count`, waiting at most `limit`. The read blocks in the
/// kernel while a wakeup event is in progress (USB cable, the emulator's USB
/// networking) and cannot be cancelled, so it runs on a helper thread that keeps the
/// file open until the kernel answers. At most one such reader exists: while it is
/// still blocked, later calls return [`WakeupCount::StillBlocked`] at once (a device
/// asleep on a charger retries every minute; one thread and one fd per retry piled
/// up). A value that arrived after its caller gave up is stale and is read again.
pub fn read_wakeup_count_ex(sys: &SysRoot, limit: Duration) -> WakeupCount {
    let path = sys.path("/sys/power/wakeup_count");
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, rx)) = pending.take() {
        if p == path && matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            *pending = Some((p, rx));
            return WakeupCount::StillBlocked;
        }
        // Finished meanwhile (stale value), or another path (tests): read again.
    }
    let (tx, rx) = mpsc::channel();
    let p = path.clone();
    let spawned = std::thread::Builder::new()
        .name("wakeup_count".into())
        .spawn(move || {
            let _ = tx.send(std::fs::read_to_string(p).map(|s| s.trim().to_string()));
        });
    if let Err(e) = spawned {
        return WakeupCount::Failed(format!("no reader thread: {e}"));
    }
    match rx.recv_timeout(limit) {
        Ok(Ok(v)) => WakeupCount::Value(v),
        Ok(Err(e)) => WakeupCount::Failed(e.to_string()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            *pending = Some((path, rx));
            WakeupCount::Blocked
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => WakeupCount::Failed("reader thread ended".into()),
    }
}

/// Reads `/sys/power/wakeup_count` with a time limit; `None` when it blocked or failed
/// (see [`read_wakeup_count_ex`]).
pub fn read_wakeup_count(sys: &SysRoot, limit: Duration) -> Option<String> {
    match read_wakeup_count_ex(sys, limit) {
        WakeupCount::Value(v) => Some(v),
        _ => None,
    }
}

/// Wakeup sources that are active now or were active for a while (`name ms`).
pub fn active_wakeup_sources(sys: &SysRoot) -> Vec<(String, u64)> {
    let mut v = Vec::new();
    for d in sys.list("/sys/class/wakeup") {
        let base = format!("/sys/class/wakeup/{d}");
        let ms = sys.read_int(&format!("{base}/active_time_ms")).unwrap_or(0).max(0) as u64;
        if ms > 0 {
            v.push((sys.read(&format!("{base}/name")).unwrap_or(d), ms));
        }
    }
    v
}

/// The RTC's own time (`since_epoch`); it can differ from the system clock (on the
/// emulator by 34 s after a network time sync).
pub fn rtc_now(sys: &SysRoot) -> Option<i64> {
    sys.read_int("/sys/class/rtc/rtc0/since_epoch").ok()
}

/// Arms the RTC to fire `after` from now; returns the alarm in RTC seconds (as read
/// back from `wakealarm`).
pub fn arm_rtc(sys: &SysRoot, after: Duration) -> Result<i64> {
    sys.write(WAKEALARM, "0")?;
    sys.write(WAKEALARM, &format!("+{}", after.as_secs().max(1)))?;
    match sys.read_int(WAKEALARM) {
        Ok(v) => Ok(v),
        Err(_) => Ok(rtc_now(sys).unwrap_or(0) + after.as_secs().max(1) as i64),
    }
}

/// Clears the RTC alarm.
pub fn clear_rtc(sys: &SysRoot) {
    let _ = sys.write(WAKEALARM, "0");
}

/// Suspends now (see the module comment). `log` receives progress lines.
pub fn kernel_suspend(sys: &SysRoot, rtc_wake: Option<Duration>, log: &mut dyn FnMut(String)) -> Result<Resume> {
    let alarm = match rtc_wake {
        Some(d) => match arm_rtc(sys, d) {
            Ok(t) => Some(t),
            Err(e) => {
                log(format!("rtc alarm not armed: {e}"));
                None
            }
        },
        None => None,
    };
    match read_wakeup_count_ex(sys, Duration::from_secs(2)) {
        WakeupCount::Value(count) => {
            if let Err(e) = sys.write("/sys/power/wakeup_count", &count) {
                clear_rtc(sys);
                return Err(DeviceError::Backend(format!("wakeup event pending (wakeup_count {count}): {e}")));
            }
        }
        WakeupCount::Blocked => log("wakeup_count blocked for 2 s (a wakeup source is active); suspending anyway".into()),
        WakeupCount::StillBlocked => {
            log("wakeup_count blocked since an earlier attempt (a wakeup source is active); suspending anyway".into())
        }
        WakeupCount::Failed(e) => log(format!("wakeup_count unreadable ({e}); suspending anyway")),
    }
    // SAFETY: sync(2) has no arguments and cannot fail.
    unsafe { libc::sync() };
    let wall0 = SystemTime::now();
    let mono0 = Instant::now();
    let r = sys.write("/sys/power/state", "mem");
    let slept = SystemTime::now().duration_since(wall0).unwrap_or_else(|_| mono0.elapsed());
    clear_rtc(sys);
    match r {
        Ok(()) => {
            let by_rtc = match (alarm, rtc_now(sys)) {
                (Some(a), Some(now)) => now + 2 >= a,
                _ => false,
            };
            Ok(Resume { slept, by_rtc })
        }
        Err(e) => Err(DeviceError::Io(e)),
    }
}

/// Frontlight LED class devices (`/sys/class/backlight/*`), e.g. `fp9967-bl1`
/// (white) and `fp9967-bl0` (amber) on the PW12.
pub fn backlights(sys: &SysRoot) -> Vec<String> {
    sys.list("/sys/class/backlight")
}

/// Saves the backlight levels and sets them to 0; returns what was saved.
pub fn backlight_off(sys: &SysRoot) -> Vec<(String, i64)> {
    let mut saved = Vec::new();
    for b in backlights(sys) {
        let p = format!("/sys/class/backlight/{b}/brightness");
        if let Ok(v) = sys.read_int(&p) {
            if sys.write(&p, "0").is_ok() {
                saved.push((b, v));
            }
        }
    }
    saved
}

/// Restores levels saved by [`backlight_off`].
pub fn backlight_restore(sys: &SysRoot, saved: &[(String, i64)]) {
    for (b, v) in saved {
        let _ = sys.write(&format!("/sys/class/backlight/{b}/brightness"), &v.to_string());
    }
}

/// `RTC_SET_TIME`: `_IOW('p', 0x0a, struct rtc_time)` with a 36-byte `rtc_time`.
const RTC_SET_TIME: u32 = 0x4024_700a;
const RTC_DEV: &str = "/dev/rtc0";

/// Writes `t` to the RTC (`/dev/rtc0`, UTC) with `RTC_SET_TIME`.
pub fn write_rtc(t: SystemTime) -> Result<()> {
    use std::os::fd::AsRawFd;
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map_err(|_| DeviceError::Backend("time before 1970".into()))?;
    let mut tm = crate::clock::rtc_time_fields(secs.as_secs() as i64);
    let f = std::fs::File::open(RTC_DEV)?;
    // SAFETY: RTC_SET_TIME reads one struct rtc_time (9 x i32) from the pointer.
    if unsafe { libc::ioctl(f.as_raw_fd(), RTC_SET_TIME as _, tm.as_mut_ptr()) } < 0 {
        return Err(DeviceError::Backend(format!("RTC_SET_TIME on {RTC_DEV}: {}", std::io::Error::last_os_error())));
    }
    Ok(())
}

/// Sets the system clock, then the RTC, so the time survives a reboot. The RTC is
/// written with `RTC_SET_TIME`; if that fails, with the `hwclock -u -w` that upstart's
/// `system.conf` uses (5.19.5 has no `hwclock` link, only the busybox applet, so
/// `busybox hwclock` is tried too). Returns what happened to the RTC; an RTC failure is
/// not an error (the system clock is set).
pub fn set_system_time(t: SystemTime) -> Result<String> {
    let d = t.duration_since(SystemTime::UNIX_EPOCH).map_err(|_| DeviceError::Backend("time before 1970".into()))?;
    #[allow(deprecated)] // libc::time_t: 32-bit on this musl target; fine until 2038.
    let ts = libc::timespec { tv_sec: d.as_secs() as libc::time_t, tv_nsec: d.subsec_nanos() as _ };
    // SAFETY: clock_settime with a valid timespec.
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let first = match write_rtc(t) {
        Ok(()) => return Ok(format!("RTC written ({RTC_DEV})")),
        Err(e) => e.to_string(),
    };
    let mut tried = vec![first];
    for (prog, args) in [("hwclock", &["-u", "-w"][..]), ("busybox", &["hwclock", "-u", "-w"][..])] {
        match std::process::Command::new(prog).args(args).status() {
            Ok(s) if s.success() => return Ok(format!("RTC written ({prog} {})", args.join(" "))),
            Ok(s) => tried.push(format!("{prog}: {s}")),
            Err(e) => tried.push(format!("{prog}: {e}")),
        }
    }
    log::warn!("rtc: not written: {}", tried.join("; "));
    Ok(format!("RTC not written ({})", tried.join("; ")))
}
