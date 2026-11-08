//! `lipc` access through child processes.
//!
//! The Kindle's `liblipc` is a glibc shared library that a static musl binary cannot
//! load, so this runs the stock tools instead:
//!
//! ```text
//! lipc-get-prop  [-i|-s] PUBLISHER PROPERTY          -> value on stdout
//! lipc-set-prop  [-i|-s] PUBLISHER PROPERTY VALUE
//! lipc-hash-prop PUBLISHER PROPERTY  < '{ key = "v", n = (1) }'   (hash-array properties)
//! lipc-wait-event [-m] [-s SECONDS] PUBLISHER EVENT[,EVENT...]  -> "name args..." lines
//! lipc-send-event PUBLISHER EVENT [-i N | -s S ...]
//! ```
//!
//! Robustness rules (all tested with fake tools in this module's tests):
//! - every call has a deadline; on expiry the child's whole process group is killed
//!   (`SIGKILL`), so a tool that forked a helper cannot keep a pipe open;
//! - output pipes are drained on threads, and the result is collected with a bounded
//!   wait, so a grandchild holding stdout never blocks the caller;
//! - on Linux every child gets `PR_SET_PDEATHSIG(SIGKILL)`: it dies with the thread
//!   that spawned it (spawn long-lived watchers from a long-lived thread);
//! - [`EventHub`] keeps one `lipc-wait-event -m` per publisher running and respawns a
//!   watcher that exited or was killed, with a growing back-off;
//! - only newline-terminated lines are events: a line cut off by a killed child is
//!   dropped.
//!
//! Hash-array formats were measured on the 5.19.5 emulator
//! (`out/evidence/device/wifi-lipc-probe-framework-running.txt`): input integers are
//! written `(0)`, strings `"x"`; output is `Hash Index: N` followed by `\tkey = "v"` /
//! `\tkey = (n)` lines. Running `lipc-hash-prop` without arguments segfaults.

use crate::{DeviceError, Result};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// One event line from `lipc-wait-event`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LipcEvent {
    /// Event name, e.g. `goingToScreenSaver`.
    pub name: String,
    /// Arguments after the name (quotes stripped).
    pub args: Vec<String>,
}

impl LipcEvent {
    /// Parses `name arg1 "arg 2"` (simple quoting as `lipc-wait-event` prints it). A
    /// leading `[HH:MM:SS.ffffff]` timestamp (from `-t`) is skipped.
    pub fn parse(line: &str) -> Option<LipcEvent> {
        let mut line = line.trim();
        if line.starts_with('[') {
            if let Some(end) = line.find(']') {
                line = line[end + 1..].trim_start();
            }
        }
        let mut words = Vec::new();
        let mut cur = String::new();
        let mut quoted = false;
        let mut any = false;
        for c in line.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    any = true;
                }
                c if c.is_whitespace() && !quoted => {
                    if any {
                        words.push(std::mem::take(&mut cur));
                        any = false;
                    }
                }
                c => {
                    cur.push(c);
                    any = true;
                }
            }
        }
        if any {
            words.push(cur);
        }
        let mut it = words.into_iter();
        let name = it.next()?;
        Some(LipcEvent { name, args: it.collect() })
    }

    /// The event as one line (`name arg1 arg2`), as `PowerEvent::parse` reads it.
    pub fn line(&self) -> String {
        std::iter::once(self.name.as_str()).chain(self.args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
    }
}

/// A value in a hash-array property.
#[derive(Clone, PartialEq, Eq)]
pub enum LipcValue {
    /// `"text"`.
    Str(String),
    /// `(42)`.
    Int(i64),
}

impl LipcValue {
    /// The string, or the integer formatted.
    pub fn as_string(&self) -> String {
        match self {
            LipcValue::Str(s) => s.clone(),
            LipcValue::Int(i) => i.to_string(),
        }
    }

    /// The integer, or a string that parses as one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            LipcValue::Int(i) => Some(*i),
            LipcValue::Str(s) => s.trim().parse().ok(),
        }
    }
}

/// Keys whose values are secrets and never printed.
pub const SECRET_KEYS: [&str; 4] = ["psk", "password", "passphrase", "wep_key"];

impl std::fmt::Debug for LipcValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LipcValue::Str(s) => write!(f, "{s:?}"),
            LipcValue::Int(i) => write!(f, "({i})"),
        }
    }
}

/// One hash of a hash-array property.
pub type LipcHash = BTreeMap<String, LipcValue>;

/// `hash` with secret values replaced by `<hidden>`, for logs.
pub fn redacted(hash: &LipcHash) -> String {
    let parts: Vec<String> = hash
        .iter()
        .map(|(k, v)| if SECRET_KEYS.contains(&k.as_str()) { format!("{k} = <hidden>") } else { format!("{k} = {v:?}") })
        .collect();
    format!("{{ {} }}", parts.join(", "))
}

/// Parses `lipc-hash-prop` output into hashes.
pub fn parse_hash_output(text: &str) -> Vec<LipcHash> {
    let mut out: Vec<LipcHash> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Hash Index:") {
            out.push(LipcHash::new());
            continue;
        }
        let Some((k, v)) = t.split_once(" = ") else { continue };
        let v = v.trim();
        let value = if let Some(s) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            LipcValue::Str(s.to_string())
        } else if let Some(n) = v.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            match n.trim().parse() {
                Ok(i) => LipcValue::Int(i),
                Err(_) => LipcValue::Str(n.to_string()),
            }
        } else {
            LipcValue::Str(v.to_string())
        };
        if out.is_empty() {
            out.push(LipcHash::new());
        }
        if let Some(h) = out.last_mut() {
            h.insert(k.trim().to_string(), value);
        }
    }
    out
}

/// Formats one hash as `lipc-hash-prop` input. Refuses strings the tool's parser cannot
/// carry (`"`, `\`, control characters) instead of trying to escape them.
pub fn format_hash_input(hash: &LipcHash) -> Result<String> {
    let mut parts = Vec::new();
    for (k, v) in hash {
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(DeviceError::Lipc(format!("invalid hash key {k:?}")));
        }
        match v {
            LipcValue::Int(i) => parts.push(format!("{k} = ({i})")),
            LipcValue::Str(s) => {
                if s.chars().any(|c| c == '"' || c == '\\' || c.is_control()) {
                    let what = if SECRET_KEYS.contains(&k.as_str()) { "<hidden>".to_string() } else { format!("{s:?}") };
                    return Err(DeviceError::Lipc(format!(
                        "{k}: value {what} contains a quote, backslash or control character, which lipc-hash-prop cannot take"
                    )));
                }
                parts.push(format!("{k} = \"{s}\""));
            }
        }
    }
    Ok(format!("{{ {} }}", parts.join(", ")))
}

/// Runs the `lipc-*` tools with a timeout.
#[derive(Clone, Debug)]
pub struct Lipc {
    /// Directory of the tools; `None` searches `PATH` (`/usr/bin` on the Kindle).
    pub bin_dir: Option<PathBuf>,
    /// Timeout for get/set calls.
    pub timeout: Duration,
}

impl Default for Lipc {
    fn default() -> Self {
        Lipc { bin_dir: None, timeout: Duration::from_secs(3) }
    }
}

/// How long to wait for a child's output pipes to close after it exited (a grandchild
/// may hold them); the group is killed after that.
const PIPE_GRACE: Duration = Duration::from_millis(300);

impl Lipc {
    /// Tools from `PATH`, 3 s timeout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Tools from `dir` (tests use a directory of fake scripts).
    pub fn with_bin_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.bin_dir = Some(dir.into());
        self
    }

    /// Changes the get/set timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn command(&self, tool: &str, own_group: bool) -> Command {
        let mut c = match &self.bin_dir {
            Some(d) => Command::new(d.join(tool)),
            None => Command::new(tool),
        };
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            if own_group {
                // A process group of its own, so a timeout kills helpers it forked too.
                c.process_group(0);
            }
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // The child dies with the thread that spawned it, so a killed rinky
            // (kill -9, SIGTERM without unwinding) leaves no `lipc-wait-event -m` behind.
            // SAFETY: prctl is async-signal-safe and only touches the child's own state.
            unsafe {
                c.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let _ = own_group;
        c
    }

    /// Reads an integer property (`lipc-get-prop -i`).
    pub fn get_int(&self, publisher: &str, prop: &str) -> Result<i64> {
        let out = self.run("lipc-get-prop", &["-i", publisher, prop], self.timeout)?;
        out.trim().parse().map_err(|_| DeviceError::Lipc(format!("{publisher} {prop}: not an integer: {out:?}")))
    }

    /// Reads a string property (`lipc-get-prop -s`).
    pub fn get_str(&self, publisher: &str, prop: &str) -> Result<String> {
        let out = self.run("lipc-get-prop", &["-s", publisher, prop], self.timeout)?;
        Ok(out.trim_end_matches('\n').to_string())
    }

    /// Writes an integer property (`lipc-set-prop -i`).
    pub fn set_int(&self, publisher: &str, prop: &str, value: i64) -> Result<()> {
        self.run("lipc-set-prop", &["-i", publisher, prop, &value.to_string()], self.timeout).map(|_| ())
    }

    /// Writes a string property (`lipc-set-prop -s`).
    pub fn set_str(&self, publisher: &str, prop: &str, value: &str) -> Result<()> {
        self.run("lipc-set-prop", &["-s", publisher, prop, value], self.timeout).map(|_| ())
    }

    /// Reads a hash-array property (`lipc-hash-prop P PROP` with an empty hash on stdin).
    pub fn hash_get(&self, publisher: &str, prop: &str) -> Result<Vec<LipcHash>> {
        self.hash_call(publisher, prop, "{}")
    }

    /// Writes a hash-array property (one hash; e.g. `createProfile`). The input goes
    /// through stdin, so secrets never appear in a process list. Returns the reply.
    pub fn hash_set(&self, publisher: &str, prop: &str, hash: &LipcHash) -> Result<Vec<LipcHash>> {
        let input = format_hash_input(hash)?;
        self.hash_call(publisher, prop, &input)
    }

    fn hash_call(&self, publisher: &str, prop: &str, input: &str) -> Result<Vec<LipcHash>> {
        let out = self.run_with_input("lipc-hash-prop", &[publisher, prop], Some(input), self.timeout)?;
        // The tool prints parse and access errors on stdout with exit status 0 or 1.
        if out.contains("Parse error") || out.contains("Failed to access") {
            return Err(DeviceError::Lipc(format!("lipc-hash-prop {publisher} {prop}: {}", out.trim())));
        }
        Ok(parse_hash_output(&out))
    }

    /// Sends an event as `publisher` (`lipc-send-event`); only works for publishers no
    /// daemon owns (e.g. `com.lab126.hal`, used by udev scripts).
    pub fn send_event(&self, publisher: &str, event: &str, args: &[LipcValue]) -> Result<()> {
        let mut a: Vec<String> = vec![publisher.into(), event.into()];
        for v in args {
            match v {
                LipcValue::Int(i) => a.extend(["-i".to_string(), i.to_string()]),
                LipcValue::Str(s) => a.extend(["-s".to_string(), s.clone()]),
            }
        }
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        let out = self.run("lipc-send-event", &refs, self.timeout)?;
        if out.contains("failed to send") || out.contains("Failed to open") {
            return Err(DeviceError::Lipc(format!("lipc-send-event {publisher} {event}: {}", out.trim())));
        }
        Ok(())
    }

    /// Waits for one of `events` (empty = any, `'*'`). Returns `Ok(None)` when nothing
    /// arrived within `timeout`.
    pub fn wait_event(&self, publisher: &str, events: &[&str], timeout: Duration) -> Result<Option<LipcEvent>> {
        let secs = timeout.as_secs().max(1).to_string();
        let filter = if events.is_empty() { "*".to_string() } else { events.join(",") };
        // Our own deadline is a little longer than the tool's -s timeout.
        match self.run("lipc-wait-event", &["-s", &secs, publisher, &filter], timeout + Duration::from_secs(2)) {
            Ok(out) => Ok(out.lines().find_map(LipcEvent::parse)),
            Err(DeviceError::Timeout(_)) => Ok(None),
            // lipc-wait-event exits non-zero when its own -s timeout expires.
            Err(DeviceError::Lipc(msg)) if msg.contains("exit status") && msg.contains("stdout \"\"") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Starts `lipc-wait-event -m PUBLISHER EVENTS` and streams its events.
    pub fn watch(&self, publisher: &str, events: &[&str]) -> Result<LipcWatcher> {
        self.watch_with_notify(publisher, events, || {})
    }

    /// Like [`Lipc::watch`], and calls `notify` on the reader thread after each event is
    /// queued (the Kindle input loop uses it to wake its `poll` through a pipe).
    pub fn watch_with_notify(
        &self,
        publisher: &str,
        events: &[&str],
        notify: impl Fn() + Send + 'static,
    ) -> Result<LipcWatcher> {
        let (child, stdout) = self.spawn_watch(publisher, events)?;
        Ok(LipcWatcher::new(child, stdout, Box::new(notify)))
    }

    fn spawn_watch(&self, publisher: &str, events: &[&str]) -> Result<(Child, ChildStdout)> {
        let filter = if events.is_empty() { "*".to_string() } else { events.join(",") };
        let mut cmd = self.command("lipc-wait-event", false);
        cmd.args(["-m", publisher, &filter]).stderr(Stdio::null());
        let mut child = spawn_retry(&mut cmd)?;
        let stdout = child.stdout.take().ok_or_else(|| DeviceError::Lipc("no stdout".into()))?;
        Ok((child, stdout))
    }

    /// Runs `tool args`, returns stdout. Kills the child (and its process group) after
    /// `timeout`.
    pub fn run(&self, tool: &str, args: &[&str], timeout: Duration) -> Result<String> {
        self.run_with_input(tool, args, None, timeout)
    }

    /// Like [`Lipc::run`], writing `input` to the child's stdin first.
    pub fn run_with_input(&self, tool: &str, args: &[&str], input: Option<&str>, timeout: Duration) -> Result<String> {
        let mut cmd = self.command(tool, true);
        cmd.args(args);
        if input.is_some() {
            cmd.stdin(Stdio::piped());
        }
        let mut child = spawn_retry(&mut cmd)?;
        let pgid = child.id() as i32;
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            // Small inputs fit the pipe buffer; a child that does not read gets EPIPE.
            let _ = stdin.write_all(text.as_bytes());
            let _ = stdin.write_all(b"\n");
        }
        let stdout = child.stdout.take().ok_or_else(|| DeviceError::Lipc("no stdout".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| DeviceError::Lipc("no stderr".into()))?;
        // Drain the pipes on threads so a chatty child cannot block on a full pipe.
        let out_rx = drain(stdout);
        let err_rx = drain(stderr);
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                kill_group(pgid);
                let _ = child.kill();
                let _ = child.wait();
                return Err(DeviceError::Timeout(format!("{tool} {} after {timeout:?}", args.join(" "))));
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let out = match out_rx.recv_timeout(PIPE_GRACE) {
            Ok(s) => s,
            Err(_) => {
                // A helper the tool forked still holds stdout: kill the group, keep going.
                kill_group(pgid);
                out_rx.recv_timeout(PIPE_GRACE).unwrap_or_default()
            }
        };
        let err = err_rx.recv_timeout(PIPE_GRACE).unwrap_or_default();
        if !status.success() {
            return Err(DeviceError::Lipc(format!(
                "{tool} {}: exit status {:?}, stdout {:?}, stderr {:?}",
                args.join(" "),
                status.code(),
                out.trim(),
                err.trim()
            )));
        }
        Ok(out)
    }
}

/// Spawns, retrying a few times on `ETXTBSY` (a script that was just written can be
/// briefly held open by a child another thread is forking).
fn spawn_retry(cmd: &mut Command) -> std::io::Result<Child> {
    let mut tries = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(26) && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            r => return r,
        }
    }
}

fn drain(mut r: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    rx
}

fn kill_group(pgid: i32) {
    #[cfg(unix)]
    if pgid > 1 {
        // SAFETY: kill(2) with a negative pid signals the process group we created.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

/// Reads newline-terminated lines and calls `f` for each event; a final line without
/// a newline (cut off by a killed child) is dropped.
fn read_events(stdout: impl Read, mut f: impl FnMut(LipcEvent) -> bool) {
    let mut r = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if buf.last() != Some(&b'\n') {
                    log::debug!("lipc: dropped a partial line {:?}", String::from_utf8_lossy(&buf));
                    break;
                }
                if let Some(ev) = LipcEvent::parse(&String::from_utf8_lossy(&buf)) {
                    if !f(ev) {
                        break;
                    }
                }
            }
        }
    }
}

/// A running `lipc-wait-event -m`. Events are read on a thread and queued; the child
/// is killed when the watcher is dropped, and on Linux also when the spawning thread
/// dies (`PR_SET_PDEATHSIG`), so it never outlives a killed rinky.
pub struct LipcWatcher {
    child: Child,
    rx: mpsc::Receiver<LipcEvent>,
}

impl LipcWatcher {
    fn new(child: Child, stdout: ChildStdout, notify: Box<dyn Fn() + Send>) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            read_events(stdout, |ev| {
                if tx.send(ev).is_err() {
                    return false;
                }
                notify();
                true
            })
        });
        LipcWatcher { child, rx }
    }

    /// Next queued event without waiting.
    pub fn try_next(&mut self) -> Option<LipcEvent> {
        self.rx.try_recv().ok()
    }

    /// Next event, waiting up to `timeout`.
    pub fn next_timeout(&mut self, timeout: Duration) -> Option<LipcEvent> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// True while the child process is alive.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Process id of the `lipc-wait-event` child.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for LipcWatcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An event with the publisher it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedEvent {
    /// `com.lab126.powerd`, ...
    pub publisher: String,
    /// The event.
    pub event: LipcEvent,
}

/// State of one [`EventHub`] subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionStatus {
    /// Publisher.
    pub publisher: String,
    /// A `lipc-wait-event -m` child is running.
    pub running: bool,
    /// How many times it was restarted.
    pub restarts: u32,
    /// Its pid when running.
    pub pid: Option<u32>,
}

enum HubMsg {
    Event(usize, u64, LipcEvent),
    Eof(usize, u64),
}

struct Subscription {
    publisher: String,
    events: Vec<String>,
    child: Option<Child>,
    generation: u64,
    restarts: u32,
    backoff: Duration,
    retry_at: Option<Instant>,
    /// When the running watcher was started.
    started_at: Option<Instant>,
}

/// Subscriptions to several publishers (`powerd`, `wifid`, `cmd`, `volumd`, `hal`),
/// each a supervised `lipc-wait-event -m`. Call [`EventHub::maintain`] regularly from
/// the thread that owns the hub (the input loop does it on every wake-up): it reaps
/// watchers that exited (killed, crashed, daemon restart) and respawns them after a
/// back-off of 1 s, doubling up to 60 s, reset when a watcher ends after it ran for
/// [`HUB_STABLE`]. A watcher that prints a line (lipc tools print errors on stdout)
/// and exits is therefore respawned at 1, 2, 4 ... 60 s, not once a second.
pub struct EventHub {
    lipc: Lipc,
    subs: Vec<Subscription>,
    tx: mpsc::Sender<HubMsg>,
    rx: mpsc::Receiver<HubMsg>,
    notify: Arc<dyn Fn() + Send + Sync>,
    pending: std::collections::VecDeque<PublishedEvent>,
}

/// Default first back-off for a respawn.
pub const HUB_BACKOFF_START: Duration = Duration::from_secs(1);
/// Longest back-off.
pub const HUB_BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A watcher that ran at least this long before it ended restarts after
/// [`HUB_BACKOFF_START`] again.
pub const HUB_STABLE: Duration = Duration::from_secs(30);

impl EventHub {
    /// An empty hub; `notify` is called (on a reader thread) whenever a message is
    /// queued, to wake a `poll` loop.
    pub fn new(lipc: Lipc, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        EventHub { lipc, subs: Vec::new(), tx, rx, notify: Arc::new(notify), pending: Default::default() }
    }

    /// Subscribes to `events` of `publisher` (empty = all) and starts the watcher now.
    /// A failed start is retried by [`EventHub::maintain`].
    pub fn subscribe(&mut self, publisher: &str, events: &[&str]) {
        self.subs.push(Subscription {
            publisher: publisher.to_string(),
            events: events.iter().map(|s| s.to_string()).collect(),
            child: None,
            generation: 0,
            restarts: 0,
            backoff: HUB_BACKOFF_START,
            retry_at: Some(Instant::now()),
            started_at: None,
        });
        let i = self.subs.len() - 1;
        self.start(i);
    }

    fn start(&mut self, i: usize) {
        let sub = &mut self.subs[i];
        let events: Vec<&str> = sub.events.iter().map(String::as_str).collect();
        match self.lipc.spawn_watch(&sub.publisher, &events) {
            Ok((child, stdout)) => {
                sub.generation += 1;
                let gen = sub.generation;
                log::info!("lipc: watching {} (pid {}, generation {gen})", sub.publisher, child.id());
                sub.child = Some(child);
                sub.retry_at = None;
                sub.started_at = Some(Instant::now());
                let tx = self.tx.clone();
                let notify = self.notify.clone();
                std::thread::spawn(move || {
                    read_events(stdout, |ev| {
                        let ok = tx.send(HubMsg::Event(i, gen, ev)).is_ok();
                        notify();
                        ok
                    });
                    let _ = tx.send(HubMsg::Eof(i, gen));
                    notify();
                });
            }
            Err(e) => {
                log::warn!("lipc: cannot watch {}: {e}; retry in {:?}", sub.publisher, sub.backoff);
                sub.retry_at = Some(Instant::now() + sub.backoff);
                sub.backoff = (sub.backoff * 2).min(HUB_BACKOFF_MAX);
            }
        }
    }

    fn reap(&mut self, i: usize, why: &str) {
        let sub = &mut self.subs[i];
        if let Some(mut c) = sub.child.take() {
            let _ = c.kill();
            let status = c.wait();
            if sub.started_at.take().is_some_and(|t| t.elapsed() >= HUB_STABLE) {
                sub.backoff = HUB_BACKOFF_START;
            }
            log::warn!("lipc: watcher for {} ended ({why}, {status:?}); restart in {:?}", sub.publisher, sub.backoff);
            sub.restarts += 1;
            sub.retry_at = Some(Instant::now() + sub.backoff);
            sub.backoff = (sub.backoff * 2).min(HUB_BACKOFF_MAX);
        }
    }

    /// Moves queued messages into the event queue, reaps dead watchers and respawns
    /// those whose back-off expired. Returns the earliest pending respawn time.
    pub fn maintain(&mut self) -> Option<Instant> {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                HubMsg::Event(i, gen, event) if self.subs[i].generation == gen => {
                    self.pending.push_back(PublishedEvent { publisher: self.subs[i].publisher.clone(), event });
                }
                HubMsg::Eof(i, gen) if self.subs[i].generation == gen => self.reap(i, "end of output"),
                _ => {}
            }
        }
        for i in 0..self.subs.len() {
            let dead = match self.subs[i].child.as_mut() {
                Some(c) => !matches!(c.try_wait(), Ok(None)),
                None => false,
            };
            if dead {
                self.reap(i, "exited");
            }
        }
        let now = Instant::now();
        for i in 0..self.subs.len() {
            if self.subs[i].child.is_none() && self.subs[i].retry_at.is_some_and(|t| t <= now) {
                self.start(i);
            }
        }
        self.subs.iter().filter(|s| s.child.is_none()).filter_map(|s| s.retry_at).min()
    }

    /// The next event, without waiting.
    pub fn try_next(&mut self) -> Option<PublishedEvent> {
        if self.pending.is_empty() {
            self.maintain();
        }
        self.pending.pop_front()
    }

    /// The next event, waiting up to `timeout` (maintaining the watchers meanwhile).
    pub fn next_timeout(&mut self, timeout: Duration) -> Option<PublishedEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(e) = self.try_next() {
                return Some(e);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            match self.rx.recv_timeout((deadline - now).min(Duration::from_millis(100))) {
                Ok(m) => {
                    // Put it back through the normal path.
                    let _ = self.tx.send(m);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Status of every subscription.
    pub fn status(&mut self) -> Vec<SubscriptionStatus> {
        self.subs
            .iter_mut()
            .map(|s| {
                let running = s.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)));
                SubscriptionStatus {
                    publisher: s.publisher.clone(),
                    running,
                    restarts: s.restarts,
                    pid: if running { s.child.as_ref().map(|c| c.id()) } else { None },
                }
            })
            .collect()
    }
}

impl Drop for EventHub {
    fn drop(&mut self) {
        for s in &mut self.subs {
            if let Some(mut c) = s.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tool_dir(tag: &str) -> PathBuf {
        crate::sysfs::test_dir(&format!("lipc-{tag}"))
    }

    fn write_tool(dir: &std::path::Path, name: &str, body: &str) {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn fake_tools() -> PathBuf {
        let dir = tool_dir("basic");
        write_tool(
            &dir,
            "lipc-get-prop",
            r#"case "$3" in flIntensity) echo 12;; state) echo screenSaver;; slow) sleep 5;; *) echo "no prop $3" >&2; exit 1;; esac"#,
        );
        write_tool(&dir, "lipc-set-prop", r#"echo "$@" >> "$(dirname "$0")/set.log""#);
        write_tool(
            &dir,
            "lipc-wait-event",
            r#"if [ "$1" = -m ]; then echo 'goingToScreenSaver 2'; echo 'outOfScreenSaver 1'; sleep 5; else echo 'charging "usb x"'; fi"#,
        );
        dir
    }

    #[test]
    fn get_set_wait_and_timeouts() {
        let dir = fake_tools();
        // Generous for calls that must succeed (shared, loaded build machine); short for
        // the one that must time out.
        let lipc = Lipc::new().with_bin_dir(&dir).with_timeout(Duration::from_secs(10));
        let quick = Lipc::new().with_bin_dir(&dir).with_timeout(Duration::from_millis(500));
        assert_eq!(lipc.get_int("com.lab126.powerd", "flIntensity").unwrap(), 12);
        assert_eq!(lipc.get_str("com.lab126.powerd", "state").unwrap(), "screenSaver");
        assert!(matches!(lipc.get_int("com.lab126.powerd", "state"), Err(DeviceError::Lipc(_))));
        assert!(matches!(lipc.get_int("com.lab126.powerd", "nope"), Err(DeviceError::Lipc(_))));
        let t = Instant::now();
        assert!(matches!(quick.get_str("com.lab126.powerd", "slow"), Err(DeviceError::Timeout(_))));
        assert!(t.elapsed() < Duration::from_secs(3), "timeout must kill the child");
        lipc.set_int("com.lab126.powerd", "flIntensity", 7).unwrap();
        lipc.set_str("com.lab126.wifid", "cmConnect", "home net").unwrap();
        let log = std::fs::read_to_string(dir.join("set.log")).unwrap();
        assert_eq!(log, "-i com.lab126.powerd flIntensity 7\n-s com.lab126.wifid cmConnect home net\n");
        let ev = lipc.wait_event("com.lab126.powerd", &["charging"], Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!(ev, LipcEvent { name: "charging".into(), args: vec!["usb x".into()] });
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h2 = hits.clone();
        let mut w = lipc
            .watch_with_notify("com.lab126.powerd", &[], move || {
                h2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .unwrap();
        assert_eq!(w.next_timeout(Duration::from_secs(2)).unwrap().name, "goingToScreenSaver");
        assert_eq!(w.next_timeout(Duration::from_secs(2)).unwrap().args, vec!["1".to_string()]);
        assert!(w.is_running());
        let t = Instant::now();
        while hits.load(std::sync::atomic::Ordering::SeqCst) < 2 && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
        drop(w);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hung_child_with_a_forked_helper_is_killed_with_its_group() {
        let dir = tool_dir("hung");
        // The tool forks a helper that keeps stdout open and then hangs itself.
        write_tool(&dir, "lipc-get-prop", "sleep 30 &\necho 5\nsleep 30");
        let lipc = Lipc::new().with_bin_dir(&dir).with_timeout(Duration::from_millis(300));
        let t = Instant::now();
        assert!(matches!(lipc.get_int("p", "x"), Err(DeviceError::Timeout(_))));
        assert!(t.elapsed() < Duration::from_secs(2));
        // The tool exits at once but its helper keeps the pipe: bounded wait, no hang.
        write_tool(&dir, "lipc-set-prop", "sleep 30 &\nexit 0");
        let lipc = Lipc::new().with_bin_dir(&dir).with_timeout(Duration::from_secs(10));
        let t = Instant::now();
        lipc.set_int("p", "x", 1).unwrap();
        assert!(t.elapsed() < Duration::from_secs(2), "took {:?}", t.elapsed());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hash_properties_round_trip_and_errors() {
        let dir = tool_dir("hash");
        write_tool(
            &dir,
            "lipc-hash-prop",
            r#"in=$(cat); echo "$in" > "$(dirname "$0")/stdin.txt"
case "$2" in
  scanList) printf 'Hash Index: 0\n\tessid = "Kindle-QEMU"\n\tsecured = "no"\n\tsignal = (5)\n\tsignal_max = (5)\n\nHash Index: 1\n\tessid = "Home net"\n\tsecured = "yes"\n\tsignal = (2)\n';;
  createProfile) echo "$in" | grep -q 'essid = "x"' && exit 0; echo 'Parse error ( syntax error )'; exit 1;;
  noprop) echo 'Failed to access hasharray property (err=8 lipcErrNoSuchProperty)';;
esac"#,
        );
        let lipc = Lipc::new().with_bin_dir(&dir).with_timeout(Duration::from_secs(2));
        let list = lipc.hash_get("com.lab126.wifid", "scanList").unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["essid"], LipcValue::Str("Kindle-QEMU".into()));
        assert_eq!(list[1]["signal"].as_int(), Some(2));
        assert_eq!(std::fs::read_to_string(dir.join("stdin.txt")).unwrap(), "{}\n");
        let mut h = LipcHash::new();
        h.insert("essid".into(), LipcValue::Str("x".into()));
        h.insert("psk".into(), LipcValue::Str("s3cret word".into()));
        h.insert("store_nw_user_pref".into(), LipcValue::Int(0));
        lipc.hash_set("com.lab126.wifid", "createProfile", &h).unwrap();
        let sent = std::fs::read_to_string(dir.join("stdin.txt")).unwrap();
        assert_eq!(sent, "{ essid = \"x\", psk = \"s3cret word\", store_nw_user_pref = (0) }\n");
        assert!(!redacted(&h).contains("s3cret"));
        h.insert("essid".into(), LipcValue::Str("y".into()));
        assert!(lipc.hash_set("com.lab126.wifid", "createProfile", &h).is_err(), "parse error must be an error");
        assert!(lipc.hash_get("com.lab126.wifid", "noprop").is_err());
        h.insert("psk".into(), LipcValue::Str("a\"b".into()));
        let e = lipc.hash_set("com.lab126.wifid", "createProfile", &h).unwrap_err().to_string();
        assert!(e.contains("<hidden>") && !e.contains("a\"b"), "{e}");
        let mut bad = LipcHash::new();
        bad.insert("bad key".into(), LipcValue::Int(1));
        assert!(format_hash_input(&bad).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn event_line_parsing() {
        assert_eq!(LipcEvent::parse("  "), None);
        assert_eq!(LipcEvent::parse("a").unwrap().args.len(), 0);
        assert_eq!(LipcEvent::parse(r#"x "" 3"#).unwrap().args, vec!["".to_string(), "3".to_string()]);
        let e = LipcEvent::parse(r#"[18:43:49.320945] cmStateChange "CONNECTED" "#).unwrap();
        assert_eq!(e, LipcEvent { name: "cmStateChange".into(), args: vec!["CONNECTED".into()] });
        assert_eq!(e.line(), "cmStateChange CONNECTED");
    }

    #[test]
    fn partial_lines_are_dropped_and_bursts_kept() {
        let mut got = Vec::new();
        let mut text = String::new();
        for i in 0..500 {
            text.push_str(&format!("battLevelChanged {i}\n"));
        }
        text.push_str("goingToScree");
        read_events(text.as_bytes(), |e| {
            got.push(e);
            true
        });
        assert_eq!(got.len(), 500);
        assert_eq!(got[499].args, vec!["499".to_string()]);
    }

    #[test]
    fn hub_restarts_a_killed_watcher_and_tags_publishers() {
        let dir = tool_dir("hub");
        // Each run prints its publisher and a run counter, then waits.
        write_tool(
            &dir,
            "lipc-wait-event",
            r#"d=$(dirname "$0"); n=$(cat "$d/n.$2" 2>/dev/null || echo 0); n=$((n+1)); echo $n > "$d/n.$2"
echo $$ > "$d/pid.$2"
echo "started $n"
exec sleep 30"#,
        );
        let lipc = Lipc::new().with_bin_dir(&dir);
        let mut hub = EventHub::new(lipc, || {});
        hub.subscribe("com.lab126.powerd", &[]);
        hub.subscribe("com.lab126.hal", &[]);
        let mut seen = Vec::new();
        while seen.len() < 2 {
            let e = hub.next_timeout(Duration::from_secs(3)).expect("first events");
            seen.push((e.publisher, e.event.line()));
        }
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("com.lab126.hal".to_string(), "started 1".to_string()),
                ("com.lab126.powerd".to_string(), "started 1".to_string())
            ]
        );
        let st = hub.status();
        assert!(st.iter().all(|s| s.running && s.restarts == 0), "{st:?}");
        // kill -9 the powerd watcher: the hub notices and starts it again after 1 s.
        let pid: i32 = std::fs::read_to_string(dir.join("pid.com.lab126.powerd")).unwrap().trim().parse().unwrap();
        unsafe { libc::kill(pid, libc::SIGKILL) };
        let e = hub.next_timeout(Duration::from_secs(5)).expect("restarted watcher event");
        assert_eq!((e.publisher.as_str(), e.event.line().as_str()), ("com.lab126.powerd", "started 2"));
        let st = hub.status();
        assert_eq!(st.iter().find(|s| s.publisher == "com.lab126.powerd").unwrap().restarts, 1);
        assert_eq!(st.iter().find(|s| s.publisher == "com.lab126.hal").unwrap().restarts, 0);
        drop(hub);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hub_retries_a_missing_tool() {
        let dir = tool_dir("hub-missing");
        let lipc = Lipc::new().with_bin_dir(&dir);
        let mut hub = EventHub::new(lipc, || {});
        hub.subscribe("com.lab126.wifid", &[]);
        let st = hub.status();
        assert!(!st[0].running);
        let retry = hub.maintain().expect("a retry is scheduled");
        assert!(retry > Instant::now());
        write_tool(&dir, "lipc-wait-event", "echo up\nexec sleep 30");
        let e = hub.next_timeout(Duration::from_secs(4)).expect("watcher started after the back-off");
        assert_eq!(e.event.name, "up");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
