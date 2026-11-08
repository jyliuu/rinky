//! The network manager: one worker thread per process owns every Wi-Fi operation of an
//! e-reader backend (radio, scans, joins, DHCP) and publishes one [`NetStatus`].
//!
//! [`NetMachine`] decides what to do; it is driven by commands ([`Cmd`]: start-up, the
//! user's on/off and joins, sleep and wake, wifid's link events) and by time, and acts
//! through a [`NetBackend`] (wifid on a Kindle, `wpa_supplicant` and DHCP on a Kobo, a
//! script in the tests). It never reads a clock itself, so the tests drive it with
//! made-up instants.
//!
//! - **Connected** means the interface is up and has an address ([`link`]): an IPv4
//!   address or a global IPv6 one. wifid's `cmState` and `wpa_supplicant`'s state only
//!   wake the machine up ([`Cmd::Link`]); neither is trusted alone (wifid says
//!   `PENDING` with the radio on and nothing joined).
//! - After a start (the radio turned on, a wake, a lost connection) the link is probed
//!   at once and every [`FAST_PROBE`] for [`FAST_FOR`]; the first scan for a saved
//!   network comes after [`FIRST_LOOK`], then scans get rarer ([`AutoJoin`]). Only a
//!   start resets that backoff, not a link event.
//! - Associated without an address and the backend runs DHCP itself
//!   ([`NetBackend::needs_dhcp`], a Kobo): DHCP runs, once per association (again after
//!   [`JOIN_FOR`] without a lease).
//! - While connected the link is probed every [`CONNECTED_PROBE`]: sysfs and
//!   `getifaddrs`, plus two `wpa_cli` calls on a Kobo or two lipc reads on a Kindle.
//!   While not connected a probe whose sysfs operational state says there is no
//!   association (`down`, `dormant`) starts no process.
//! - "Wi-Fi was on" ([`NetEvent::WasOn`]): set by turning Wi-Fi on or a connection,
//!   cleared only by the user turning it off. The app keeps it in the settings and
//!   passes it back at start-up ([`Cmd::Boot`]); with it set, the radio is turned on
//!   again at start-up and after a wake when it is off (a Kobo turns it off to sleep).
//! - The radio is turned on only by a command (start-up, the user, a wake), never
//!   because a probe found it off: a Kobo turns it off right before it suspends.

use crate::services::{WifiNetwork, WifiState};
use crate::wifi::AutoJoin;
use crate::Result;
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Probe interval right after a start.
pub const FAST_PROBE: Duration = Duration::from_millis(500);
/// How long after a start the link is probed every [`FAST_PROBE`].
pub const FAST_FOR: Duration = Duration::from_secs(10);
/// Probe interval while searching after the fast window (a network the radio joins by
/// itself is noticed this late at worst).
pub const SLOW_PROBE: Duration = Duration::from_secs(10);
/// The first scan for a saved network after a start (the radio may rejoin the network
/// it had by itself before that).
pub const FIRST_LOOK: Duration = Duration::from_secs(3);
/// Probe interval while connected.
pub const CONNECTED_PROBE: Duration = Duration::from_secs(30);
/// How long a join may take before the network counts as failed (and how long DHCP
/// may take before it runs again).
pub const JOIN_FOR: Duration = Duration::from_secs(30);

/// What the network is doing, as the UI shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetStatus {
    /// The radio is off.
    Off,
    /// The radio is on and a network is being joined (`essid`, when known) or looked
    /// for right after a start.
    Connecting {
        /// The network being joined.
        essid: Option<String>,
    },
    /// Joined, with an address.
    Connected {
        /// Network name ("" when the backend cannot tell).
        essid: String,
        /// Signal 0..=5.
        signal: Option<u8>,
    },
    /// The radio is on; no saved network was found (scans go on, less often).
    NoNetwork,
}

impl NetStatus {
    /// The network when connected.
    pub fn connected_to(&self) -> Option<&str> {
        match self {
            NetStatus::Connected { essid, .. } => Some(essid),
            _ => None,
        }
    }

    /// The radio is on.
    pub fn radio_on(&self) -> bool {
        !matches!(self, NetStatus::Off)
    }

    /// The [`WifiState`] for this status: an ESSID only when connected.
    pub fn to_state(&self) -> WifiState {
        let (enabled, state) = match self {
            NetStatus::Off => (false, "NA"),
            NetStatus::Connecting { .. } => (true, "PENDING"),
            NetStatus::Connected { .. } => (true, "CONNECTED"),
            NetStatus::NoNetwork => (true, "NA"),
        };
        let (essid, signal) = match self {
            NetStatus::Connected { essid, signal } => (Some(essid.clone()), *signal),
            _ => (None, None),
        };
        WifiState { enabled, state: state.into(), essid, signal }
    }

    /// The status a [`WifiState`] read directly stands for (backends without a worker).
    pub fn from_state(s: &WifiState) -> NetStatus {
        if !s.enabled {
            return NetStatus::Off;
        }
        match s.state.as_str() {
            "CONNECTED" => NetStatus::Connected { essid: s.essid.clone().unwrap_or_default(), signal: s.signal },
            "PENDING" => NetStatus::Connecting { essid: s.essid.clone() },
            _ => NetStatus::NoNetwork,
        }
    }
}

/// What the worker reports to the app ([`crate::PowerEvent::Network`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetEvent {
    /// The status changed (sent only on a change).
    Status(NetStatus),
    /// Joining this network failed (it is skipped for a while by the automatic joins).
    JoinFailed(String),
    /// A scan finished: the number of networks found, or why it failed. The list is
    /// [`crate::Wifi::scan_list`].
    Scanned(std::result::Result<usize, String>),
    /// "Wi-Fi was on" changed: keep it for the next start ([`Cmd::Boot`]).
    WasOn(bool),
}

/// What a probe of the link found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    /// The radio is on.
    pub radio: bool,
    /// The interface is up and has an address ([`link::has_address`]).
    pub address: bool,
    /// Associated with a network (with or without an address).
    pub associated: bool,
    /// The network associated with (or being joined).
    pub essid: Option<String>,
    /// Signal 0..=5 when known.
    pub signal: Option<u8>,
}

/// A command for the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// The app started; `restore`: Wi-Fi was on when it last ran.
    Boot {
        /// Turn the radio on when it is off.
        restore: bool,
    },
    /// The user turned Wi-Fi on.
    Enable,
    /// The user turned Wi-Fi off.
    Disable,
    /// The device left (`true`) or entered (`false`) the screensaver.
    Awake(bool),
    /// wifid's connection state changed (`cmStateChange`): probe now.
    Link(String),
    /// The user joins a network, with a password for a new profile.
    Connect {
        /// Network name.
        essid: String,
        /// Password (`None`: a saved or open network).
        password: Option<String>,
    },
    /// The user asked for a scan.
    Scan,
}

/// The platform's Wi-Fi operations. Each call may block (the worker thread makes them).
pub trait NetBackend {
    /// The radio is on now.
    fn radio_on(&mut self) -> bool;
    /// Turns the radio on (with whatever the platform needs to join networks) or off.
    fn set_radio(&mut self, on: bool) -> Result<()>;
    /// The radio was on at start-up: completes what [`NetBackend::set_radio`] would have
    /// set up (a Kobo starts `wpa_supplicant` when the launcher left only the interface
    /// up). The default does nothing: switching a Kindle's radio on again could drop
    /// the connection it has.
    fn prepare(&mut self) -> Result<()> {
        Ok(())
    }
    /// Looks at the link.
    fn probe(&mut self) -> Probe;
    /// Scans and returns the networks found, strongest first.
    fn scan(&mut self) -> Result<Vec<WifiNetwork>>;
    /// Starts joining `essid`, saving `password` first when given. Returns at once; the
    /// probes tell when the join finished.
    fn join(&mut self, essid: &str, password: Option<&str>) -> Result<()>;
    /// The platform's DHCP must be run by rinky ([`NetBackend::obtain_ip`]).
    fn needs_dhcp(&self) -> bool {
        false
    }
    /// Gets an address (blocks until there is one or the DHCP client gives up).
    fn obtain_ip(&mut self) -> Result<()> {
        Ok(())
    }
    /// A connection was made (the Kobo sets the clock).
    fn on_connected(&mut self) {}
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    /// Before [`Cmd::Boot`].
    Unstarted,
    /// The radio is off (the user, or it never came on).
    Off,
    /// The radio went off by itself (a Kobo before suspend); a wake turns it on again.
    Down,
    /// The radio is on, nothing joined.
    Searching,
    /// A join is running.
    Joining { essid: String, since: Instant },
    /// Joined, with an address.
    Connected { essid: String },
}

/// The network state machine (see the module documentation).
#[derive(Debug)]
pub struct NetMachine {
    state: State,
    was_on: bool,
    awake: bool,
    policy: AutoJoin,
    /// The last start: the fast probes run for [`FAST_FOR`] after it.
    started: Option<Instant>,
    /// A scan since the last start found nothing to join.
    looked: bool,
    /// The last probe found the radio associated (no scan then: it would only disturb
    /// the join or DHCP in progress).
    associated: bool,
    next_probe: Option<Instant>,
    next_look: Option<Instant>,
    /// The association DHCP last ran for, and when.
    dhcp: Option<(Option<String>, Instant)>,
    status: Option<NetStatus>,
    out: Vec<NetEvent>,
}

impl Default for NetMachine {
    fn default() -> Self {
        NetMachine {
            state: State::Unstarted,
            was_on: false,
            awake: true,
            policy: AutoJoin::default(),
            started: None,
            looked: false,
            associated: false,
            next_probe: None,
            next_look: None,
            dhcp: None,
            status: None,
            out: Vec::new(),
        }
    }
}

impl NetMachine {
    /// A machine waiting for [`Cmd::Boot`].
    pub fn new() -> Self {
        Self::default()
    }

    /// The status last published.
    pub fn status(&self) -> Option<&NetStatus> {
        self.status.as_ref()
    }

    /// "Wi-Fi was on".
    pub fn was_on(&self) -> bool {
        self.was_on
    }

    /// When [`NetMachine::tick`] has work next (`None`: only a command wakes it).
    pub fn deadline(&self) -> Option<Instant> {
        if !self.awake || !matches!(self.state, State::Searching | State::Joining { .. } | State::Connected { .. }) {
            return None;
        }
        [self.next_probe, self.next_look].into_iter().flatten().min()
    }

    /// Handles a command at `now`, then does the work that is due.
    pub fn handle(&mut self, cmd: Cmd, now: Instant, b: &mut dyn NetBackend) -> Vec<NetEvent> {
        log::info!("net: {cmd:?} in {:?}", self.state);
        match cmd {
            Cmd::Boot { restore } => {
                self.was_on = restore;
                self.awake = true;
                // A radio already on is prepared: on a Kobo that starts wpa_supplicant
                // when the launcher left the interface up without it.
                self.bring_up(now, b, true);
            }
            Cmd::Enable => {
                self.set_was_on(true);
                match b.set_radio(true) {
                    Ok(()) => self.start(now),
                    Err(e) => {
                        log::warn!("net: radio on: {e}");
                        self.off();
                    }
                }
            }
            Cmd::Disable => {
                self.set_was_on(false);
                if let Err(e) = b.set_radio(false) {
                    log::warn!("net: radio off: {e}");
                }
                self.off();
            }
            Cmd::Awake(false) => self.awake = false,
            Cmd::Awake(true) => {
                self.awake = true;
                match self.state {
                    State::Unstarted | State::Off => {}
                    _ if !b.radio_on() => self.bring_up(now, b, false),
                    // Still connected, most likely: check without showing a search.
                    State::Connected { .. } => self.next_probe = Some(now),
                    _ => self.start(now),
                }
            }
            Cmd::Link(_) => {
                if matches!(self.state, State::Searching | State::Joining { .. } | State::Connected { .. }) {
                    self.next_probe = Some(now);
                }
            }
            Cmd::Connect { essid, password } => {
                self.set_was_on(true);
                self.policy.forget(&essid);
                let radio = b.radio_on() || b.set_radio(true).is_ok();
                if !matches!(self.state, State::Searching | State::Joining { .. } | State::Connected { .. }) {
                    self.start(now);
                }
                match (radio, b.join(&essid, password.as_deref())) {
                    (true, Ok(())) => self.joining(essid, now),
                    (_, r) => {
                        if let Err(e) = r {
                            log::warn!("net: join {essid:?}: {e}");
                        }
                        self.out.push(NetEvent::JoinFailed(essid));
                    }
                }
            }
            Cmd::Scan => {
                let r = if b.radio_on() { b.scan().map(|n| n.len()).map_err(|e| e.to_string()) } else { Err("Wi-Fi is off".into()) };
                self.out.push(NetEvent::Scanned(r));
            }
        }
        self.tick(now, b)
    }

    /// Does the probe or scan that is due at `now` (one step; call again while
    /// [`NetMachine::deadline`] has passed).
    pub fn tick(&mut self, now: Instant, b: &mut dyn NetBackend) -> Vec<NetEvent> {
        let due = |t: Option<Instant>| t.is_some_and(|t| t <= now);
        if self.awake && matches!(self.state, State::Searching | State::Joining { .. } | State::Connected { .. }) {
            if due(self.next_probe) {
                let p = b.probe();
                self.after_probe(p, now, b);
            } else if self.state == State::Searching && due(self.next_look) {
                if self.associated {
                    self.next_look = Some(now + SLOW_PROBE);
                } else {
                    self.look(now, b);
                }
            }
        }
        std::mem::take(&mut self.out)
    }

    fn publish(&mut self, s: NetStatus) {
        if self.status.as_ref() != Some(&s) {
            log::info!("net: status {s:?}");
            self.status = Some(s.clone());
            self.out.push(NetEvent::Status(s));
        }
    }

    fn set_was_on(&mut self, on: bool) {
        if self.was_on != on {
            self.was_on = on;
            self.out.push(NetEvent::WasOn(on));
        }
    }

    fn off(&mut self) {
        self.state = State::Off;
        (self.next_probe, self.next_look, self.dhcp) = (None, None, None);
        self.publish(NetStatus::Off);
    }

    /// The radio is on again: probe at once, scan soon, scans often again.
    fn start(&mut self, now: Instant) {
        self.state = State::Searching;
        self.started = Some(now);
        self.looked = false;
        self.policy.reset();
        self.next_probe = Some(now);
        self.next_look = Some(now + FIRST_LOOK);
        self.publish(NetStatus::Connecting { essid: None });
    }

    /// Turns the radio on when it is off and Wi-Fi was on, then starts; else off.
    /// `prepare`: a radio found on is prepared ([`NetBackend::prepare`]).
    fn bring_up(&mut self, now: Instant, b: &mut dyn NetBackend, prepare: bool) {
        let on = b.radio_on();
        if !on && !self.was_on {
            return self.off();
        }
        let r = if !on {
            b.set_radio(true)
        } else if prepare {
            b.prepare()
        } else {
            Ok(())
        };
        if let Err(e) = r {
            log::warn!("net: radio on: {e}");
            return self.off();
        }
        self.start(now);
    }

    fn joining(&mut self, essid: String, now: Instant) {
        self.publish(NetStatus::Connecting { essid: Some(essid.clone()) });
        self.state = State::Joining { essid, since: now };
        self.next_probe = Some(now + FAST_PROBE);
        self.next_look = None;
    }

    fn fail(&mut self, essid: String, now: Instant) {
        log::warn!("net: {essid:?} did not connect; skipped for {} min", AutoJoin::SKIP_FAILED.as_secs() / 60);
        self.policy.failed(&essid, now);
        self.out.push(NetEvent::JoinFailed(essid));
        self.state = State::Searching;
        self.looked = true;
        self.next_look = Some(now + self.policy.next_wait());
        self.next_probe = Some(now + SLOW_PROBE);
        self.publish(NetStatus::NoNetwork);
    }

    fn after_probe(&mut self, p: Probe, now: Instant, b: &mut dyn NetBackend) {
        self.associated = p.associated;
        if !p.radio {
            self.state = State::Down;
            (self.next_probe, self.next_look) = (None, None);
            return self.publish(NetStatus::Off);
        }
        if p.address {
            let essid = p.essid.unwrap_or_default();
            let new = !matches!(&self.state, State::Connected { essid: e } if *e == essid);
            self.state = State::Connected { essid: essid.clone() };
            self.next_probe = Some(now + CONNECTED_PROBE);
            self.next_look = None;
            self.policy.reset();
            self.set_was_on(true);
            self.publish(NetStatus::Connected { essid, signal: p.signal });
            if new {
                b.on_connected();
            }
            return;
        }
        if matches!(self.state, State::Connected { .. }) {
            log::info!("net: connection lost");
            self.start(now);
            self.next_probe = Some(now + FAST_PROBE);
        }
        if !p.associated {
            self.dhcp = None;
        } else if b.needs_dhcp() && self.dhcp.as_ref().is_none_or(|(e, at)| *e != p.essid || now >= *at + JOIN_FOR) {
            self.publish(NetStatus::Connecting { essid: p.essid.clone() });
            self.dhcp = Some((p.essid.clone(), now));
            if let Err(e) = b.obtain_ip() {
                log::warn!("net: DHCP: {e}");
            }
            if let State::Joining { since, .. } = &mut self.state {
                // DHCP gets its own time.
                *since = now;
            }
            self.next_probe = Some(now);
            return;
        }
        let fast = self.started.is_some_and(|s| now < s + FAST_FOR);
        match &self.state {
            State::Joining { essid, since } => {
                if now >= *since + JOIN_FOR {
                    let essid = essid.clone();
                    self.fail(essid, now);
                } else {
                    self.next_probe = Some(now + FAST_PROBE);
                }
            }
            _ => {
                let s = if p.associated || !self.looked { NetStatus::Connecting { essid: p.essid } } else { NetStatus::NoNetwork };
                self.publish(s);
                self.next_probe = Some(now + if fast { FAST_PROBE } else { SLOW_PROBE });
            }
        }
    }

    /// Scans and joins the strongest saved network found.
    fn look(&mut self, now: Instant, b: &mut dyn NetBackend) {
        let nets = match b.scan() {
            Ok(n) => n,
            Err(e) => {
                log::warn!("net: scan: {e}");
                self.next_look = Some(now + self.policy.next_wait());
                return;
            }
        };
        self.out.push(NetEvent::Scanned(Ok(nets.len())));
        match self.policy.pick(&nets, now).map(|n| n.essid.clone()) {
            None => {
                log::info!("net: no saved network among {} found", nets.len());
                self.looked = true;
                self.next_look = Some(now + self.policy.next_wait());
                self.publish(NetStatus::NoNetwork);
            }
            Some(essid) => {
                log::info!("net: joining saved network {essid:?}");
                match b.join(&essid, None) {
                    Ok(()) => self.joining(essid, now),
                    Err(e) => {
                        log::warn!("net: join {essid:?}: {e}");
                        self.fail(essid, now);
                    }
                }
            }
        }
    }
}

/// Events from the worker for the input loop, which delivers them as
/// [`crate::PowerEvent::Network`]. `wake` makes the input loop's poll return (a pipe).
pub struct NetQueue {
    events: Mutex<VecDeque<NetEvent>>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for NetQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetQueue").finish_non_exhaustive()
    }
}

impl NetQueue {
    /// Most events kept while nobody takes them (the oldest go first).
    pub const MAX: usize = 64;

    /// A queue that calls `wake` after each push.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(NetQueue { events: Mutex::new(VecDeque::new()), wake: Box::new(wake) })
    }

    /// Adds events (none: nothing happens).
    pub fn push(&self, evs: Vec<NetEvent>) {
        if evs.is_empty() {
            return;
        }
        {
            let mut q = self.events.lock().unwrap_or_else(|e| e.into_inner());
            q.extend(evs);
            while q.len() > Self::MAX {
                q.pop_front();
            }
        }
        (self.wake)();
    }

    /// Takes every waiting event.
    pub fn take(&self) -> Vec<NetEvent> {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }
}

/// The handle the services keep: sends commands and answers the last status.
#[derive(Clone, Debug)]
pub struct NetHandle {
    tx: mpsc::Sender<Cmd>,
    status: Arc<Mutex<Option<NetStatus>>>,
}

impl NetHandle {
    /// Sends a command (false when the worker is gone).
    pub fn send(&self, cmd: Cmd) -> bool {
        self.tx.send(cmd).is_ok()
    }

    /// The last status published (`None` before the first).
    pub fn status(&self) -> Option<NetStatus> {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Starts the worker thread over `backend`; its events go to `queue`.
pub fn spawn(mut backend: Box<dyn NetBackend + Send>, queue: Arc<NetQueue>) -> Option<NetHandle> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let status = Arc::new(Mutex::new(None));
    let shown = status.clone();
    let r = std::thread::Builder::new().name("net".into()).spawn(move || {
        let mut m = NetMachine::new();
        loop {
            let now = Instant::now();
            let got = match m.deadline() {
                Some(d) if d <= now => Err(mpsc::RecvTimeoutError::Timeout),
                Some(d) => rx.recv_timeout(d - now),
                None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };
            let evs = match got {
                Ok(cmd) => m.handle(cmd, Instant::now(), backend.as_mut()),
                Err(mpsc::RecvTimeoutError::Timeout) => m.tick(Instant::now(), backend.as_mut()),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            *shown.lock().unwrap_or_else(|e| e.into_inner()) = m.status().cloned();
            queue.push(evs);
        }
    });
    match r {
        Ok(_) => Some(NetHandle { tx, status }),
        Err(e) => {
            log::warn!("net: cannot start the network thread: {e}");
            None
        }
    }
}

/// The link checks: interface up (`/sys/class/net/IF/operstate`) with an address.
#[cfg(unix)]
pub mod link {
    use std::path::Path;

    /// The interface's operational state (`/sys/class/net/IF/operstate`, trimmed):
    /// `up`, `dormant` (Wi-Fi not authenticated yet), `down`, `unknown` (a driver
    /// that does not report it), ... `None` when the file cannot be read (no such
    /// interface, or a kernel without it). `sys` is the sysfs root (`/sys` on a device).
    pub fn operstate(sys: &Path, iface: &str) -> Option<String> {
        std::fs::read_to_string(sys.join("class/net").join(iface).join("operstate")).ok().map(|s| s.trim().to_string())
    }

    /// The interface's operational state is `up` (for Wi-Fi: associated and
    /// authenticated).
    pub fn operstate_up(sys: &Path, iface: &str) -> bool {
        operstate(sys, iface).as_deref() == Some("up")
    }

    /// The operational state `state` may come with an association: not when sysfs says
    /// plainly that there is none (`down`, `dormant`, ...). `unknown` and an unreadable
    /// file may (a driver that does not report the state): the caller then asks the
    /// platform. A probe skips its process calls when this is false.
    pub fn may_be_associated(state: Option<&str>) -> bool {
        !matches!(state, Some("down" | "dormant" | "lowerlayerdown" | "notpresent"))
    }

    /// An address that means a working connection: IPv4, or IPv6 outside the
    /// link-local range (an interface gets `fe80::` as soon as it is up, before DHCP).
    pub fn usable(addr: &std::net::IpAddr) -> bool {
        match addr {
            std::net::IpAddr::V4(a) => !a.is_unspecified() && !a.is_link_local(),
            std::net::IpAddr::V6(a) => !a.is_unspecified() && !a.is_loopback() && (a.segments()[0] & 0xffc0) != 0xfe80,
        }
    }

    /// The addresses of `iface` (`getifaddrs`).
    pub fn addresses(iface: &str) -> Vec<std::net::IpAddr> {
        let mut out = Vec::new();
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs fills `list`, freed below with freeifaddrs; every pointer
        // is checked before it is read and the strings are NUL-terminated by libc.
        unsafe {
            if libc::getifaddrs(&mut list) != 0 {
                return out;
            }
            let mut p = list;
            while !p.is_null() {
                let ifa = &*p;
                p = ifa.ifa_next;
                if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
                    continue;
                }
                if std::ffi::CStr::from_ptr(ifa.ifa_name).to_bytes() != iface.as_bytes() {
                    continue;
                }
                match (*ifa.ifa_addr).sa_family as i32 {
                    libc::AF_INET => {
                        let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                        out.push(std::net::IpAddr::V4(std::net::Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr))));
                    }
                    libc::AF_INET6 => {
                        let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                        out.push(std::net::IpAddr::V6(std::net::Ipv6Addr::from(sa.sin6_addr.s6_addr)));
                    }
                    _ => {}
                }
            }
            libc::freeifaddrs(list);
        }
        out
    }

    /// The interface is up and has a usable address: connected. An operational state
    /// of `unknown` or none readable counts as up (some Wi-Fi drivers never set it;
    /// the address then decides alone).
    pub fn has_address(sys: &Path, iface: &str) -> bool {
        matches!(operstate(sys, iface).as_deref(), Some("up" | "unknown") | None) && addresses(iface).iter().any(usable)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn operstate_and_addresses() {
            let d = std::env::temp_dir().join(format!("rinky-link-{}", std::process::id()));
            std::fs::create_dir_all(d.join("class/net/wlan0")).unwrap();
            std::fs::write(d.join("class/net/wlan0/operstate"), "dormant\n").unwrap();
            assert!(!operstate_up(&d, "wlan0"));
            std::fs::write(d.join("class/net/wlan0/operstate"), "up\n").unwrap();
            assert!(operstate_up(&d, "wlan0"));
            assert!(!operstate_up(&d, "eth9"));
            assert_eq!(operstate(&d, "wlan0").as_deref(), Some("up"));
            assert_eq!(operstate(&d, "eth9"), None);
            assert!(!may_be_associated(Some("dormant")) && !may_be_associated(Some("down")));
            assert!(may_be_associated(Some("up")) && may_be_associated(Some("unknown")) && may_be_associated(None));
            // A driver that reports "unknown", or no sysfs file: the address decides.
            let lo = if cfg!(target_os = "linux") { "lo" } else { "lo0" };
            std::fs::create_dir_all(d.join(format!("class/net/{lo}"))).unwrap();
            std::fs::write(d.join(format!("class/net/{lo}/operstate")), "down\n").unwrap();
            assert!(!has_address(&d, lo), "down: not connected whatever the addresses");
            // (127.0.0.1 stands in for a real address here.)
            std::fs::write(d.join(format!("class/net/{lo}/operstate")), "unknown\n").unwrap();
            assert!(has_address(&d, lo));
            std::fs::remove_file(d.join(format!("class/net/{lo}/operstate"))).unwrap();
            assert!(has_address(&d, lo));
            let _ = std::fs::remove_dir_all(&d);
            // The loopback interface has 127.0.0.1 on every unix host.
            let lo = if cfg!(target_os = "linux") { "lo" } else { "lo0" };
            assert!(addresses(lo).iter().any(|a| a.is_loopback()), "{:?}", addresses(lo));
            assert!(addresses("no-such-if0").is_empty());
        }

        #[test]
        fn link_local_addresses_do_not_count() {
            let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
            assert!(usable(&ip("192.168.1.20")));
            assert!(usable(&ip("2001:db8::5")));
            assert!(!usable(&ip("fe80::1")));
            assert!(!usable(&ip("169.254.3.4")));
            assert!(!usable(&ip("0.0.0.0")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceError;

    /// A backend whose link the test sets; records what the machine did.
    #[derive(Default)]
    struct Fake {
        radio: bool,
        probe: Probe,
        nets: Vec<WifiNetwork>,
        dhcp: bool,
        join_ok: bool,
        /// A join gives an address at once (the next probe sees it).
        join_connects: bool,
        calls: Vec<String>,
    }

    impl NetBackend for Fake {
        fn radio_on(&mut self) -> bool {
            self.radio
        }
        fn set_radio(&mut self, on: bool) -> Result<()> {
            self.calls.push(format!("radio {on}"));
            self.radio = on;
            self.probe.radio = on;
            Ok(())
        }
        fn prepare(&mut self) -> Result<()> {
            self.calls.push("prepare".into());
            Ok(())
        }
        fn probe(&mut self) -> Probe {
            self.calls.push("probe".into());
            Probe { radio: self.radio, ..self.probe.clone() }
        }
        fn scan(&mut self) -> Result<Vec<WifiNetwork>> {
            self.calls.push("scan".into());
            Ok(self.nets.clone())
        }
        fn join(&mut self, essid: &str, _password: Option<&str>) -> Result<()> {
            self.calls.push(format!("join {essid}"));
            if !self.join_ok {
                return Err(DeviceError::Backend("no".into()));
            }
            if self.join_connects {
                self.probe = connected(essid);
            }
            Ok(())
        }
        fn needs_dhcp(&self) -> bool {
            self.dhcp
        }
        fn obtain_ip(&mut self) -> Result<()> {
            self.calls.push("dhcp".into());
            Ok(())
        }
        fn on_connected(&mut self) {
            self.calls.push("connected".into());
        }
    }

    fn connected(essid: &str) -> Probe {
        Probe { radio: true, address: true, associated: true, essid: Some(essid.into()), signal: Some(4) }
    }

    fn net(essid: &str, known: bool) -> WifiNetwork {
        WifiNetwork { essid: essid.into(), signal: 4, secured: true, known }
    }

    fn status(e: &[NetEvent]) -> Vec<NetStatus> {
        e.iter().filter_map(|e| if let NetEvent::Status(s) = e { Some(s.clone()) } else { None }).collect()
    }

    /// Ticks at every deadline up to `until`; returns every event.
    fn run(m: &mut NetMachine, b: &mut Fake, from: Instant, until: Instant) -> Vec<NetEvent> {
        let mut out = Vec::new();
        let mut now = from;
        for _ in 0..10_000 {
            match m.deadline() {
                Some(d) if d <= until => {
                    now = now.max(d);
                    out.extend(m.tick(now, b));
                }
                _ => break,
            }
        }
        out
    }

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn boot_with_wifi_was_on_turns_the_radio_on_and_reports_the_connection() {
        let (mut m, mut b, t) = (NetMachine::new(), Fake::default(), Instant::now());
        let ev = m.handle(Cmd::Boot { restore: true }, t, &mut b);
        assert_eq!(b.calls[..2], ["radio true".to_string(), "probe".into()]);
        assert_eq!(status(&ev), [NetStatus::Connecting { essid: None }]);
        // The radio rejoins its network by itself a second later.
        b.probe = connected("Home");
        let ev = run(&mut m, &mut b, t, t + s(2));
        assert_eq!(status(&ev), [NetStatus::Connected { essid: "Home".into(), signal: Some(4) }]);
        assert!(ev.contains(&NetEvent::WasOn(true)) || m.was_on());
        assert!(b.calls.contains(&"connected".to_string()));
        // While connected: one cheap probe every 30 s, no scan.
        b.calls.clear();
        run(&mut m, &mut b, t + s(2), t + s(62));
        assert_eq!(b.calls, ["probe", "probe"]);
    }

    #[test]
    fn a_radio_on_at_start_up_is_prepared_not_switched_again() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, probe: connected("Home"), ..Fake::default() };
        let ev = m.handle(Cmd::Boot { restore: false }, t, &mut b);
        assert_eq!(b.calls[..2], ["prepare".to_string(), "probe".into()]);
        assert!(!b.calls.iter().any(|c| c.starts_with("radio")), "{:?}", b.calls);
        assert_eq!(status(&ev).last(), Some(&NetStatus::Connected { essid: "Home".into(), signal: Some(4) }));
    }

    #[test]
    fn boot_without_wifi_was_on_leaves_the_radio_off() {
        let (mut m, mut b, t) = (NetMachine::new(), Fake::default(), Instant::now());
        let ev = m.handle(Cmd::Boot { restore: false }, t, &mut b);
        assert_eq!(status(&ev), [NetStatus::Off]);
        assert!(b.calls.is_empty());
        assert_eq!(m.deadline(), None);
    }

    #[test]
    fn a_kobo_left_associated_without_an_address_gets_dhcp_at_start_up() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, dhcp: true, ..Fake::default() };
        b.probe = Probe { radio: true, associated: true, essid: Some("Home".into()), ..Probe::default() };
        let ev = m.handle(Cmd::Boot { restore: false }, t, &mut b);
        // The half-up radio is prepared (wpa_supplicant), then DHCP runs once.
        assert_eq!(b.calls, ["prepare", "probe", "dhcp"]);
        assert_eq!(status(&ev).last(), Some(&NetStatus::Connecting { essid: Some("Home".into()) }));
        b.probe.address = true;
        let ev = run(&mut m, &mut b, t, t + s(1));
        assert_eq!(status(&ev), [NetStatus::Connected { essid: "Home".into(), signal: None }]);
        assert_eq!(b.calls.iter().filter(|c| *c == "dhcp").count(), 1);
    }

    #[test]
    fn dhcp_runs_once_per_association_and_again_after_a_while() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, dhcp: true, ..Fake::default() };
        b.probe = Probe { radio: true, associated: true, essid: Some("Home".into()), ..Probe::default() };
        m.handle(Cmd::Boot { restore: true }, t, &mut b);
        run(&mut m, &mut b, t, t + s(20));
        assert_eq!(b.calls.iter().filter(|c| *c == "dhcp").count(), 1);
        run(&mut m, &mut b, t + s(20), t + s(41));
        assert_eq!(b.calls.iter().filter(|c| *c == "dhcp").count(), 2, "{:?}", b.calls);
        // Roamed to another network: DHCP for it at once (the old lease is stale).
        b.probe.essid = Some("Cafe".into());
        run(&mut m, &mut b, t + s(41), t + s(52));
        assert_eq!(b.calls.iter().filter(|c| *c == "dhcp").count(), 3);
    }

    /// wifid says PENDING with the radio on and nothing joined: the machine still
    /// scans and joins (0.5.2 waited for a wake instead).
    #[test]
    fn pending_link_events_do_not_stop_the_search() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, join_ok: true, join_connects: true, nets: vec![net("Cafe", false), net("Home", true)], ..Fake::default() };
        b.probe.radio = true;
        m.handle(Cmd::Boot { restore: true }, t, &mut b);
        m.handle(Cmd::Link("PENDING".into()), t + Duration::from_millis(100), &mut b);
        let ev = run(&mut m, &mut b, t, t + s(5));
        assert!(b.calls.contains(&"scan".to_string()));
        assert!(b.calls.contains(&"join Home".to_string()));
        assert_eq!(status(&ev).last(), Some(&NetStatus::Connected { essid: "Home".into(), signal: Some(4) }));
        assert!(ev.contains(&NetEvent::Scanned(Ok(2))));
    }

    #[test]
    fn the_first_probe_comes_at_once_and_the_first_scan_after_three_seconds() {
        let (mut m, mut b, t) = (NetMachine::new(), Fake::default(), Instant::now());
        m.handle(Cmd::Boot { restore: false }, t, &mut b);
        b.calls.clear();
        m.handle(Cmd::Enable, t, &mut b);
        assert_eq!(b.calls, ["radio true", "probe"]);
        run(&mut m, &mut b, t, t + FIRST_LOOK - Duration::from_millis(1));
        assert!(!b.calls.contains(&"scan".to_string()));
        run(&mut m, &mut b, t, t + FIRST_LOOK);
        assert!(b.calls.contains(&"scan".to_string()));
    }

    #[test]
    fn no_saved_network_is_reported_and_scans_back_off_without_link_events_resetting_them() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, nets: vec![net("Cafe", false)], ..Fake::default() };
        let ev = m.handle(Cmd::Boot { restore: true }, t, &mut b);
        assert_eq!(status(&ev), [NetStatus::Connecting { essid: None }]);
        let ev = run(&mut m, &mut b, t, t + s(4));
        assert_eq!(status(&ev), [NetStatus::NoNetwork]);
        // Link events (wifid's NA/READY churn) probe, but the scans stay 15, 30, 60 s apart.
        let mut now = t + s(4);
        let mut scans = Vec::new();
        while now < t + s(200) {
            now += s(5);
            m.handle(Cmd::Link("NA".into()), now, &mut b);
            b.calls.clear();
            run(&mut m, &mut b, now, now + s(5) - Duration::from_millis(1));
            if b.calls.contains(&"scan".to_string()) {
                scans.push(now);
            }
        }
        let gaps: Vec<u64> = scans.windows(2).map(|w| (w[1] - w[0]).as_secs()).collect();
        assert!(gaps.iter().all(|g| *g >= 15), "{gaps:?}");
        assert!(gaps.windows(2).any(|w| w[1] > w[0]), "backoff grows: {gaps:?}");
    }

    #[test]
    fn a_failed_join_is_reported_and_skipped() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, join_ok: true, nets: vec![net("Home", true), net("Office", true)], ..Fake::default() };
        m.handle(Cmd::Boot { restore: true }, t, &mut b);
        let ev = run(&mut m, &mut b, t, t + FIRST_LOOK + JOIN_FOR + s(1));
        assert!(ev.contains(&NetEvent::JoinFailed("Home".into())), "{ev:?}");
        assert_eq!(status(&ev).last(), Some(&NetStatus::NoNetwork));
        b.calls.clear();
        run(&mut m, &mut b, t + FIRST_LOOK + JOIN_FOR + s(1), t + FIRST_LOOK + JOIN_FOR + s(20));
        assert!(b.calls.contains(&"join Office".to_string()), "{:?}", b.calls);
        assert!(!b.calls.contains(&"join Home".to_string()));
    }

    #[test]
    fn a_lost_connection_starts_over() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, probe: connected("Home"), ..Fake::default() };
        m.handle(Cmd::Boot { restore: true }, t, &mut b);
        b.probe = Probe { radio: true, ..Probe::default() };
        let ev = m.handle(Cmd::Link("NA".into()), t + s(1), &mut b);
        assert_eq!(status(&ev), [NetStatus::Connecting { essid: None }]);
        b.calls.clear();
        run(&mut m, &mut b, t + s(1), t + s(5));
        assert!(b.calls.contains(&"scan".to_string()), "searching again: {:?}", b.calls);
    }

    #[test]
    fn user_off_clears_wifi_was_on_and_sleep_does_not() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, probe: connected("Home"), ..Fake::default() };
        m.handle(Cmd::Boot { restore: true }, t, &mut b);
        m.handle(Cmd::Awake(false), t + s(1), &mut b);
        assert_eq!(m.deadline(), None, "nothing runs while asleep");
        // A Kobo turns the radio off to suspend; the wake turns it on again.
        b.radio = false;
        b.calls.clear();
        let ev = m.handle(Cmd::Awake(true), t + s(100), &mut b);
        assert_eq!(b.calls[0], "radio true");
        assert!(m.was_on());
        assert!(!ev.contains(&NetEvent::WasOn(false)));
        let ev = m.handle(Cmd::Disable, t + s(101), &mut b);
        assert!(ev.contains(&NetEvent::WasOn(false)));
        assert_eq!(status(&ev), [NetStatus::Off]);
        b.calls.clear();
        m.handle(Cmd::Awake(false), t + s(102), &mut b);
        m.handle(Cmd::Awake(true), t + s(200), &mut b);
        assert!(b.calls.is_empty(), "off stays off: {:?}", b.calls);
    }

    #[test]
    fn a_wake_while_still_connected_shows_no_search() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, probe: connected("Home"), ..Fake::default() };
        m.handle(Cmd::Boot { restore: true }, t, &mut b);
        m.handle(Cmd::Awake(false), t + s(1), &mut b);
        let ev = m.handle(Cmd::Awake(true), t + s(500), &mut b);
        assert!(status(&ev).is_empty(), "{ev:?}");
        assert_eq!(b.calls.iter().filter(|c| *c == "connected").count(), 1, "one connection, not two");
    }

    #[test]
    fn a_radio_that_went_off_is_not_turned_on_by_a_probe() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, ..Fake::default() };
        m.handle(Cmd::Boot { restore: true }, t, &mut b);
        b.radio = false;
        b.calls.clear();
        let ev = run(&mut m, &mut b, t, t + s(60));
        assert_eq!(status(&ev), [NetStatus::Off]);
        assert!(!b.calls.iter().any(|c| c == "radio true"), "{:?}", b.calls);
        assert_eq!(m.deadline(), None);
    }

    #[test]
    fn user_connect_reports_failure_and_success() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, ..Fake::default() };
        m.handle(Cmd::Boot { restore: true }, t, &mut b);
        let ev = m.handle(Cmd::Connect { essid: "Home".into(), password: Some("secret12".into()) }, t, &mut b);
        assert!(ev.contains(&NetEvent::JoinFailed("Home".into())));
        b.join_ok = true;
        b.join_connects = true;
        let ev = m.handle(Cmd::Connect { essid: "Home".into(), password: None }, t + s(1), &mut b);
        assert_eq!(status(&ev).last(), Some(&NetStatus::Connecting { essid: Some("Home".into()) }));
        let ev = run(&mut m, &mut b, t + s(1), t + s(2));
        assert_eq!(status(&ev), [NetStatus::Connected { essid: "Home".into(), signal: Some(4) }]);
    }

    #[test]
    fn scan_command_reports_the_count_or_that_wifi_is_off() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { nets: vec![net("A", false), net("B", true)], ..Fake::default() };
        m.handle(Cmd::Boot { restore: false }, t, &mut b);
        assert!(m.handle(Cmd::Scan, t, &mut b).contains(&NetEvent::Scanned(Err("Wi-Fi is off".into()))));
        b.radio = true;
        assert!(m.handle(Cmd::Scan, t, &mut b).contains(&NetEvent::Scanned(Ok(2))));
    }

    #[test]
    fn identical_statuses_are_published_once() {
        let (mut m, t) = (NetMachine::new(), Instant::now());
        let mut b = Fake { radio: true, probe: connected("Home"), ..Fake::default() };
        let mut ev = m.handle(Cmd::Boot { restore: true }, t, &mut b);
        for i in 1..20 {
            ev.extend(m.handle(Cmd::Link("CONNECTED".into()), t + s(i), &mut b));
        }
        assert_eq!(status(&ev).iter().filter(|s| matches!(s, NetStatus::Connected { .. })).count(), 1);
    }

    #[test]
    fn status_and_state_agree() {
        let c = NetStatus::Connected { essid: "Home".into(), signal: Some(3) };
        assert_eq!(NetStatus::from_state(&c.to_state()), c);
        let joining = NetStatus::Connecting { essid: Some("Home".into()) };
        assert_eq!(joining.to_state().essid, None, "an ESSID only when connected");
        assert_eq!(NetStatus::from_state(&NetStatus::Off.to_state()), NetStatus::Off);
        assert_eq!(NetStatus::from_state(&NetStatus::NoNetwork.to_state()), NetStatus::NoNetwork);
        assert_eq!(c.connected_to(), Some("Home"));
        assert!(!NetStatus::Off.radio_on());
    }

    #[test]
    fn queue_keeps_the_newest_events_and_wakes() {
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = woken.clone();
        let q = NetQueue::new(move || {
            w.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        q.push(Vec::new());
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 0);
        q.push((0..100).map(|i| NetEvent::Scanned(Ok(i))).collect());
        let got = q.take();
        assert_eq!(got.len(), NetQueue::MAX);
        assert_eq!(got.last(), Some(&NetEvent::Scanned(Ok(99))));
        assert!(q.take().is_empty());
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn worker_thread_publishes_through_the_queue() {
        let q = NetQueue::new(|| {});
        let b = Fake { radio: true, probe: connected("Home"), ..Fake::default() };
        let h = spawn(Box::new(b), q.clone()).unwrap();
        assert!(h.send(Cmd::Boot { restore: true }));
        let mut got = Vec::new();
        for _ in 0..500 {
            got.extend(q.take());
            if got.iter().any(|e| matches!(e, NetEvent::Status(NetStatus::Connected { .. }))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(got.contains(&NetEvent::Status(NetStatus::Connected { essid: "Home".into(), signal: Some(4) })), "{got:?}");
        assert_eq!(h.status(), Some(NetStatus::Connected { essid: "Home".into(), signal: Some(4) }));
    }
}
