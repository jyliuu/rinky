//! One connection to calibre, run on a worker thread: answers calibre's requests until
//! calibre ejects the device, the connection breaks, the user disconnects or nothing
//! arrives for [`Config::idle_timeout`].
//!
//! The stream must have a short read timeout (about 1 s): reads that time out are how the
//! session sees the cancel flag and the idle limit.

use crate::cache::Cache;
use crate::protocol::{self, op, Frame, FrameError};
use ink_plugin::serde_json::{self, json, Value};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Formats offered to calibre, preferred first (all open in rinky).
pub const EXTENSIONS: &[&str] = &["epub", "azw3", "mobi", "pdf", "fb2", "cbz", "azw", "prc", "txt", "html", "htm"];

/// Space kept free beyond a book's size (KOReader: 128 KiB).
pub const SPACE_MARGIN: u64 = 128 * 1024;

/// What the session needs (copied from the plugin settings on the UI thread).
#[derive(Clone, Debug)]
pub struct Config {
    /// Where books go (`lpath` is relative to it).
    pub books_dir: PathBuf,
    /// calibre's wireless password, empty for none.
    pub password: String,
    /// Name shown in calibre.
    pub device_name: String,
    /// Version reported to calibre.
    pub app_version: String,
    /// The book list ([`Cache`]).
    pub cache_file: PathBuf,
    /// calibre's drive info (JSON object from `SET_CALIBRE_DEVICE_INFO`).
    pub drive_file: PathBuf,
    /// Disconnect after this long without a message from calibre.
    pub idle_timeout: Duration,
}

/// Progress reported to the UI thread.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// calibre accepted the device (password, if any, was right).
    Connected {
        /// calibre's version, `7.20.0`.
        calibre_version: String,
    },
    /// Name of calibre's current library.
    Library(String),
    /// A book started (`this` is 1-based).
    Receiving {
        /// Number in the batch, from 1.
        this: usize,
        /// Books in the batch.
        total: usize,
        /// Title.
        title: String,
    },
    /// A book is in the books folder.
    Received {
        /// Number in the batch, from 1.
        this: usize,
        /// Books in the batch.
        total: usize,
        /// Title.
        title: String,
        /// The file.
        path: PathBuf,
    },
    /// A book could not be stored.
    ReceiveFailed {
        /// Title or lpath.
        title: String,
        /// Why.
        why: String,
    },
    /// The last book of a batch was handled.
    BatchDone {
        /// Books in the batch.
        total: usize,
    },
    /// calibre deleted books (titles or lpaths).
    Deleted(Vec<String>),
    /// A message from calibre.
    Message(String),
}

/// Why a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// calibre ejected the device.
    Ejected,
    /// The user disconnected.
    Cancelled,
    /// Nothing arrived for the idle timeout.
    Idle,
    /// calibre closed the connection.
    Closed,
    /// calibre refused the password.
    WrongPassword,
    /// Anything else (I/O error, broken stream).
    Failed(String),
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            End::Ejected => write!(f, "calibre ejected the device"),
            End::Cancelled => write!(f, "disconnected"),
            End::Idle => write!(f, "no activity from calibre"),
            End::Closed => write!(f, "calibre closed the connection"),
            End::WrongPassword => write!(f, "wrong password"),
            End::Failed(m) => write!(f, "{m}"),
        }
    }
}

/// `(free, total)` bytes of the file system holding `dir` (or its nearest existing
/// parent).
pub fn disk_space(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let mut d = dir;
    while !d.exists() {
        d = d.parent()?;
    }
    let c = std::ffi::CString::new(d.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` is a valid NUL-terminated path; `st` is plain data filled by statvfs.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // field types differ between Linux and macOS
    let (frsize, avail, blocks) = (st.f_frsize as u64, st.f_bavail as u64, st.f_blocks as u64);
    Some((avail.saturating_mul(frsize), blocks.saturating_mul(frsize)))
}

/// Runs a session on `stream` until it ends. `emit` gets progress; `cancelled` is
/// checked at least once per read timeout.
pub fn run<S: Read + Write>(stream: S, cfg: &Config, emit: &dyn Fn(Event), cancelled: &dyn Fn() -> bool) -> End {
    let mut cache = Cache::load(&cfg.cache_file);
    let pruned = cache.prune(&cfg.books_dir);
    if pruned > 0 {
        log::info!("calibre: dropped {pruned} book(s) from the list whose file is gone");
        if let Err(e) = cache.save() {
            log::warn!("calibre: cannot save {}: {e}", cache.path.display());
        }
    }
    log::info!("calibre: book list {} ({} books)", cfg.cache_file.display(), cache.books.len());
    let mut s = Session { stream, buf: Vec::new(), cfg, cache, emit, cancelled, last_seen: Instant::now(), version: Vec::new() };
    let end = s.serve();
    if end == End::Cancelled {
        // Parting NOOP, as KOReader does (calibre then drops the device at once).
        let _ = s.send(op::NOOP, &json!({}));
    }
    log::info!("calibre: session ended: {end}");
    end
}

struct Session<'a, S> {
    stream: S,
    buf: Vec<u8>,
    cfg: &'a Config,
    cache: Cache,
    emit: &'a dyn Fn(Event),
    cancelled: &'a dyn Fn() -> bool,
    last_seen: Instant,
    version: Vec<i64>,
}

fn s<'v>(v: &'v Value, k: &str) -> &'v str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

impl<S: Read + Write> Session<'_, S> {
    fn serve(&mut self) -> End {
        loop {
            match protocol::decode(&self.buf) {
                Ok(Some((frame, used))) => {
                    self.buf.drain(..used);
                    if let Err(end) = self.handle(frame) {
                        return end;
                    }
                }
                Ok(None) => {
                    if let Err(end) = self.fill() {
                        return end;
                    }
                }
                Err(FrameError::BadJson { len, why }) => {
                    log::warn!("calibre: skipped a frame of {len} bytes: {why}");
                    self.buf.drain(..len);
                }
                Err(e) => return End::Failed(format!("calibre sent a {e}")),
            }
        }
    }

    /// Reads more bytes into the buffer.
    fn fill(&mut self) -> Result<(), End> {
        let mut tmp = [0u8; 16 * 1024]; // the worker thread has a 256 KiB stack
        loop {
            if (self.cancelled)() {
                return Err(End::Cancelled);
            }
            match self.stream.read(&mut tmp) {
                Ok(0) => return Err(End::Closed),
                Ok(n) => {
                    self.buf.extend_from_slice(&tmp[..n]);
                    self.last_seen = Instant::now();
                    return Ok(());
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    if self.last_seen.elapsed() >= self.cfg.idle_timeout {
                        return Err(End::Idle);
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(End::Failed(format!("reading from calibre: {e}"))),
            }
        }
    }

    fn send(&mut self, opcode: i64, data: &Value) -> Result<(), End> {
        log::debug!("calibre: -> {} {}", op::name(opcode), data);
        self.stream.write_all(&protocol::encode(opcode, data)).and_then(|_| self.stream.flush()).map_err(|e| End::Failed(format!("writing to calibre: {e}")))
    }

    fn at_least(&self, v: [i64; 3]) -> bool {
        let n = |a: &[i64]| a.first().copied().unwrap_or(0) * 100_000 + a.get(1).copied().unwrap_or(0) * 1000 + a.get(2).copied().unwrap_or(0);
        n(&self.version) >= n(&v)
    }

    fn handle(&mut self, f: Frame) -> Result<(), End> {
        log::debug!("calibre: <- {} {}", op::name(f.op), short(&f.arg));
        let a = &f.arg;
        match f.op {
            op::GET_INITIALIZATION_INFO => {
                self.version = a.get("calibre_version").and_then(Value::as_array).map(|v| v.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
                let lengths: serde_json::Map<String, Value> = EXTENSIONS.iter().map(|e| (e.to_string(), json!(37))).collect();
                let info = json!({
                    "appName": "rinky",
                    "acceptedExtensions": EXTENSIONS,
                    "cacheUsesLpaths": true,
                    "canAcceptLibraryInfo": true,
                    "canDeleteMultipleBooks": true,
                    "canReceiveBookBinary": true,
                    "canSendOkToSendbook": true,
                    "canStreamBooks": true,
                    "canStreamMetadata": true,
                    "canUseCachedMetadata": true,
                    "ccVersionNumber": self.cfg.app_version,
                    "coverHeight": 240,
                    "deviceKind": "Kindle",
                    "deviceName": self.cfg.device_name,
                    "extensionPathLengths": lengths,
                    "passwordHash": protocol::password_hash(&self.cfg.password, s(a, "passwordChallenge")),
                    "maxBookContentPacketLen": 4096,
                    "useUuidFileNames": false,
                    "versionOK": true,
                });
                log::info!("calibre: handshake with calibre {:?}, library {:?}", self.version, s(a, "currentLibraryName"));
                self.send(op::OK, &info)
            }
            op::GET_DEVICE_INFORMATION => {
                let drive = self.drive_info();
                let mut di = json!({"device_name": self.cfg.device_name});
                if let Some(u) = drive.get("device_store_uuid").filter(|u| u.is_string()) {
                    di["device_store_uuid"] = u.clone();
                }
                let reply = json!({"device_info": di, "version": self.cfg.app_version, "device_version": self.cfg.app_version});
                self.send(op::OK, &reply)?;
                let v = self.version.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(".");
                (self.emit)(Event::Connected { calibre_version: v });
                Ok(())
            }
            op::SET_CALIBRE_DEVICE_INFO => {
                let mut drive = a.clone();
                // Keep the first driver name (KOReader: tells which calibre driver set it up).
                if let Some(n) = self.drive_info().get("device_name").cloned() {
                    drive["device_name"] = n;
                }
                if let Err(e) = write_json(&self.cfg.drive_file, &drive) {
                    log::warn!("calibre: cannot save {}: {e}", self.cfg.drive_file.display());
                }
                self.send(op::OK, &json!({}))
            }
            op::SET_LIBRARY_INFO => {
                (self.emit)(Event::Library(s(a, "libraryName").to_string()));
                self.send(op::OK, &json!({}))
            }
            op::TOTAL_SPACE => {
                let total = disk_space(&self.cfg.books_dir).map(|d| d.1).unwrap_or(1 << 30);
                self.send(op::OK, &json!({"total_space_on_device": total}))
            }
            op::FREE_SPACE => {
                let free = disk_space(&self.cfg.books_dir).map(|d| d.0).unwrap_or(1 << 30);
                self.send(op::OK, &json!({"free_space_on_device": free}))
            }
            op::GET_BOOK_COUNT => {
                let n = self.cache.books.len();
                self.send(op::OK, &json!({"willStream": true, "willScan": true, "count": n}))?;
                for i in 0..n {
                    let b = &self.cache.books[i];
                    let id = json!({"priKey": i + 1, "uuid": b["uuid"], "lpath": b["lpath"], "last_modified": b["last_modified"]});
                    self.send(op::OK, &id)?;
                }
                Ok(())
            }
            op::NOOP => {
                if a.get("ejecting").and_then(Value::as_bool).unwrap_or(false) {
                    self.send(op::OK, &json!({}))?;
                    return Err(End::Ejected);
                }
                if a.get("count").is_some() {
                    // Announces how many metadata requests follow; nothing to answer.
                    return Ok(());
                }
                if let Some(k) = a.get("priKey").and_then(Value::as_u64) {
                    let book = (k as usize).checked_sub(1).and_then(|i| self.cache.books.get(i)).cloned().unwrap_or_else(|| json!({}));
                    return self.send(op::OK, &book);
                }
                self.send(op::OK, &json!({}))
            }
            op::SEND_BOOKLISTS => Ok(()),
            op::SEND_BOOK_METADATA => {
                if let Some(d) = a.get("data") {
                    self.cache.update(d);
                }
                let (i, n) = (a.get("index").and_then(Value::as_u64).unwrap_or(0), a.get("count").and_then(Value::as_u64).unwrap_or(0));
                if i + 1 >= n {
                    self.save_cache();
                }
                Ok(())
            }
            op::SEND_BOOK => self.receive_book(a),
            op::DELETE_BOOK => self.delete_books(a),
            op::GET_BOOK_FILE_SEGMENT => self.send_book(s(a, "lpath")),
            op::DISPLAY_MESSAGE => {
                if a.get("messageKind").and_then(Value::as_i64) == Some(1) {
                    log::warn!("calibre: wrong password ({})", s(a, "message"));
                    return Err(End::WrongPassword);
                }
                (self.emit)(Event::Message(s(a, "message").to_string()));
                Ok(())
            }
            op::ERROR => {
                log::warn!("calibre: error from calibre: {}", a);
                (self.emit)(Event::Message(s(a, "message").to_string()));
                Ok(())
            }
            other => {
                log::info!("calibre: ignored opcode {other} ({})", op::name(other));
                Ok(())
            }
        }
    }

    fn drive_info(&self) -> Value {
        std::fs::read(&self.cfg.drive_file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}))
    }

    fn save_cache(&self) {
        if let Err(e) = self.cache.save() {
            log::warn!("calibre: cannot save {}: {e}", self.cache.path.display());
        }
    }

    /// SEND_BOOK: answer, then `length` raw bytes into `<lpath>.part`, renamed when all
    /// arrived.
    fn receive_book(&mut self, a: &Value) -> Result<(), End> {
        let lpath = s(a, "lpath").to_string();
        let length = a.get("length").and_then(Value::as_u64).unwrap_or(0);
        let this = a.get("thisBook").and_then(Value::as_u64).unwrap_or(0) as usize + 1;
        let total = (a.get("totalBooks").and_then(Value::as_u64).unwrap_or(1) as usize).max(this);
        let meta = a.get("metadata").cloned().unwrap_or_else(|| json!({}));
        let title = Some(s(&meta, "title")).filter(|t| !t.is_empty()).unwrap_or(&lpath).to_string();
        let wants_ok = a.get("wantsSendOkToSendbook").and_then(Value::as_bool).unwrap_or(false);
        let target = self.cfg.books_dir.join(&lpath);
        let problem = if !protocol::safe_lpath(&lpath) {
            Some(format!("refused path {lpath:?}"))
        } else {
            match disk_space(&self.cfg.books_dir) {
                Some((free, _)) if free < length.saturating_add(SPACE_MARGIN) => Some(format!("no space left ({} KB free, book {} KB)", free / 1024, length / 1024)),
                _ => None,
            }
        };
        let part = PathBuf::from(format!("{}.part", target.display()));
        let mut file = None;
        let mut why = problem;
        if why.is_none() {
            let opened = target.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::File::create(&part));
            match opened {
                Ok(f) => file = Some(f),
                Err(e) => why = Some(format!("cannot write {}: {e}", part.display())),
            }
        }
        if let Some(w) = &why {
            log::warn!("calibre: book {this}/{total} {lpath:?}: {w}");
            if wants_ok && self.at_least([4, 18, 0]) {
                // calibre then sends no bytes and shows the message.
                self.send(op::ERROR, &json!({"message": format!("Cannot receive {title}: {w}")}))?;
                (self.emit)(Event::ReceiveFailed { title, why: w.clone() });
                if this == total {
                    (self.emit)(Event::BatchDone { total });
                }
                return Ok(());
            }
        } else {
            log::info!("calibre: receiving {this}/{total} {lpath:?} ({length} bytes)");
            (self.emit)(Event::Receiving { this, total, title: title.clone() });
        }
        if wants_ok {
            self.send(op::OK, &json!({}))?;
        }
        // Read exactly `length` bytes (also when they are thrown away, to stay in step).
        let mut left = length;
        while left > 0 {
            if self.buf.is_empty() {
                if let Err(end) = self.fill() {
                    drop(file.take());
                    let _ = std::fs::remove_file(&part);
                    log::warn!("calibre: {lpath:?} broken off with {left} of {length} bytes missing: {end}");
                    return Err(end);
                }
            }
            let take = (left.min(self.buf.len() as u64)) as usize;
            if let Some(f) = file.as_mut() {
                if let Err(e) = f.write_all(&self.buf[..take]) {
                    why = Some(format!("cannot write {}: {e}", part.display()));
                    file = None;
                    let _ = std::fs::remove_file(&part);
                }
            }
            self.buf.drain(..take);
            left -= take as u64;
        }
        if let Some(f) = file.take() {
            let done = f.sync_all().and_then(|_| {
                drop(f);
                std::fs::rename(&part, &target)
            });
            match done {
                Ok(()) => {
                    let mut m = meta;
                    if m.is_object() {
                        m["lpath"] = json!(lpath);
                        if m.get("size").is_none_or(Value::is_null) {
                            m["size"] = json!(length);
                        }
                        self.cache.add(&m);
                        self.save_cache();
                    }
                    log::info!("calibre: received {}", target.display());
                    (self.emit)(Event::Received { this, total, title: title.clone(), path: target });
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    why = Some(format!("cannot store {}: {e}", target.display()));
                }
            }
        }
        if let Some(w) = why {
            (self.emit)(Event::ReceiveFailed { title, why: w });
        }
        if this == total {
            (self.emit)(Event::BatchDone { total });
        }
        Ok(())
    }

    /// DELETE_BOOK: `OK {}`, then `OK {uuid}` per lpath.
    fn delete_books(&mut self, a: &Value) -> Result<(), End> {
        self.send(op::OK, &json!({}))?;
        let lpaths: Vec<String> = a.get("lpaths").and_then(Value::as_array).map(|v| v.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        let mut gone = Vec::new();
        for lpath in &lpaths {
            let known = protocol::safe_lpath(lpath).then(|| self.cache.find(lpath)).flatten();
            let uuid = match known {
                Some(i) => {
                    let title = s(&self.cache.books[i], "title").to_string();
                    let path = self.cfg.books_dir.join(lpath);
                    match std::fs::remove_file(&path) {
                        Ok(()) => log::info!("calibre: deleted {}", path.display()),
                        Err(e) if e.kind() == ErrorKind::NotFound => {}
                        Err(e) => log::warn!("calibre: cannot delete {}: {e}", path.display()),
                    }
                    remove_empty_parents(&path, &self.cfg.books_dir);
                    gone.push(if title.is_empty() { lpath.clone() } else { title });
                    self.cache.remove(lpath).unwrap_or_else(|| "none".into())
                }
                None => {
                    log::warn!("calibre: asked to delete {lpath:?}, which is not in the book list");
                    "none".to_string()
                }
            };
            self.send(op::OK, &json!({"uuid": uuid}))?;
        }
        self.save_cache();
        if !gone.is_empty() {
            (self.emit)(Event::Deleted(gone));
        }
        Ok(())
    }

    /// GET_BOOK_FILE_SEGMENT: `OK {fileLength}` and the bytes, or `NOOP` when missing.
    fn send_book(&mut self, lpath: &str) -> Result<(), End> {
        let path = self.cfg.books_dir.join(lpath);
        let file = protocol::safe_lpath(lpath).then(|| std::fs::File::open(&path).ok()).flatten();
        let Some(mut file) = file else { return self.send(op::NOOP, &json!({})) };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.send(op::OK, &json!({"fileLength": len}))?;
        let mut left = len;
        let mut chunk = [0u8; 4096];
        while left > 0 {
            let n = match file.read(&mut chunk[..(left.min(4096) as usize)]) {
                Ok(0) => return Err(End::Failed(format!("{} shrank while sending", path.display()))),
                Ok(n) => n,
                Err(e) => return Err(End::Failed(format!("reading {}: {e}", path.display()))),
            };
            self.stream.write_all(&chunk[..n]).map_err(|e| End::Failed(format!("writing to calibre: {e}")))?;
            left -= n as u64;
        }
        log::info!("calibre: sent {} ({len} bytes)", path.display());
        Ok(())
    }
}

/// Removes empty folders from `file`'s parent up to (not including) `root`.
fn remove_empty_parents(file: &Path, root: &Path) {
    let mut d = file.parent();
    while let Some(dir) = d {
        if dir == root || !dir.starts_with(root) || std::fs::remove_dir(dir).is_err() {
            break;
        }
        d = dir.parent();
    }
}

fn write_json(path: &Path, v: &Value) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, path)
}

/// A JSON value shortened for the debug log.
fn short(v: &Value) -> String {
    let t = v.to_string();
    if t.len() <= 300 {
        t
    } else {
        format!("{}...", &t[..t.char_indices().take_while(|(i, _)| *i < 300).last().map_or(0, |(i, _)| i)])
    }
}
