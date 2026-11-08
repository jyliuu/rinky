//! Logging, crash logs and exit reasons.
//!
//! Every line looks like `[INFO 12:34:56.789 +1234 ink_app::app] text`: level, local
//! wall-clock time, milliseconds since the process started
//! ([`ink_device::trace::uptime_ms`]) and the module.
//!
//! - [`init`] installs the logger. [`LogTarget::File`] (the `run` and `library`
//!   commands, or `RINKY_LOG_FILE`) appends to the file through a buffer that a
//!   thread flushes at most one second after a line arrives; warnings and errors flush
//!   at once, and so do [`flush`], [`log_exit`], the panic hook and the signal thread.
//!   The file is rotated at [`MAX_LOG_BYTES`] to `.1` (the old `.1` becomes `.2`; two
//!   old files are kept) and fd 2 is pointed at the current file, so panic text and the
//!   fatal-signal note land there too. [`LogTarget::Stderr`] writes each line to stderr.
//! - The last [`RING_LINES`] lines are kept in memory for crash logs.
//! - Level: `RINKY_LOG` or `RUST_LOG` (`debug`, `info`, ...), else the settings
//!   (`[plugins.diagnostics] log_level`, the Diagnostics page), else info.
//! - [`set_exit_reason`] and [`log_exit`]: every exit path ends with one
//!   `exit: reason=<...> status=<n>` line; the reason is also written to
//!   `$RINKY_RUN/exit-reason` for the launcher.
//! - [`install_panic_hook`]: the panic message, location, a backtrace and the last
//!   [`RING_LINES`] log lines go to `<home>/crash-<YYYYmmdd-HHMMSS>.log`; then an
//!   on-screen note ([`set_crash_screen`]) and the abort.
//! - Diagnostics helpers: [`tail_file`], [`crash_logs`], [`write_debug_report`].

use ink_device::trace;
use ink_store::{Paths, Settings};
use log::{Level, LevelFilter};
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

/// The log file is rotated when it reaches this size.
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
/// Rotated files kept (`rinky.log.1`, `rinky.log.2`).
pub const KEEP_ROTATED: usize = 2;
/// Lines kept in memory for crash logs.
pub const RING_LINES: usize = 300;
/// Buffered lines are written at most this long after they were logged.
pub const FLUSH_EVERY: Duration = Duration::from_secs(1);
/// A buffer this full is written at once.
const BUF_LIMIT: usize = 64 * 1024;
/// Settings table that holds `log_level`.
pub const SETTINGS_TABLE: &str = "diagnostics";
/// Log levels the Diagnostics page offers.
pub const LEVELS: [&str; 2] = ["info", "debug"];

/// Where log lines go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogTarget {
    /// Each line straight to stderr.
    Stderr,
    /// Buffered appends to this file (rotated); stderr is pointed at it.
    File(PathBuf),
}

enum Output {
    Stderr,
    File { file: File, path: PathBuf, size: u64, write_failed: bool },
}

struct Inner {
    out: Output,
    buf: Vec<u8>,
    line: String,
    ring: VecDeque<String>,
}

struct Logger {
    level: AtomicUsize,
    inner: Mutex<Inner>,
    wake: Condvar,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();
static ENV_LEVEL: AtomicBool = AtomicBool::new(false);
static HOME: Mutex<Option<PathBuf>> = Mutex::new(None);
static EXIT_REASON: Mutex<Option<String>> = Mutex::new(None);
static CRASH_SCREEN: OnceLock<fn(&str)> = OnceLock::new();
static EXIT_LOGGED: AtomicBool = AtomicBool::new(false);
/// Set by the hook when the main thread panics under `panic = "unwind"`: (location, crash file name).
static MAIN_PANIC: Mutex<Option<(String, String)>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn level_index(l: LevelFilter) -> usize {
    l as usize
}

fn index_level(i: usize) -> LevelFilter {
    [LevelFilter::Off, LevelFilter::Error, LevelFilter::Warn, LevelFilter::Info, LevelFilter::Debug, LevelFilter::Trace]
        .get(i)
        .copied()
        .unwrap_or(LevelFilter::Info)
}

/// Parses `debug`, `info`, ... or a `RUST_LOG`-style `a=debug,info` (the last plain
/// level, else the last `target=level` level).
pub fn parse_level(s: &str) -> Option<LevelFilter> {
    let one = |v: &str| v.trim().parse::<LevelFilter>().ok();
    let parts: Vec<&str> = s.split(',').collect();
    parts.iter().rev().find_map(|p| if p.contains('=') { None } else { one(p) }).or_else(|| {
        parts.iter().rev().find_map(|p| p.split_once('=').and_then(|(_, v)| one(v)))
    })
}

/// The level from `RINKY_LOG`, else `RUST_LOG`.
pub fn level_from_env() -> Option<LevelFilter> {
    ["RINKY_LOG", "RUST_LOG"].iter().find_map(|k| std::env::var(k).ok().and_then(|v| parse_level(&v)))
}

/// The Diagnostics page's log level of `s` (`info` or `debug`).
pub fn settings_log_level(s: &Settings) -> &'static str {
    let v = s
        .plugins
        .get(SETTINGS_TABLE)
        .and_then(|t| t.get("log_level"))
        .and_then(|v| v.as_str())
        .unwrap_or("info");
    LEVELS.iter().copied().find(|l| *l == v).unwrap_or("info")
}

/// Stores the Diagnostics page's log level in `s` (unknown values are ignored).
pub fn set_settings_log_level(s: &mut Settings, level: &str) {
    if !LEVELS.contains(&level) {
        return;
    }
    let mut table = match s.plugin_value::<serde_json::Value>(SETTINGS_TABLE) {
        Ok(Some(serde_json::Value::Object(m))) => m,
        _ => serde_json::Map::new(),
    };
    table.insert("log_level".into(), serde_json::Value::String(level.into()));
    if let Err(e) = s.set_plugin_value(SETTINGS_TABLE, &serde_json::Value::Object(table)) {
        log::warn!("diagnostics: cannot store the log level: {e}");
    }
}

/// The current maximum level.
pub fn level() -> LevelFilter {
    LOGGER.get().map_or(log::max_level(), |l| index_level(l.level.load(Ordering::Relaxed)))
}

/// Changes the level now (the environment wins: with `RINKY_LOG`/`RUST_LOG` set,
/// only the setting is stored). Returns the level in force.
pub fn set_level(l: LevelFilter) -> LevelFilter {
    if ENV_LEVEL.load(Ordering::Relaxed) {
        return level();
    }
    if let Some(lg) = LOGGER.get() {
        lg.level.store(level_index(l), Ordering::Relaxed);
    }
    log::set_max_level(l);
    l
}

/// Applies the settings' log level unless the environment set one.
pub fn apply_settings(s: &Settings) {
    let want = parse_level(settings_log_level(s)).unwrap_or(LevelFilter::Info);
    if want != level() && !ENV_LEVEL.load(Ordering::Relaxed) {
        log::info!("log level {want} (settings)");
        set_level(want);
    }
}

/// Folder for crash logs and debug reports (the install folder, `/mnt/us/rinky`).
pub fn set_home(dir: &Path) {
    *lock(&HOME) = Some(dir.to_path_buf());
}

fn home() -> PathBuf {
    lock(&HOME).clone().unwrap_or_else(|| Paths::device().root)
}

/// The log file, when logging to a file.
pub fn log_path() -> Option<PathBuf> {
    let lg = LOGGER.get()?;
    match &lock(&lg.inner).out {
        Output::File { path, .. } => Some(path.clone()),
        Output::Stderr => None,
    }
}

/// The lines in the in-memory ring, oldest first.
pub fn recent_lines() -> Vec<String> {
    LOGGER.get().map(|lg| lock(&lg.inner).ring.iter().cloned().collect()).unwrap_or_default()
}

/// Writes buffered lines now.
pub fn flush() {
    if let Some(lg) = LOGGER.get() {
        lock(&lg.inner).flush();
    }
}

fn open_append(path: &Path) -> std::io::Result<(File, u64)> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let f = OpenOptions::new().create(true).append(true).open(path)?;
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    Ok((f, size))
}

/// `path` with `.n` appended.
pub fn rotated_path(path: &Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Shifts `path` -> `.1` -> `.2` (the oldest is dropped).
fn shift_files(path: &Path) {
    for n in (1..KEEP_ROTATED).rev() {
        let _ = std::fs::rename(rotated_path(path, n), rotated_path(path, n + 1));
    }
    let _ = std::fs::rename(path, rotated_path(path, 1));
}

#[cfg(unix)]
fn point_stderr_at(f: &File) {
    use std::os::fd::AsRawFd as _;
    trace::redirect_stderr(f.as_raw_fd());
}

#[cfg(not(unix))]
fn point_stderr_at(_f: &File) {}

impl Inner {
    fn format(&mut self, level: Level, target: &str, args: &std::fmt::Arguments) {
        self.line.clear();
        let _ = write!(self.line, "[{level} ");
        trace::wall_clock(&mut self.line);
        let _ = writeln!(self.line, " +{} {target}] {args}", trace::uptime_ms());
    }

    fn push_ring(&mut self) {
        let text = self.line.trim_end();
        if self.ring.len() >= RING_LINES {
            if let Some(mut old) = self.ring.pop_front() {
                old.clear();
                old.push_str(text);
                self.ring.push_back(old);
                return;
            }
        }
        self.ring.push_back(text.to_string());
    }

    fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let mut rotate = false;
        if let Output::File { file, size, write_failed, .. } = &mut self.out {
            match file.write_all(&self.buf) {
                Ok(()) => {
                    *size += self.buf.len() as u64;
                    *write_failed = false;
                    rotate = *size >= MAX_LOG_BYTES;
                }
                Err(e) => {
                    if !*write_failed {
                        *write_failed = true;
                        let _ = writeln!(std::io::stderr(), "[ERROR rinky] cannot write the log file: {e}");
                    }
                }
            }
        }
        self.buf.clear();
        if rotate {
            self.rotate();
        }
    }

    fn rotate(&mut self) {
        let path = match &self.out {
            Output::File { path, .. } => path.clone(),
            Output::Stderr => return,
        };
        shift_files(&path);
        match open_append(&path) {
            Ok((f, n)) => {
                point_stderr_at(&f);
                if let Output::File { file, size, .. } = &mut self.out {
                    *file = f;
                    *size = n;
                }
                let note = format!("log rotated at {MAX_LOG_BYTES} bytes; earlier lines are in {}.1", path.display());
                self.format(Level::Info, "ink_app::logging", &format_args!("{note}"));
                self.buf.extend_from_slice(self.line.as_bytes());
            }
            // Keep writing to the renamed file; try again after the next 2 MB.
            Err(_) => {
                if let Output::File { size, .. } = &mut self.out {
                    *size = 0;
                }
            }
        }
    }
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        level_index(m.level().to_level_filter()) <= self.level.load(Ordering::Relaxed)
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let mut g = lock(&self.inner);
        g.format(r.level(), r.target(), r.args());
        g.push_ring();
        let g = &mut *g;
        match g.out {
            // Not eprintln!: it panics (and with panic = "abort" kills rinky) when
            // stderr cannot be written (read-only or full userstore log).
            Output::Stderr => {
                let _ = std::io::stderr().write_all(g.line.as_bytes());
            }
            Output::File { .. } => {
                let was_empty = g.buf.is_empty();
                g.buf.extend_from_slice(g.line.as_bytes());
                if r.level() <= Level::Warn || g.buf.len() >= BUF_LIMIT {
                    g.flush();
                } else if was_empty {
                    self.wake.notify_one();
                }
            }
        }
    }

    fn flush(&self) {
        lock(&self.inner).flush();
    }
}

/// Installs the logger (once; later calls do nothing). A log file that cannot be opened
/// falls back to stderr with a warning.
pub fn init(target: LogTarget) {
    trace::init();
    let env = level_from_env();
    ENV_LEVEL.store(env.is_some(), Ordering::Relaxed);
    let level = env.unwrap_or(LevelFilter::Info);
    let mut warn = None;
    let out = match &target {
        LogTarget::Stderr => Output::Stderr,
        LogTarget::File(path) => {
            let opened = open_append(path).and_then(|(f, n)| {
                if n >= MAX_LOG_BYTES {
                    drop(f);
                    shift_files(path);
                    open_append(path)
                } else {
                    Ok((f, n))
                }
            });
            match opened {
                Ok((file, size)) => {
                    point_stderr_at(&file);
                    Output::File { file, path: path.clone(), size, write_failed: false }
                }
                Err(e) => {
                    warn = Some(format!("cannot open the log file {}: {e}; logging to stderr", path.display()));
                    Output::Stderr
                }
            }
        }
    };
    let file = matches!(out, Output::File { .. });
    let logger = Logger {
        level: AtomicUsize::new(level_index(level)),
        inner: Mutex::new(Inner { out, buf: Vec::with_capacity(BUF_LIMIT), line: String::with_capacity(256), ring: VecDeque::with_capacity(RING_LINES) }),
        wake: Condvar::new(),
    };
    if LOGGER.set(logger).is_err() {
        return;
    }
    let Some(lg) = LOGGER.get() else { return };
    if log::set_logger(lg).is_ok() {
        log::set_max_level(level);
    }
    if file {
        let _ = std::thread::Builder::new().name("log-flush".into()).spawn(flush_loop);
    }
    if let Some(w) = warn {
        log::warn!("{w}");
    }
}

fn flush_loop() {
    let Some(lg) = LOGGER.get() else { return };
    loop {
        {
            let mut g = lock(&lg.inner);
            while g.buf.is_empty() {
                g = lg.wake.wait(g).unwrap_or_else(|e| e.into_inner());
            }
        }
        std::thread::sleep(FLUSH_EVERY);
        lock(&lg.inner).flush();
    }
}

/// Writes `line` (and flushes) without waiting for the logger lock: for the panic hook
/// and the signal thread. A held lock (a panic inside the logger) sends the line to
/// stderr instead.
fn emergency(level: Level, text: &str) {
    let Some(lg) = LOGGER.get() else {
        let _ = writeln!(std::io::stderr(), "[{level} rinky] {text}");
        return;
    };
    // Another thread (the flusher) holds the lock only briefly; this thread holding it
    // (a panic inside the logger) never lets go, so give up after about 100 ms.
    let mut tries = 0;
    let got = loop {
        match lg.inner.try_lock() {
            Ok(g) => break Some(g),
            Err(std::sync::TryLockError::Poisoned(e)) => break Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) if tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(std::sync::TryLockError::WouldBlock) => break None,
        }
    };
    match got {
        Some(mut g) => {
            g.format(level, "ink_app::logging", &format_args!("{text}"));
            g.push_ring();
            let g = &mut *g;
            match g.out {
                Output::Stderr => {
                    let _ = std::io::stderr().write_all(g.line.as_bytes());
                }
                Output::File { .. } => {
                    g.buf.extend_from_slice(g.line.as_bytes());
                    g.flush();
                }
            }
        }
        None => {
            let _ = writeln!(std::io::stderr(), "[{level} rinky] {text}");
        }
    }
}

/// Records why the app is about to exit (the first reason wins; `force` replaces it).
pub fn set_exit_reason(reason: impl Into<String>, force: bool) {
    let mut g = lock(&EXIT_REASON);
    if force || g.is_none() {
        *g = Some(reason.into());
    }
}

/// The recorded exit reason.
pub fn exit_reason() -> Option<String> {
    lock(&EXIT_REASON).clone()
}

/// Writes `reason` to `$RINKY_RUN/exit-reason` for the launcher (no-op without
/// `RINKY_RUN`).
fn write_reason_file(reason: &str) {
    if let Some(run) = std::env::var_os("RINKY_RUN") {
        let _ = std::fs::write(Path::new(&run).join("exit-reason"), format!("{reason}\n"));
    }
}

/// Logs `exit: reason=<reason> status=<status>` once, writes the reason file and flushes.
/// Without a recorded reason the status decides (`normal` or `error`).
pub fn log_exit(status: i32) {
    if EXIT_LOGGED.swap(true, Ordering::SeqCst) {
        return;
    }
    let reason = exit_reason().unwrap_or_else(|| if status == 0 { "normal".into() } else { "error".into() });
    // emergency() writes the buffer and this line without waiting on a lock this thread
    // may hold (a panic inside the logger).
    emergency(Level::Info, &format!("exit: reason={reason} status={status} uptime={} ms", trace::uptime_ms()));
    write_reason_file(&reason);
}

/// How long the signal thread waits for the app loop to close its screens after an
/// exit signal before it exits the process itself. `rinky-ctl.sh stop` and the
/// launcher send SIGKILL 3 s after SIGTERM.
pub const STOP_GRACE: std::time::Duration = std::time::Duration::from_millis(2000);

/// The signal thread's callback. While an app loop runs, it asks the loop to stop
/// ([`trace::request_stop`]): the loop closes every screen (the reader writes its
/// position to the sidecar) and `cli::main` logs the exit and returns 128 + the signal.
/// When no loop runs, or the loop has not finished after [`STOP_GRACE`], it logs the
/// signal as the exit reason, flushes and exits with 128 + the signal number.
pub fn on_exit_signal(sig: i32) {
    if trace::stop_listener() && trace::stop_requested().is_none() {
        trace::request_stop(sig);
        std::thread::sleep(STOP_GRACE);
        log::warn!("signal {}: the app loop did not stop within {} ms; exiting", trace::signal_name(sig), STOP_GRACE.as_millis());
    }
    set_exit_reason(format!("signal {}", trace::signal_name(sig)), true);
    log_exit(128 + sig);
    std::process::exit(128 + sig);
}

/// Sets the function the panic hook calls to show a one-line note on the screen.
pub fn set_crash_screen(f: fn(&str)) {
    let _ = CRASH_SCREEN.set(f);
}

/// Text of a panic payload.
fn payload_text(info: &std::panic::PanicHookInfo) -> String {
    let p = info.payload();
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "(no message)".into()
    }
}

/// Build description: version, commit, build time.
pub fn build_info() -> String {
    format!(
        "rinky {} commit {} built {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("RINKY_COMMIT").unwrap_or("unknown"),
        option_env!("RINKY_BUILD_TIME").unwrap_or("unknown")
    )
}

/// The crash log text: header, message, backtrace and the recent log lines.
pub fn crash_report(thread: &str, location: &str, message: &str, backtrace: &str, lines: &[String]) -> String {
    let mut s = String::with_capacity(4096 + lines.iter().map(|l| l.len() + 1).sum::<usize>());
    let _ = writeln!(s, "rinky crash log");
    let _ = writeln!(s, "build: {}", build_info());
    let _ = writeln!(s, "time: {} (uptime {} ms)", trace::file_stamp(), trace::uptime_ms());
    let _ = writeln!(s, "thread: {thread}");
    let _ = writeln!(s, "location: {location}");
    let _ = writeln!(s, "message: {message}");
    let _ = writeln!(s, "\nbacktrace:\n{backtrace}");
    let _ = writeln!(s, "\nlast {} log lines:", lines.len());
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// Called by `main` after a panic unwound out of the main thread (release builds use
/// `panic = "unwind"`, so the hook returns and the stack unwinds): records the exit
/// reason, logs the exit line, shows the crash note and returns the exit status 134, so
/// the launcher restores the framework as after an abort.
pub fn finish_after_main_panic() -> i32 {
    let (location, name) = lock(&MAIN_PANIC).take().unwrap_or_else(|| ("unknown".into(), String::new()));
    set_exit_reason(format!("panic at {location}"), true);
    log_exit(134);
    if let Some(show) = CRASH_SCREEN.get() {
        show(&name);
    }
    134
}

/// Installs the panic hook (once). With `panic = "abort"` the process aborts after it
/// (exit status 134 in the launcher).
pub fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            static IN_PANIC: AtomicBool = AtomicBool::new(false);
            if IN_PANIC.swap(true, Ordering::SeqCst) {
                let _ = writeln!(std::io::stderr(), "[ERROR rinky] panic inside the panic hook: {info}");
                return;
            }
            let message = payload_text(info);
            let location = info.location().map_or("unknown".to_string(), |l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
            let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
            emergency(Level::Error, &format!("panic in thread '{thread}' at {location}: {message}"));
            let backtrace = std::backtrace::Backtrace::force_capture().to_string();
            // The ring without waiting: a panic inside the logger holds its lock.
            let lines: Vec<String> = match LOGGER.get().map(|lg| lg.inner.try_lock()) {
                Some(Ok(g)) => g.ring.iter().cloned().collect(),
                _ => vec!["(log lines not available: the logger was busy)".into()],
            };
            let text = crash_report(&thread, &location, &message, &backtrace, &lines);
            let file = home().join(format!("crash-{}.log", trace::file_stamp()));
            let written = match std::fs::write(&file, text.as_bytes()) {
                Ok(()) => {
                    emergency(Level::Error, &format!("crash log written: {}", file.display()));
                    true
                }
                Err(e) => {
                    emergency(Level::Error, &format!("cannot write the crash log {}: {e}; backtrace follows\n{backtrace}", file.display()));
                    false
                }
            };
            if !cfg!(panic = "abort") {
                // Unwinding (dev builds): the thread ends, the process may go on.
                emergency(Level::Error, "panic unwinds; a thread other than main ends, the process goes on");
                if std::thread::current().name() == Some("main") {
                    let name = if written { file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { String::new() };
                    *lock(&MAIN_PANIC) = Some((location.clone(), name));
                }
                IN_PANIC.store(false, Ordering::SeqCst);
                return;
            }
            set_exit_reason(format!("panic at {location}"), true);
            log_exit(134);
            if let Some(show) = CRASH_SCREEN.get() {
                let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                show(if written { &name } else { "" });
            }
        }));
    });
}

fn mb(kib: u64) -> String {
    trace::human_bytes(kib * 1024)
}

fn disk_line(path: &str) -> String {
    match trace::free_space(Path::new(path)) {
        Some((free, total)) => format!("{path} {} free of {}", trace::human_bytes(free), trace::human_bytes(total)),
        None => format!("{path} n/a"),
    }
}

/// One-line settings summary for the startup report.
pub fn settings_summary(s: &Settings) -> String {
    format!(
        "font {}px family {:?} spacing {} margins {}/{}/{} justify {} flash every {} | rotation {} night {} sleep {} min screensaver {} | ui font {:?} nav {:?} keyboards {:?} | library {:?} | gestures {} | plugins {} | log level {}",
        s.reader.font_px,
        s.reader.font_family,
        s.reader.line_spacing,
        s.reader.margin_x,
        s.reader.margin_top,
        s.reader.margin_bottom,
        s.reader.justify,
        s.reader.flash_every,
        s.device.rotation,
        s.device.night_mode,
        s.device.sleep_minutes,
        s.device.screensaver,
        s.ui.font_family,
        s.ui.nav_items,
        s.ui.keyboard_layouts,
        s.library_dirs,
        if s.gestures == ink_store::prefs::GestureMap::default() { "default" } else { "custom" },
        s.plugins.len(),
        settings_log_level(s),
    )
}

/// Logs what helps to explain a problem later: build, device, screen, memory, free
/// space and settings.
pub fn startup_report(device: &ink_device::Device, settings: &Settings, paths: &Paths) {
    let t = trace::Span::new("startup report");
    log::info!("start: {} pid {} args {:?}", build_info(), std::process::id(), std::env::args().collect::<Vec<_>>());
    let panel = device.screen.panel_size();
    let size = device.screen.size();
    log::info!(
        "device: backend {} model {:?} firmware {:?}; framebuffer panel {}x{} logical {}x{} rotation {:?} night {}",
        device.info.backend,
        device.info.model,
        device.info.firmware,
        panel.w,
        panel.h,
        size.w,
        size.h,
        device.screen.rotation(),
        device.screen.night_mode()
    );
    match trace::meminfo() {
        Some((total, avail)) => log::info!("memory: {} available of {}", mb(avail), mb(total)),
        None => log::info!("memory: /proc/meminfo not available"),
    }
    let root = paths.root.display().to_string();
    log::info!("disk: {}; {}; {}; {}", disk_line("/var"), disk_line("/tmp"), disk_line(ink_platform::current().user_root()), disk_line(&root));
    log::info!("settings: {}", settings_summary(settings));
    log::info!(
        "log: {} level {} (rotated at {} MB, {KEEP_ROTATED} old files kept)",
        log_path().map_or("stderr".to_string(), |p| p.display().to_string()),
        level(),
        MAX_LOG_BYTES / (1024 * 1024)
    );
    drop(t);
}

/// The last `n` lines of the text file `path` (reads at most the last 512 KiB).
pub fn tail_file(path: &Path, n: usize) -> std::io::Result<Vec<String>> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(512 * 1024);
    f.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok(tail_text(&String::from_utf8_lossy(&bytes), n, start > 0))
}

/// The last `n` lines of `text`; `cut`: the text starts mid-file (its first line is
/// partial and dropped).
pub fn tail_text(text: &str, n: usize, cut: bool) -> Vec<String> {
    let mut lines: Vec<&str> = text.lines().collect();
    if cut && !lines.is_empty() {
        lines.remove(0);
    }
    let from = lines.len().saturating_sub(n);
    lines[from..].iter().map(|l| l.to_string()).collect()
}

/// Crash logs (`crash-*.log`) in `dir`, newest first.
pub fn crash_logs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".log"))
                })
                .collect()
        })
        .unwrap_or_default();
    // The names hold the time, so name order is time order.
    v.sort();
    v.reverse();
    v
}

/// `settings.toml` text with secret values replaced: any `key = value` line whose key
/// contains `password`, `token`, `secret` or `key` (any case).
pub fn redact_settings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let secret = line.split_once('=').is_some_and(|(k, _)| {
            let k = k.trim().to_ascii_lowercase();
            !k.starts_with('[') && ["password", "token", "secret", "key"].iter().any(|w| k.contains(w))
        });
        match (secret, line.split_once('=')) {
            (true, Some((k, _))) => {
                out.push_str(k.trim_end());
                out.push_str(" = \"<redacted>\"");
            }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// Copies the logs, `settings.toml` (secrets redacted), `VERSION` and the crash logs into
/// `<root>/debug-report-<YYYYmmdd-HHMMSS>/` and writes `summary.txt` there. Returns the
/// folder and the number of files.
pub fn write_debug_report(paths: &Paths) -> std::io::Result<(PathBuf, usize)> {
    let _t = trace::Span::new("debug report");
    flush();
    let dir = paths.root.join(format!("debug-report-{}", trace::file_stamp()));
    std::fs::create_dir_all(&dir)?;
    let mut count = 0;
    let log = log_path().unwrap_or_else(|| paths.root.join("rinky.log"));
    let mut copy = |from: &Path, name: &str| {
        if from.is_file() && std::fs::copy(from, dir.join(name)).is_ok() {
            count += 1;
        }
    };
    for (i, name) in ["rinky.log", "rinky.log.1", "rinky.log.2"].iter().enumerate() {
        let src = if i == 0 { log.clone() } else { rotated_path(&log, i) };
        copy(&src, name);
    }
    for name in ["watchdog.log", "scriptlet.log", "VERSION"] {
        copy(&paths.root.join(name), name);
    }
    for c in crash_logs(&paths.root) {
        if let Some(n) = c.file_name().and_then(|n| n.to_str()) {
            copy(&c, n);
        }
    }
    if let Ok(text) = std::fs::read_to_string(paths.settings()) {
        std::fs::write(dir.join("settings.toml"), redact_settings(&text))?;
        count += 1;
    }
    let summary = format!(
        "{}\nwritten {} uptime {} ms\nlog {}\nlevel {}\n{}\n{}\n",
        build_info(),
        trace::file_stamp(),
        trace::uptime_ms(),
        log.display(),
        level(),
        disk_line("/var"),
        disk_line(ink_platform::current().user_root())
    );
    std::fs::write(dir.join("summary.txt"), summary)?;
    count += 1;
    log::info!("diagnostics: debug report {} ({count} files)", dir.display());
    Ok((dir, count))
}

/// A log line shortened for the on-screen viewer: `[INFO 12:34:56.789 +1234 module] text`
/// becomes `12:34:56 INFO text`; other lines (launcher, crash log text) stay as they are.
pub fn display_line(line: &str) -> String {
    let parsed = line.strip_prefix('[').and_then(|rest| {
        let (head, text) = rest.split_once("] ")?;
        let mut it = head.split(' ');
        let (level, clock) = (it.next()?, it.next()?);
        (level.chars().all(|c| c.is_ascii_uppercase()) && clock.len() >= 8).then(|| format!("{} {level} {text}", &clock[..8]))
    });
    parsed.unwrap_or_else(|| line.to_string())
}

/// Splits `line` into pieces of at most `width` characters (at a space when there is
/// one in the second half of the piece).
pub fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let mut end = (i + width).min(chars.len());
        if end < chars.len() {
            if let Some(sp) = (i + width / 2..end).rev().find(|&k| chars[k] == ' ') {
                end = sp + 1;
            }
        }
        out.push(chars[i..end].iter().collect::<String>().trim_end().to_string());
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic that unwound out of the main thread (release builds unwind) ends as exit
    /// status 134 with the panic location as the exit reason, so the launcher restores
    /// the framework as after an abort.
    #[test]
    fn main_thread_panic_ends_with_status_134_and_a_reason() {
        *lock(&MAIN_PANIC) = Some(("crates/x/src/y.rs:7:9".into(), String::new()));
        assert_eq!(finish_after_main_panic(), 134);
        assert_eq!(exit_reason().as_deref(), Some("panic at crates/x/src/y.rs:7:9"));
        assert!(lock(&MAIN_PANIC).is_none(), "the record is taken once");
    }

    #[test]
    fn levels_tails_and_redaction() {
        assert_eq!(parse_level("debug"), Some(LevelFilter::Debug));
        assert_eq!(parse_level("ink_app=debug,warn"), Some(LevelFilter::Warn));
        assert_eq!(parse_level("ink_html=trace"), Some(LevelFilter::Trace));
        assert_eq!(parse_level("loud"), None);
        let mut s = Settings::default();
        assert_eq!(settings_log_level(&s), "info");
        set_settings_log_level(&mut s, "debug");
        assert_eq!(settings_log_level(&s), "debug");
        set_settings_log_level(&mut s, "bogus");
        assert_eq!(settings_log_level(&s), "debug");
        assert_eq!(tail_text("a\nb\nc\n", 2, false), ["b", "c"]);
        assert_eq!(tail_text("rt\nb\nc", 5, true), ["b", "c"]);
        let r = redact_settings("[plugins.kosync]\npassword = \"x\"\napi_key=\"y\"\nuser = \"me\"\n");
        assert_eq!(r, "[plugins.kosync]\npassword = \"<redacted>\"\napi_key = \"<redacted>\"\nuser = \"me\"\n");
        assert_eq!(display_line("[INFO 12:34:56.789 +12 ink_app::app] screen: a -> b"), "12:34:56 INFO screen: a -> b");
        assert_eq!(display_line("[launcher 12:00:00] start"), "[launcher 12:00:00] start");
        assert_eq!(wrap_line("aaaa bbbb cccc", 10), ["aaaa bbbb", "cccc"]);
        assert_eq!(wrap_line("abcdefghijkl", 8), ["abcdefgh", "ijkl"]);
    }
}
