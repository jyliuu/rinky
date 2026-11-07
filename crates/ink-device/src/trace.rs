//! Tracing helpers shared by every crate: the process start time, timing spans, the
//! wall clock for log lines, free RAM and disk space, and the signal handlers the app's
//! logger installs.
//!
//! - [`init`] records the process start (call it first thing in `main`); [`uptime_ms`]
//!   counts from there.
//! - [`Span`] logs `time: <name> <ms> ms` at info when it is dropped (or
//!   [`Span::finish`]ed): `let _t = ink_device::trace::Span::new("open book");`.
//! - [`wall_clock`] formats `HH:MM:SS.mmm` local time without a call into libc per line
//!   (the UTC offset is read once).
//! - [`free_space`] (statvfs) and [`meminfo`] (`/proc/meminfo`) for the startup report.
//! - [`install_fatal_handlers`] writes a one-line note to stderr (the log file in the app)
//!   on SIGSEGV, SIGBUS, SIGILL and SIGFPE using only `write(2)`; [`block_exit_signals`]
//!   and [`spawn_exit_signal_thread`] turn SIGTERM, SIGINT and SIGHUP into a normal
//!   callback on a thread of their own (sigwait), so the logger can write the reason and
//!   flush before the process exits.

use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static START: OnceLock<Instant> = OnceLock::new();
static UTC_OFFSET: OnceLock<i64> = OnceLock::new();

/// Frames (event handling plus painting) slower than this are logged as warnings.
pub const SLOW_FRAME: Duration = Duration::from_millis(200);

/// Records the process start time (the first call wins). Returns it.
pub fn init() -> Instant {
    *START.get_or_init(Instant::now)
}

/// The process start time ([`init`]; the first call of any function here otherwise).
pub fn process_start() -> Instant {
    init()
}

/// Milliseconds since [`init`].
pub fn uptime_ms() -> u64 {
    process_start().elapsed().as_millis() as u64
}

/// A timing span: logs `time: <name> <ms> ms` at info when dropped. Use
/// [`Span::finish`] to end it early and get the duration; [`Span::cancel`] drops it
/// without a line.
#[derive(Debug)]
pub struct Span {
    name: std::borrow::Cow<'static, str>,
    start: Instant,
    done: bool,
}

impl Span {
    /// Starts a span now.
    pub fn new(name: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        Span { name: name.into(), start: Instant::now(), done: false }
    }

    /// Starts a span at `start` (e.g. [`process_start`]).
    pub fn since(name: impl Into<std::borrow::Cow<'static, str>>, start: Instant) -> Self {
        Span { name: name.into(), start, done: false }
    }

    /// The span's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Time since the span started.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Ends the span: logs the line and returns the duration.
    pub fn finish(mut self) -> Duration {
        self.log()
    }

    /// Ends the span without a log line.
    pub fn cancel(mut self) {
        self.done = true;
    }

    fn log(&mut self) -> Duration {
        let d = self.start.elapsed();
        if !self.done {
            self.done = true;
            log::info!("time: {} {} ms", self.name, d.as_millis());
        }
        d
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        self.log();
    }
}

/// The local time zone's offset from UTC in seconds, read once.
pub fn utc_offset_secs() -> i64 {
    *UTC_OFFSET.get_or_init(read_utc_offset)
}

#[cfg(unix)]
fn read_utc_offset() -> i64 {
    // SAFETY: time and localtime_r with valid pointers; tm is plain data.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
fn read_utc_offset() -> i64 {
    0
}

/// Seconds since the Unix epoch and milliseconds, from the system clock.
fn now_epoch() -> (i64, u32) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, d.subsec_millis()),
        Err(_) => (0, 0),
    }
}

/// Local wall-clock time as `HH:MM:SS.mmm` into `out` (no allocation once `out` has room).
pub fn wall_clock(out: &mut String) {
    let (secs, ms) = now_epoch();
    let day = (secs + utc_offset_secs()).rem_euclid(86_400);
    let (h, m, s) = (day / 3600, day / 60 % 60, day % 60);
    use std::fmt::Write as _;
    let _ = write!(out, "{h:02}:{m:02}:{s:02}.{ms:03}");
}

/// Local date and time as `YYYYmmdd-HHMMSS` (crash and report file names).
pub fn file_stamp() -> String {
    let (secs, _) = now_epoch();
    let local = secs + utc_offset_secs();
    let (y, mo, d) = crate::clock::civil_from_days(local.div_euclid(86_400));
    let day = local.rem_euclid(86_400);
    format!("{y:04}{mo:02}{d:02}-{:02}{:02}{:02}", day / 3600, day / 60 % 60, day % 60)
}


/// Free and total bytes of the file system holding `path` (statvfs).
#[cfg(unix)]
pub fn free_space(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs with a NUL-terminated path and a zeroed out-struct.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        let frsize = st.f_frsize as u64;
        Some((st.f_bavail as u64 * frsize, st.f_blocks as u64 * frsize))
    }
}

/// Free and total bytes of the file system holding `path` (not available here).
#[cfg(not(unix))]
pub fn free_space(_path: &std::path::Path) -> Option<(u64, u64)> {
    None
}

/// `MemTotal` and `MemAvailable` (or `MemFree`) in KiB from `/proc/meminfo`.
pub fn meminfo() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo(&text)
}

/// [`meminfo`] on the text of `/proc/meminfo`.
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':')?.split_whitespace().next()?.parse::<u64>().ok())
    };
    Some((field("MemTotal")?, field("MemAvailable").or_else(|| field("MemFree"))?))
}

/// Bytes as `12.3 MB` (base 1024).
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Points fd 2 (stderr) at the open file `fd` (dup2), so panic text, stray `eprintln!`
/// output and the fatal-signal note land in the log file. False when it failed.
#[cfg(unix)]
pub fn redirect_stderr(fd: i32) -> bool {
    // SAFETY: dup2 on descriptors the caller owns; fd 2 is replaced atomically.
    unsafe { libc::dup2(fd, 2) == 2 }
}

/// No descriptors on this platform.
#[cfg(not(unix))]
pub fn redirect_stderr(_fd: i32) -> bool {
    false
}

/// Signals that end the process with a note in the log ([`install_fatal_handlers`]).
#[cfg(unix)]
const FATAL: [(libc::c_int, &str); 4] = [
    (libc::SIGSEGV, "SIGSEGV (invalid memory access)"),
    (libc::SIGBUS, "SIGBUS (bus error)"),
    (libc::SIGILL, "SIGILL (illegal instruction)"),
    (libc::SIGFPE, "SIGFPE (arithmetic fault)"),
];

#[cfg(unix)]
static mut PREVIOUS: [std::mem::MaybeUninit<libc::sigaction>; 4] = [const { std::mem::MaybeUninit::uninit() }; 4];
#[cfg(unix)]
static PREVIOUS_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Async-signal-safe: `write(2)` of `parts` to fd 2.
#[cfg(unix)]
fn raw_write(parts: &[&[u8]]) {
    for p in parts {
        // SAFETY: write(2) on fd 2 with a valid buffer; the result is ignored.
        unsafe {
            libc::write(2, p.as_ptr().cast(), p.len());
        }
    }
}

/// Async-signal-safe decimal formatting into `buf`; returns the digits.
#[cfg(unix)]
fn fmt_u64(mut v: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 || i == 0 {
            break;
        }
    }
    &buf[i..]
}

#[cfg(unix)]
extern "C" fn fatal_handler(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    let idx = FATAL.iter().position(|(s, _)| *s == sig);
    let name = idx.map_or("fatal signal", |i| FATAL[i].1);
    // Monotonic milliseconds since boot (clock_gettime is async-signal-safe).
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime with a valid out pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    let mut b = [0u8; 20];
    let ms = fmt_u64(ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000, &mut b);
    raw_write(&[b"[ERROR rinky] fatal signal: ", name.as_bytes(), b" at monotonic ", ms, b" ms; exit: reason=signal\n"]);
    // Put the previous handler back (std's stack-overflow handler, or the default) and
    // run it: the faulting instruction runs again and reaches it.
    if let (Some(i), true) = (idx, PREVIOUS_SET.load(std::sync::atomic::Ordering::Acquire)) {
        // SAFETY: PREVIOUS[i] was written before PREVIOUS_SET was set and is not changed
        // after; sigaction is async-signal-safe.
        unsafe {
            let prev = (*std::ptr::addr_of!(PREVIOUS))[i].assume_init_ref();
            libc::sigaction(sig, prev, std::ptr::null_mut());
            let h = prev.sa_sigaction;
            if h != libc::SIG_DFL && h != libc::SIG_IGN {
                if prev.sa_flags & libc::SA_SIGINFO != 0 {
                    let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) = std::mem::transmute(h);
                    f(sig, info, ctx);
                } else {
                    let f: extern "C" fn(libc::c_int) = std::mem::transmute(h);
                    f(sig);
                }
            }
        }
    } else {
        // SAFETY: signal(2) to the default action; async-signal-safe.
        unsafe { libc::signal(sig, libc::SIG_DFL) };
    }
}

/// Installs the SIGSEGV/SIGBUS/SIGILL/SIGFPE note writer (once). The note goes to fd 2;
/// the app points fd 2 at its log file. Log lines still in the logger's buffer (at most
/// about one second) are lost on such a crash.
#[cfg(unix)]
pub fn install_fatal_handlers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: sigaction with zeroed structs; PREVIOUS is written only here, before
        // PREVIOUS_SET is published, and the handlers are installed after that.
        unsafe {
            let prev = &mut *std::ptr::addr_of_mut!(PREVIOUS);
            for (i, (sig, _)) in FATAL.iter().enumerate() {
                let mut old: libc::sigaction = std::mem::zeroed();
                libc::sigaction(*sig, std::ptr::null(), &mut old);
                prev[i].write(old);
            }
            PREVIOUS_SET.store(true, std::sync::atomic::Ordering::Release);
            for (sig, _) in FATAL {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = fatal_handler as *const () as usize;
                sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                libc::sigemptyset(&mut sa.sa_mask);
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
        }
    });
}

/// No signal handlers on this platform.
#[cfg(not(unix))]
pub fn install_fatal_handlers() {}

#[cfg(unix)]
fn exit_set() -> libc::sigset_t {
    // SAFETY: sigemptyset/sigaddset on a local set.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, s);
        }
        set
    }
}

/// Blocks SIGTERM, SIGINT and SIGHUP in the calling thread. Call it in `main` before
/// any thread starts (threads inherit the mask; child processes started with
/// `std::process::Command` get an empty mask again).
#[cfg(unix)]
pub fn block_exit_signals() {
    let set = exit_set();
    // SAFETY: pthread_sigmask with a valid set.
    unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
}

/// No signals on this platform.
#[cfg(not(unix))]
pub fn block_exit_signals() {}

/// Name of an exit signal number.
pub fn signal_name(sig: i32) -> &'static str {
    #[cfg(unix)]
    {
        match sig {
            libc::SIGTERM => "SIGTERM",
            libc::SIGINT => "SIGINT",
            libc::SIGHUP => "SIGHUP",
            _ => "signal",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = sig;
        "signal"
    }
}

/// Starts a thread that waits for SIGTERM, SIGINT or SIGHUP (blocked with
/// [`block_exit_signals`]) and calls `on_signal(number)`. `on_signal` normally logs,
/// flushes and exits the process.
#[cfg(unix)]
pub fn spawn_exit_signal_thread(on_signal: fn(i32)) {
    let _ = std::thread::Builder::new().name("signals".into()).spawn(move || loop {
        let set = exit_set();
        let mut sig: libc::c_int = 0;
        // SAFETY: sigwait with a valid set and out pointer.
        if unsafe { libc::sigwait(&set, &mut sig) } == 0 {
            on_signal(sig);
        }
    });
}

/// No signals on this platform.
#[cfg(not(unix))]
pub fn spawn_exit_signal_thread(_on_signal: fn(i32)) {}

/// The exit signal the app loop is asked to stop for (0: none). Set by
/// [`request_stop`], read by the input backends and the app loop ([`stop_requested`]).
static STOP_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
/// The pipe that wakes an input backend blocked in `poll(2)`: (read end, write end), or
/// -1 before [`stop_wake_fd`] made it.
static STOP_PIPE: [std::sync::atomic::AtomicI32; 2] = [std::sync::atomic::AtomicI32::new(-1), std::sync::atomic::AtomicI32::new(-1)];
/// True while an app loop runs and will handle a stop request ([`set_stop_listener`]).
static STOP_LISTENER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Asks the app loop to stop for exit signal `sig` (SIGTERM, SIGINT, SIGHUP): stores the
/// number and writes one byte to the wake pipe. Uses only an atomic store and `write(2)`,
/// so it is also safe inside a signal handler.
pub fn request_stop(sig: i32) {
    use std::sync::atomic::Ordering;
    STOP_SIGNAL.store(sig.max(1), Ordering::SeqCst);
    #[cfg(unix)]
    {
        let fd = STOP_PIPE[1].load(Ordering::SeqCst);
        if fd >= 0 {
            let b = 1u8;
            // SAFETY: write(2) of one byte from a local to a pipe fd we own; non-blocking.
            unsafe { libc::write(fd, (&b as *const u8).cast(), 1) };
        }
    }
}

/// The exit signal a stop was requested for ([`request_stop`]).
pub fn stop_requested() -> Option<i32> {
    match STOP_SIGNAL.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        s => Some(s),
    }
}

/// Marks whether an app loop is running that reads [`stop_requested`] and closes its
/// screens (the reader writes its position) before the process exits.
pub fn set_stop_listener(on: bool) {
    STOP_LISTENER.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// True while an app loop handles stop requests ([`set_stop_listener`]).
pub fn stop_listener() -> bool {
    STOP_LISTENER.load(std::sync::atomic::Ordering::SeqCst)
}

/// The read end of the stop wake pipe (made on the first call; non-blocking,
/// close-on-exec). An input backend that blocks in `poll(2)` adds it to its fd set: it
/// becomes readable on [`request_stop`]. None when the pipe cannot be made.
#[cfg(unix)]
pub fn stop_wake_fd() -> Option<i32> {
    use std::sync::atomic::Ordering;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut fds = [-1i32; 2];
        // SAFETY: pipe(2) into a local array of two ints.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return;
        }
        for fd in fds {
            // SAFETY: fcntl on the fds pipe(2) just returned.
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        STOP_PIPE[0].store(fds[0], Ordering::SeqCst);
        STOP_PIPE[1].store(fds[1], Ordering::SeqCst);
        // A request made before the pipe existed still wakes the next poll.
        if stop_requested().is_some() {
            let b = 1u8;
            // SAFETY: write(2) of one byte to the pipe made above.
            unsafe { libc::write(fds[1], (&b as *const u8).cast(), 1) };
        }
    });
    let fd = STOP_PIPE[0].load(Ordering::SeqCst);
    (fd >= 0).then_some(fd)
}

/// No pipe on this platform.
#[cfg(not(unix))]
pub fn stop_wake_fd() -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_clock_and_sizes() {
        assert_eq!(file_stamp().len(), 15);
        let m = "MemTotal:  505848 kB\nMemFree:  1000 kB\nMemAvailable:   200000 kB\n";
        assert_eq!(parse_meminfo(m), Some((505_848, 200_000)));
        assert_eq!(parse_meminfo("MemTotal: 10 kB\nMemFree: 4 kB\n"), Some((10, 4)));
        assert_eq!(parse_meminfo("nothing"), None);
        let mut s = String::new();
        wall_clock(&mut s);
        assert_eq!(s.len(), 12, "{s}");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(3 * 1024 * 1024 / 2), "1.5 MB");
    }
}
