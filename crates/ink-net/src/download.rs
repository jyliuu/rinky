//! Downloads to a file: progress, cancel, resume with `Range`, checksum check and an
//! atomic rename.
//!
//! Files on disk while a download runs, for `dest = /mnt/us/Books/x.epub`:
//!
//! - `x.epub.part`: the bytes received so far (same directory, so the final rename stays
//!   on one file system);
//! - `x.epub.part.json`: URL, `ETag`, `Last-Modified` and total size, used to resume;
//! - `x.epub.part.lock`: held with `flock` by the run that owns the `.part`; removed when
//!   the run ends.
//!
//! A run (`Download::run`):
//!
//! 1. checks [`Connectivity`] ([`NetError::Offline`] when Wi-Fi is down);
//! 2. registers with [`NetActivity`] and asks [`KeepAwake`] to defer suspend, renewing it
//!    while data flows;
//! 3. takes the lock for `dest`: a second run for the same `dest` (a double tap) fails at
//!    once with [`NetError::Busy`] and leaves the first one alone. Two layers: a set of
//!    running destinations in this process (always works), and `flock` on
//!    `x.epub.part.lock` against other processes (a file system without `flock` logs a
//!    warning and keeps only the first layer);
//! 4. sends `GET` with `Accept-Encoding: identity` (so `Range` offsets and checksums apply
//!    to the stored bytes) and, when a `.part` exists for the same URL,
//!    `Range: bytes=N-` plus `If-Range` with the stored validator;
//! 5. on `206` checks that `Content-Range` starts at N and appends; on `200` starts over;
//!    on `416` treats the `.part` as complete when its size equals the total in the
//!    server's `Content-Range: bytes */T` (else the recorded total), otherwise starts
//!    over;
//! 6. copies the body in 64 KiB reads, checking the [`CancelToken`] between reads
//!    ([`NetError::Cancelled`] keeps the `.part` for a later resume);
//! 7. checks that the body is complete: its size must equal `Content-Length` or the
//!    `Content-Range` total; a chunked body ends with its last chunk (ureq refuses a
//!    cut one). A body with neither (it ends when the connection ends) cannot be told
//!    apart from a cut connection: ureq treats a TCP reset or a TLS connection closed
//!    without `close_notify` as a normal end. Such a body counts only when the checksum,
//!    [`Download::expected_len`] or [`Download::trust_connection_close`] confirms it;
//!    otherwise the run fails and keeps the `.part` for a resume. Note that ureq removes
//!    `Content-Length` when it decodes a gzip body (a server that ignores
//!    `Accept-Encoding: identity`), so such a body also needs that confirmation unless
//!    it is chunked;
//! 8. verifies the checksum (a mismatch deletes the `.part`), fsyncs and renames `.part`
//!    to `dest`.

use crate::connectivity::{Connectivity, KeepAwake, NetActivity};
use crate::http::{HttpClient, Request, Response};
use crate::{hex, CancelToken, NetError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Expected digest of a download (lowercase or uppercase hex).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checksum {
    /// SHA-256 (release assets, the phase 11 updater).
    Sha256(String),
    /// MD5.
    Md5(String),
}

impl Checksum {
    /// Parses `sha256:HEX`, `md5:HEX`, or bare hex (64 digits = SHA-256, 32 = MD5).
    pub fn parse(s: &str) -> Option<Checksum> {
        let s = s.trim();
        let (kind, h) = match s.split_once(':') {
            Some((k, h)) => (k.to_ascii_lowercase(), h.trim()),
            None => (String::new(), s),
        };
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        match (kind.as_str(), h.len()) {
            ("sha256" | "sha-256", 64) | ("", 64) => Some(Checksum::Sha256(h.to_ascii_lowercase())),
            ("md5", 32) | ("", 32) => Some(Checksum::Md5(h.to_ascii_lowercase())),
            _ => None,
        }
    }

    fn expected(&self) -> String {
        match self {
            Checksum::Sha256(h) | Checksum::Md5(h) => h.to_ascii_lowercase(),
        }
    }
}

/// Progress of a running download.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes in the file so far (including resumed bytes).
    pub received: u64,
    /// Total size, when the server said.
    pub total: Option<u64>,
    /// Bytes that were already on disk when this run started.
    pub resumed_from: u64,
}

impl DownloadProgress {
    /// 0.0..=1.0, when the total is known.
    pub fn fraction(&self) -> Option<f64> {
        self.total.filter(|t| *t > 0).map(|t| (self.received as f64 / t as f64).min(1.0))
    }
}

/// A finished download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadOutcome {
    /// The final file.
    pub path: PathBuf,
    /// Its size.
    pub bytes: u64,
    /// Bytes reused from an earlier `.part`.
    pub resumed_from: u64,
    /// SHA-256 of the file (lowercase hex).
    pub sha256: String,
    /// URL after redirects.
    pub url: String,
    /// `Content-Type` without parameters.
    pub mime: String,
    /// File name from `Content-Disposition`, if the server sent one.
    pub server_filename: Option<String>,
}

/// A progress callback.
pub type ProgressFn<'a> = Box<dyn FnMut(&DownloadProgress) + 'a>;

/// Hooks for one run. `Default` = online, no keep-awake, no tracking, no progress.
#[derive(Default)]
pub struct Hooks<'a> {
    /// Cancels the run between reads.
    pub cancel: CancelToken,
    /// Checked before the request.
    pub connectivity: Option<&'a dyn Connectivity>,
    /// Asked to defer suspend while data flows.
    pub keep_awake: Option<&'a dyn KeepAwake>,
    /// Counts the run as active.
    pub activity: Option<&'a NetActivity>,
    /// Called after each read.
    pub on_progress: Option<ProgressFn<'a>>,
}

impl std::fmt::Debug for Hooks<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").field("cancel", &self.cancel).field("connectivity", &self.connectivity.is_some()).field("keep_awake", &self.keep_awake.is_some()).finish_non_exhaustive()
    }
}

/// A download job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    /// Source URL.
    pub url: String,
    /// Final path.
    pub dest: PathBuf,
    /// Expected digest; `None` = no check.
    pub checksum: Option<Checksum>,
    /// Continue an earlier `.part` (default true).
    pub resume: bool,
    /// Extra request headers (authorization for a private OPDS server).
    pub headers: Vec<(String, String)>,
    /// Refuse bodies larger than this.
    pub max_bytes: Option<u64>,
    /// Per-read timeout.
    pub read_timeout: Duration,
    /// Size announced elsewhere (an OPDS link's `length`). Used only to confirm a body
    /// that the server sent without `Content-Length` or chunked framing; a real
    /// `Content-Length` always wins (catalog sizes go stale).
    pub expected_len: Option<u64>,
    /// Accept a body without `Content-Length` or chunked framing as complete when the
    /// connection ends, with nothing else to confirm it (default false). Only for a server
    /// known to send such bodies: a cut connection then gives a short file.
    pub trust_connection_close: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PartMeta {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    total: Option<u64>,
}

/// `<dest>.part`.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

fn meta_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part.json");
    PathBuf::from(s)
}

/// `<dest>.part.lock`.
pub fn lock_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part.lock");
    PathBuf::from(s)
}

/// `bytes A-B/T` -> `(Some(A), Some(T))`; `T` may be `*` (`None`); `bytes */T`, the form
/// of a 416 answer, -> `(None, Some(T))`.
fn parse_content_range(v: &str) -> Option<(Option<u64>, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let range = range.trim();
    let start = if range == "*" { None } else { Some(range.split('-').next()?.trim().parse().ok()?) };
    Some((start, total.trim().parse().ok()))
}

/// True when the body ends with a zero-length chunk (ureq reports a cut chunked body as
/// an error).
fn is_chunked(resp: &Response) -> bool {
    resp.header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
}

/// Destinations with a run in this process (key: canonical directory + file name).
fn running() -> &'static Mutex<HashSet<PathBuf>> {
    static RUNNING: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

fn busy(dest: &Path) -> NetError {
    NetError::Busy(format!("a download into {} is already running", dest.display()))
}

#[cfg(unix)]
fn same_file(f: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (f.metadata(), std::fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_f: &File, _path: &Path) -> bool {
    true
}

/// The lock a run holds for its `dest` (see the module documentation, step 3).
struct PartLock {
    key: PathBuf,
    path: PathBuf,
    /// The open, `flock`ed lock file; `None` when the file system has no `flock`.
    file: Option<File>,
}

impl PartLock {
    /// Takes the lock or fails with [`NetError::Busy`]. The directory of `dest` exists.
    fn acquire(dest: &Path) -> Result<PartLock> {
        let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        let key = match dest.file_name() {
            Some(n) => dir.join(n),
            None => dest.to_path_buf(),
        };
        if !running().lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
            return Err(busy(dest));
        }
        // From here on, dropping `lock` releases the in-process entry.
        let mut lock = PartLock { key, path: lock_path(dest), file: None };
        for _ in 0..3 {
            let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&lock.path)?;
            match f.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => return Err(busy(dest)),
                Err(TryLockError::Error(e)) => {
                    log::warn!("download: no flock on {} ({e}); only this process is guarded", lock.path.display());
                    drop(f);
                    let _ = std::fs::remove_file(&lock.path);
                    return Ok(lock);
                }
            }
            // A run in another process may have removed the file after we opened it and
            // before we locked it; then our lock guards nothing. Open again.
            if same_file(&f, &lock.path) {
                lock.file = Some(f);
                return Ok(lock);
            }
        }
        Err(busy(dest))
    }
}

impl Drop for PartLock {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            // Remove the file while still holding the lock, then unlock (close).
            let _ = std::fs::remove_file(&self.path);
            drop(f);
        }
        running().lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
    }
}

impl Download {
    /// A job with resume on, no checksum, 30 s read timeout.
    pub fn new(url: impl Into<String>, dest: impl Into<PathBuf>) -> Self {
        Download {
            url: url.into(),
            dest: dest.into(),
            checksum: None,
            resume: true,
            headers: Vec::new(),
            max_bytes: None,
            read_timeout: Duration::from_secs(30),
            expected_len: None,
            trust_connection_close: false,
        }
    }

    /// Sets the expected checksum.
    pub fn checksum(mut self, c: Checksum) -> Self {
        self.checksum = Some(c);
        self
    }

    /// Sets [`Download::expected_len`].
    pub fn expected_len(mut self, len: Option<u64>) -> Self {
        self.expected_len = len;
        self
    }

    /// Adds a request header.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Bytes already in `<dest>.part` (0 when none).
    pub fn partial_bytes(&self) -> u64 {
        std::fs::metadata(part_path(&self.dest)).map(|m| m.len()).unwrap_or(0)
    }

    /// Deletes the `.part` and its metadata. Does not take the lock: do not call it while
    /// a run for the same `dest` is active.
    pub fn discard_partial(&self) {
        let _ = std::fs::remove_file(part_path(&self.dest));
        let _ = std::fs::remove_file(meta_path(&self.dest));
    }

    fn load_meta(&self) -> Option<PartMeta> {
        let m: PartMeta = serde_json::from_slice(&std::fs::read(meta_path(&self.dest)).ok()?).ok()?;
        (m.url == self.url).then_some(m)
    }

    fn save_meta(&self, m: &PartMeta) -> Result<()> {
        std::fs::write(meta_path(&self.dest), serde_json::to_vec(m).map_err(|e| NetError::Format(e.to_string()))?)?;
        Ok(())
    }

    /// Runs the download (see the module documentation). A second run for the same
    /// `dest` while this one runs fails with [`NetError::Busy`].
    pub fn run(&self, client: &dyn HttpClient, hooks: &mut Hooks<'_>) -> Result<DownloadOutcome> {
        if let Some(c) = hooks.connectivity {
            c.check()?;
        }
        hooks.cancel.check()?;
        let activity = hooks.activity.cloned().unwrap_or_default();
        let mut guard = activity.begin(&hooks.cancel, hooks.keep_awake);
        if let Some(dir) = self.dest.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        // Held until the rename (or the error) at the end of this function.
        let _lock = PartLock::acquire(&self.dest)?;
        let part = part_path(&self.dest);
        let mut restarted = false;
        loop {
            // What is on disk?
            let meta = if self.resume { self.load_meta() } else { None };
            let mut have = if meta.is_some() { std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) } else { 0 };
            if meta.is_none() || restarted {
                self.discard_partial();
                have = 0;
            }
            let meta = if have > 0 { meta.unwrap_or_default() } else { PartMeta::default() };

            let mut req = Request::get(&self.url).read_timeout(self.read_timeout).set_header("Accept-Encoding", "identity");
            for (k, v) in &self.headers {
                req = req.header(k, v);
            }
            if have > 0 {
                req = req.set_header("Range", &format!("bytes={have}-"));
                let validator = meta.etag.as_deref().filter(|e| !e.starts_with("W/")).or(meta.last_modified.as_deref());
                if let Some(v) = validator {
                    req = req.set_header("If-Range", v);
                }
            }
            let resp = client.send(req)?;
            let mut append = false;
            let total;
            match resp.status {
                206 if have > 0 => {
                    let (start, t) = resp
                        .header("content-range")
                        .and_then(parse_content_range)
                        .and_then(|(s, t)| Some((s?, t)))
                        .ok_or_else(|| NetError::Format("206 without a valid Content-Range".into()))?;
                    if start != have {
                        log::warn!("download: asked for bytes {have}-, server sent from {start}; starting over");
                        if restarted {
                            return Err(NetError::Format(format!("server resumed at {start}, expected {have}")));
                        }
                        restarted = true;
                        continue;
                    }
                    append = true;
                    total = t.or(meta.total);
                }
                416 if have > 0 => {
                    let t = resp.header("content-range").and_then(parse_content_range).and_then(|(_, t)| t).or(meta.total);
                    if t == Some(have) {
                        drop(resp);
                        log::info!("download: {} already complete ({have} bytes)", part.display());
                        return self.finish(&part, have, have, &self.url, String::new(), None);
                    }
                    if restarted {
                        return Err(NetError::Status(416));
                    }
                    restarted = true;
                    continue;
                }
                200..=299 => {
                    total = resp.content_length();
                }
                _ => return Err(resp.error_for_status().err().unwrap_or(NetError::Status(0))),
            }
            if let (Some(max), Some(t)) = (self.max_bytes, total) {
                if t > max {
                    return Err(NetError::TooLarge(max as usize));
                }
            }
            // Does the body have an end of its own (a length or a last chunk)?
            let framed = total.is_some() || resp.content_length().is_some() || is_chunked(&resp);
            let new_meta = PartMeta {
                url: self.url.clone(),
                etag: resp.header("etag").map(str::to_string),
                last_modified: resp.header("last-modified").map(str::to_string),
                total,
            };
            self.save_meta(&new_meta)?;
            let resumed_from = if append { have } else { 0 };
            let url = resp.url.clone();
            let mime = resp.mime();
            let server_filename = resp.header("content-disposition").and_then(content_disposition_filename);
            let mut file = if append { OpenOptions::new().append(true).open(&part)? } else { File::create(&part)? };
            let mut received = resumed_from;
            let mut body = resp.body;
            let mut buf = vec![0u8; 64 * 1024];
            let mut progress = DownloadProgress { received, total, resumed_from };
            if let Some(cb) = hooks.on_progress.as_mut() {
                cb(&progress);
            }
            let copy: Result<()> = loop {
                if let Err(e) = hooks.cancel.check() {
                    break Err(e);
                }
                let n = match body.read(&mut buf) {
                    Ok(0) => break Ok(()),
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => break Err(NetError::from_read(e)),
                };
                if let Err(e) = file.write_all(&buf[..n]) {
                    break Err(e.into());
                }
                received += n as u64;
                if let Some(max) = self.max_bytes {
                    if received > max {
                        break Err(NetError::TooLarge(max as usize));
                    }
                }
                guard.tick();
                progress.received = received;
                if let Some(cb) = hooks.on_progress.as_mut() {
                    cb(&progress);
                }
            };
            file.flush()?;
            file.sync_all()?;
            drop(file);
            if let Err(e) = copy {
                if matches!(e, NetError::TooLarge(_)) {
                    self.discard_partial();
                }
                log::info!("download: stopped at {received} bytes: {e}");
                return Err(e);
            }
            if let Some(t) = total {
                if received != t {
                    return Err(NetError::Http(format!("connection closed after {received} of {t} bytes")));
                }
            }
            if !framed {
                let confirmed = self.checksum.is_some() || self.expected_len == Some(received) || self.trust_connection_close;
                if !confirmed {
                    // The .part and its metadata stay: a server with Range support answers
                    // the next run with 416 (complete) or 206 (the rest).
                    let expected = self.expected_len.map(|n| format!(", expected {n}")).unwrap_or_default();
                    log::warn!("download: unframed body ended after {received} bytes{expected}; not renamed");
                    return Err(NetError::Http(format!(
                        "the body had no Content-Length and was not chunked; it ended with the connection after {received} bytes{expected}, which a cut connection also does (give a checksum or the expected size)"
                    )));
                }
            }
            drop(body);
            return self.finish(&part, received, resumed_from, &url, mime, server_filename);
        }
    }

    fn finish(&self, part: &Path, bytes: u64, resumed_from: u64, url: &str, mime: String, server_filename: Option<String>) -> Result<DownloadOutcome> {
        let (sha256, md5) = digest_file(part)?;
        if let Some(c) = &self.checksum {
            let actual = match c {
                Checksum::Sha256(_) => &sha256,
                Checksum::Md5(_) => &md5,
            };
            if *actual != c.expected() {
                self.discard_partial();
                return Err(NetError::Checksum { expected: c.expected(), actual: actual.clone() });
            }
        }
        std::fs::rename(part, &self.dest)?;
        let _ = std::fs::remove_file(meta_path(&self.dest));
        sync_dir(&self.dest);
        Ok(DownloadOutcome { path: self.dest.clone(), bytes, resumed_from, sha256, url: url.to_string(), mime, server_filename })
    }
}

fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// SHA-256 and MD5 of a file (lowercase hex).
pub fn digest_file(path: &Path) -> Result<(String, String)> {
    use md5::Digest;
    let mut f = File::open(path)?;
    let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
    let mut md5 = md5::Md5::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        md5.update(&buf[..n]);
    }
    Ok((hex(sha.finish().as_ref()), hex(&md5.finalize())))
}

/// SHA-256 of bytes (lowercase hex).
pub fn sha256_hex(data: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
}

/// The file name in a `Content-Disposition` header (`filename*=UTF-8''...` preferred),
/// without any directory part.
pub fn content_disposition_filename(v: &str) -> Option<String> {
    let mut plain = None;
    for part in v.split(';').map(str::trim) {
        let Some((k, val)) = part.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let val = val.trim();
        if k == "filename*" {
            let enc = val.rsplit_once('\'').map(|(_, v)| v).unwrap_or(val);
            let name = crate::url::decode_component(enc.trim_matches('"'));
            if !name.is_empty() {
                return Some(basename(&name));
            }
        } else if k == "filename" {
            plain = Some(basename(val.trim_matches('"')));
        }
    }
    plain.filter(|n| !n.is_empty())
}

fn basename(s: &str) -> String {
    s.rsplit(['/', '\\']).next().unwrap_or(s).to_string()
}

/// A file name that is safe on the Kindle's FAT userstore: no control characters, no
/// `<>:"/\|?*`, no leading/trailing dots or spaces, at most 200 bytes, never empty.
pub fn sanitize_filename(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s = s.trim_matches(|c: char| c == '.' || c == ' ').to_string();
    if s.len() > 200 {
        // Keep the extension.
        let ext = s.rfind('.').filter(|&i| s.len() - i <= 10).map(|i| s[i..].to_string()).unwrap_or_default();
        let mut cut = 200 - ext.len();
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s = format!("{}{}", s[..cut].trim_end(), ext);
    }
    if s.is_empty() {
        "download".into()
    } else {
        s
    }
}

/// File extension (with the dot) for an e-book MIME type, if known.
pub fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let m = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match m.as_str() {
        "application/epub+zip" => ".epub",
        "application/pdf" => ".pdf",
        "application/x-mobipocket-ebook" => ".mobi",
        "application/vnd.amazon.ebook" | "application/vnd.amazon.mobi8-ebook" => ".azw3",
        "application/x-fictionbook+xml" | "text/fb2+xml" => ".fb2",
        "application/x-zip-compressed-fb2" => ".fb2.zip",
        "application/vnd.comicbook+zip" | "application/x-cbz" => ".cbz",
        "application/x-cbr" | "application/vnd.comicbook-rar" => ".cbr",
        "application/vnd.ms-htmlhelp" | "application/x-chm" => ".chm",
        "application/rtf" | "text/rtf" => ".rtf",
        "application/zip" => ".zip",
        "text/plain" => ".txt",
        "text/html" | "application/xhtml+xml" => ".html",
        "image/vnd.djvu" => ".djvu",
        _ => return None,
    })
}

/// A file name for a download: the server's `Content-Disposition` name, else the title
/// plus the MIME type's extension, else the URL's last path segment; then sanitized.
pub fn suggest_filename(server_filename: Option<&str>, title: Option<&str>, mime: &str, url: &str) -> String {
    if let Some(n) = server_filename.filter(|n| !n.trim().is_empty()) {
        return sanitize_filename(n);
    }
    if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
        let ext = extension_for_mime(mime).unwrap_or("");
        return sanitize_filename(&format!("{}{ext}", t.trim()));
    }
    let seg = crate::url::last_segment(url);
    let mut name = sanitize_filename(&seg);
    if let Some(ext) = extension_for_mime(mime) {
        if !name.to_ascii_lowercase().ends_with(ext) {
            name.push_str(ext);
        }
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{file_bytes, TestServer};
    use crate::UreqClient;
    use std::cell::RefCell;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ink-net-dl-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn checksum_parsing() {
        let h = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(Checksum::parse(h), Some(Checksum::Sha256(h.into())));
        assert_eq!(Checksum::parse(&format!("SHA256:{}", h.to_uppercase())), Some(Checksum::Sha256(h.into())));
        assert_eq!(Checksum::parse("md5:d41d8cd98f00b204e9800998ecf8427e"), Some(Checksum::Md5("d41d8cd98f00b204e9800998ecf8427e".into())));
        assert_eq!(Checksum::parse("abc"), None);
        assert_eq!(Checksum::parse("sha256:zz"), None);
        assert_eq!(sha256_hex(b""), h);
    }

    #[test]
    fn content_range_and_dispositions() {
        assert_eq!(parse_content_range("bytes 100-199/200"), Some((Some(100), Some(200))));
        assert_eq!(parse_content_range("bytes 0-9/*"), Some((Some(0), None)));
        assert_eq!(parse_content_range("bytes */4000"), Some((None, Some(4000))));
        assert_eq!(parse_content_range("bytes x-9/10"), None);
        assert_eq!(parse_content_range("items 0-9/10"), None);
        assert_eq!(content_disposition_filename("attachment; filename=\"pg1342.epub\"").as_deref(), Some("pg1342.epub"));
        assert_eq!(content_disposition_filename("attachment; filename=\"x.epub\"; filename*=UTF-8''%C3%A6%20b.epub").as_deref(), Some("\u{e6} b.epub"));
        assert_eq!(content_disposition_filename("attachment; filename=\"../../etc/passwd\"").as_deref(), Some("passwd"));
        assert_eq!(content_disposition_filename("inline"), None);
    }

    #[test]
    fn filenames_are_fat_safe() {
        assert_eq!(sanitize_filename("Pride: and \"Prejudice\"?.epub"), "Pride_ and _Prejudice__.epub");
        assert_eq!(sanitize_filename("  ..hidden. "), "hidden");
        assert_eq!(sanitize_filename("a/b\\c\u{7}d"), "a_b_c_d");
        assert_eq!(sanitize_filename(""), "download");
        let long = format!("{}.epub", "\u{f8}".repeat(150));
        let s = sanitize_filename(&long);
        assert!(s.len() <= 200 && s.ends_with(".epub"), "{s}");
        assert_eq!(suggest_filename(None, Some("Alpha & Omega"), "application/epub+zip", "http://h/x"), "Alpha & Omega.epub");
        assert_eq!(suggest_filename(Some("srv.epub"), Some("T"), "", "http://h/x"), "srv.epub");
        assert_eq!(suggest_filename(None, None, "application/epub+zip", "https://www.gutenberg.org/ebooks/1342.epub3.images"), "1342.epub3.images.epub");
        assert_eq!(suggest_filename(None, None, "application/pdf", "http://h/a%20b.pdf"), "a b.pdf");
    }

    #[test]
    fn download_with_progress_and_checksum() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("basic");
        let dest = dir.join("sub").join("book.epub");
        let want = file_bytes(200_000);
        let dl = Download::new(s.url("/files/200000?name=b.epub&type=application/epub%2Bzip"), &dest).checksum(Checksum::Sha256(sha256_hex(&want)));
        let seen = RefCell::new(Vec::new());
        let mut hooks = Hooks { on_progress: Some(Box::new(|p: &DownloadProgress| seen.borrow_mut().push(*p))), ..Default::default() };
        let out = dl.run(&UreqClient::new(), &mut hooks).unwrap();
        drop(hooks);
        assert_eq!(std::fs::read(&dest).unwrap(), want);
        assert_eq!(out.bytes, 200_000);
        assert_eq!(out.mime, "application/epub+zip");
        assert_eq!(out.server_filename.as_deref(), Some("b.epub"));
        assert!(!part_path(&dest).exists());
        assert!(!meta_path(&dest).exists());
        let seen = seen.borrow();
        assert_eq!(seen.last().unwrap().received, 200_000);
        assert_eq!(seen.last().unwrap().total, Some(200_000));
        assert_eq!(seen.last().unwrap().fraction(), Some(1.0));
        assert!(seen.windows(2).all(|w| w[0].received <= w[1].received));
        let req = &s.state.requests_to("/files/")[0];
        assert_eq!(req.header("accept-encoding"), Some("identity"));
        assert_eq!(req.header("range"), None);
    }

    #[test]
    fn resume_after_a_broken_connection() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("flaky");
        let dest = dir.join("flaky.bin");
        let want = file_bytes(300_000);
        let dl = Download::new(s.url("/flaky/300000"), &dest).checksum(Checksum::Sha256(sha256_hex(&want)));
        let client = UreqClient::new();
        let first = dl.run(&client, &mut Hooks::default());
        assert!(first.is_err(), "first run must break: {first:?}");
        let have = dl.partial_bytes();
        assert!(have > 0 && have < 300_000, "partial {have}");
        let out = dl.run(&client, &mut Hooks::default()).unwrap();
        assert_eq!(out.resumed_from, have);
        assert_eq!(std::fs::read(&dest).unwrap(), want);
        let reqs = s.state.requests_to("/flaky/");
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].header("range").map(str::to_string), Some(format!("bytes={have}-")));
        assert_eq!(reqs[1].header("if-range"), Some("\"f300000\""));
    }

    #[test]
    fn cancel_keeps_the_part_and_resume_finishes() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("cancel");
        let dest = dir.join("c.bin");
        let want = file_bytes(1_000_000);
        let dl = Download::new(s.url("/files/1000000"), &dest).checksum(Checksum::Md5({
            use md5::Digest;
            hex(&md5::Md5::digest(&want))
        }));
        let token = CancelToken::new();
        let t2 = token.clone();
        let mut hooks = Hooks {
            cancel: token.clone(),
            on_progress: Some(Box::new(move |p: &DownloadProgress| {
                if p.received >= 100_000 {
                    t2.cancel();
                }
            })),
            ..Default::default()
        };
        let r = dl.run(&UreqClient::new(), &mut hooks);
        assert!(matches!(r, Err(NetError::Cancelled)), "{r:?}");
        let have = dl.partial_bytes();
        assert!((100_000..1_000_000).contains(&have), "{have}");
        assert!(!dest.exists());
        let out = dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
        assert_eq!(out.resumed_from, have);
        assert_eq!(std::fs::read(&dest).unwrap(), want);
    }

    #[test]
    fn checksum_mismatch_deletes_the_part() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("badsum");
        let dest = dir.join("x.bin");
        let dl = Download::new(s.url("/files/1000"), &dest).checksum(Checksum::Sha256("0".repeat(64)));
        let r = dl.run(&UreqClient::new(), &mut Hooks::default());
        assert!(matches!(&r, Err(NetError::Checksum { expected, actual }) if expected == &"0".repeat(64) && actual == &sha256_hex(&file_bytes(1000))), "{r:?}");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn servers_without_range_support_restart_cleanly() {
        let s = TestServer::start().unwrap();
        let client = UreqClient::new();
        for kind in ["norange", "badrange"] {
            let dir = tmpdir(kind);
            let dest = dir.join("f.bin");
            let dl = Download::new(s.url(&format!("/{kind}/5000")), &dest);
            // A stale half file recorded for this URL.
            std::fs::write(part_path(&dest), &file_bytes(5000)[..1000]).unwrap();
            dl.save_meta(&PartMeta { url: dl.url.clone(), etag: None, last_modified: None, total: Some(5000) }).unwrap();
            let out = dl.run(&client, &mut Hooks::default()).unwrap();
            assert_eq!(out.resumed_from, 0, "{kind}");
            assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(5000), "{kind}");
        }
    }

    #[test]
    fn complete_part_and_416() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("416");
        let dest = dir.join("done.bin");
        let dl = Download::new(s.url("/files/2000"), &dest);
        std::fs::write(part_path(&dest), file_bytes(2000)).unwrap();
        dl.save_meta(&PartMeta { url: dl.url.clone(), etag: Some("\"f2000\"".into()), last_modified: None, total: Some(2000) }).unwrap();
        let out = dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
        assert_eq!(out.bytes, 2000);
        assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(2000));
        assert_eq!(s.state.requests_to("/files/")[0].header("range"), Some("bytes=2000-"));
    }

    #[test]
    fn a_416_with_another_size_starts_over() {
        // A .part longer than the file now on the server (it was replaced, and the server
        // keeps the same ETag): the 416's `bytes */2000` beats the recorded total.
        let s = TestServer::start().unwrap();
        let dir = tmpdir("416-other");
        let dest = dir.join("o.bin");
        let dl = Download::new(s.url("/files/2000"), &dest);
        std::fs::write(part_path(&dest), vec![7u8; 2500]).unwrap();
        dl.save_meta(&PartMeta { url: dl.url.clone(), etag: Some("\"f2000\"".into()), last_modified: None, total: Some(2500) }).unwrap();
        let out = dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
        assert_eq!((out.bytes, out.resumed_from), (2000, 0));
        assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(2000));
        let reqs = s.state.requests_to("/files/");
        assert_eq!(reqs[0].header("range"), Some("bytes=2500-"));
        assert_eq!(reqs[1].header("range"), None);
    }

    #[test]
    fn a_second_run_for_the_same_dest_is_busy() {
        // A double tap: the second run starts while the first holds the .part.
        let s = TestServer::start().unwrap();
        let dir = tmpdir("twice");
        let dest = dir.join("t.epub");
        let dl = Download::new(s.url("/files/300000"), &dest);
        let second = RefCell::new(None);
        let mut hooks = Hooks {
            on_progress: Some(Box::new(|p: &DownloadProgress| {
                if p.received > 0 && second.borrow().is_none() {
                    // Same dest spelled differently (an extra `.` component).
                    let other = Download::new(dl.url.clone(), dir.join(".").join("t.epub"));
                    *second.borrow_mut() = Some(other.run(&UreqClient::new(), &mut Hooks::default()));
                }
            })),
            ..Default::default()
        };
        let out = dl.run(&UreqClient::new(), &mut hooks).unwrap();
        drop(hooks);
        assert!(matches!(second.borrow().as_ref(), Some(Err(NetError::Busy(_)))), "{:?}", second.borrow());
        assert_eq!(out.bytes, 300_000);
        assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(300_000));
        assert_eq!(s.state.requests_to("/files/").len(), 1, "the busy run sent no request");
        assert!(!lock_path(&dest).exists(), "lock file removed after the run");
        // The lock is released: the next run works.
        std::fs::remove_file(&dest).unwrap();
        dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
    }

    #[test]
    fn a_lock_held_by_another_process_is_busy_and_a_stale_lock_file_is_not() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("flock");
        let dest = dir.join("f.epub");
        let dl = Download::new(s.url("/files/5000"), &dest);
        // Another process: an flock on the lock file (a separate open file description
        // conflicts like another process does; this process's registry is empty).
        let other = File::create(lock_path(&dest)).unwrap();
        other.try_lock().unwrap();
        let r = dl.run(&UreqClient::new(), &mut Hooks::default());
        assert!(matches!(r, Err(NetError::Busy(_))), "{r:?}");
        assert!(s.state.requests().is_empty());
        // That process died: the kernel dropped its flock; the file is still there.
        drop(other);
        assert!(lock_path(&dest).exists());
        dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(5000));
        assert!(!lock_path(&dest).exists());
    }

    #[test]
    fn unframed_bodies_need_a_confirmation() {
        let s = TestServer::start().unwrap();
        let client = UreqClient::new();
        let want = file_bytes(50_000);
        let dir = tmpdir("unframed");
        // Complete, but nothing confirms it: refused, .part kept for a resume.
        let dest = dir.join("a.epub");
        let r = Download::new(s.url("/unframed/50000"), &dest).run(&client, &mut Hooks::default());
        assert!(matches!(&r, Err(NetError::Http(m)) if m.contains("no Content-Length")), "{r:?}");
        assert!(!dest.exists());
        assert_eq!(std::fs::metadata(part_path(&dest)).unwrap().len(), 50_000);
        // Each confirmation works.
        let out = Download::new(s.url("/unframed/50000"), dir.join("b.epub")).checksum(Checksum::Sha256(sha256_hex(&want))).run(&client, &mut Hooks::default()).unwrap();
        assert_eq!(out.bytes, 50_000);
        let out = Download::new(s.url("/unframed/50000"), dir.join("c.epub")).expected_len(Some(50_000)).run(&client, &mut Hooks::default()).unwrap();
        assert_eq!(out.bytes, 50_000);
        let mut trusting = Download::new(s.url("/unframed/50000"), dir.join("d.epub"));
        trusting.trust_connection_close = true;
        assert_eq!(trusting.run(&client, &mut Hooks::default()).unwrap().bytes, 50_000);
        assert_eq!(std::fs::read(dir.join("d.epub")).unwrap(), want);
        // Cut: the expected size or the checksum catches it.
        let dest = dir.join("e.epub");
        let r = Download::new(s.url("/unframed/50000?cut=20000"), &dest).expected_len(Some(50_000)).run(&client, &mut Hooks::default());
        assert!(matches!(&r, Err(NetError::Http(m)) if m.contains("after 20000 bytes, expected 50000")), "{r:?}");
        assert!(!dest.exists());
        let r = Download::new(s.url("/unframed/50000?cut=20000"), dir.join("f.epub")).checksum(Checksum::Sha256(sha256_hex(&want))).run(&client, &mut Hooks::default());
        assert!(matches!(r, Err(NetError::Checksum { .. })), "{r:?}");
        assert!(!dir.join("f.epub").exists());
    }

    #[test]
    fn unframed_https_body_cut_without_close_notify_is_refused() {
        let s = TestServer::start_tls().unwrap();
        let client = UreqClient::with_ca_pem(crate::testserver::TEST_CA_PEM).unwrap();
        let dir = tmpdir("unframed-tls");
        let dest = dir.join("t.epub");
        let r = Download::new(s.url("/unframed/50000?cut=20000"), &dest).run(&client, &mut Hooks::default());
        assert!(matches!(&r, Err(NetError::Http(m)) if m.contains("after 20000 bytes")), "{r:?}");
        assert!(!dest.exists());
    }

    #[test]
    fn a_part_for_another_url_is_not_reused() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("otherurl");
        let dest = dir.join("o.bin");
        std::fs::write(part_path(&dest), b"garbage").unwrap();
        std::fs::write(meta_path(&dest), br#"{"url":"http://elsewhere/","etag":null,"last_modified":null,"total":10}"#).unwrap();
        let out = Download::new(s.url("/files/3000"), &dest).run(&UreqClient::new(), &mut Hooks::default()).unwrap();
        assert_eq!(out.resumed_from, 0);
        assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(3000));
        assert_eq!(s.state.requests_to("/files/")[0].header("range"), None);
    }

    #[test]
    fn identity_encoding_means_no_gzip_surprise() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("gzip");
        let out = Download::new(s.url("/gzip/4000"), dir.join("g.bin")).run(&UreqClient::new(), &mut Hooks::default()).unwrap();
        assert_eq!(out.bytes, 4000);
    }

    #[test]
    fn errors_offline_status_and_size_limit() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("errors");
        let client = UreqClient::new();
        let offline = || crate::connectivity::wifi_rule(true, "PENDING");
        let mut hooks = Hooks { connectivity: Some(&offline), ..Default::default() };
        let r = Download::new(s.url("/files/10"), dir.join("a")).run(&client, &mut hooks);
        assert!(matches!(r, Err(NetError::Offline(_))), "{r:?}");
        assert!(s.state.requests().is_empty(), "no request while offline");
        let r = Download::new(s.url("/status/404"), dir.join("b")).run(&client, &mut Hooks::default());
        assert!(matches!(r, Err(NetError::Server { status: 404, .. })), "{r:?}");
        let mut dl = Download::new(s.url("/files/5000"), dir.join("c"));
        dl.max_bytes = Some(1000);
        assert!(matches!(dl.run(&client, &mut Hooks::default()), Err(NetError::TooLarge(1000))));
    }

    #[test]
    fn downloads_hold_suspend_and_count_as_activity() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("awake");
        let holds = RefCell::new(0u32);
        let keep = |_d: Duration| {
            *holds.borrow_mut() += 1;
            Ok(())
        };
        let activity = NetActivity::new();
        let busy_during = RefCell::new(false);
        let act2 = activity.clone();
        let mut hooks = Hooks {
            keep_awake: Some(&keep),
            activity: Some(&activity),
            on_progress: Some(Box::new(|_p: &DownloadProgress| {
                if act2.busy() && !act2.may_auto_suspend() {
                    *busy_during.borrow_mut() = true;
                }
            })),
            ..Default::default()
        };
        Download::new(s.url("/files/100000"), dir.join("k.bin")).run(&UreqClient::new(), &mut hooks).unwrap();
        drop(hooks);
        assert!(*busy_during.borrow());
        assert!(*holds.borrow() >= 1);
        assert!(!activity.busy());
        assert_eq!(activity.started(), 1);
    }

    #[test]
    fn interrupt_all_before_sleep_stops_the_download_for_a_later_resume() {
        let s = TestServer::start().unwrap();
        let dir = tmpdir("sleep");
        let dest = dir.join("s.bin");
        let activity = NetActivity::new();
        let act2 = activity.clone();
        let mut hooks = Hooks {
            activity: Some(&activity),
            on_progress: Some(Box::new(move |p: &DownloadProgress| {
                if p.received > 50_000 {
                    // What the power-key handler does before suspending.
                    act2.interrupt_all();
                }
            })),
            ..Default::default()
        };
        let dl = Download::new(s.url("/files/400000"), &dest);
        assert!(matches!(dl.run(&UreqClient::new(), &mut hooks), Err(NetError::Cancelled)));
        drop(hooks);
        assert!(!activity.busy());
        assert!(dl.partial_bytes() > 50_000);
        let out = dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
        assert!(out.resumed_from > 50_000);
        assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(400_000));
    }
}
