//! In-app update from a GitHub release (phase 11; owner: the `packaging` agent).
//!
//! [`update`] does, in order:
//!
//! 1. GET the release description from the configured URL. The format is GitHub's
//!    `GET /repos/OWNER/REPO/releases/latest` answer: `tag_name` and
//!    `assets[].{name, browser_download_url, size, digest}`.
//! 2. Pick the package asset `rinky-<version>-kindlehf.tar.gz`, on a Kobo
//!    `rinky-<version>-kobo.tar.gz` (both built by `scripts/package.sh`), and find its
//!    SHA-256: the `SHA256SUMS` asset of the release
//!    and/or the asset's `digest` (`sha256:<hex>`, which GitHub fills in). No checksum:
//!    refuse. Both present and different: refuse.
//! 3. Download the package with a size limit, hashing while writing. A wrong size or
//!    hash deletes the download and refuses.
//! 4. Unpack it (gzip + ustar) with checks: only regular files and directories under
//!    `rinky/` and `extensions/rinky/` (`nm/rinky` in the Kobo package), no absolute
//!    paths, no `..`. The file is hashed again while it is unpacked and must still have
//!    the expected SHA-256; the
//!    gzip CRC is checked too. The package's `rinky/VERSION` must name the release's
//!    version.
//! 5. Run the package's own `rinky/install.sh --upgrade`, the same installer as a
//!    first install. It copies every file next to its target first and renames the
//!    copies into place only when all copies succeeded, so the running binary is not
//!    disturbed and a failed copy (no space left) leaves the old version installed.
//!    The new version runs after the next start; the Update page asks the launcher for
//!    that start at once ([`request_restart`]).
//!
//! Every failure leaves the installed files as one version: the old one, unless the
//! installer failed while renaming (it then says so). The download and the unpacked copy
//! are deleted on every path.
//!
//! Steps 3 to 5 ([`install`]) hold an exclusive lock, `flock` on `<work_dir>.lock`
//! (`/mnt/us/rinky/.update.lock`), so a second update at the same time (KUAL tapped
//! twice, KUAL plus SSH, two threads) fails at once with [`UpdateError::Busy`]. The lock
//! file is removed when the update ends.
//!
//! The release URL comes from `--url`, else `RINKY_UPDATE_URL`, else
//! `[plugins.update] url` in `settings.toml`, else [`DEFAULT_RELEASE_URL`]. Plain
//! `http://` is refused unless `allow_http` is set (`--allow-http` or
//! `[plugins.update] allow_http = true`), for local mirrors and the mock server.
//!
//! Command line: `rinky update [check|install] [--url URL] [--allow-http] [--force]`
//! ([`cli`]). The KUAL menu calls `rinky update install`.

use ink_net::{HttpClient, NetError, Request, Response};
use ink_store::{Paths, Settings};
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Where releases are looked up unless configured otherwise. Release builds set
/// `RINKY_RELEASE_URL` at compile time (the GitHub workflow: the repository it runs in),
/// so a fork's packages update from the fork.
pub const DEFAULT_RELEASE_URL: &str = match option_env!("RINKY_RELEASE_URL") {
    Some(url) => url,
    None => "https://api.github.com/repos/jyliuu/rinky/releases/latest",
};
/// Name of the checksum asset (`sha256sum` format).
pub const SUMS_ASSET: &str = "SHA256SUMS";
/// Package asset names start with this ...
pub const PACKAGE_PREFIX: &str = "rinky-";
/// ... and end with this (the Kindle package) ...
pub const PACKAGE_SUFFIX: &str = "-kindlehf.tar.gz";
/// ... or this, on a Kobo.
pub const KOBO_PACKAGE_SUFFIX: &str = "-kobo.tar.gz";

/// The package suffix of the e-reader this runs on.
pub fn package_suffix() -> &'static str {
    if ink_store::platform::on_kobo() {
        KOBO_PACKAGE_SUFFIX
    } else {
        PACKAGE_SUFFIX
    }
}
/// Default download limit for the package.
pub const MAX_PACKAGE_BYTES: u64 = 64 << 20;
/// Limit for the unpacked package.
pub const MAX_UNPACKED_BYTES: u64 = 256 << 20;
const MAX_JSON_BYTES: usize = 4 << 20;
const MAX_SUMS_BYTES: usize = 64 << 10;

/// Why an update did not happen.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// URL or settings problem.
    #[error("update configuration: {0}")]
    Config(String),
    /// Connection, TLS or timeout.
    #[error("{url}: {source}")]
    Net {
        /// The URL requested.
        url: String,
        /// What went wrong.
        #[source]
        source: NetError,
    },
    /// The server answered with a non-2xx status.
    #[error("{url}: HTTP status {status}")]
    Status {
        /// The URL requested.
        url: String,
        /// Status code.
        status: u16,
    },
    /// The release description is not usable.
    #[error("release: {0}")]
    Release(String),
    /// Neither `SHA256SUMS` nor a digest names the package's SHA-256.
    #[error("release {tag} has no SHA-256 for {name}; update refused")]
    NoChecksum {
        /// Release tag.
        tag: String,
        /// Package asset name.
        name: String,
    },
    /// The downloaded package does not have the expected SHA-256.
    #[error("checksum mismatch for {name}: expected {expected}, got {actual}; update refused")]
    ChecksumMismatch {
        /// Package asset name.
        name: String,
        /// SHA-256 from the release.
        expected: String,
        /// SHA-256 of the download.
        actual: String,
    },
    /// The download is larger than allowed.
    #[error("{name} is larger than {limit} bytes; update refused")]
    TooLarge {
        /// Asset name.
        name: String,
        /// The limit.
        limit: u64,
    },
    /// The download has a different size than the release says.
    #[error("{name}: downloaded {got} bytes, the release says {expected}; update refused")]
    SizeMismatch {
        /// Asset name.
        name: String,
        /// Size from the release.
        expected: u64,
        /// Bytes received.
        got: u64,
    },
    /// The archive has entries outside the allowed layout.
    #[error("unsafe package: {0}; update refused")]
    UnsafeArchive(String),
    /// `install.sh --upgrade` failed.
    #[error("installer failed: {0}")]
    Install(String),
    /// Another update holds the update lock (the lock file is named).
    #[error("another update is running ({0}); try again when it has finished")]
    Busy(String),
    /// The release is not newer than the running version.
    #[error("{available} is not newer than the installed {current}")]
    NotNewer {
        /// Running version.
        current: Version,
        /// Release version.
        available: Version,
    },
    /// File system error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result alias for this module.
pub type Result<T, E = UpdateError> = std::result::Result<T, E>;

/// A release version: `MAJOR.MINOR.PATCH[-PRE][+BUILD]`, with an optional leading `v`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    /// Major.
    pub major: u64,
    /// Minor.
    pub minor: u64,
    /// Patch.
    pub patch: u64,
    /// Pre-release label (`rc.1`); empty for a release. Sorts before the release.
    pub pre: String,
}

impl Version {
    /// Parses `v0.3.0`, `0.3`, `1.2.3-rc.1`, `1.2.3+build`; `None` otherwise.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        let s = s.split('+').next()?;
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, p.to_string()),
            Some(_) => return None,
            None => (s, String::new()),
        };
        let mut it = core.split('.');
        let num = |p: Option<&str>| -> Option<u64> {
            match p {
                None => Some(0),
                Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok(),
                Some(_) => None,
            }
        };
        let major = num(Some(it.next()?))?;
        let minor = num(it.next())?;
        let patch = num(it.next())?;
        if it.next().is_some() {
            return None;
        }
        Some(Version { major, minor, patch, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch).cmp(&(o.major, o.minor, o.patch)).then_with(|| {
            match (self.pre.is_empty(), o.pre.is_empty()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => self.pre.cmp(&o.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

/// Asks the launcher to run the installed rinky again when this one exits, without
/// starting the Kindle UI in between: writes `$RINKY_RUN/restart`, as
/// `rinky-ctl.sh restart` does. False (nothing written) when rinky was not started by
/// a launcher that can restart (no `$RINKY_RUN/restart-capable`): the user then exits
/// and starts rinky again.
pub fn request_restart() -> bool {
    std::env::var_os("RINKY_RUN").is_some_and(|run| request_restart_in(Path::new(&run)))
}

/// [`request_restart`] with the launcher's run directory `run`.
pub fn request_restart_in(run: &Path) -> bool {
    if !run.join("restart-capable").exists() {
        log::info!("update: the launcher cannot restart rinky ({} missing)", run.join("restart-capable").display());
        return false;
    }
    match std::fs::write(run.join("restart"), b"") {
        Ok(()) => {
            log::info!("update: installed; the launcher restarts rinky into it");
            true
        }
        Err(e) => {
            log::warn!("update: cannot ask for a restart ({e})");
            false
        }
    }
}

/// The version of this binary.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is a version")
}

/// One release asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    /// File name.
    pub name: String,
    /// Download URL (`browser_download_url`).
    pub url: String,
    /// Size in bytes, if the release says.
    pub size: Option<u64>,
    /// Lowercase hex SHA-256 from the asset's `digest` (`sha256:<hex>`), if present.
    pub sha256: Option<String>,
}

/// What a release offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Tag name (`v0.3.0`).
    pub tag: String,
    /// Version parsed from the tag.
    pub version: Version,
    /// The Kindle package.
    pub package: Asset,
    /// The `SHA256SUMS` asset, if present.
    pub sums: Option<Asset>,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// True for `rinky-<version>-kindlehf.tar.gz` (on a Kobo `rinky-<version>-kobo.tar.gz`)
/// with a plain file name.
pub fn is_package_name(name: &str) -> bool {
    let suffix = package_suffix();
    name.len() > PACKAGE_PREFIX.len() + suffix.len()
        && name.starts_with(PACKAGE_PREFIX)
        && name.ends_with(suffix)
        && !name.contains("..")
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Parses a GitHub release description.
pub fn parse_release(body: &[u8]) -> Result<Release> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|e| UpdateError::Release(format!("not JSON: {e}")))?;
    if v.get("draft").and_then(|d| d.as_bool()) == Some(true) {
        return Err(UpdateError::Release("the release is a draft".into()));
    }
    let tag = v.get("tag_name").and_then(|t| t.as_str()).ok_or_else(|| UpdateError::Release("no tag_name".into()))?;
    let version = Version::parse(tag).ok_or_else(|| UpdateError::Release(format!("tag {tag:?} is not a version")))?;
    let list = v.get("assets").and_then(|a| a.as_array()).ok_or_else(|| UpdateError::Release("no assets".into()))?;
    let mut assets = Vec::new();
    for a in list {
        let (Some(name), Some(url)) = (a.get("name").and_then(|n| n.as_str()), a.get("browser_download_url").and_then(|u| u.as_str())) else {
            continue;
        };
        let sha256 = a
            .get("digest")
            .and_then(|d| d.as_str())
            .and_then(|d| d.strip_prefix("sha256:"))
            .filter(|h| is_hex64(h))
            .map(|h| h.to_ascii_lowercase());
        assets.push(Asset { name: name.to_string(), url: url.to_string(), size: a.get("size").and_then(|s| s.as_u64()), sha256 });
    }
    let mut packages: Vec<&Asset> = assets.iter().filter(|a| is_package_name(&a.name)).collect();
    // Prefer the package whose name carries the tag's version.
    let suffix = package_suffix();
    let exact = format!("{PACKAGE_PREFIX}{version}{suffix}");
    if packages.len() > 1 {
        packages.retain(|a| a.name == exact);
    }
    let package = match packages.as_slice() {
        [one] => (*one).clone(),
        [] => return Err(UpdateError::Release(format!("{tag} has no asset named {PACKAGE_PREFIX}<version>{suffix}"))),
        _ => return Err(UpdateError::Release(format!("{tag} has several {} packages", ink_store::platform::device_name()))),
    };
    let sums = assets.iter().find(|a| a.name == SUMS_ASSET).cloned();
    Ok(Release { tag: tag.to_string(), version, package, sums })
}

/// Parses `sha256sum` output: `(file name, lowercase hex)` per valid line.
pub fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (hash, name) = line.split_once(char::is_whitespace)?;
            let name = name.trim_start();
            let name = name.strip_prefix('*').unwrap_or(name);
            let name = name.strip_prefix("./").unwrap_or(name);
            (is_hex64(hash) && !name.is_empty()).then(|| (name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// Update settings.
#[derive(Clone, Debug)]
pub struct UpdateConfig {
    /// Release description URL.
    pub release_url: String,
    /// Accept `http://` URLs (local mirrors, tests).
    pub allow_http: bool,
    /// Scratch directory for the download and the unpacked package (`<root>/.update`).
    pub work_dir: PathBuf,
    /// Download limit for the package.
    pub max_bytes: u64,
    /// Timeout for the package download.
    pub download_timeout: Duration,
    /// Extra environment for `install.sh` (tests: `RINKY_PREFIX`, `RINKY_RUN`).
    pub install_env: Vec<(String, String)>,
}

impl UpdateConfig {
    /// Defaults for `release_url`, with the scratch directory `work_dir`.
    pub fn new(release_url: impl Into<String>, work_dir: impl Into<PathBuf>) -> Self {
        UpdateConfig {
            release_url: release_url.into(),
            allow_http: false,
            work_dir: work_dir.into(),
            max_bytes: MAX_PACKAGE_BYTES,
            download_timeout: Duration::from_secs(600),
            install_env: Vec::new(),
        }
    }

    /// From `RINKY_UPDATE_URL`, then `[plugins.update]` (`url`, `allow_http`) in the
    /// settings, then [`DEFAULT_RELEASE_URL`]. The scratch directory is `<root>/.update`.
    pub fn from_settings(settings: &Settings, paths: &Paths) -> Self {
        let table = settings.plugins.get("update").and_then(|v| v.as_table());
        let from_settings = table.and_then(|t| t.get("url")).and_then(|v| v.as_str()).map(str::to_string);
        let url = std::env::var("RINKY_UPDATE_URL").ok().filter(|u| !u.is_empty()).or(from_settings);
        let mut cfg = UpdateConfig::new(url.unwrap_or_else(|| DEFAULT_RELEASE_URL.to_string()), paths.root.join(".update"));
        cfg.allow_http = table.and_then(|t| t.get("allow_http")).and_then(|v| v.as_bool()).unwrap_or(false);
        cfg
    }

    fn check_url(&self, url: &str) -> Result<()> {
        if url.starts_with("https://") || (self.allow_http && url.starts_with("http://")) {
            Ok(())
        } else if url.starts_with("http://") {
            Err(UpdateError::Config(format!("{url}: plain http is refused (set allow_http for a local mirror)")))
        } else {
            Err(UpdateError::Config(format!("{url}: not an https URL")))
        }
    }
}

fn get(client: &dyn HttpClient, cfg: &UpdateConfig, url: &str, accept: &str, timeout: Duration) -> Result<Response> {
    cfg.check_url(url)?;
    let req = Request::get(url).header("Accept", accept).timeout(timeout);
    let resp = client.send(req).map_err(|source| UpdateError::Net { url: url.to_string(), source })?;
    if !resp.ok() {
        return Err(UpdateError::Status { url: url.to_string(), status: resp.status });
    }
    Ok(resp)
}

fn net_err(url: &str) -> impl FnOnce(NetError) -> UpdateError + '_ {
    move |source| UpdateError::Net { url: url.to_string(), source }
}

/// Fetches and parses the release description.
pub fn fetch_release(client: &dyn HttpClient, cfg: &UpdateConfig) -> Result<Release> {
    let url = cfg.release_url.as_str();
    let body = get(client, cfg, url, "application/vnd.github+json", Duration::from_secs(30))?.bytes(MAX_JSON_BYTES).map_err(net_err(url))?;
    parse_release(&body)
}

/// The package's SHA-256 from `SHA256SUMS` and/or the asset digest; refuses without one.
pub fn expected_sha256(client: &dyn HttpClient, cfg: &UpdateConfig, release: &Release) -> Result<String> {
    let name = &release.package.name;
    let mut expected = None;
    if let Some(sums) = &release.sums {
        let text = get(client, cfg, &sums.url, "application/octet-stream", Duration::from_secs(60))?
            .text(MAX_SUMS_BYTES)
            .map_err(net_err(&sums.url))?;
        match parse_sums(&text).into_iter().find(|(n, _)| n == name) {
            Some((_, h)) => expected = Some(h),
            None => return Err(UpdateError::NoChecksum { tag: release.tag.clone(), name: name.clone() }),
        }
    }
    if let Some(digest) = &release.package.sha256 {
        match &expected {
            Some(e) if e != digest => {
                return Err(UpdateError::Release(format!("{SUMS_ASSET} ({e}) and the asset digest ({digest}) disagree for {name}")))
            }
            Some(_) => {}
            None => expected = Some(digest.clone()),
        }
    }
    expected.ok_or_else(|| UpdateError::NoChecksum { tag: release.tag.clone(), name: name.clone() })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(hex(ctx.finish().as_ref()))
}

/// Downloads the package into `work_dir` and checks its size and SHA-256 against
/// `expected`. Returns the verified file; deletes the download on any failure.
pub fn download_verified(client: &dyn HttpClient, cfg: &UpdateConfig, release: &Release, expected: &str) -> Result<PathBuf> {
    let asset = &release.package;
    if !is_package_name(&asset.name) {
        return Err(UpdateError::Release(format!("bad package name {:?}", asset.name)));
    }
    std::fs::create_dir_all(&cfg.work_dir)?;
    let part = cfg.work_dir.join(format!("{}.part", asset.name));
    let dest = cfg.work_dir.join(&asset.name);
    if let Err(e) = fetch_checked(client, cfg, asset, expected, &part) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

/// Streams `asset` into `part`, hashing on the way; checks size and SHA-256.
fn fetch_checked(client: &dyn HttpClient, cfg: &UpdateConfig, asset: &Asset, expected: &str, part: &Path) -> Result<()> {
    let mut resp = get(client, cfg, &asset.url, "application/octet-stream", cfg.download_timeout)?;
    let mut file = File::create(part)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 << 10];
    let mut total = 0u64;
    loop {
        let n = resp.body.read(&mut buf).map_err(|e| UpdateError::Net { url: asset.url.clone(), source: NetError::Io(e) })?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > cfg.max_bytes {
            return Err(UpdateError::TooLarge { name: asset.name.clone(), limit: cfg.max_bytes });
        }
        ctx.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }
    file.sync_all()?;
    if let Some(size) = asset.size {
        if size != total {
            return Err(UpdateError::SizeMismatch { name: asset.name.clone(), expected: size, got: total });
        }
    }
    let actual = hex(ctx.finish().as_ref());
    let expected = expected.to_ascii_lowercase();
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch { name: asset.name.clone(), expected, actual });
    }
    Ok(())
}

/// Hashes everything read through it.
struct HashingReader<R> {
    inner: R,
    ctx: ring::digest::Context,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.ctx.update(&buf[..n]);
        Ok(n)
    }
}

/// Checks one archive entry name; returns it without `./` and trailing `/`.
fn safe_entry_path(name: &str) -> Result<String> {
    let bad = |why: &str| Err(UpdateError::UnsafeArchive(format!("entry {name:?}: {why}")));
    if name.starts_with('/') {
        return bad("absolute path");
    }
    if name.contains('\\') || name.contains('\0') {
        return bad("bad character");
    }
    let n = name.strip_prefix("./").unwrap_or(name).trim_end_matches('/');
    let parts: Vec<&str> = n.split('/').collect();
    if n.is_empty() || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return bad("empty, . or .. component");
    }
    if !matches!(parts.as_slice(), ["rinky", ..] | ["extensions"] | ["extensions", "rinky", ..] | ["nm"] | ["nm", "rinky"]) {
        return bad("outside rinky/, extensions/rinky/ and nm/rinky");
    }
    Ok(n.to_string())
}

fn field_str(field: &[u8]) -> Result<&str> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).map_err(|_| UpdateError::UnsafeArchive("entry name is not UTF-8".into()))
}

fn field_octal(field: &[u8]) -> Result<u64> {
    let s = field_str(field)?.trim_matches(|c: char| c == ' ' || c == '\0');
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|_| UpdateError::UnsafeArchive(format!("bad number field {s:?}")))
}

/// Unpacks a gzip-compressed ustar archive into `dest` (created fresh). Only regular
/// files and directories under `rinky/` and `extensions/rinky/` are accepted;
/// links, devices, pax/GNU extension headers, absolute paths and `..` are refused. The
/// gzip stream is read to its end, so its CRC-32 and length are checked.
/// Returns the file paths, relative to `dest`.
pub fn unpack(archive: &Path, dest: &Path, max_unpacked: u64) -> Result<Vec<String>> {
    unpack_inner(archive, dest, max_unpacked, None)
}

/// [`unpack`], and the archive file must have the SHA-256 `expected` (lowercase or
/// uppercase hex). The hash is taken over the bytes that are unpacked, so the unpacked
/// files come from exactly the verified file. On a mismatch `dest` is deleted.
pub fn unpack_verified(archive: &Path, dest: &Path, max_unpacked: u64, expected: &str) -> Result<Vec<String>> {
    unpack_inner(archive, dest, max_unpacked, Some(expected))
}

fn unpack_inner(archive: &Path, dest: &Path, max_unpacked: u64, expected: Option<&str>) -> Result<Vec<String>> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let raw = HashingReader { inner: File::open(archive)?, ctx: ring::digest::Context::new(&ring::digest::SHA256) };
    // flate2's read::GzDecoder buffers internally; every byte it takes from `raw` is hashed.
    let mut gz = flate2::read::GzDecoder::new(raw);
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut header = [0u8; 512];
    loop {
        if let Err(e) = gz.read_exact(&mut header) {
            return Err(UpdateError::UnsafeArchive(format!("truncated archive ({e})")));
        }
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let stored = field_octal(&header[148..156])?;
        let sum: u64 = header.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as u64 }).sum();
        if stored != sum {
            return Err(UpdateError::UnsafeArchive("bad tar header checksum".into()));
        }
        let mut name = field_str(&header[0..100])?.to_string();
        if &header[257..263] == b"ustar\0" {
            let prefix = field_str(&header[345..500])?;
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        let size = field_octal(&header[124..136])?;
        let mode = field_octal(&header[100..108])?;
        let kind = header[156];
        match kind {
            b'0' | 0 => {
                let rel = safe_entry_path(&name)?;
                if name.ends_with('/') {
                    return Err(UpdateError::UnsafeArchive(format!("file entry {name:?} ends with /")));
                }
                total += size;
                if total > max_unpacked {
                    return Err(UpdateError::UnsafeArchive(format!("more than {max_unpacked} bytes unpacked")));
                }
                let path = dest.join(&rel);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out = File::create(&path)?;
                let copied = std::io::copy(&mut (&mut gz).take(size), &mut out)?;
                if copied != size {
                    return Err(UpdateError::UnsafeArchive(format!("{rel}: truncated")));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let m = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(m))?;
                }
                #[cfg(not(unix))]
                let _ = mode;
                files.push(rel);
            }
            b'5' => {
                let rel = safe_entry_path(&name)?;
                std::fs::create_dir_all(dest.join(rel))?;
            }
            b'x' | b'g' => return Err(UpdateError::UnsafeArchive(format!("pax header for {name:?} (package must be plain ustar)"))),
            other => return Err(UpdateError::UnsafeArchive(format!("entry {name:?} has type {:?} (only files and directories)", other as char))),
        }
        // Data is padded to a whole 512-byte block.
        let pad = (512 - size % 512) % 512;
        if kind == b'5' {
            std::io::copy(&mut (&mut gz).take(size), &mut std::io::sink())?;
        }
        std::io::copy(&mut (&mut gz).take(pad), &mut std::io::sink())?;
    }
    // The rest of the gzip stream (end blocks, record padding); flate2 checks the CRC-32
    // and the length when it reaches the end. Bounded, like the entries.
    let rest = max_unpacked.saturating_sub(total) + (1 << 20);
    let gz_err = |e: std::io::Error| UpdateError::UnsafeArchive(format!("gzip stream: {e}"));
    std::io::copy(&mut (&mut gz).take(rest), &mut std::io::sink()).map_err(gz_err)?;
    if gz.read(&mut [0u8; 1]).map_err(gz_err)? != 0 {
        return Err(UpdateError::UnsafeArchive("too much data after the end of the archive".into()));
    }
    if let Some(expected) = expected {
        let mut raw = gz.into_inner();
        std::io::copy(&mut raw, &mut std::io::sink())?;
        let actual = hex(raw.ctx.finish().as_ref());
        let expected = expected.to_ascii_lowercase();
        if actual != expected {
            let _ = std::fs::remove_dir_all(dest);
            let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            return Err(UpdateError::ChecksumMismatch { name, expected, actual });
        }
    }
    Ok(files)
}

/// Checks an unpacked package: installer, binary and a VERSION naming `version`.
pub fn check_unpacked(root: &Path, version: &Version) -> Result<()> {
    for f in ["rinky/install.sh", "rinky/rinky", "rinky/VERSION", "rinky/FILES"] {
        if !root.join(f).is_file() {
            return Err(UpdateError::UnsafeArchive(format!("{f} is missing")));
        }
    }
    let text = std::fs::read_to_string(root.join("rinky/VERSION"))?;
    let v = text.lines().find_map(|l| l.strip_prefix("version=")).and_then(Version::parse);
    match v {
        Some(v) if &v == version => Ok(()),
        Some(v) => Err(UpdateError::Release(format!("the package says version {v}, the release says {version}"))),
        None => Err(UpdateError::Release("the package's VERSION has no version= line".into())),
    }
}

/// Runs `<root>/rinky/install.sh --upgrade`; returns its output.
pub fn run_installer(cfg: &UpdateConfig, root: &Path) -> Result<String> {
    let out = Command::new("sh")
        .arg(root.join("rinky/install.sh"))
        .arg("--upgrade")
        .envs(cfg.install_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .output()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        let tail: Vec<&str> = text.lines().rev().take(5).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(UpdateError::Install(format!("{} ({})", tail.join(" / "), out.status)));
    }
    Ok(text)
}

/// Result of [`update`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing newer.
    UpToDate {
        /// Running version.
        current: Version,
        /// Latest release.
        latest: Version,
    },
    /// Installed; takes effect after a restart of rinky.
    Installed {
        /// The installed version.
        version: Version,
        /// Installer output.
        log: String,
    },
}

/// Looks up the latest release; `true` when it is newer than this binary.
pub fn check(client: &dyn HttpClient, cfg: &UpdateConfig) -> Result<(Release, bool)> {
    let release = fetch_release(client, cfg)?;
    let newer = release.version > current_version();
    Ok((release, newer))
}

/// The update lock: an exclusive `flock` on `<work_dir>.lock`, so one update runs at a
/// time, across processes and threads (each acquire opens the file anew). The file is
/// removed on release while it is still locked; a run that locked a file that was just
/// removed sees that the path names another file (or none) and tries again.
struct UpdateLock {
    path: PathBuf,
    _file: File,
}

impl UpdateLock {
    fn acquire(work_dir: &Path) -> Result<UpdateLock> {
        let (Some(parent), Some(name)) = (work_dir.parent(), work_dir.file_name()) else {
            return Err(UpdateError::Config(format!("{}: not a usable scratch folder", work_dir.display())));
        };
        let mut lock_name = name.to_os_string();
        lock_name.push(".lock");
        let path = parent.join(lock_name);
        std::fs::create_dir_all(parent)?;
        for _ in 0..16 {
            let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?;
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Err(UpdateError::Busy(path.display().to_string())),
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
            if same_file(&file, &path) {
                // The owner's pid, for a person looking at the file; not used for locking.
                let _ = file.set_len(0);
                let _ = writeln!(&file, "{}", std::process::id());
                return Ok(UpdateLock { path, _file: file });
            }
        }
        Err(UpdateError::Busy(path.display().to_string()))
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        // `_file` is closed (and the lock released) after this, so nobody can lock this
        // path's file between the removal and the release.
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::symlink_metadata(path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_file: &File, path: &Path) -> bool {
    path.is_file()
}

/// Removes what an update left in `work_dir`: the `package` folder and downloads
/// (`rinky-*-kindlehf.tar.gz` and `*.part`). Other files are left alone. Called with
/// the lock held, so nothing in there belongs to a running update.
fn clean_scratch(work_dir: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(work_dir) else {
        return Ok(());
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if name == "package" && is_dir {
            std::fs::remove_dir_all(e.path())?;
        } else if name == "package" || name.ends_with(".part") || is_package_name(&name) {
            std::fs::remove_file(e.path())?;
        }
    }
    Ok(())
}

/// Downloads, verifies, unpacks and installs `release` (no version comparison), holding
/// the update lock. Fails at once with [`UpdateError::Busy`] while another update runs.
/// Leftovers of an interrupted update are removed first; the scratch files in
/// `work_dir` are removed on every path.
pub fn install(client: &dyn HttpClient, cfg: &UpdateConfig, release: &Release) -> Result<String> {
    let _lock = UpdateLock::acquire(&cfg.work_dir)?;
    clean_scratch(&cfg.work_dir)?;
    let staged = cfg.work_dir.join("package");
    let result = install_staged(client, cfg, release, &staged);
    let _ = clean_scratch(&cfg.work_dir);
    let _ = std::fs::remove_dir(&cfg.work_dir);
    result
}

fn install_staged(client: &dyn HttpClient, cfg: &UpdateConfig, release: &Release, staged: &Path) -> Result<String> {
    let expected = expected_sha256(client, cfg, release)?;
    let archive = download_verified(client, cfg, release, &expected)?;
    // The download was hashed while it arrived; the file is hashed again while it is
    // unpacked, so the installer gets exactly the verified bytes.
    let unpacked = unpack_verified(&archive, staged, MAX_UNPACKED_BYTES, &expected);
    let _ = std::fs::remove_file(&archive);
    unpacked?;
    check_unpacked(staged, &release.version)?;
    run_installer(cfg, staged)
}

/// Installs the latest release when it is newer than this binary (or always with `force`).
pub fn update(client: &dyn HttpClient, cfg: &UpdateConfig, force: bool) -> Result<Outcome> {
    let (release, newer) = check(client, cfg)?;
    if !newer && !force {
        return Ok(Outcome::UpToDate { current: current_version(), latest: release.version });
    }
    let log = install(client, cfg, &release)?;
    Ok(Outcome::Installed { version: release.version, log })
}

/// `rinky update [check|install] [--url URL] [--allow-http] [--force]`.
/// Exit status 0 = up to date or installed, 1 = failed, 2 = usage. The last line printed
/// is the result (the KUAL menu shows it).
pub fn cli(args: &[String]) -> anyhow::Result<i32> {
    let mut action = "install";
    let mut url = None;
    let mut force = false;
    let mut allow_http = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "check" => action = "check",
            "install" => action = "install",
            "--force" => force = true,
            "--allow-http" => allow_http = true,
            "--url" => url = Some(it.next().cloned().ok_or_else(|| anyhow::anyhow!("--url needs a value"))?),
            other => {
                eprintln!("unknown update option {other}\nusage: rinky update [check|install] [--url URL] [--allow-http] [--force]");
                return Ok(2);
            }
        }
    }
    let (settings, paths) = crate::cli::load_settings();
    let mut cfg = UpdateConfig::from_settings(&settings, &paths);
    if let Some(u) = url {
        cfg.release_url = u;
    }
    cfg.allow_http |= allow_http;
    let client = ink_net::UreqClient::new();
    println!("rinky {}: release {}", current_version(), cfg.release_url);
    if action == "check" {
        return Ok(match check(&client, &cfg) {
            Ok((r, true)) => {
                println!("update available: {} ({})", r.version, r.package.name);
                0
            }
            Ok((r, false)) => {
                println!("up to date: installed {}, latest {}", current_version(), r.version);
                0
            }
            Err(e) => {
                println!("update check failed: {e}");
                1
            }
        });
    }
    Ok(match update(&client, &cfg, force) {
        Ok(Outcome::UpToDate { current, latest }) => {
            println!("up to date: installed {current}, latest {latest}");
            0
        }
        Ok(Outcome::Installed { version, log }) => {
            print!("{log}");
            println!("installed rinky {version}; restart rinky to use it");
            0
        }
        Err(e) => {
            println!("update failed: {e}");
            1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// After an install the app asks the launcher for a restart into the new version,
    /// only from a launcher that can do it (an older one would start the Kindle UI).
    #[test]
    fn restart_is_requested_only_from_a_restart_capable_launcher() {
        let run = std::env::temp_dir().join(format!("ink-update-restart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&run);
        std::fs::create_dir_all(&run).unwrap();
        assert!(!request_restart_in(&run), "no restart-capable file");
        assert!(!run.join("restart").exists());
        std::fs::write(run.join("restart-capable"), b"").unwrap();
        assert!(request_restart_in(&run));
        assert!(run.join("restart").exists(), "the launcher's restart request");
        assert!(!request_restart_in(&run.join("missing")), "no run directory");
        let _ = std::fs::remove_dir_all(&run);
    }

    // ---- archive helpers (ustar + gzip), the format scripts/package.sh writes ----

    fn header(name: &str, size: usize, mode: u32, kind: u8) -> [u8; 512] {
        let mut h = [0u8; 512];
        let (prefix, base) = if name.len() > 100 { name.rsplit_once('/').unwrap() } else { ("", name) };
        h[..base.len()].copy_from_slice(base.as_bytes());
        h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    /// (name, data, mode, kind) entries -> .tar.gz bytes.
    fn targz(entries: &[(String, Vec<u8>, u32, u8)]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, data, mode, kind) in entries {
            tar.extend_from_slice(&header(name, data.len(), *mode, *kind));
            tar.extend_from_slice(data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.extend_from_slice(&[0u8; 1024]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    fn sha(data: &[u8]) -> String {
        hex(ring::digest::digest(&ring::digest::SHA256, data).as_ref())
    }

    /// A package like scripts/package.sh builds, with the real packaging/install.sh.
    fn package(version: &str) -> Vec<u8> {
        let install = include_bytes!("../../../packaging/install.sh").to_vec();
        let mut files: Vec<(String, Vec<u8>, u32)> = vec![
            ("rinky/rinky".into(), format!("fake binary {version}\n").into_bytes(), 0o755),
            ("rinky/install.sh".into(), install, 0o755),
            ("rinky/VERSION".into(), format!("version={version}\ncommit=test\n").into_bytes(), 0o644),
            ("extensions/rinky/menu.json".into(), b"{\"items\": []}\n".to_vec(), 0o644),
            ("extensions/rinky/bin/kual.sh".into(), b"#!/bin/sh\n".to_vec(), 0o755),
        ];
        let mut names: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
        names.push("rinky/FILES".into());
        names.push("rinky/SHA256SUMS".into());
        names.sort();
        let list = names.join("\n") + "\n";
        files.push(("rinky/FILES".into(), list.into_bytes(), 0o644));
        let sums: String = files.iter().map(|(n, d, _)| format!("{}  {n}\n", sha(d))).collect();
        files.push(("rinky/SHA256SUMS".into(), sums.into_bytes(), 0o644));
        let mut entries: Vec<(String, Vec<u8>, u32, u8)> =
            ["rinky/", "extensions/", "extensions/rinky/", "extensions/rinky/bin/"].iter().map(|d| (d.to_string(), Vec::new(), 0o755, b'5')).collect();
        entries.extend(files.into_iter().map(|(n, d, m)| (n, d, m, b'0')));
        targz(&entries)
    }

    // ---- mock release server ----

    #[derive(Clone)]
    struct Route {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn ok(body: impl Into<Vec<u8>>) -> Route {
        Route { status: 200, headers: vec![], body: body.into() }
    }

    struct Mock {
        base: String,
        routes: Arc<Mutex<HashMap<String, Route>>>,
        hits: Arc<Mutex<Vec<String>>>,
    }

    impl Mock {
        fn start() -> Mock {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", l.local_addr().unwrap());
            let routes: Arc<Mutex<HashMap<String, Route>>> = Arc::default();
            let hits: Arc<Mutex<Vec<String>>> = Arc::default();
            let (r, h) = (routes.clone(), hits.clone());
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(s) = s else { continue };
                    let mut reader = std::io::BufReader::new(s.try_clone().unwrap());
                    let mut first = String::new();
                    if reader.read_line(&mut first).is_err() {
                        continue;
                    }
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                    }
                    let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                    h.lock().unwrap().push(path.clone());
                    let route = r.lock().unwrap().get(&path).cloned().unwrap_or(Route { status: 404, headers: vec![], body: b"not found".to_vec() });
                    let mut out = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", route.status, route.body.len());
                    for (k, v) in &route.headers {
                        out.push_str(&format!("{k}: {v}\r\n"));
                    }
                    out.push_str("\r\n");
                    let mut s = s;
                    let _ = s.write_all(out.as_bytes());
                    let _ = s.write_all(&route.body);
                }
            });
            Mock { base, routes, hits }
        }

        fn route(&self, path: &str, r: Route) {
            self.routes.lock().unwrap().insert(path.to_string(), r);
        }

        /// A GitHub-shaped release with the package behind a 302 redirect.
        fn release(&self, tag: &str, pkg: &[u8], sums: Option<&str>, digest: Option<&str>) -> String {
            let version = tag.trim_start_matches('v');
            let name = format!("rinky-{version}-kindlehf.tar.gz");
            self.route(&format!("/download/{tag}/{name}"), Route { status: 302, headers: vec![("Location".into(), format!("{}/objects/{name}", self.base))], body: vec![] });
            self.route(&format!("/objects/{name}"), ok(pkg.to_vec()));
            let mut assets = vec![serde_json::json!({
                "name": name, "size": pkg.len(),
                "browser_download_url": format!("{}/download/{tag}/{name}", self.base),
            })];
            if let Some(d) = digest {
                assets[0]["digest"] = serde_json::Value::from(format!("sha256:{d}"));
            }
            if let Some(s) = sums {
                self.route(&format!("/download/{tag}/SHA256SUMS"), ok(s.as_bytes().to_vec()));
                assets.push(serde_json::json!({"name": "SHA256SUMS", "size": s.len(), "browser_download_url": format!("{}/download/{tag}/SHA256SUMS", self.base)}));
            }
            let json = serde_json::json!({"tag_name": tag, "draft": false, "prerelease": false, "assets": assets});
            self.route("/repos/rinky/rinky/releases/latest", ok(json.to_string().into_bytes()));
            format!("{}/repos/rinky/rinky/releases/latest", self.base)
        }
    }

    /// A fake userstore: <tmp>/mnt/us, with the update scratch dir inside the install dir.
    struct Fixture {
        dir: PathBuf,
        cfg: UpdateConfig,
    }

    impl Fixture {
        fn new(tag: &str, url: &str) -> Fixture {
            let dir = std::env::temp_dir().join(format!("ink-update-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()).replace(['(', ')', ' '], ""));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("mnt/us")).unwrap();
            let mut cfg = UpdateConfig::new(url, dir.join("mnt/us/rinky/.update"));
            cfg.allow_http = true;
            cfg.install_env = vec![
                ("RINKY_PREFIX".into(), dir.display().to_string()),
                ("RINKY_RUN".into(), dir.join("run").display().to_string()),
            ];
            Fixture { dir, cfg }
        }

        fn us(&self, rel: &str) -> PathBuf {
            self.dir.join("mnt/us").join(rel)
        }

        fn work_dir_empty(&self) -> bool {
            std::fs::read_dir(&self.cfg.work_dir).map(|mut d| d.next().is_none()).unwrap_or(true)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const NEXT: &str = "v99.0.0";
    const NEXT_NAME: &str = "rinky-99.0.0-kindlehf.tar.gz";

    #[test]
    fn versions_parse_and_order() {
        let v = |s| Version::parse(s).unwrap();
        assert_eq!(v("v0.3.0"), Version { major: 0, minor: 3, patch: 0, pre: String::new() });
        assert_eq!(v("1.2").to_string(), "1.2.0");
        assert_eq!(v("1.2.3-rc.1+abc").pre, "rc.1");
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
        assert!(v("v0.2.0") == v("0.2.0"));
        for bad in ["", "v", "1..2", "1.2.3.4", "x1", "1.-2", "1.2.3-"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(current_version().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn release_json_and_sums() {
        let json = br#"{"tag_name": "v0.3.0", "assets": [
            {"name": "SHA256SUMS", "browser_download_url": "https://x/SHA256SUMS", "size": 99},
            {"name": "rinky-0.3.0-kindlehf.tar.gz", "browser_download_url": "https://x/p", "size": 5,
             "digest": "sha256:ABCDEF0000000000000000000000000000000000000000000000000000000000"},
            {"name": "source.zip", "browser_download_url": "https://x/s"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version.to_string(), "0.3.0");
        assert_eq!(r.package.url, "https://x/p");
        assert_eq!(r.package.size, Some(5));
        assert_eq!(r.package.sha256.as_deref(), Some("abcdef0000000000000000000000000000000000000000000000000000000000"));
        assert_eq!(r.sums.unwrap().url, "https://x/SHA256SUMS");
        assert!(matches!(parse_release(br#"{"tag_name": "v1", "assets": []}"#), Err(UpdateError::Release(_))));
        assert!(matches!(parse_release(br#"{"tag_name": "v1", "draft": true, "assets": []}"#), Err(UpdateError::Release(_))));
        assert!(matches!(parse_release(b"<html>"), Err(UpdateError::Release(_))));
        assert!(!is_package_name("rinky-../x-kindlehf.tar.gz"));
        assert!(!is_package_name("rinky-1/2-kindlehf.tar.gz"));
        let h = "a".repeat(64);
        let sums = parse_sums(&format!("{h}  rinky-1-kindlehf.tar.gz\n{h} *./b.bin\nnot a line\n{}  short\n", "a".repeat(10)));
        assert_eq!(sums, vec![("rinky-1-kindlehf.tar.gz".into(), h.clone()), ("b.bin".into(), h)]);
    }

    #[test]
    fn settings_and_url_rules() {
        let mut s = Settings::default();
        let paths = Paths { root: PathBuf::from("/mnt/us/rinky") };
        let cfg = UpdateConfig::from_settings(&s, &paths);
        if std::env::var("RINKY_UPDATE_URL").is_err() {
            assert_eq!(cfg.release_url, DEFAULT_RELEASE_URL);
        }
        assert_eq!(cfg.work_dir, PathBuf::from("/mnt/us/rinky/.update"));
        assert!(!cfg.allow_http);
        s.plugins = "[update]\nurl = \"http://10.0.2.2:8000/latest\"\nallow_http = true\n".parse().unwrap();
        let cfg = UpdateConfig::from_settings(&s, &paths);
        if std::env::var("RINKY_UPDATE_URL").is_err() {
            assert_eq!(cfg.release_url, "http://10.0.2.2:8000/latest");
        }
        assert!(cfg.allow_http);
        let strict = UpdateConfig::new("http://example.org/latest", "/tmp/x");
        assert!(matches!(fetch_release(&ink_net::UreqClient::new(), &strict), Err(UpdateError::Config(_))));
        let ftp = UpdateConfig::new("ftp://example.org/latest", "/tmp/x");
        assert!(matches!(fetch_release(&ink_net::UreqClient::new(), &ftp), Err(UpdateError::Config(_))));
    }

    #[test]
    fn installs_a_verified_release() {
        let mock = Mock::start();
        let pkg = package("99.0.0");
        let url = mock.release(NEXT, &pkg, Some(&format!("{}  {NEXT_NAME}\n", sha(&pkg))), Some(&sha(&pkg)));
        let fx = Fixture::new("good", &url);
        let out = update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap();
        let Outcome::Installed { version, log } = out else { panic!("{out:?}") };
        assert_eq!(version.to_string(), "99.0.0");
        assert!(log.contains("install: rinky 99.0.0 installed"), "{log}");
        assert_eq!(std::fs::read_to_string(fx.us("rinky/rinky")).unwrap(), "fake binary 99.0.0\n");
        assert!(fx.us("extensions/rinky/menu.json").is_file());
        let state = std::fs::read_to_string(fx.us("rinky/.install-state")).unwrap();
        assert!(state.contains("version=99.0.0") && state.contains("created=extensions"), "{state}");
        assert!(fx.work_dir_empty(), "scratch files left in {}", fx.cfg.work_dir.display());
        let hits = mock.hits.lock().unwrap().clone();
        assert!(hits.iter().any(|h| h.starts_with("/objects/")), "redirect followed: {hits:?}");
    }

    #[test]
    fn bad_checksum_is_refused_and_nothing_changes() {
        let mock = Mock::start();
        let pkg = package("99.0.0");
        let wrong = sha(b"something else");
        let url = mock.release(NEXT, &pkg, Some(&format!("{wrong}  {NEXT_NAME}\n")), None);
        let fx = Fixture::new("badsum", &url);
        // An installed older version must stay exactly as it is.
        std::fs::create_dir_all(fx.us("rinky")).unwrap();
        std::fs::write(fx.us("rinky/rinky"), "old binary").unwrap();
        let err = update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap_err();
        match &err {
            UpdateError::ChecksumMismatch { expected, actual, .. } => {
                assert_eq!(expected, &wrong);
                assert_eq!(actual, &sha(&pkg));
            }
            other => panic!("expected a checksum mismatch, got {other:?}"),
        }
        assert!(err.to_string().contains("update refused"));
        assert_eq!(std::fs::read_to_string(fx.us("rinky/rinky")).unwrap(), "old binary");
        assert!(!fx.us("extensions").exists());
        assert!(fx.work_dir_empty());
    }

    #[test]
    fn digest_mismatch_and_missing_checksum_are_refused() {
        let mock = Mock::start();
        let pkg = package("99.0.0");
        let url = mock.release(NEXT, &pkg, None, Some(&sha(b"other")));
        let fx = Fixture::new("digest", &url);
        assert!(matches!(update(&ink_net::UreqClient::new(), &fx.cfg, false), Err(UpdateError::ChecksumMismatch { .. })));
        assert!(!fx.us("rinky/rinky").exists());

        mock.release(NEXT, &pkg, None, None);
        assert!(matches!(update(&ink_net::UreqClient::new(), &fx.cfg, false), Err(UpdateError::NoChecksum { .. })));

        // SHA256SUMS without a line for the package.
        mock.release(NEXT, &pkg, Some(&format!("{}  other-file\n", sha(&pkg))), None);
        assert!(matches!(update(&ink_net::UreqClient::new(), &fx.cfg, false), Err(UpdateError::NoChecksum { .. })));

        // SHA256SUMS and digest disagree.
        mock.release(NEXT, &pkg, Some(&format!("{}  {NEXT_NAME}\n", sha(&pkg))), Some(&sha(b"x")));
        assert!(matches!(update(&ink_net::UreqClient::new(), &fx.cfg, false), Err(UpdateError::Release(_))));
        assert!(!fx.us("rinky/rinky").exists());
        assert!(fx.work_dir_empty());
    }

    #[test]
    fn server_errors_size_limits_and_versions() {
        let mock = Mock::start();
        let fx = Fixture::new("errors", &format!("{}/repos/rinky/rinky/releases/latest", mock.base));
        assert!(matches!(update(&ink_net::UreqClient::new(), &fx.cfg, false), Err(UpdateError::Status { status: 404, .. })));

        let pkg = package("99.0.0");
        let sums = format!("{}  {NEXT_NAME}\n", sha(&pkg));
        mock.release(NEXT, &pkg, Some(&sums), None);
        let mut small = fx.cfg.clone();
        small.max_bytes = 100;
        assert!(matches!(update(&ink_net::UreqClient::new(), &small, false), Err(UpdateError::TooLarge { .. })));
        assert!(fx.work_dir_empty());

        // Not newer: nothing is downloaded unless forced.
        let current = current_version().to_string();
        let same = package(&current);
        let tag = format!("v{current}");
        mock.release(&tag, &same, Some(&format!("{}  rinky-{current}-kindlehf.tar.gz\n", sha(&same))), None);
        let before = mock.hits.lock().unwrap().len();
        let out = update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap();
        assert!(matches!(out, Outcome::UpToDate { .. }), "{out:?}");
        assert_eq!(mock.hits.lock().unwrap().len(), before + 1, "only the release JSON was fetched");
        assert!(matches!(update(&ink_net::UreqClient::new(), &fx.cfg, true).unwrap(), Outcome::Installed { .. }));

        // The package must carry the release's version.
        let liar = package("98.0.0");
        mock.release(NEXT, &liar, Some(&format!("{}  {NEXT_NAME}\n", sha(&liar))), None);
        let err = update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap_err();
        assert!(matches!(&err, UpdateError::Release(m) if m.contains("98.0.0")), "{err}");
    }

    #[test]
    fn unsafe_archives_are_refused() {
        let dir = std::env::temp_dir().join(format!("ink-update-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("a.tar.gz");
        let out = dir.join("out");
        let check = |entries: Vec<(String, Vec<u8>, u32, u8)>| {
            std::fs::write(&archive, targz(&entries)).unwrap();
            unpack(&archive, &out, MAX_UNPACKED_BYTES)
        };
        let file = |n: &str| (n.to_string(), b"x".to_vec(), 0o644, b'0');
        assert_eq!(check(vec![file("rinky/a"), file("./extensions/rinky/b")]).unwrap(), vec!["rinky/a", "extensions/rinky/b"]);
        for bad in ["rinky/../../etc/passwd", "/mnt/us/x", "documents/x", "extensions/other/x", "rinky//x", "rinky/./x"] {
            assert!(matches!(check(vec![file(bad)]), Err(UpdateError::UnsafeArchive(_))), "{bad}");
        }
        let link = ("rinky/l".to_string(), Vec::new(), 0o777, b'2');
        assert!(matches!(check(vec![link]), Err(UpdateError::UnsafeArchive(_))));
        let pax = ("rinky/p".to_string(), b"30 path=rinky/x\n".to_vec(), 0o644, b'x');
        assert!(matches!(check(vec![pax]), Err(UpdateError::UnsafeArchive(_))));
        assert!(matches!(
            unpack(&{
                std::fs::write(&archive, targz(&[("rinky/big".into(), vec![0; 2048], 0o644, b'0')])).unwrap();
                archive.clone()
            }, &out, 1024),
            Err(UpdateError::UnsafeArchive(_))
        ));
        // Truncated archive.
        let full = targz(&[file("rinky/a")]);
        let mut gz = flate2::read::GzDecoder::new(&full[..]);
        let mut raw = Vec::new();
        gz.read_to_end(&mut raw).unwrap();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(&raw[..600]).unwrap();
        std::fs::write(&archive, enc.finish().unwrap()).unwrap();
        assert!(matches!(unpack(&archive, &out, MAX_UNPACKED_BYTES), Err(UpdateError::UnsafeArchive(_))));
        // A long path uses the ustar prefix field.
        let long = format!("rinky/{}/file", "d".repeat(95));
        assert_eq!(check(vec![file(&long)]).unwrap(), vec![long]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn package_check_requires_installer_and_version() {
        let dir = std::env::temp_dir().join(format!("ink-update-check-{}", std::process::id()));
        let archive = dir.join("p.tar.gz");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&archive, package("1.2.3")).unwrap();
        let out = dir.join("out");
        let files = unpack(&archive, &out, MAX_UNPACKED_BYTES).unwrap();
        assert!(files.contains(&"rinky/install.sh".to_string()));
        check_unpacked(&out, &Version::parse("1.2.3").unwrap()).unwrap();
        assert!(check_unpacked(&out, &Version::parse("1.2.4").unwrap()).is_err());
        std::fs::remove_file(out.join("rinky/install.sh")).unwrap();
        assert!(matches!(check_unpacked(&out, &Version::parse("1.2.3").unwrap()), Err(UpdateError::UnsafeArchive(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// While one update holds the lock, a second one fails at once with `Busy` and
    /// changes nothing. The lock file is removed on release, and a lock file that nobody
    /// holds (left by a killed update) does not block.
    #[test]
    fn a_second_update_is_refused_while_one_holds_the_lock() {
        let mock = Mock::start();
        let pkg = package("99.0.0");
        let url = mock.release(NEXT, &pkg, Some(&format!("{}  {NEXT_NAME}\n", sha(&pkg))), None);
        let fx = Fixture::new("busy", &url);
        std::fs::create_dir_all(fx.us("rinky")).unwrap();
        std::fs::write(fx.us("rinky/rinky"), "old binary").unwrap();
        let lock_path = fx.us("rinky/.update.lock");
        {
            let _held = UpdateLock::acquire(&fx.cfg.work_dir).unwrap();
            assert!(lock_path.is_file());
            let err = update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap_err();
            assert!(matches!(err, UpdateError::Busy(_)), "{err}");
            assert!(err.to_string().contains("another update is running"), "{err}");
            assert_eq!(std::fs::read_to_string(fx.us("rinky/rinky")).unwrap(), "old binary");
            assert!(!fx.us("extensions").exists());
            let hits = mock.hits.lock().unwrap().clone();
            assert!(!hits.iter().any(|h| h.starts_with("/objects/") || h.ends_with("SHA256SUMS")), "nothing downloaded: {hits:?}");
        }
        assert!(!lock_path.exists(), "lock file left after release");
        std::fs::write(&lock_path, "12345\n").unwrap();
        update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap();
        assert_eq!(std::fs::read_to_string(fx.us("rinky/rinky")).unwrap(), "fake binary 99.0.0\n");
        assert!(!lock_path.exists(), "stale lock file left");
        assert!(fx.work_dir_empty());
    }

    /// `unpack_verified` hashes the bytes it unpacks; `unpack` checks the gzip CRC.
    #[test]
    fn unpack_checks_the_file_hash_and_the_gzip_crc() {
        let dir = std::env::temp_dir().join(format!("ink-update-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join(NEXT_NAME);
        let out = dir.join("out");
        let pkg = package("1.2.3");
        std::fs::write(&archive, &pkg).unwrap();
        unpack_verified(&archive, &out, MAX_UNPACKED_BYTES, &sha(&pkg)).unwrap();
        unpack_verified(&archive, &out, MAX_UNPACKED_BYTES, &sha(&pkg).to_uppercase()).unwrap();
        let err = unpack_verified(&archive, &out, MAX_UNPACKED_BYTES, &sha(b"other")).unwrap_err();
        assert!(matches!(&err, UpdateError::ChecksumMismatch { name, .. } if name == NEXT_NAME), "{err}");
        assert!(!out.exists(), "unpacked files left after a hash mismatch");
        // Bytes appended after the gzip stream change the file's hash.
        let mut longer = pkg.clone();
        longer.extend_from_slice(b"appended");
        std::fs::write(&archive, &longer).unwrap();
        assert!(matches!(unpack_verified(&archive, &out, MAX_UNPACKED_BYTES, &sha(&pkg)), Err(UpdateError::ChecksumMismatch { .. })));
        // A wrong CRC-32 in the gzip trailer (last 8 bytes: CRC-32, length).
        let mut bad = pkg.clone();
        let n = bad.len();
        bad[n - 8] ^= 0xff;
        std::fs::write(&archive, &bad).unwrap();
        let err = unpack(&archive, &out, MAX_UNPACKED_BYTES).unwrap_err();
        assert!(matches!(&err, UpdateError::UnsafeArchive(m) if m.contains("gzip")), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- verifier, cycle 1: hostile downloads, release descriptions and timing ----

    /// Like `package`, with the given bytes as the binary.
    fn package_bin(version: &str, bin: Vec<u8>) -> Vec<u8> {
        let install = include_bytes!("../../../packaging/install.sh").to_vec();
        let mut files: Vec<(String, Vec<u8>, u32)> = vec![
            ("rinky/rinky".into(), bin, 0o755),
            ("rinky/install.sh".into(), install, 0o755),
            ("rinky/VERSION".into(), format!("version={version}\ncommit=test\n").into_bytes(), 0o644),
            ("rinky/rinky.sh".into(), format!("#!/bin/sh\n# launcher {version}\n").into_bytes(), 0o755),
            ("extensions/rinky/menu.json".into(), format!("{{\"items\": [], \"v\": \"{version}\"}}\n").into_bytes(), 0o644),
            ("extensions/rinky/bin/kual.sh".into(), b"#!/bin/sh\n".to_vec(), 0o755),
        ];
        let mut names: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
        names.push("rinky/FILES".into());
        names.push("rinky/SHA256SUMS".into());
        names.sort();
        files.push(("rinky/FILES".into(), (names.join("\n") + "\n").into_bytes(), 0o644));
        let sums: String = files.iter().map(|(n, d, _)| format!("{}  {n}\n", sha(d))).collect();
        files.push(("rinky/SHA256SUMS".into(), sums.into_bytes(), 0o644));
        let mut entries: Vec<(String, Vec<u8>, u32, u8)> =
            ["rinky/", "extensions/", "extensions/rinky/", "extensions/rinky/bin/"].iter().map(|d| (d.to_string(), Vec::new(), 0o755, b'5')).collect();
        entries.extend(files.into_iter().map(|(n, d, m)| (n, d, m, b'0')));
        targz(&entries)
    }

    /// `n` bytes that do not compress (so download, unpack and copy take real time).
    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    /// The installed tree is one version: every file matches the installed SHA256SUMS
    /// (what `sha256sum -c rinky/SHA256SUMS` in /mnt/us checks), VERSION and
    /// .install-state name the same version, and no temporary `.new` files are left.
    /// Returns the version.
    fn consistent(fx: &Fixture) -> std::result::Result<String, String> {
        let mut bad = Vec::new();
        let sums = std::fs::read_to_string(fx.us("rinky/SHA256SUMS")).map_err(|e| format!("rinky/SHA256SUMS: {e}"))?;
        for (rel, want) in parse_sums(&sums) {
            match sha256_file(&fx.us(&rel)) {
                Ok(h) if h == want => {}
                Ok(_) => bad.push(format!("{rel} does not match SHA256SUMS")),
                Err(e) => bad.push(format!("{rel}: {e}")),
            }
        }
        let line = |f: &str| std::fs::read_to_string(fx.us(f)).ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("version=").map(str::to_string)));
        let (version, state) = (line("rinky/VERSION"), line("rinky/.install-state"));
        if version != state {
            bad.push(format!("VERSION says {version:?}, .install-state says {state:?}"));
        }
        for d in ["rinky", "extensions/rinky", "extensions/rinky/bin"] {
            for e in std::fs::read_dir(fx.us(d)).into_iter().flatten().flatten() {
                if e.file_name().to_string_lossy().contains(".new") {
                    bad.push(format!("temporary file {d}/{} left", e.file_name().to_string_lossy()));
                }
            }
        }
        if bad.is_empty() {
            version.ok_or_else(|| "no VERSION".to_string())
        } else {
            Err(bad.join("; "))
        }
    }

    type RawRoutes = Arc<Mutex<HashMap<String, Vec<u8>>>>;

    /// A server that answers each path with canned raw bytes (status line, headers and
    /// body as given, which may lie about Content-Length), then closes the connection.
    fn raw_server(routes: RawRoutes) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(s) = s else { continue };
                let mut reader = std::io::BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                let raw = routes.lock().unwrap().get(&path).cloned();
                let raw = raw.unwrap_or_else(|| b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec());
                let mut s = s;
                let _ = s.write_all(&raw);
                let _ = s.flush();
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        });
        base
    }

    fn raw(content_length: Option<usize>, body: &[u8]) -> Vec<u8> {
        let mut out = b"HTTP/1.1 200 OK\r\nConnection: close\r\n".to_vec();
        if let Some(n) = content_length {
            out.extend_from_slice(format!("Content-Length: {n}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(body);
        out
    }

    /// A truncated or mis-sized download is refused; the installed files stay as they
    /// were and no scratch file is left.
    #[test]
    fn verifier_truncated_or_missized_download_is_refused() {
        let pkg = package("99.0.0");
        let half = &pkg[..pkg.len() / 2];
        let routes: RawRoutes = Arc::default();
        let base = raw_server(routes.clone());
        let sums = format!("{}  {NEXT_NAME}\n", sha(&pkg));
        // (label, size in the release JSON, raw answer for the package)
        let cases: Vec<(&str, Option<usize>, Vec<u8>)> = vec![
            ("cl-full-body-half", Some(pkg.len()), raw(Some(pkg.len()), half)),
            ("nosize-cl-full-body-half", None, raw(Some(pkg.len()), half)),
            ("nosize-honest-short-cl", None, raw(Some(half.len()), half)),
            ("size-full-no-cl-close-half", Some(pkg.len()), raw(None, half)),
            ("json-size-too-small", Some(pkg.len() - 1), raw(Some(pkg.len()), &pkg)),
            ("json-size-too-big", Some(pkg.len() + 1), raw(Some(pkg.len()), &pkg)),
        ];
        for (label, size, answer) in cases {
            let mut asset = serde_json::json!({"name": NEXT_NAME, "browser_download_url": format!("{base}/pkg")});
            if let Some(n) = size {
                asset["size"] = serde_json::Value::from(n);
            }
            let sums_asset = serde_json::json!({"name": "SHA256SUMS", "browser_download_url": format!("{base}/sums")});
            let json = serde_json::json!({"tag_name": NEXT, "assets": [asset, sums_asset]}).to_string();
            {
                let mut r = routes.lock().unwrap();
                r.insert("/latest".into(), raw(Some(json.len()), json.as_bytes()));
                r.insert("/sums".into(), raw(Some(sums.len()), sums.as_bytes()));
                r.insert("/pkg".into(), answer);
            }
            let fx = Fixture::new(&format!("trunc-{label}"), &format!("{base}/latest"));
            std::fs::create_dir_all(fx.us("rinky")).unwrap();
            std::fs::write(fx.us("rinky/rinky"), "old binary").unwrap();
            let r = update(&ink_net::UreqClient::new(), &fx.cfg, false);
            assert!(r.is_err(), "{label}: a bad download was accepted: {r:?}");
            assert_eq!(std::fs::read_to_string(fx.us("rinky/rinky")).unwrap(), "old binary", "{label}");
            assert!(!fx.us("extensions").exists(), "{label}: installer ran");
            assert!(fx.work_dir_empty(), "{label}: scratch files left in {}", fx.cfg.work_dir.display());
        }
    }

    /// Release descriptions that point elsewhere or are oversized are refused before any
    /// file is written.
    #[test]
    fn verifier_hostile_release_descriptions_are_refused() {
        let routes: RawRoutes = Arc::default();
        let base = raw_server(routes.clone());
        // A JSON answer over the 4 MiB limit.
        let pad = "x".repeat(MAX_JSON_BYTES + 10);
        let json = format!("{{\"tag_name\": \"{NEXT}\", \"assets\": [], \"pad\": \"{pad}\"}}");
        routes.lock().unwrap().insert("/huge".into(), raw(Some(json.len()), json.as_bytes()));
        let fx = Fixture::new("huge-json", &format!("{base}/huge"));
        let r = update(&ink_net::UreqClient::new(), &fx.cfg, false);
        assert!(matches!(r, Err(UpdateError::Net { .. }) | Err(UpdateError::Release(_))), "{r:?}");
        assert!(!fx.us("rinky").exists());

        // Asset URLs with other schemes, or plain http without allow_http.
        let digest = "a".repeat(64);
        let cases = [("file:///etc/passwd", true), ("ftp://127.0.0.1/p", true), ("http://127.0.0.1:9/p", false), ("HTTPS://x/p", true)];
        for (url, allow_http) in cases {
            let fx = Fixture::new("scheme", "https://unused.invalid/latest");
            std::fs::create_dir_all(fx.us("rinky")).unwrap();
            std::fs::write(fx.us("rinky/rinky"), "old binary").unwrap();
            let mut cfg = fx.cfg.clone();
            cfg.allow_http = allow_http;
            let release = Release {
                tag: NEXT.into(),
                version: Version::parse(NEXT).unwrap(),
                package: Asset { name: NEXT_NAME.into(), url: url.into(), size: None, sha256: Some(digest.clone()) },
                sums: None,
            };
            let r = install(&ink_net::UreqClient::new(), &cfg, &release);
            assert!(matches!(r, Err(UpdateError::Config(_))), "{url}: {r:?}");
            let sums_release = Release { sums: Some(Asset { name: SUMS_ASSET.into(), url: url.into(), size: None, sha256: None }), ..release };
            let r = install(&ink_net::UreqClient::new(), &cfg, &sums_release);
            assert!(matches!(r, Err(UpdateError::Config(_))), "SHA256SUMS at {url}: {r:?}");
            let left: Vec<String> = std::fs::read_dir(fx.us("rinky")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            assert_eq!(left, vec!["rinky".to_string()], "{url}: files written");
            assert!(!fx.us("extensions").exists(), "{url}: installer ran");
        }

        // A package name with a path in it is not taken as the package.
        let json = br#"{"tag_name": "v99.0.0", "assets": [{"name": "../rinky-99.0.0-kindlehf.tar.gz", "browser_download_url": "https://x/p"}]}"#;
        assert!(matches!(parse_release(json), Err(UpdateError::Release(_))));
    }

    /// Leftovers of an interrupted update (a stale .part and a stale unpacked package
    /// with an extra file) do not end up installed and are cleaned up.
    #[test]
    fn verifier_stale_scratch_from_an_interrupted_update() {
        let mock = Mock::start();
        let pkg = package("99.0.0");
        let url = mock.release(NEXT, &pkg, Some(&format!("{}  {NEXT_NAME}\n", sha(&pkg))), None);
        let fx = Fixture::new("stale", &url);
        std::fs::create_dir_all(fx.cfg.work_dir.join("package/rinky")).unwrap();
        std::fs::write(fx.cfg.work_dir.join("package/rinky/evil.sh"), "stale").unwrap();
        std::fs::write(fx.cfg.work_dir.join(format!("{NEXT_NAME}.part")), vec![7u8; 100_000]).unwrap();
        update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap();
        assert_eq!(consistent(&fx).unwrap(), "99.0.0");
        assert!(!fx.us("rinky/evil.sh").exists());
        assert!(fx.work_dir_empty(), "scratch left in {}", fx.cfg.work_dir.display());
    }

    /// No space left while the installer copies the binary (simulated by a `cp` that
    /// fails for that one file, as busybox cp does on ENOSPC). Requirement: a failed
    /// update leaves the installed files as one consistent version, and VERSION does not
    /// claim a version whose binary is not installed.
    #[test]
    fn verifier_failed_install_keeps_one_consistent_version() {
        let mock = Mock::start();
        let old = package_bin("98.0.0", noise(200_000, 98));
        let url = mock.release("v98.0.0", &old, Some(&format!("{}  rinky-98.0.0-kindlehf.tar.gz\n", sha(&old))), None);
        let fx = Fixture::new("enospc", &url);
        update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap();
        assert_eq!(consistent(&fx).unwrap(), "98.0.0");

        let shim = fx.dir.join("shim");
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::write(
            shim.join("cp"),
            "#!/bin/sh\ncase \"$1\" in */package/rinky/rinky) echo \"cp: write error: No space left on device\" >&2; exit 1 ;; esac\n\
             for c in /bin/cp /usr/bin/cp; do [ -x \"$c\" ] && exec \"$c\" \"$@\"; done\nexit 127\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(shim.join("cp"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut cfg = fx.cfg.clone();
        cfg.install_env.push(("PATH".into(), format!("{}:{}", shim.display(), std::env::var("PATH").unwrap_or_default())));
        let new = package_bin("99.0.0", noise(200_000, 99));
        mock.release(NEXT, &new, Some(&format!("{}  {NEXT_NAME}\n", sha(&new))), None);
        let err = update(&ink_net::UreqClient::new(), &cfg, false).unwrap_err();
        assert!(matches!(err, UpdateError::Install(_)), "{err}");
        let v = consistent(&fx).unwrap_or_else(|e| panic!("after the failed update: {e}"));
        assert_eq!(v, "98.0.0", "the old version must still be the installed one");
    }

    /// The installer log lines that show what a run did: files removed as "old", the
    /// file count, and errors.
    fn key_lines(log: &str) -> String {
        let keep: Vec<&str> = log.lines().filter(|l| l.contains("removed old file") || l.contains(" installed in ") || l.contains("grep:") || l.contains("cp:")).collect();
        format!("installed: {}", keep.join(" | "))
    }

    /// Two updates at the same time (KUAL "Update rinky" tapped twice, KUAL plus SSH,
    /// or two taps on a settings entry that runs `install` on a worker thread).
    /// Requirement: afterwards the installed files are one consistent version.
    #[test]
    fn verifier_two_concurrent_updates_leave_one_consistent_version() {
        let mock = Mock::start();
        let url = format!("{}/repos/rinky/rinky/releases/latest", mock.base);
        let fx = Fixture::new("concurrent", &url);
        let mut failures = Vec::new();
        let mut busy = 0;
        // Nothing installed yet: no file in rinky/ (besides scratch) and no KUAL entry.
        let state = |fx: &Fixture| -> std::result::Result<Option<String>, String> {
            let files = std::fs::read_dir(fx.us("rinky")).into_iter().flatten().flatten().filter(|e| e.file_name() != ".update").count();
            if files == 0 && !fx.us("extensions/rinky").exists() {
                return Ok(None);
            }
            consistent(fx).map(Some)
        };
        for round in 0..24u32 {
            let before = state(&fx);
            let version = format!("99.0.{round}");
            let pkg = package_bin(&version, noise(1 << 20, round));
            let name = format!("rinky-{version}-kindlehf.tar.gz");
            mock.release(&format!("v{version}"), &pkg, Some(&format!("{}  {name}\n", sha(&pkg))), None);
            let delay = Duration::from_millis(u64::from(round) * 15);
            let (a, b) = std::thread::scope(|s| {
                let a = s.spawn(|| update(&ink_net::UreqClient::new(), &fx.cfg, false));
                let b = s.spawn(|| {
                    std::thread::sleep(delay);
                    update(&ink_net::UreqClient::new(), &fx.cfg, false)
                });
                (a.join().unwrap(), b.join().unwrap())
            });
            busy += [&a, &b].iter().filter(|r| matches!(r, Err(UpdateError::Busy(_)))).count();
            // Someone installed this round's version completely, or both runs failed and
            // the previous state is untouched.
            match state(&fx) {
                Ok(Some(v)) if v == version => {}
                after if a.is_err() && b.is_err() && after == before => {}
                other => {
                    let show = |f: &str| std::fs::read_to_string(fx.us(f)).unwrap_or_else(|e| e.to_string()).replace('\n', " ");
                    failures.push(format!(
                        "round {round} (B delayed {delay:?}): {other:?}\n  A: {:?}\n  B: {:?}\n  installed VERSION: {}\n  installed menu.json: {}\n  .install-state: {}",
                        a.map(|o| match o {
                            Outcome::Installed { log, .. } => key_lines(&log),
                            o => format!("{o:?}"),
                        }),
                        b.map(|o| match o {
                            Outcome::Installed { log, .. } => key_lines(&log),
                            o => format!("{o:?}"),
                        }),
                        show("rinky/VERSION"),
                        show("extensions/rinky/menu.json"),
                        show("rinky/.install-state"),
                    ))
                }
            }
            if !failures.is_empty() {
                break;
            }
        }
        eprintln!("concurrent updates: {busy} of 48 runs refused as busy");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    // ---- verifier cycle 2 ----

    const UNINSTALL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/uninstall.sh");

    /// Every path under `<fixture>/mnt/us`: `D path` for a folder, `F path sha256` for a
    /// file; sorted.
    fn tree(fx: &Fixture) -> Vec<String> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = e.path();
                let rel = p.strip_prefix(base).unwrap().display().to_string();
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    out.push(format!("D {rel}"));
                    walk(base, &p, out);
                } else {
                    out.push(format!("F {rel} {}", sha256_file(&p).unwrap_or_default()));
                }
            }
        }
        let base = fx.dir.join("mnt/us");
        let mut out = Vec::new();
        walk(&base, &base, &mut out);
        out.sort();
        out
    }

    /// Lines only in `after`, and lines only in `before`.
    fn tree_diff(before: &[String], after: &[String]) -> (Vec<String>, Vec<String>) {
        let added = after.iter().filter(|l| !before.contains(l)).cloned().collect();
        let removed = before.iter().filter(|l| !after.contains(l)).cloned().collect();
        (added, removed)
    }

    fn write_script(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn path_with(dir: &Path) -> String {
        format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default())
    }

    /// `sh SCRIPT ARGS` with the fixture's prefix and runtime dir, and `shim` first in PATH.
    fn sh_cmd(fx: &Fixture, script: &Path, args: &[&str], shim: Option<&Path>) -> Command {
        let mut c = Command::new("sh");
        c.arg(script).args(args).envs(fx.cfg.install_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(d) = shim {
            c.env("PATH", path_with(d));
        }
        c
    }

    fn out_text(o: &std::process::Output) -> String {
        format!("{}{}({})", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr), o.status)
    }

    /// Power loss (here: SIGKILL) during the first install, while install.sh copies the
    /// binary. The user installs again, which works, and later uninstalls completely.
    /// Requirement (acceptance check 3): the userstore is as it was before the first
    /// install. Found: the killed run created /mnt/us/extensions; the only record of that
    /// was in its uncommitted .install-state.new<pid>; the second install sees extensions/
    /// as pre-existing, so the uninstall leaves an empty /mnt/us/extensions behind.
    #[test]
    #[ignore = "verifier: killed first install leaves extensions/ after reinstall+uninstall"]
    fn verifier2_killed_first_install_then_reinstall_and_uninstall_leaves_nothing() {
        let fx = Fixture::new("killed-first", "https://unused.invalid/latest");
        std::fs::create_dir_all(fx.us("Books/a.sdr")).unwrap();
        std::fs::write(fx.us("Books/a.epub"), "book").unwrap();
        std::fs::write(fx.us("Books/a.sdr/metadata.epub.lua"), "-- koreader").unwrap();
        let before = tree(&fx);
        let pkg_file = fx.dir.join("pkg.tar.gz");
        std::fs::write(&pkg_file, package_bin("99.0.0", noise(100_000, 7))).unwrap();
        let pkg = fx.dir.join("pkg");
        unpack(&pkg_file, &pkg, MAX_UNPACKED_BYTES).unwrap();
        let install = pkg.join("rinky/install.sh");
        // A cp that kills the installer (its parent shell) when it reaches the binary.
        let shim = fx.dir.join("kill-cp");
        write_script(
            &shim.join("cp"),
            "#!/bin/sh\ncase \"$1\" in */rinky/rinky) kill -KILL $PPID; exit 1 ;; esac\n\
             for c in /bin/cp /usr/bin/cp; do [ -x \"$c\" ] && exec \"$c\" \"$@\"; done\nexit 127\n",
        );
        let killed = sh_cmd(&fx, &install, &[], Some(&shim)).output().unwrap();
        assert!(!killed.status.success(), "the first install was not killed: {}", out_text(&killed));
        let again = sh_cmd(&fx, &install, &[], None).output().unwrap();
        assert!(again.status.success(), "install after the killed one: {}", out_text(&again));
        assert_eq!(consistent(&fx).unwrap(), "99.0.0");
        let un = sh_cmd(&fx, Path::new(UNINSTALL), &[], None).output().unwrap();
        assert!(un.status.success(), "uninstall: {}", out_text(&un));
        let (added, removed) = tree_diff(&before, &tree(&fx));
        assert!(
            added.is_empty() && removed.is_empty(),
            "killed first install, install, uninstall: userstore differs from before the first install\n  added: {added:?}\n  removed: {removed:?}"
        );
    }

    /// "Uninstall completely" is tapped, then "Update rinky" while the uninstall still
    /// runs (it first walks the whole userstore for per-book files, which takes seconds
    /// on a full Kindle; uninstall.sh looks at .install.lock and .update.lock only when it
    /// starts). The walk is held until the installer copies its first file, and the installer copies one file per
    /// 0.25 s, so the installer's copies span the moment the uninstall deletes the
    /// folders. Requirement: the end state is either nothing of rinky (the userstore
    /// as before the install) or one complete, consistent install; no stray folders,
    /// copies or locks.
    #[test]
    fn verifier2_update_started_during_uninstall_leaves_one_state() {
        let mock = Mock::start();
        let old = package_bin("98.0.0", noise(100_000, 98));
        let url = mock.release("v98.0.0", &old, Some(&format!("{}  rinky-98.0.0-kindlehf.tar.gz\n", sha(&old))), None);
        let fx = Fixture::new("update-during-uninstall", &url);
        std::fs::create_dir_all(fx.us("Books")).unwrap();
        std::fs::write(fx.us("Books/a.epub"), "book").unwrap();
        let before = tree(&fx);
        update(&ink_net::UreqClient::new(), &fx.cfg, false).unwrap();
        assert_eq!(consistent(&fx).unwrap(), "98.0.0");
        let new = package_bin("99.0.0", noise(100_000, 99));
        mock.release(NEXT, &new, Some(&format!("{}  {NEXT_NAME}\n", sha(&new))), None);

        // Ordering by marker files, not by sleeps: the uninstall's `find` writes
        // `find-started` and then waits (bounded, 20 s) for `cp-started`, which the
        // installer's first `cp` writes. So the uninstall is certainly inside its walk
        // when the update starts, and certainly resumes only after the installer has
        // begun copying; the 0.25 s per copy then spans the folder deletes.
        let find_started = fx.dir.join("find-started");
        let cp_started = fx.dir.join("cp-started");
        let slow_find = fx.dir.join("slow-find");
        write_script(
            &slow_find.join("find"),
            &format!(
                "#!/bin/sh\ntouch '{}'\ni=0\nwhile [ ! -e '{}' ] && [ $i -lt 200 ]; do sleep 0.1; i=$((i+1)); done\nfor c in /usr/bin/find /bin/find; do [ -x \"$c\" ] && exec \"$c\" \"$@\"; done\nexit 127\n",
                find_started.display(),
                cp_started.display()
            ),
        );
        let slow_cp = fx.dir.join("slow-cp");
        write_script(
            &slow_cp.join("cp"),
            &format!("#!/bin/sh\ntouch '{}'\nsleep 0.25\nfor c in /bin/cp /usr/bin/cp; do [ -x \"$c\" ] && exec \"$c\" \"$@\"; done\nexit 127\n", cp_started.display()),
        );
        let uninstall = sh_cmd(&fx, Path::new(UNINSTALL), &[], Some(&slow_find)).spawn().unwrap();
        let mut waited = 0;
        while !find_started.exists() && waited < 600 {
            std::thread::sleep(Duration::from_millis(50));
            waited += 1;
        }
        assert!(find_started.exists(), "the uninstall never reached its find walk");
        let mut cfg = fx.cfg.clone();
        cfg.install_env.push(("PATH".into(), path_with(&slow_cp)));
        let upd = update(&ink_net::UreqClient::new(), &cfg, false);
        let un = uninstall.wait_with_output().unwrap();
        let after = tree(&fx);
        eprintln!("update during uninstall: update {:?}\n  uninstall: {}", upd.as_ref().map(|_| "installed"), out_text(&un));
        if after == before {
            return;
        }
        let one_install = consistent(&fx);
        let locks: Vec<&String> = after.iter().filter(|l| l.contains(".install.lock") || l.contains(".update")).collect();
        if one_install.is_ok() && locks.is_empty() {
            return;
        }
        let (added, removed) = tree_diff(&before, &after);
        panic!(
            "update during uninstall left a mixed state\n  update: {:?}\n  uninstall: {}\n  consistent install: {one_install:?}\n  added since before the install: {added:#?}\n  removed: {removed:?}",
            upd.map(|o| match o {
                Outcome::Installed { version, .. } => format!("installed {version}"),
                o => format!("{o:?}"),
            }),
            out_text(&un)
        );
    }
}
