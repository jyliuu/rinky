//! Persistent data for rinky.
//!
//! - [`md5`]: KOReader's partial MD5, the book id used everywhere (sync, stats, plugins).
//! - [`sidecar`]: per-book state in `<book>.sdr/rinky.<ext>.json` (one file per format).
//! - [`library`]: the library index (`library.json`).
//! - [`settings`]: global settings (`settings.toml`).
//! - [`plugin`]: per-plugin settings, per-book plugin data and data directories.
//! - [`stats`]: rinky's reading statistics (`statistics.sqlite3`).
//! - [`vocab`]: dictionary lookups for the vocabulary builder (`vocabulary.sqlite3`).
//! - [`export`]: highlights, notes and bookmarks of a book as Markdown in `exports/`.
//! - [`koreader`]: read-only KOReader import (metadata sidecars, statistics, vocabulary).
//! - [`lua`]: the Lua table literal reader used by the KOReader import (never runs Lua).
//! - [`selftest`]: store checks for `rinky selftest` and the emulator.
//! - [`Paths`]: where everything lives on the device.
//!
//! Files are written with [`write_atomic`] (temp file, fsync, rename, directory fsync).
//! JSON state files keep a `.old` backup at most once a minute; a file that does not
//! parse is moved aside to `<name>.corrupt-<time>` and the backup is used instead.
//! On the Kindle's user store (`fuse.fsp` on firmware 5.19.5) a file can be missing for
//! a moment while a new version is renamed over it; [`load_recovering`] waits for such a
//! save instead of falling back to the older backup.
//!
//! Owner from wave A: the `store` agent.

#![warn(missing_docs)]

pub mod export;
pub mod koreader;
pub mod library;
pub mod lua;
pub mod md5;
pub mod plugin;
pub mod prefs;
pub mod selftest;
pub mod settings;
pub mod sidecar;
pub mod stats;
pub mod vocab;

pub use library::{BookEntry, Collection, LibraryIndex};
pub use md5::partial_md5;
pub use plugin::PluginStore;
pub use settings::Settings;
pub use sidecar::{BookState, ReadingStatus};
pub use stats::StatsDb;

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// File system error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// A file exists but cannot be parsed.
    #[error("bad file: {0}")]
    Format(String),
    /// A file that should exist does not.
    #[error("not found: {0}")]
    NotFound(String),
    /// Not implemented yet; the message says when.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// A caller passed a value the store refuses (bad plugin name, duplicate collection).
    #[error("invalid: {0}")]
    Invalid(String),
    /// SQLite error.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Result alias for this crate.
pub type Result<T, E = StoreError> = std::result::Result<T, E>;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The bytes of an OS string (file names are bytes on Linux; not UTF-8 in general).
#[cfg(unix)]
pub(crate) fn os_bytes(s: &OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes()
}

/// The bytes of an OS string (lossy off Unix).
#[cfg(not(unix))]
pub(crate) fn os_bytes(s: &OsStr) -> std::borrow::Cow<'_, [u8]> {
    match s.to_string_lossy() {
        std::borrow::Cow::Borrowed(t) => std::borrow::Cow::Borrowed(t.as_bytes()),
        std::borrow::Cow::Owned(t) => std::borrow::Cow::Owned(t.into_bytes()),
    }
}

/// An OS string from bytes (kept as they are on Unix, lossy elsewhere).
#[cfg(unix)]
pub(crate) fn os_from_bytes(b: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(b)
}

/// An OS string from bytes (kept as they are on Unix, lossy elsewhere).
#[cfg(not(unix))]
pub(crate) fn os_from_bytes(b: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&b).into_owned())
}

/// `<file name>.tmp.`: the prefix of every temporary file [`write_atomic`] makes for `path`.
fn temp_prefix(path: &Path) -> Vec<u8> {
    let mut p = path.file_name().map(|n| os_bytes(n).to_vec()).unwrap_or_else(|| b"file".to_vec());
    p.extend_from_slice(b".tmp.");
    p
}

/// A temporary name next to `path`, unique within this process and between processes:
/// `<name>.tmp.<pid>.<counter>` (the name's bytes are kept, also when not UTF-8).
fn temp_sibling(path: &Path) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = temp_prefix(path);
    name.extend_from_slice(format!("{}.{n}", std::process::id()).as_bytes());
    path.with_file_name(os_from_bytes(name))
}

/// Temporary files of [`write_atomic`] for `path` that exist now (any process), with
/// their modification times. Only names that start with `<exact file name>.tmp.` count,
/// so `rinky.pdf.json.tmp.*` is never taken for `rinky.epub.json`.
fn temp_siblings(path: &Path) -> Vec<(PathBuf, SystemTime)> {
    let prefix = temp_prefix(path);
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|e| os_bytes(&e.file_name()).starts_with(&prefix))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (e.path(), m.modified().unwrap_or(UNIX_EPOCH)))
        })
        .collect()
}

/// Temporary files older than this are left-overs of a crash and are removed by the
/// next successful [`save_with_backup`] of the same file.
pub const STALE_TEMP_AGE: Duration = Duration::from_secs(3600);

/// Writes `bytes` to `path` so a crash or power loss leaves either the old or the new
/// file: the bytes go to a temporary file in the same directory, which is flushed to
/// disk (`fsync`) and renamed over `path`; then the directory itself is flushed (errors
/// from that last step are ignored, some file systems refuse it).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = temp_sibling(path);
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    sync_dir(path);
    Ok(())
}

/// Flushes the directory that holds `path` (best effort).
fn sync_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// JSON `bytes` as a `T`, or the parse error as text (a parser for [`load_recovering`]).
pub(crate) fn parse_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> std::result::Result<T, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

/// `path` with `suffix` appended to its file name (`a.json` + `.old` = `a.json.old`).
pub(crate) fn with_suffix(path: &Path, suffix: impl AsRef<OsStr>) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The backup of a state file: `<path>.old` (KOReader's convention).
pub fn backup_path(path: &Path) -> PathBuf {
    with_suffix(path, ".old")
}

/// A backup is refreshed at most this often, to limit flash writes.
pub const BACKUP_INTERVAL: Duration = Duration::from_secs(60);

/// Where a recovered state file came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoadSource {
    /// The file itself.
    Primary,
    /// A complete temporary copy (`<name>.tmp.<pid>.<n>`) newer than the backup, found
    /// while the file itself was missing: a save wrote and synced it but its rename had
    /// not happened (still running, or the process or the power died in between).
    Temp,
    /// Its `.old` backup (the file was missing or did not parse).
    Backup,
    /// Neither existed or parsed.
    #[default]
    Missing,
}

/// Waits between reads of a missing file while a save of it may be running
/// (127 ms in all).
const SETTLE_DELAYS_MS: [u64; 7] = [1, 2, 4, 8, 16, 32, 64];

static SETTLE_WAITS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    /// [`SETTLE_WAITS`] for loads on this thread only: tests in the same process wait too.
    static SETTLE_WAITS_HERE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Run once, on this thread, at the next settle wait: see [`on_settle_wait`].
    static ON_SETTLE_WAIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` when the next load on this thread finds its file missing and is about to
/// wait for a save in progress. Tests finish their "save in progress" there: from
/// another thread, the save had to land inside the 127 ms of waits, which a busy machine
/// does not guarantee.
#[cfg(test)]
pub(crate) fn on_settle_wait(f: impl FnOnce() + 'static) {
    ON_SETTLE_WAIT.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

/// How many times loads in this process found a file missing while a save of it might
/// be running and read it again (diagnostics for the selftest and the tests).
pub fn load_waits() -> u64 {
    SETTLE_WAITS.load(Ordering::Relaxed)
}

/// Reads `path`; `None` if it does not exist. When it is missing but a save of it may be
/// running (a temporary file of [`write_atomic`] for it exists, or `check_backup` is set
/// and its `.old` backup exists, which means the file existed before), reads again after
/// short waits: on the Kindle's FUSE user store a file renamed over is missing for a
/// moment, and without the wait a load during a save would return the older backup.
fn read_settled(path: &Path, check_backup: bool) -> std::io::Result<Option<Vec<u8>>> {
    let mut waits = SETTLE_DELAYS_MS.iter();
    loop {
        match std::fs::read(path) {
            Ok(b) => return Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let busy = (check_backup && backup_path(path).exists()) || !temp_siblings(path).is_empty();
                match waits.next() {
                    Some(&ms) if busy => {
                        SETTLE_WAITS.fetch_add(1, Ordering::Relaxed);
                        #[cfg(test)]
                        {
                            SETTLE_WAITS_HERE.with(|w| w.set(w.get() + 1));
                            if let Some(f) = ON_SETTLE_WAIT.with(|h| h.borrow_mut().take()) {
                                f();
                            }
                        }
                        std::thread::sleep(Duration::from_millis(ms));
                    }
                    _ => return Ok(None),
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// The newest temporary copy of `path` that parses and is not older than `not_before`.
/// Temporary files are never moved or removed here: one may be another writer's file
/// in progress (a partial one does not parse and is skipped).
fn newest_temp_copy<T>(path: &Path, not_before: Option<SystemTime>, parse: &impl Fn(&[u8]) -> std::result::Result<T, String>) -> Option<T> {
    let mut temps = temp_siblings(path);
    temps.sort_by_key(|t| std::cmp::Reverse(t.1));
    temps
        .into_iter()
        .filter(|(_, t)| not_before.is_none_or(|nb| *t >= nb))
        .find_map(|(p, _)| std::fs::read(&p).ok().and_then(|b| parse(&b).ok()))
}

/// Result of [`load_recovering`].
#[derive(Debug, Default)]
pub struct Recovered<T> {
    /// The parsed value, if any file parsed.
    pub value: Option<T>,
    /// Which file it came from.
    pub source: LoadSource,
    /// Files that did not parse and were moved aside (new names). A file that could not
    /// be moved (read-only file system) is listed with its original name.
    pub set_aside: Vec<PathBuf>,
    /// Parse errors, one per bad file.
    pub errors: Vec<String>,
}

/// Loads `path` with `parse`; if it is missing or does not parse, tries its `.old`
/// backup. A file that does not parse is renamed to `<name>.corrupt-<secs>-<n>` so the
/// next save cannot overwrite the only copy of the data. Never fails on bad content;
/// I/O errors other than "not found" are returned.
///
/// A missing file is read again for up to 127 ms while a save of it may be running (see
/// `read_settled`); if it is still missing, a complete temporary copy newer than the
/// backup is used ([`LoadSource::Temp`]) before the backup. When there is no such copy
/// the file is read once more: a save that renamed its copy in place after the last read
/// but before the copies were listed has made the file appear.
pub fn load_recovering<T>(path: &Path, parse: impl Fn(&[u8]) -> std::result::Result<T, String>) -> Result<Recovered<T>> {
    let mut out = Recovered { value: None, source: LoadSource::Missing, set_aside: Vec::new(), errors: Vec::new() };
    // Takes the parsed bytes of `file`, or moves the file aside when they do not parse.
    let take = |out: &mut Recovered<T>, file: &Path, bytes: &[u8], source: LoadSource| match parse(bytes) {
        Ok(v) => {
            out.value = Some(v);
            out.source = source;
        }
        Err(e) => {
            out.errors.push(format!("{}: {e}", file.display()));
            out.set_aside.push(set_aside(file));
        }
    };
    let backup = backup_path(path);
    match read_settled(path, true)? {
        Some(bytes) => take(&mut out, path, &bytes, LoadSource::Primary),
        None => {
            let backup_time = std::fs::metadata(&backup).and_then(|m| m.modified()).ok();
            out.value = newest_temp_copy(path, backup_time, &parse);
            if out.value.is_some() {
                out.source = LoadSource::Temp;
            } else if let Ok(bytes) = std::fs::read(path) {
                take(&mut out, path, &bytes, LoadSource::Primary);
            }
        }
    }
    if out.value.is_none() {
        if let Some(bytes) = read_settled(&backup, false)? {
            take(&mut out, &backup, &bytes, LoadSource::Backup);
        }
    }
    Ok(out)
}

/// Renames a bad file to `<name>.corrupt-<secs>-<n>`; returns the new name, or the old
/// one if the rename failed.
pub fn set_aside(file: &Path) -> PathBuf {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dest = with_suffix(file, format!(".corrupt-{}-{n}", now_secs()));
    match std::fs::rename(file, &dest) {
        Ok(()) => dest,
        Err(_) => file.to_path_buf(),
    }
}

/// True for SQLite's "not a database" and "database disk image is malformed" errors: the
/// file is damaged, as opposed to locked, read-only or a newer schema.
pub fn is_damaged_sqlite(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if matches!(f.code, rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt))
}

/// Opens the SQLite database at `path` with `open`. A file that is not a database or is
/// damaged ([`is_damaged_sqlite`]) is moved aside with its `-journal` and a new database
/// is opened in its place; the damaged file's new name is returned. Other errors
/// (locked, read-only, a newer schema) are returned and nothing is moved.
pub(crate) fn open_db_or_recover<T>(path: &Path, open: impl Fn(&Path) -> Result<T>) -> Result<(T, Option<PathBuf>)> {
    match open(path) {
        Err(StoreError::Sqlite(e)) if is_damaged_sqlite(&e) => {
            let Some(moved) = set_aside_database(path) else { return Err(StoreError::Sqlite(e)) };
            Ok((open(path)?, Some(moved)))
        }
        r => r.map(|db| (db, None)),
    }
}

/// Creates the directory that holds `path`, if `path` has one.
pub(crate) fn create_parent_dir(path: &Path) -> std::io::Result<()> {
    match path.parent().filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => std::fs::create_dir_all(dir),
        None => Ok(()),
    }
}

/// Moves a damaged SQLite file (and its `-journal`) to `<name>.corrupt-<secs>-<n>`;
/// returns the new name, or `None` when the rename failed (nothing moved).
pub fn set_aside_database(path: &Path) -> Option<PathBuf> {
    let moved = set_aside(path);
    if moved == path {
        return None;
    }
    let journal = with_suffix(path, "-journal");
    if journal.exists() {
        let _ = std::fs::rename(&journal, with_suffix(&moved, "-journal"));
    }
    Some(moved)
}

/// Writes `bytes` to `path` atomically, first refreshing `<path>.old` from the current
/// file when the backup is missing or older than [`BACKUP_INTERVAL`] and the current
/// file passes `valid` (a corrupt file never replaces a good backup).
pub fn save_with_backup(path: &Path, bytes: &[u8], valid: impl Fn(&[u8]) -> bool) -> Result<()> {
    create_parent_dir(path)?;
    if path.exists() {
        let bak = backup_path(path);
        let stale = match std::fs::metadata(&bak).and_then(|m| m.modified()) {
            Ok(t) => SystemTime::now().duration_since(t).map(|age| age >= BACKUP_INTERVAL).unwrap_or(true),
            Err(_) => true,
        };
        if stale {
            if let Ok(cur) = std::fs::read(path) {
                if valid(&cur) {
                    write_atomic(&bak, &cur)?;
                }
            }
        }
    }
    write_atomic(path, bytes)?;
    remove_stale_temps(path);
    Ok(())
}

/// Removes temporary files of `path` (and of its backup) older than [`STALE_TEMP_AGE`]:
/// left by a process that died between writing and renaming. Called after a successful
/// save, when the file itself is newer than any of them. Best effort.
fn remove_stale_temps(path: &Path) {
    let now = SystemTime::now();
    for p in [path.to_path_buf(), backup_path(path)] {
        for (t, mtime) in temp_siblings(&p) {
            if now.duration_since(mtime).is_ok_and(|age| age >= STALE_TEMP_AGE) {
                let _ = std::fs::remove_file(t);
            }
        }
    }
}

/// Where rinky keeps its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// Install and data root (`/mnt/us/rinky` on the Kindle).
    pub root: PathBuf,
}

impl Paths {
    /// The Kindle layout.
    pub fn kindle() -> Self {
        Paths { root: ink_platform::Platform::Kindle.install_root() }
    }

    /// The layout of the e-reader this runs on (Kindle or Kobo).
    pub fn device() -> Self {
        Paths { root: ink_platform::current().install_root() }
    }

    /// `settings.toml`.
    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.toml")
    }

    /// `library.json`.
    pub fn library(&self) -> PathBuf {
        self.root.join("library.json")
    }

    /// `statistics.sqlite3`: rinky's reading statistics ([`stats::StatsDb`]).
    pub fn statistics(&self) -> PathBuf {
        self.root.join("statistics.sqlite3")
    }

    /// `battery.csv`: battery samples for the battery page (`ink_app::battery_log`).
    pub fn battery_log(&self) -> PathBuf {
        self.root.join("battery.csv")
    }

    /// Cache directory (covers, page maps).
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// Cover thumbnail directory.
    pub fn covers(&self) -> PathBuf {
        self.cache().join("covers")
    }

    /// Cover thumbnail of the book with partial MD5 `md5`:
    /// `cache/covers/<first two hex digits>/<md5>.png` (256 files per directory at most
    /// on average for 64k books, which keeps directory lookups fast). Upper-case hex is
    /// lowercased. Any other string (book ids come from files) gives
    /// `cache/covers/invalid/<MD5 of the string>.png`: the path always stays inside
    /// [`Paths::covers`].
    pub fn cover(&self, md5: &str) -> PathBuf {
        match crate::md5::normalize_md5(md5) {
            Some(m) => self.covers().join(&m[..2]).join(format!("{m}.png")),
            None => {
                use ::md5::{Digest, Md5};
                let h = Md5::digest(md5.as_bytes());
                self.covers().join("invalid").join(format!("{h:x}.png"))
            }
        }
    }

    /// Scratch directory on the user store (copies of KOReader databases while they
    /// are read; `/tmp` on the Kindle is a small RAM disk).
    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Highlight exports (`exports/<title>.md`, see [`export`]).
    pub fn exports(&self) -> PathBuf {
        self.root.join("exports")
    }

    /// Directory of plugins (`plugins/<name>/plugin.toml`, `main.lua`).
    pub fn plugins(&self) -> PathBuf {
        self.root.join("plugins")
    }

    /// Directory of plugin `name`.
    pub fn plugin_dir(&self, name: &str) -> PathBuf {
        self.plugins().join(name)
    }

    /// Data directory of plugin `name` (`plugins/<name>/data`).
    pub fn plugin_data(&self, name: &str) -> PathBuf {
        self.plugin_dir(name).join("data")
    }

    /// Settings file of plugin `name` (`plugins/<name>/settings.json`).
    pub fn plugin_settings(&self, name: &str) -> PathBuf {
        self.plugin_dir(name).join("settings.json")
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// A fresh, empty directory under the system temp dir, removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> TempDir {
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("ink-store-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Sets the modification time of `p` to `secs_ago` seconds ago, by path
    /// (`utimensat`): on the Kindle's FUSE user store (`fuse.fsp`) times set through a
    /// file handle (`futimens`) do not stick. Checks the result.
    pub fn set_mtime(p: &Path, secs_ago: u64) {
        use std::os::unix::ffi::OsStrExt;
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        let d = (SystemTime::now() - Duration::from_secs(secs_ago)).duration_since(UNIX_EPOCH).unwrap();
        let ts = libc::timespec { tv_sec: d.as_secs() as _, tv_nsec: d.subsec_nanos() as _ };
        let times = [ts, ts];
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a NUL-terminated path and `times` holds two timespecs, as
        // utimensat(2) requires; both outlive the call.
        let r = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(r, 0, "utimensat {}: {}", p.display(), std::io::Error::last_os_error());
        let got = std::fs::metadata(p).unwrap().modified().unwrap();
        let age = SystemTime::now().duration_since(got).unwrap_or_default().as_secs();
        assert!(age + 3 >= secs_ago && age <= secs_ago + 3, "{}: mtime {age} s ago, wanted {secs_ago}", p.display());
    }
}

#[cfg(test)]
mod verify_tests;

#[cfg(test)]
mod verify2_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use testutil::TempDir;

    #[test]
    fn atomic_write_and_paths() {
        let dir = TempDir::new("atomic");
        let f = dir.path().join("a.json");
        write_atomic(&f, b"1").unwrap();
        write_atomic(&f, b"2").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"2");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no temp files left");
        assert!(write_atomic(&dir.path().join("missing/x"), b"1").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no temp files left after a failure");
        let p = Paths::kindle();
        assert_eq!(p.plugin_data("stats"), PathBuf::from("/mnt/us/rinky/plugins/stats/data"));
        assert_eq!(p.plugin_settings("stats"), PathBuf::from("/mnt/us/rinky/plugins/stats/settings.json"));
        assert_eq!(p.settings(), PathBuf::from("/mnt/us/rinky/settings.toml"));
        assert_eq!(p.cover("cf7ce9b8d4e135c8ac340f18d1e58c85"), PathBuf::from("/mnt/us/rinky/cache/covers/cf/cf7ce9b8d4e135c8ac340f18d1e58c85.png"));
    }

    #[test]
    fn concurrent_atomic_writes_do_not_collide() {
        let dir = TempDir::new("atomic-threads");
        let f = dir.path().join("shared.json");
        std::thread::scope(|s| {
            for t in 0..8 {
                let f = &f;
                s.spawn(move || {
                    for i in 0..16 {
                        write_atomic(f, format!("{t}-{i}").as_bytes()).unwrap();
                    }
                });
            }
        });
        let last = std::fs::read_to_string(&f).unwrap();
        assert!(last.ends_with("-15"), "{last}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    fn parse_json(b: &[u8]) -> std::result::Result<serde_json::Value, String> {
        serde_json::from_slice::<serde_json::Value>(b).map_err(|e| e.to_string())
    }

    /// The file renamed over is missing (fuse.fsp during a rename, or a crash in that
    /// moment): a complete temporary copy newer than the backup is used, a partial or
    /// older one is not, and another file's temporary copy never is.
    #[test]
    fn missing_file_uses_a_complete_temp_copy() {
        let dir = TempDir::new("temp-copy");
        let f = dir.path().join("state.json");
        let valid = |b: &[u8]| parse_json(b).is_ok();
        save_with_backup(&f, b"{\"v\":1}", valid).unwrap();
        save_with_backup(&f, b"{\"v\":2}", valid).unwrap();
        std::fs::remove_file(&f).unwrap();
        // Someone else's files with similar names are never taken.
        std::fs::write(dir.path().join("state.json2.tmp.1.0"), b"{\"v\":20}").unwrap();
        std::fs::write(dir.path().join("other.json.tmp.1.0"), b"{\"v\":30}").unwrap();
        let tmp = with_suffix(&f, ".tmp.4242.7");
        std::fs::write(&tmp, b"{\"v\":3}").unwrap();
        let r = load_recovering(&f, parse_json).unwrap();
        assert_eq!((r.source, r.value.unwrap()["v"].as_i64()), (LoadSource::Temp, Some(3)));
        assert!(r.set_aside.is_empty() && tmp.exists(), "temporary files are left alone");
        // A partial temporary file (a save still writing it) is skipped: the backup.
        std::fs::write(&tmp, b"{\"v\":").unwrap();
        let r = load_recovering(&f, parse_json).unwrap();
        assert_eq!((r.source, r.value.unwrap()["v"].as_i64()), (LoadSource::Backup, Some(1)));
        // A complete one older than the backup is not newer data.
        std::fs::write(&tmp, b"{\"v\":0}").unwrap();
        testutil::set_mtime(&tmp, 3600);
        let r = load_recovering(&f, parse_json).unwrap();
        assert_eq!((r.source, r.value.unwrap()["v"].as_i64()), (LoadSource::Backup, Some(1)));
        // The next save removes left-overs older than STALE_TEMP_AGE, not young ones.
        let young = with_suffix(&f, ".tmp.4243.0");
        std::fs::write(&young, b"{").unwrap();
        save_with_backup(&f, b"{\"v\":4}", valid).unwrap();
        assert!(!tmp.exists() && young.exists());
        assert!(dir.path().join("other.json.tmp.1.0").exists());
    }

    /// A load that finds the file missing while a save of it is running reads it again
    /// instead of returning the older backup.
    #[test]
    fn load_waits_for_a_save_in_progress() {
        let dir = TempDir::new("settle");
        let f = dir.path().join("state.json");
        let valid = |b: &[u8]| parse_json(b).is_ok();
        save_with_backup(&f, b"{\"v\":1}", valid).unwrap();
        save_with_backup(&f, b"{\"v\":2}", valid).unwrap();
        // The moment inside a rename on fuse.fsp: no file, its temporary file present.
        let tmp = with_suffix(&f, ".tmp.4242.0");
        std::fs::rename(&f, &tmp).unwrap();
        std::fs::write(&tmp, b"{\"v\":").unwrap(); // still being written: not usable
        let before = load_waits();
        // The save finishes while the load waits for it.
        on_settle_wait({
            let (f, tmp) = (f.clone(), tmp.clone());
            move || {
                std::fs::write(&tmp, b"{\"v\":3}").unwrap();
                std::fs::rename(&tmp, &f).unwrap();
            }
        });
        let r = load_recovering(&f, parse_json).unwrap();
        assert_eq!((r.source, r.value.unwrap()["v"].as_i64()), (LoadSource::Primary, Some(3)));
        assert!(load_waits() > before);
        // A file that never existed (no backup, no temporary file) is not waited for.
        // Counted on this thread: a time bound below the 127 ms of waits can be missed on
        // a busy machine, and the process-wide count includes other tests' loads.
        let here = SETTLE_WAITS_HERE.with(|w| w.get());
        assert_eq!(load_recovering(&dir.path().join("new.json"), parse_json).unwrap().source, LoadSource::Missing);
        assert_eq!(SETTLE_WAITS_HERE.with(|w| w.get()), here, "a missing file with nothing pending is not waited for");
    }

    /// A save that renames its copy into place after the load's last read, while the
    /// load tries the temporary copies, is still found: the file is read once more
    /// before the older backup is used.
    #[test]
    fn a_rename_during_the_temp_scan_is_not_missed() {
        let dir = TempDir::new("rename-late");
        let f = dir.path().join("state.json");
        let valid = |b: &[u8]| parse_json(b).is_ok();
        save_with_backup(&f, b"{\"v\":1}", valid).unwrap();
        save_with_backup(&f, b"{\"v\":2}", valid).unwrap();
        std::fs::remove_file(&f).unwrap();
        // A partial temporary copy: the load waits for it, then tries it, and the save
        // finishes while it does.
        std::fs::write(with_suffix(&f, ".tmp.4242.0"), b"{\"v\":").unwrap();
        let parse = |b: &[u8]| {
            if b == b"{\"v\":" && !f.exists() {
                std::fs::write(&f, b"{\"v\":3}").unwrap();
            }
            parse_json(b)
        };
        let r = load_recovering(&f, parse).unwrap();
        assert_eq!((r.source, r.value.unwrap()["v"].as_i64()), (LoadSource::Primary, Some(3)));
    }

    #[test]
    fn cover_paths_stay_in_the_cache() {
        let p = Paths::kindle();
        assert_eq!(p.cover("CF7CE9B8D4E135C8AC340F18D1E58C85"), p.cover("cf7ce9b8d4e135c8ac340f18d1e58c85"));
        for bad in ["", "a", "../../x", "/etc/passwd", "ab/../../c", ".."] {
            let c = p.cover(bad);
            assert!(c.starts_with(p.covers().join("invalid")) && c.components().count() == p.covers().components().count() + 2, "{bad:?} -> {}", c.display());
        }
    }

    #[test]
    fn recovery_and_backup_rotation() {
        let dir = TempDir::new("recover");
        let f = dir.path().join("state.json");
        let parse = |b: &[u8]| serde_json::from_slice::<serde_json::Value>(b).map_err(|e| e.to_string());
        let valid = |b: &[u8]| parse(b).is_ok();
        // Missing: nothing.
        let r = load_recovering(&f, parse).unwrap();
        assert_eq!((r.value.is_none(), r.source), (true, LoadSource::Missing));
        // First save: no backup yet (no previous file).
        save_with_backup(&f, b"{\"v\":1}", valid).unwrap();
        assert!(!backup_path(&f).exists());
        // Second save: the previous good file becomes the backup.
        save_with_backup(&f, b"{\"v\":2}", valid).unwrap();
        assert_eq!(std::fs::read(backup_path(&f)).unwrap(), b"{\"v\":1}");
        // Third save within the interval: the backup is not refreshed.
        save_with_backup(&f, b"{\"v\":3}", valid).unwrap();
        assert_eq!(std::fs::read(backup_path(&f)).unwrap(), b"{\"v\":1}");
        // Corrupt primary: moved aside, backup used.
        std::fs::write(&f, b"{\"v\":").unwrap();
        let r = load_recovering(&f, parse).unwrap();
        assert_eq!(r.source, LoadSource::Backup);
        assert_eq!(r.value.unwrap()["v"], 1);
        assert_eq!(r.set_aside.len(), 1);
        assert!(r.set_aside[0].to_string_lossy().contains(".corrupt-"));
        assert_eq!(std::fs::read(&r.set_aside[0]).unwrap(), b"{\"v\":");
        assert!(!f.exists());
        // A corrupt file never replaces a good backup.
        std::fs::write(&f, b"").unwrap();
        let old = backup_path(&f);
        testutil::set_mtime(&old, 3600);
        save_with_backup(&f, b"{\"v\":4}", valid).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"{\"v\":1}");
        assert_eq!(std::fs::read(&f).unwrap(), b"{\"v\":4}");
    }
}
