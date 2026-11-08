//! KOReader progress sync (KOSync), compatible with KOReader's `kosync.koplugin` and
//! koreader-sync-server.
//!
//! - Settings page "Progress sync (KOSync)": server, user, password (only its MD5 is
//!   stored), log in, register, automatic sync on/off, log out.
//! - Book open: pull the server's position (`GET /syncs/progress/<partial MD5>`); when
//!   another device is further on (KOReader's rules, [`ink_net::kosync::decide`]) a
//!   dialog asks "Jump to the synced position?" and "Jump" moves the reader
//!   ([`UiHost::go_to`]). When this device is newer, its position is pushed.
//! - Page turns only record the position (no I/O on a page turn). A push starts on a
//!   page turn when the last push is at least [`PUSH_EVERY`] seconds old and the
//!   position changed, and always on close and before sleep (a debounce without timers:
//!   the trailing position goes out on the next turn, close or sleep).
//! - Progress is the crengine xpointer for reflowable books and the 1-based page number
//!   for PDFs (KOReader's format); the document id is the partial MD5 the host computes.
//! - All requests run on the plugin network worker ([`HostCtx::start_request`]); the UI
//!   loop and the watchdog heartbeat keep going. Failed pushes go to a
//!   [`SyncQueue`] in `data/queue.json` and are sent on the next open or wake. A push on
//!   close is queued before it starts, so a push cut off by the app exiting is not lost.

use ink_net::kosync::{self, decide, KoSyncAccount, KoSyncClient, Progress, SyncDecision, SyncQueue};
use ink_net::{HttpClient, NetError, Request, Response};
use ink_plugin::serde_json::{self, json, Value};
use ink_plugin::kit::{Answer, Pending, Requests};
use ink_plugin::{HostCtx, HttpEvent, Permission, Plugin, PluginError, PluginManifest, Registrar, Result, StreamMode};
use std::cell::RefCell;

/// Plugin name.
pub const NAME: &str = "kosync";
/// Minimum seconds between pushes made on page turns.
pub const PUSH_EVERY: u64 = 30;
/// Default device name sent to the server.
pub const DEFAULT_DEVICE: &str = "rinky";

const MENU: u32 = 1;
const IN_SERVER: u32 = 2;
const IN_USER: u32 = 3;
const IN_PASS: u32 = 4;
const JUMP: u32 = 5;

/// KOSync progress string for a position string: xpointers as they are, `page:N` as the
/// 1-based page number; `None` for positions KOReader cannot read (`flow:`).
pub fn to_koreader(pos: &str) -> Option<String> {
    if pos.starts_with('/') {
        return Some(pos.to_string());
    }
    let rest = pos.strip_prefix("page:")?;
    let n: u64 = rest.split(';').next()?.parse().ok()?;
    Some((n + 1).to_string())
}

/// Position string for a KOSync progress: xpointers as they are, a page number
/// (1-based) as `page:N` (0-based).
pub fn from_koreader(progress: &str) -> Option<String> {
    let p = progress.trim();
    if p.starts_with('/') {
        return Some(p.to_string());
    }
    let n: u64 = p.parse().ok()?;
    Some(format!("page:{}", n.saturating_sub(1)))
}

/// An [`HttpClient`] that keeps the request and sends nothing: builds requests exactly as
/// [`KoSyncClient`] does, for the worker thread.
#[derive(Default)]
struct Capture(RefCell<Option<Request>>);

impl HttpClient for Capture {
    fn send(&self, req: Request) -> ink_net::Result<Response> {
        *self.0.borrow_mut() = Some(req);
        Err(NetError::Http("captured".into()))
    }
}

/// The request `call` makes on a [`KoSyncClient`] for `account` (its answer is ignored).
fn build<R>(account: &KoSyncAccount, call: impl FnOnce(&KoSyncClient<'_>) -> R) -> Result<Request> {
    let cap = Capture::default();
    call(&KoSyncClient::new(&cap, account.clone()));
    cap.0.into_inner().ok_or_else(|| failed("no request"))
}

/// Checks an answer like [`KoSyncClient`] does: expected status, no HTML page (captive
/// portal), the message of an error body.
pub fn check_answer(status: u16, mime: &str, body: &[u8], ok: &[u16]) -> std::result::Result<Value, NetError> {
    let text = String::from_utf8_lossy(body);
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if ok.contains(&status) {
        if mime == "text/html" || text.trim_start().starts_with('<') {
            return Err(NetError::Format(format!("status {status} with an HTML page, not a sync server answer (captive portal?)")));
        }
        return Ok(v);
    }
    let message = v.get("message").and_then(|m| m.as_str()).map(str::to_string).unwrap_or_else(|| text.trim().chars().take(200).collect());
    Err(if status == 401 { NetError::Unauthorized(message) } else { NetError::Server { status, message } })
}

#[derive(Clone, Debug)]
enum Call {
    Auth,
    Register,
    Pull { doc: String, manual: bool },
    Push { progress: Progress, manual: bool },
    Drain { progress: Progress },
}

/// The open book as the plugin sees it.
#[derive(Clone, Debug, Default)]
struct Book {
    id: String,
    /// KOSync progress string ("" when the position cannot be synced).
    progress: String,
    percentage: f64,
    /// Time of the last local page turn (seconds).
    changed_at: u64,
    /// Last progress the server has from us (pushed or found in sync).
    synced: Option<String>,
    /// The pull on open answered (pushes wait for it, so they cannot overwrite a newer
    /// remote position before the user decided).
    pulled: bool,
}

/// The KOSync plugin.
pub struct KoSyncPlugin {
    manifest: PluginManifest,
    clock: Box<dyn Fn() -> u64>,
    inflight: Requests<Call>,
    book: Option<Book>,
    last_push_at: u64,
    /// Remote position offered in the jump dialog.
    offer: Option<String>,
    draining: bool,
}

impl Default for KoSyncPlugin {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn failed(e: impl std::fmt::Display) -> PluginError {
    PluginError::Failed(e.to_string())
}

impl KoSyncPlugin {
    /// The plugin with the system clock.
    pub fn new() -> Self {
        KoSyncPlugin {
            manifest: PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Document, Permission::Ui, Permission::Storage, Permission::Network]),
            clock: Box::new(now_secs),
            inflight: Requests::default(),
            book: None,
            last_push_at: 0,
            offer: None,
            draining: false,
        }
    }

    /// Uses `clock` (seconds) instead of the system clock (tests).
    pub fn with_clock(mut self, clock: impl Fn() -> u64 + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    /// A text setting; `None` when unset or empty.
    fn setting(cx: &mut HostCtx, key: &str) -> Option<String> {
        cx.setting_str(key).filter(|s| !s.is_empty())
    }

    fn auto_sync(cx: &mut HostCtx) -> bool {
        cx.setting_bool("auto_sync", true)
    }

    /// The account, when user and password are set.
    fn account(cx: &mut HostCtx) -> Option<KoSyncAccount> {
        let user = Self::setting(cx, "user")?;
        let userkey = Self::setting(cx, "userkey")?;
        let server = Self::setting(cx, "server").unwrap_or_else(|| kosync::DEFAULT_SERVER.to_string());
        Some(KoSyncAccount { server: server.trim().to_string(), user, userkey })
    }

    fn device(cx: &mut HostCtx) -> Result<(String, String)> {
        let name = Self::setting(cx, "device_name").unwrap_or_else(|| DEFAULT_DEVICE.to_string());
        let id = match Self::setting(cx, "device_id") {
            Some(id) => id,
            None => {
                let id = kosync::new_device_id();
                cx.storage()?.set("device_id", Value::String(id.clone()))?;
                id
            }
        };
        Ok((name, id))
    }

    fn start(&mut self, cx: &mut HostCtx, req: Request, call: Call) -> Result<()> {
        log::info!("kosync: {} {} ({call:?})", req.method.as_str(), req.url);
        self.inflight.start(cx, req, StreamMode::Whole { limit: 1 << 20 }, call)?;
        Ok(())
    }

    fn queue_path(cx: &mut HostCtx) -> Result<std::path::PathBuf> {
        let p = cx.data_file("queue.json")?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(failed)?;
        }
        Ok(p)
    }

    fn with_queue(cx: &mut HostCtx, f: impl FnOnce(&mut SyncQueue)) -> Result<usize> {
        let p = Self::queue_path(cx)?;
        let mut q = SyncQueue::load(&p);
        f(&mut q);
        q.save(&p).map_err(failed)?;
        Ok(q.items.len())
    }

    /// Reads the reader's position into the open book (the change time moves when it
    /// changed). False when the position cannot be read.
    fn note_position(&mut self, cx: &mut HostCtx) -> bool {
        let Some((progress, pct)) = Self::read_position(cx) else { return false };
        let now = self.now();
        if let Some(b) = self.book.as_mut().filter(|b| b.progress != progress) {
            b.progress = progress;
            b.percentage = pct;
            b.changed_at = now;
        }
        true
    }

    /// Reads the open book's position from the document.
    fn read_position(cx: &mut HostCtx) -> Option<(String, f64)> {
        let doc = cx.document().ok()?;
        let pos = doc.current_position().ok()?.to_string();
        let progress = to_koreader(&pos).unwrap_or_default();
        let page = doc.current_page().unwrap_or(0);
        let count = doc.page_count().unwrap_or(0).max(1);
        // KOReader: paging documents current/total (1-based), rolling ones a fraction from 0.
        let pct = if pos.starts_with("page:") { (page + 1) as f64 / count as f64 } else { page as f64 / count as f64 };
        Some((progress, pct.clamp(0.0, 1.0)))
    }

    fn progress_for(&self, cx: &mut HostCtx) -> Result<Option<Progress>> {
        let Some(b) = self.book.as_ref() else { return Ok(None) };
        if b.progress.is_empty() || b.id.is_empty() {
            return Ok(None);
        }
        let (device, device_id) = Self::device(cx)?;
        Ok(Some(Progress { document: b.id.clone(), progress: b.progress.clone(), percentage: b.percentage, device, device_id, timestamp: None, metadata: None }))
    }

    /// Pushes the open book's position. `queue_first` puts it in the queue before the
    /// request starts (on close: the app may exit before the answer).
    fn push_now(&mut self, cx: &mut HostCtx, manual: bool, queue_first: bool) -> Result<()> {
        let Some(account) = Self::account(cx) else { return Ok(()) };
        let Some(p) = self.progress_for(cx)? else { return Ok(()) };
        let now = self.now();
        self.last_push_at = now;
        // `changed_at` reaches the sidecar on close (a sidecar write on a page turn
        // would cost a file sync).
        if queue_first {
            Self::with_queue(cx, |q| q.push(p.clone(), now))?;
        }
        let req = build(&account, |c| c.push(&p))?;
        self.start(cx, req, Call::Push { progress: p, manual })
    }

    fn pull_now(&mut self, cx: &mut HostCtx, manual: bool) -> Result<()> {
        let Some(account) = Self::account(cx) else {
            if manual {
                cx.toast("Progress sync: log in first (Settings, Plugins, Progress sync)")?;
            }
            return Ok(());
        };
        let Some(doc) = self.book.as_ref().map(|b| b.id.clone()).filter(|d| !d.is_empty()) else { return Ok(()) };
        let req = build(&account, |c| c.pull(&doc))?;
        self.start(cx, req, Call::Pull { doc, manual })
    }

    /// Sends the first queued push (the rest follow one by one as each succeeds).
    fn drain_next(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.draining {
            return Ok(());
        }
        let Some(account) = Self::account(cx) else { return Ok(()) };
        let q = SyncQueue::load(&Self::queue_path(cx)?);
        // Positions of the open book still waiting go out with the next push instead.
        let open = self.book.as_ref().map(|b| b.id.clone()).unwrap_or_default();
        let Some(first) = q.items.iter().find(|i| i.progress.document != open).map(|i| i.progress.clone()) else { return Ok(()) };
        let req = build(&account, |c| c.push(&first))?;
        // Marked only once the request runs: a send that could not start is tried again.
        self.start(cx, req, Call::Drain { progress: first })?;
        self.draining = true;
        Ok(())
    }

    /// Drops every queued push of `doc`: called after a push or pull of it succeeded,
    /// so all of them are older than what the server has now (sending one later would
    /// move the server back).
    fn drop_queued(cx: &mut HostCtx, doc: &str) -> Result<()> {
        let path = Self::queue_path(cx)?;
        let mut q = SyncQueue::load(&path);
        let before = q.items.len();
        q.items.retain(|i| i.progress.document != doc);
        if q.items.len() != before {
            q.save(&path).map_err(failed)?;
        }
        Ok(())
    }

    fn show_menu(&mut self, cx: &mut HostCtx) -> Result<()> {
        let server = Self::setting(cx, "server").unwrap_or_else(|| kosync::DEFAULT_SERVER.to_string());
        let user = Self::setting(cx, "user");
        let has_key = Self::setting(cx, "userkey").is_some();
        let auto = Self::auto_sync(cx);
        let items = [
            // "label\tvalue": the host shows the value right-aligned in muted type.
            (format!("Server\t{server}"), "server"),
            (format!("User\t{}", user.as_deref().unwrap_or("not set")), "user"),
            (format!("Password\t{}", if has_key { "set" } else { "not set" }), "password"),
            ("Log in\tcheck the account".to_string(), "login"),
            ("Register a new account".to_string(), "register"),
            (format!("Automatic sync\t{}", if auto { "on" } else { "off" }), "auto"),
            ("Log out".to_string(), "logout"),
        ];
        cx.menu(MENU, "Progress sync (KOSync)", items)
    }

    fn menu_pick(&mut self, cx: &mut HostCtx, pick: &str) -> Result<()> {
        match pick {
            "server" => {
                let cur = Self::setting(cx, "server").unwrap_or_else(|| kosync::DEFAULT_SERVER.to_string());
                cx.input(IN_SERVER, "Sync server URL", &cur)?;
            }
            "user" => {
                let cur = Self::setting(cx, "user").unwrap_or_default();
                cx.input(IN_USER, "User name", &cur)?;
            }
            "password" => cx.input(IN_PASS, "Password (only its MD5 is stored)", "")?,
            "login" | "register" => {
                let Some(account) = Self::account(cx) else {
                    cx.toast("Set the user name and the password first")?;
                    return self.show_menu(cx);
                };
                let (req, call, toast) = if pick == "login" {
                    (build(&account, |c| c.authorize())?, Call::Auth, "Logging in...")
                } else {
                    (build(&account, |c| c.register())?, Call::Register, "Registering...")
                };
                self.start(cx, req, call)?;
                cx.toast(toast)?;
            }
            "auto" => {
                let on = !Self::auto_sync(cx);
                cx.storage()?.set("auto_sync", json!(on))?;
                self.show_menu(cx)?;
            }
            "logout" => {
                cx.storage()?.set("userkey", Value::Null)?;
                cx.toast("Logged out")?;
                self.show_menu(cx)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `answer` is the body, or the transport error message.
    fn finish(&mut self, cx: &mut HostCtx, f: Pending<Call>, answer: Answer) -> Result<()> {
        let mime = f.mime();
        let check = |ok: &[u16]| match &answer {
            Ok(b) => check_answer(f.status, &mime, b, ok),
            Err(m) if m.contains("timed out") => Err(NetError::Timeout),
            Err(m) => Err(NetError::Http(m.clone())),
        };
        match f.job {
            Call::Auth => match check(&[200]) {
                Ok(v) if matches!(v.get("authorized").and_then(|a| a.as_str()), Some("OK") | None) => {
                    let user = Self::setting(cx, "user").unwrap_or_default();
                    cx.toast(&format!("Logged in as {user}"))?;
                    self.drain_next(cx)?;
                }
                Ok(v) => cx.toast(&format!("Login refused: {v}"))?,
                Err(NetError::Unauthorized(_)) => cx.toast("Login failed: wrong user name or password")?,
                Err(e) => cx.toast(&format!("Login failed: {e}"))?,
            },
            Call::Register => match check(&[201]) {
                Ok(_) => cx.toast("Account registered; you are logged in")?,
                Err(NetError::Server { status: 402, .. }) => cx.toast("That user name is taken")?,
                Err(e) => cx.toast(&format!("Registration failed: {e}"))?,
            },
            Call::Push { progress, manual } => match check(&[200, 202]) {
                Ok(_) => {
                    log::info!("kosync: pushed {} {:.4}", progress.progress, progress.percentage);
                    Self::drop_queued(cx, &progress.document)?;
                    if let Some(b) = self.book.as_mut().filter(|b| b.id == progress.document) {
                        b.synced = Some(progress.progress.clone());
                    }
                    if manual {
                        cx.toast("Progress sent to the sync server")?;
                    }
                    self.drain_next(cx)?;
                }
                Err(e) => {
                    log::warn!("kosync: push failed: {e}");
                    if kosync::should_queue(&e) {
                        let now = self.now();
                        Self::with_queue(cx, |q| q.push(progress, now))?;
                    } else {
                        Self::drop_queued(cx, &progress.document)?;
                    }
                    if manual || matches!(e, NetError::Unauthorized(_)) {
                        cx.toast(&format!("Progress sync: {e}"))?;
                    }
                }
            },
            Call::Drain { progress } => {
                self.draining = false;
                match check(&[200, 202]) {
                    Ok(_) => {
                        Self::drop_queued(cx, &progress.document)?;
                        self.drain_next(cx)?;
                    }
                    Err(NetError::Unauthorized(_)) => {
                        Self::with_queue(cx, |q| q.items.clear())?;
                    }
                    Err(e) => log::info!("kosync: queue stays: {e}"),
                }
            }
            Call::Pull { doc, manual } => {
                let Some(book) = self.book.as_mut().filter(|b| b.id == doc) else { return Ok(()) };
                book.pulled = true;
                let remote = match check(&[200]) {
                    Ok(v) => {
                        // The server answered: older queued positions of this book are
                        // replaced by the decision below.
                        Self::drop_queued(cx, &doc)?;
                        Progress::from_pull(&v, &doc)
                    }
                    Err(e) => {
                        log::warn!("kosync: pull failed: {e}");
                        if manual || matches!(e, NetError::Unauthorized(_)) {
                            cx.toast(&format!("Progress sync: {e}"))?;
                        }
                        return Ok(());
                    }
                };
                let (progress, pct, changed_at) = (book.progress.clone(), book.percentage, book.changed_at);
                let (device, device_id) = Self::device(cx)?;
                let d = decide(remote.as_ref(), &progress, pct, changed_at, &device, &device_id);
                log::info!("kosync: pull {doc}: {d:?} (remote {:?})", remote.as_ref().map(|r| (&r.progress, r.percentage, &r.device)));
                match d {
                    SyncDecision::RemoteNewer => {
                        let r = remote.expect("remote");
                        match from_koreader(&r.progress) {
                            Some(pos) => {
                                self.offer = Some(pos);
                                let body = format!(
                                    "{} is at {:.0}% (this device: {:.0}%).\n\nJump to the synced position?",
                                    if r.device.is_empty() { "Another device" } else { r.device.as_str() },
                                    r.percentage * 100.0,
                                    pct * 100.0
                                );
                                cx.dialog(JUMP, "Progress sync", &body, &[("No", "no"), ("Jump", "jump")])?;
                            }
                            None => log::warn!("kosync: cannot read remote progress {:?}", r.progress),
                        }
                    }
                    SyncDecision::LocalNewer | SyncDecision::NoRemote => {
                        if Self::auto_sync(cx) || manual {
                            self.push_now(cx, manual, false)?;
                        }
                    }
                    SyncDecision::InSync | SyncDecision::SameDevice => {
                        if let Some(b) = self.book.as_mut() {
                            if d == SyncDecision::InSync {
                                b.synced = Some(b.progress.clone());
                            }
                        }
                        if manual {
                            cx.toast("Progress is in sync")?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Plugin for KoSyncPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.settings_page("kosync", "Progress sync (KOSync)");
        // The same name as the control-center tile; it also fits a reader-menu pill.
        reg.menu_item("sync_now", "Sync progress", 60);
        // The control-center tile (Settings, Control center buttons); a tap runs sync_now.
        reg.control_tile("sync_now", "Sync progress", "sync").state("Reading progress").needs_wifi("Turn on Wi-Fi to sync").working("Syncing reading progress…");
        Ok(())
    }

    fn on_book_open(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.offer = None;
        let id = cx.document()?.book_id().unwrap_or_default();
        let (progress, percentage) = Self::read_position(cx).unwrap_or_default();
        let changed_at = cx.storage()?.book_get("changed_at").and_then(|v| v.as_u64()).unwrap_or(0);
        self.book = Some(Book { id, progress, percentage, changed_at, synced: None, pulled: false });
        if Self::account(cx).is_none() || !Self::auto_sync(cx) {
            if let Some(b) = self.book.as_mut() {
                b.pulled = true;
            }
            return Ok(());
        }
        self.pull_now(cx, false)?;
        self.drain_next(cx)
    }

    fn on_page_change(&mut self, cx: &mut HostCtx, _page: usize) -> Result<()> {
        if !self.note_position(cx) {
            return Ok(());
        }
        let now = self.now();
        let Some(b) = self.book.as_ref() else { return Ok(()) };
        let due = b.pulled && self.offer.is_none() && b.synced.as_deref() != Some(b.progress.as_str()) && now.saturating_sub(self.last_push_at) >= PUSH_EVERY;
        if due && Self::auto_sync(cx) && self.inflight.jobs().all(|c| !matches!(c, Call::Push { .. })) {
            self.push_now(cx, false, false)?;
        }
        Ok(())
    }

    fn on_suspend(&mut self, cx: &mut HostCtx) -> Result<()> {
        // While the jump question is open the server's position is the newer one.
        if self.offer.is_none() && self.book.as_ref().is_some_and(|b| b.pulled && b.synced.as_deref() != Some(b.progress.as_str())) && Self::auto_sync(cx) {
            self.push_now(cx, false, true)?;
        }
        Ok(())
    }

    fn on_resume(&mut self, cx: &mut HostCtx) -> Result<()> {
        if Self::auto_sync(cx) {
            self.drain_next(cx)?;
        }
        Ok(())
    }

    fn on_close(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.note_position(cx);
        // Closed while the jump question was open: the user did not choose, so the
        // newer remote position stays on the server.
        let due = self.offer.is_none() && self.book.as_ref().is_some_and(|b| b.synced.as_deref() != Some(b.progress.as_str()));
        if due && Self::auto_sync(cx) {
            self.push_now(cx, false, true)?;
        }
        if let Some(b) = self.book.as_ref() {
            cx.storage()?.book_set("changed_at", json!(b.changed_at))?;
        }
        self.book = None;
        self.offer = None;
        Ok(())
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        match id {
            "settings:kosync" | "kosync" => self.show_menu(cx),
            "sync_now" => {
                if self.book.is_none() {
                    return Ok(());
                }
                self.note_position(cx);
                self.pull_now(cx, true)
            }
            _ => Ok(()),
        }
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        match id {
            MENU => self.menu_pick(cx, result),
            IN_SERVER | IN_USER if !result.trim().is_empty() => {
                let key = if id == IN_SERVER { "server" } else { "user" };
                cx.storage()?.set(key, Value::String(result.trim().to_string()))?;
                self.show_menu(cx)
            }
            IN_PASS if !result.is_empty() => {
                cx.storage()?.set("userkey", Value::String(kosync::userkey(result)))?;
                self.show_menu(cx)
            }
            IN_SERVER | IN_USER | IN_PASS => self.show_menu(cx),
            JUMP => {
                if let Some(pos) = self.offer.take() {
                    if result == "jump" {
                        log::info!("kosync: jump to {pos}");
                        cx.ui()?.go_to(&pos);
                        if let Some(b) = self.book.as_mut() {
                            // The reader shows the remote position next; it is what the
                            // server has.
                            b.synced = to_koreader(&pos);
                        }
                    } else if Self::auto_sync(cx) {
                        // Keep this device's position: it becomes the newest.
                        let now = self.now();
                        if let Some(b) = self.book.as_mut() {
                            b.changed_at = now;
                        }
                        self.push_now(cx, false, false)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn on_http(&mut self, cx: &mut HostCtx, id: u32, ev: &HttpEvent) -> Result<()> {
        match self.inflight.event(id, ev) {
            Some((f, answer)) => self.finish(cx, f, answer),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
