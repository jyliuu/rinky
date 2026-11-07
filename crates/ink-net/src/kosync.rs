//! KOReader sync (KOSync) client, compatible with KOReader's `kosync.koplugin` and
//! servers such as koreader-sync-server (`https://sync.koreader.rocks`).
//!
//! Protocol (KOReader `plugins/kosync.koplugin/api.json` and `KOSyncClient.lua`), JSON
//! over HTTPS, every request with `Accept: application/vnd.koreader.v1+json`:
//!
//! | Call | Request | Success |
//! | --- | --- | --- |
//! | register | `POST /users/create` `{username, password}` | 201 (402 = name taken) |
//! | authorize | `GET /users/auth` | 200 `{"authorized":"OK"}` (401 = refused) |
//! | push | `PUT /syncs/progress` `{document, progress, percentage, device, device_id[, metadata]}` | 200 `{document, timestamp}` |
//! | pull | `GET /syncs/progress/<document>` | 200 with the stored fields, or `{}` |
//!
//! `password` and the `x-auth-key` header are the lowercase MD5 hex of the password
//! (KOReader never sends the password itself); `x-auth-user` is the user name.
//! `document` is the book's partial MD5 (`ink_store::partial_md5`), `progress` a crengine
//! xpointer for reflowable books or the page number as a string for PDFs, `percentage`
//! is rounded down to 4 decimals (`Math.roundPercent`), `device_id` is 32 uppercase hex
//! digits (`random.uuid()` without dashes).

use crate::http::{HttpClient, Method, Request};
use crate::url::{encode_component, join_path};
use crate::{NetError, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The `Accept` header value.
pub const ACCEPT: &str = "application/vnd.koreader.v1+json";
/// KOReader's default server.
pub const DEFAULT_SERVER: &str = "https://sync.koreader.rocks";

/// Lowercase MD5 hex of `password`: the `x-auth-key` value and the registered password.
pub fn userkey(password: &str) -> String {
    use md5::Digest;
    crate::hex(&md5::Md5::digest(password.as_bytes()))
}

/// A new device id like KOReader's (`random.uuid()` without dashes): 32 uppercase hex
/// digits with the version 4 and variant bits set.
pub fn new_device_id() -> String {
    let mut b = [0u8; 16];
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).is_err() {
        // No randomness source: derive from the clock and the process id.
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let seed = t ^ ((std::process::id() as u128) << 64);
        b.copy_from_slice(&seed.to_le_bytes());
    }
    b[6] = (b[6] & 0x4f) | 0x40;
    b[8] = (b[8] & 0xbf) | 0x80;
    crate::hex(&b).to_ascii_uppercase()
}

/// `floor(p * 10000) / 10000`, as KOReader's `Math.roundPercent`.
pub fn round_percentage(p: f64) -> f64 {
    (p * 10000.0).floor() / 10000.0
}

/// Account on a KOSync server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KoSyncAccount {
    /// Server base URL, e.g. `https://sync.koreader.rocks` (a path prefix is allowed).
    pub server: String,
    /// User name.
    pub user: String,
    /// MD5 hex of the password (`x-auth-key`).
    pub userkey: String,
}

impl KoSyncAccount {
    /// An account from a clear-text password (only its MD5 is kept).
    pub fn new(server: &str, user: &str, password: &str) -> Self {
        KoSyncAccount { server: server.trim().to_string(), user: user.trim().to_string(), userkey: userkey(password) }
    }
}

/// Optional document metadata (KOReader's "Send document metadata" setting).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocMetadata {
    /// File name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Authors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authors: Option<String>,
}

/// Progress as the server stores it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// Book id (partial MD5).
    pub document: String,
    /// xpointer or page.
    pub progress: String,
    /// 0.0..=1.0.
    pub percentage: f64,
    /// Device name.
    pub device: String,
    /// Device id.
    pub device_id: String,
    /// Server timestamp (seconds), on pulls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    /// Document metadata, sent only when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<DocMetadata>,
}

impl Progress {
    /// The JSON body of a push: `percentage` rounded down to 4 decimals, no timestamp.
    pub fn push_body(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "document": self.document,
            "progress": self.progress,
            "percentage": round_percentage(self.percentage),
            "device": self.device,
            "device_id": self.device_id,
        });
        if let Some(m) = &self.metadata {
            v["metadata"] = serde_json::to_value(m).unwrap_or_default();
        }
        v
    }

    /// Reads a pull answer leniently: `{}` (or no `percentage`) is `None`; numbers given as
    /// strings are accepted.
    pub fn from_pull(v: &serde_json::Value, document: &str) -> Option<Progress> {
        let num = |k: &str| match v.get(k)? {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        };
        let text = |k: &str| match v.get(k) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        let percentage = num("percentage")?;
        Some(Progress {
            document: v.get("document").and_then(|d| d.as_str()).unwrap_or(document).to_string(),
            progress: text("progress"),
            percentage,
            device: text("device"),
            device_id: text("device_id"),
            timestamp: num("timestamp").map(|t| t as u64),
            metadata: None,
        })
    }
}

/// What to do after a pull (KOReader's `KOSync:getProgress` rules).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncDecision {
    /// The server has nothing for this book.
    NoRemote,
    /// The remote progress came from this device.
    SameDevice,
    /// Same position (equal percentage or equal progress string).
    InSync,
    /// The remote position is newer: offer to jump forward/backward.
    RemoteNewer,
    /// The local position is newer: push it.
    LocalNewer,
}

/// Compares a pulled progress with the local one. `local_changed_at` is the time of the
/// last local page turn (seconds); with a server timestamp the newer one wins, without
/// one the larger percentage wins (old servers), as in KOReader.
pub fn decide(remote: Option<&Progress>, local_progress: &str, local_percentage: f64, local_changed_at: u64, device: &str, device_id: &str) -> SyncDecision {
    let Some(r) = remote else { return SyncDecision::NoRemote };
    if r.device == device && r.device_id == device_id {
        return SyncDecision::SameDevice;
    }
    let lp = round_percentage(local_percentage);
    if round_percentage(r.percentage) == lp || r.progress == local_progress {
        return SyncDecision::InSync;
    }
    let remote_newer = match r.timestamp {
        Some(ts) => ts > local_changed_at,
        None => r.percentage > lp,
    };
    if remote_newer {
        SyncDecision::RemoteNewer
    } else {
        SyncDecision::LocalNewer
    }
}

/// KOSync client.
pub struct KoSyncClient<'a> {
    /// HTTP transport.
    pub http: &'a dyn HttpClient,
    /// Account.
    pub account: KoSyncAccount,
    /// Head timeout for register/authorize (KOReader: 5 s connect, 10 s total).
    pub auth_timeout: Duration,
    /// Head timeout for push/pull (KOReader: 2 s / 5 s; the Kindle's Wi-Fi needs a bit more).
    pub progress_timeout: Duration,
}

impl std::fmt::Debug for KoSyncClient<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KoSyncClient").field("server", &self.account.server).field("user", &self.account.user).finish_non_exhaustive()
    }
}

impl<'a> KoSyncClient<'a> {
    /// A client with the default timeouts.
    pub fn new(http: &'a dyn HttpClient, account: KoSyncAccount) -> Self {
        KoSyncClient { http, account, auth_timeout: Duration::from_secs(10), progress_timeout: Duration::from_secs(5) }
    }

    fn request(&self, method: Method, path: &str, timeout: Duration, auth: bool) -> Request {
        let mut r = Request::new(method, join_path(&self.account.server, path)).timeout(timeout).read_timeout(timeout * 2).header("Accept", ACCEPT);
        if auth {
            r = r.header("x-auth-user", &self.account.user).header("x-auth-key", &self.account.userkey);
        }
        r
    }

    fn answer(&self, req: Request, ok: &[u16]) -> Result<serde_json::Value> {
        let resp = self.http.send(req)?;
        let status = resp.status;
        let mime = resp.mime();
        let text = resp.text(1 << 20)?;
        if ok.contains(&status) {
            // A hotel or train Wi-Fi login page answers 200 to everything. A sync server
            // never sends HTML; counting it as success would drop queued pushes.
            if mime == "text/html" || text.trim_start().starts_with('<') {
                return Err(NetError::Format(format!("status {status} with an HTML page, not a sync server answer (captive portal?)")));
            }
            return Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null));
        }
        Err(crate::http::status_error(status, &text))
    }

    /// Creates the account (`POST /users/create`). A taken name is
    /// `NetError::Server { status: 402, .. }`.
    pub fn register(&self) -> Result<()> {
        let body = serde_json::json!({ "username": self.account.user, "password": self.account.userkey });
        let req = self.request(Method::Post, "/users/create", self.auth_timeout, false).json_body(&body);
        self.answer(req, &[201]).map(|_| ())
    }

    /// Checks the credentials (`GET /users/auth`).
    pub fn authorize(&self) -> Result<()> {
        let v = self.answer(self.request(Method::Get, "/users/auth", self.auth_timeout, true), &[200])?;
        match v.get("authorized").and_then(|a| a.as_str()) {
            Some("OK") | None => Ok(()),
            Some(other) => Err(NetError::Unauthorized(other.to_string())),
        }
    }

    /// Pushes progress (`PUT /syncs/progress`); returns the server timestamp if given.
    pub fn push(&self, progress: &Progress) -> Result<Option<u64>> {
        let req = self.request(Method::Put, "/syncs/progress", self.progress_timeout, true).json_body(&progress.push_body());
        let v = self.answer(req, &[200, 202])?;
        Ok(v.get("timestamp").and_then(|t| t.as_u64()))
    }

    /// Pulls the last progress for `document` (`GET /syncs/progress/<document>`);
    /// `None` when the server has none.
    pub fn pull(&self, document: &str) -> Result<Option<Progress>> {
        let path = format!("/syncs/progress/{}", encode_component(document));
        let v = self.answer(self.request(Method::Get, &path, self.progress_timeout, true), &[200])?;
        Ok(Progress::from_pull(&v, document))
    }
}

/// A progress push that failed and waits for the network (KOReader's `KOSyncQueue`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueuedProgress {
    /// What to push.
    pub progress: Progress,
    /// When it was queued (seconds).
    pub queued_at: u64,
}

/// Failed pushes kept for later: one entry per document per day, at most 4 weeks old,
/// at most 200 entries. Auth failures (401) are not queued.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncQueue {
    /// Oldest first.
    pub items: Vec<QueuedProgress>,
}

/// Queue entries older than this are dropped.
pub const QUEUE_MAX_AGE_S: u64 = 28 * 24 * 3600;
/// At most this many queued pushes.
pub const QUEUE_MAX: usize = 200;

impl SyncQueue {
    /// Loads `path`; a missing or broken file is an empty queue.
    pub fn load(path: &std::path::Path) -> SyncQueue {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Saves to `path` (write to a temporary file, then rename).
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(|e| NetError::Format(e.to_string()))?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Queues a push made at `now`.
    pub fn push(&mut self, progress: Progress, now: u64) {
        let day = now / 86400;
        self.items.retain(|q| now.saturating_sub(q.queued_at) < QUEUE_MAX_AGE_S && !(q.progress.document == progress.document && q.queued_at / 86400 == day));
        self.items.push(QueuedProgress { progress, queued_at: now });
        let excess = self.items.len().saturating_sub(QUEUE_MAX);
        self.items.drain(..excess);
    }

    /// Sends queued pushes in order until one fails; returns how many were sent. A 401
    /// drops the rest (the account changed), other errors keep them.
    pub fn drain(&mut self, client: &KoSyncClient<'_>) -> usize {
        let mut sent = 0;
        while let Some(first) = self.items.first() {
            match client.push(&first.progress) {
                Ok(_) => {
                    self.items.remove(0);
                    sent += 1;
                }
                Err(NetError::Unauthorized(_)) => {
                    self.items.clear();
                    break;
                }
                Err(e) => {
                    log::info!("kosync queue: {} left after error: {e}", self.items.len());
                    break;
                }
            }
        }
        sent
    }
}

/// True for errors worth queueing (network, timeouts, 5xx), false for refusals.
pub fn should_queue(e: &NetError) -> bool {
    match e {
        NetError::Unauthorized(_) => false,
        NetError::Server { status, .. } => *status >= 500,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{TestServer, KOSYNC_ACCEPT};
    use crate::UreqClient;

    fn progress(doc: &str, pct: f64) -> Progress {
        Progress {
            document: doc.into(),
            progress: "/body/DocFragment[3]/body/p[2]/text().42".into(),
            percentage: pct,
            device: "rinky".into(),
            device_id: "0123456789ABCDEF0123456789ABCDEF".into(),
            timestamp: None,
            metadata: None,
        }
    }

    #[test]
    fn userkey_is_lowercase_md5() {
        assert_eq!(userkey(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(userkey("password"), "5f4dcc3b5aa765d61d8327deb882cf99");
    }

    #[test]
    fn device_ids_look_like_koreaders() {
        let a = new_device_id();
        let b = new_device_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c)), "{a}");
        assert_eq!(&a[12..13], "4", "version nibble");
        assert!(matches!(&a[16..17], "8" | "9" | "A" | "B"), "variant nibble {a}");
    }

    #[test]
    fn percentage_rounding_and_json_shape() {
        assert_eq!(round_percentage(0.123456), 0.1234);
        assert_eq!(round_percentage(0.99999), 0.9999);
        assert_eq!(round_percentage(1.0), 1.0);
        let mut p = progress("cf7ce9b8d4e135c8ac340f18d1e58c85", 0.256789);
        let v = p.push_body();
        assert_eq!(v["percentage"], 0.2567);
        assert_eq!(v["progress"], "/body/DocFragment[3]/body/p[2]/text().42");
        assert!(v.get("timestamp").is_none());
        assert!(v.get("metadata").is_none());
        p.metadata = Some(DocMetadata { filename: Some("b.epub".into()), title: Some("B".into()), authors: None });
        let v = p.push_body();
        assert_eq!(v["metadata"], serde_json::json!({"filename": "b.epub", "title": "B"}));
        assert_eq!(Progress::from_pull(&serde_json::json!({}), "d"), None);
        let got = Progress::from_pull(&serde_json::json!({"percentage": "0.5", "progress": 69, "device": "kpw", "device_id": "X", "timestamp": 1700000000}), "d").unwrap();
        assert_eq!((got.document.as_str(), got.progress.as_str(), got.percentage, got.timestamp), ("d", "69", 0.5, Some(1700000000)));
    }

    #[test]
    fn decisions_follow_koreader() {
        let mut r = progress("d", 0.5);
        r.device = "kpw".into();
        r.device_id = "OTHER".into();
        assert_eq!(decide(None, "x", 0.1, 0, "rinky", "ME"), SyncDecision::NoRemote);
        assert_eq!(decide(Some(&r), "x", 0.1, 0, "kpw", "OTHER"), SyncDecision::SameDevice);
        assert_eq!(decide(Some(&r), "x", 0.50004, 0, "rinky", "ME"), SyncDecision::InSync);
        assert_eq!(decide(Some(&r), &r.progress, 0.1, 0, "rinky", "ME"), SyncDecision::InSync);
        assert_eq!(decide(Some(&r), "x", 0.1, 0, "rinky", "ME"), SyncDecision::RemoteNewer);
        assert_eq!(decide(Some(&r), "x", 0.9, 0, "rinky", "ME"), SyncDecision::LocalNewer);
        r.timestamp = Some(100);
        assert_eq!(decide(Some(&r), "x", 0.9, 50, "rinky", "ME"), SyncDecision::RemoteNewer);
        assert_eq!(decide(Some(&r), "x", 0.1, 150, "rinky", "ME"), SyncDecision::LocalNewer);
    }

    #[test]
    fn full_protocol_against_the_mock_server() {
        let s = TestServer::start().unwrap();
        let http = UreqClient::new();
        // A path prefix with a trailing slash, as users type it.
        let account = KoSyncAccount::new(&format!("{}/kosync/", s.base()), "reader", "s3cret");
        assert_eq!(account.userkey, userkey("s3cret"));
        let c = KoSyncClient::new(&http, account.clone());
        assert!(matches!(c.authorize(), Err(NetError::Unauthorized(m)) if m == "Unauthorized"));
        c.register().unwrap();
        assert!(matches!(c.register(), Err(NetError::Server { status: 402, message }) if message.contains("already registered")));
        c.authorize().unwrap();
        let doc = "cf7ce9b8d4e135c8ac340f18d1e58c85";
        assert_eq!(c.pull(doc).unwrap(), None);
        let p = progress(doc, 0.4321987);
        let ts = c.push(&p).unwrap();
        assert!(ts.is_some());
        let got = c.pull(doc).unwrap().unwrap();
        assert_eq!(got.document, doc);
        assert_eq!(got.progress, p.progress);
        assert_eq!(got.percentage, 0.4321);
        assert_eq!(got.device, "rinky");
        assert_eq!(got.device_id, p.device_id);
        assert_eq!(got.timestamp, ts);

        // Wire format as KOReader sends it.
        let reqs = s.state.requests();
        let create = reqs.iter().find(|r| r.path == "/kosync/users/create").unwrap();
        assert_eq!(create.method, "POST");
        assert_eq!(create.header("accept"), Some(KOSYNC_ACCEPT));
        assert_eq!(create.header("x-auth-key"), None, "register sends no auth headers");
        let body: serde_json::Value = serde_json::from_slice(&create.body).unwrap();
        assert_eq!(body, serde_json::json!({"username": "reader", "password": userkey("s3cret")}));
        let put = reqs.iter().find(|r| r.method == "PUT").unwrap();
        assert_eq!(put.path, "/kosync/syncs/progress");
        assert_eq!(put.header("x-auth-user"), Some("reader"));
        assert_eq!(put.header("x-auth-key"), Some(userkey("s3cret").as_str()));
        assert!(put.header("content-type").unwrap().starts_with("application/json"));
        let body: serde_json::Value = serde_json::from_slice(&put.body).unwrap();
        let keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["device", "device_id", "document", "percentage", "progress"]);
        assert!(reqs.iter().any(|r| r.path == format!("/kosync/syncs/progress/{doc}") && r.method == "GET"));
        assert!(reqs.iter().all(|r| !r.path.contains("//")));

        // Wrong password: 401 on every authenticated call.
        let bad = KoSyncClient::new(&http, KoSyncAccount::new(&s.url("/kosync"), "reader", "wrong"));
        assert!(matches!(bad.push(&p), Err(NetError::Unauthorized(_))));
        assert!(matches!(bad.pull(doc), Err(NetError::Unauthorized(_))));
        // The server root works too (no prefix).
        let root = KoSyncClient::new(&http, KoSyncAccount::new(&s.base(), "reader", "s3cret"));
        root.authorize().unwrap();
    }

    /// Answers every request with a captive-portal login page.
    struct Portal;

    impl HttpClient for Portal {
        fn send(&self, _req: Request) -> Result<crate::Response> {
            let page = b"<!DOCTYPE html><html><body><form action=/login>Log in to Hotel Wi-Fi</form></body></html>".to_vec();
            Ok(crate::Response::new(200, vec![("Content-Type".into(), "text/html; charset=utf-8".into())], std::io::Cursor::new(page), "http://portal/"))
        }
    }

    #[test]
    fn captive_portal_pages_are_not_success() {
        let c = KoSyncClient::new(&Portal, KoSyncAccount::new("http://sync", "u", "p"));
        assert!(matches!(c.authorize(), Err(NetError::Format(m)) if m.contains("captive portal")));
        assert!(matches!(c.pull("doc"), Err(NetError::Format(_))));
        let p = Progress { document: "d".into(), progress: "/p".into(), percentage: 0.1, device: "i".into(), device_id: "X".into(), timestamp: None, metadata: None };
        assert!(matches!(c.push(&p), Err(NetError::Format(_))));
        let mut q = SyncQueue::default();
        q.push(p, 0);
        assert_eq!(q.drain(&c), 0);
        assert_eq!(q.items.len(), 1, "the queued push stays until a real server takes it");
    }

    #[test]
    fn queue_dedupes_expires_and_drains() {
        let s = TestServer::start().unwrap();
        s.state.add_user("q", &userkey("pw"));
        let http = UreqClient::new();
        let mut q = SyncQueue::default();
        let day = 86400;
        q.push(progress("a", 0.1), 10 * day);
        q.push(progress("a", 0.2), 10 * day + 5);
        assert_eq!(q.items.len(), 1, "same document, same day");
        assert_eq!(q.items[0].progress.percentage, 0.2);
        q.push(progress("a", 0.3), 11 * day);
        q.push(progress("b", 0.3), 11 * day);
        assert_eq!(q.items.len(), 3);
        q.push(progress("c", 0.3), 10 * day + 5 + QUEUE_MAX_AGE_S);
        assert_eq!(q.items.iter().filter(|i| i.queued_at == 10 * day + 5).count(), 0, "four weeks old entries expire");
        for i in 0..250 {
            q.push(progress(&format!("doc{i}"), 0.5), 40 * day);
        }
        assert_eq!(q.items.len(), QUEUE_MAX);

        let path = std::env::temp_dir().join(format!("ink-net-kosync-queue-{}.json", std::process::id()));
        q.save(&path).unwrap();
        let mut q2 = SyncQueue::load(&path);
        assert_eq!(q2, q);
        let _ = std::fs::remove_file(&path);
        q2.items.truncate(3);
        let good = KoSyncClient::new(&http, KoSyncAccount::new(&s.base(), "q", "pw"));
        assert_eq!(q2.drain(&good), 3);
        assert!(q2.items.is_empty());
        q2.push(progress("z", 0.1), 0);
        let bad = KoSyncClient::new(&http, KoSyncAccount::new(&s.base(), "q", "nope"));
        assert_eq!(q2.drain(&bad), 0);
        assert!(q2.items.is_empty(), "401 drops the queue");
        q2.push(progress("z", 0.1), 0);
        let down = KoSyncClient::new(&http, KoSyncAccount::new("http://127.0.0.1:1", "q", "pw"));
        assert_eq!(q2.drain(&down), 0);
        assert_eq!(q2.items.len(), 1, "network errors keep the queue");
        assert!(should_queue(&NetError::Timeout));
        assert!(!should_queue(&NetError::Unauthorized(String::new())));
        assert!(!should_queue(&NetError::Server { status: 403, message: String::new() }));
        assert!(should_queue(&NetError::Server { status: 503, message: String::new() }));
    }
}
