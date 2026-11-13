//! `plugin.toml`: name, version, permissions, limits.
//!
//! ```toml
//! name = "wordcount"              # [a-z0-9_-]; Lua plugins: equal to the folder name
//! version = "0.1.0"
//! description = "Counts words on the page"
//! permissions = ["document", "ui", "storage", "network"]
//! entry = "main.lua"              # Lua plugins only (default main.lua)
//! native = "libwordcount.so"      # native plugins: the library in the plugin folder
//! time_limit_ms = 1000            # Lua: limit per call (default 1000, page turns 500)
//! memory_mb = 32                  # Lua: heap limit of the plugin's Lua state (default 32)
//! hosts = ["api.example.com"]     # network: allowed hosts (empty = any host)
//! ```

use crate::{PluginError, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// What a plugin may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Read the open book (text, TOC, search, selection).
    Document,
    /// Show dialogs, menus, viewers, keyboard input, toasts, notifications.
    Ui,
    /// Keep settings, per-book data and files.
    Storage,
    /// HTTP(S) requests.
    Network,
}

impl Permission {
    /// The name used in `plugin.toml`.
    pub fn name(self) -> &'static str {
        match self {
            Permission::Document => "document",
            Permission::Ui => "ui",
            Permission::Storage => "storage",
            Permission::Network => "network",
        }
    }
}

/// Default Lua time limit per call.
pub const DEFAULT_TIME_LIMIT_MS: u64 = 1000;
/// Lua time limit for `on_page_change` (it runs on every page turn).
pub const PAGE_TURN_LIMIT_MS: u64 = 500;
/// Highest time limit a manifest may ask for.
pub const MAX_TIME_LIMIT_MS: u64 = 10_000;
/// Default Lua heap limit.
pub const DEFAULT_MEMORY_MB: u32 = 32;
/// Highest heap limit a manifest may ask for.
pub const MAX_MEMORY_MB: u32 = 128;

/// A plugin's manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique name, `[a-z0-9_-]`.
    pub name: String,
    /// Version string.
    pub version: String,
    /// One line for the plugin list.
    #[serde(default)]
    pub description: String,
    /// Requested permissions.
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// Lua entry file (default `main.lua`); unused for Rust plugins.
    #[serde(default)]
    pub entry: Option<String>,
    /// Native plugins: the shared library in the plugin folder (`libfoo.so`; on the
    /// macOS simulator `libfoo.dylib`). A manifest with it is not a Lua plugin.
    #[serde(default)]
    pub native: Option<String>,
    /// Lua time limit per call in milliseconds (default [`DEFAULT_TIME_LIMIT_MS`], at most
    /// [`MAX_TIME_LIMIT_MS`]); `on_page_change` gets at most [`PAGE_TURN_LIMIT_MS`] unless
    /// this is set.
    #[serde(default)]
    pub time_limit_ms: Option<u64>,
    /// Lua heap limit in MiB (default [`DEFAULT_MEMORY_MB`], at most [`MAX_MEMORY_MB`]).
    #[serde(default)]
    pub memory_mb: Option<u32>,
    /// Hosts the plugin may contact with the `network` permission; empty = any host.
    /// An entry `example.com` also allows its subdomains.
    #[serde(default)]
    pub hosts: Vec<String>,
}

impl PluginManifest {
    /// A manifest for a compiled-in plugin.
    pub fn new(name: &str, version: &str, permissions: &[Permission]) -> Self {
        PluginManifest {
            name: name.into(),
            version: version.into(),
            description: String::new(),
            permissions: permissions.to_vec(),
            entry: None,
            native: None,
            time_limit_ms: None,
            memory_mb: None,
            hosts: Vec::new(),
        }
    }

    /// Parses `plugin.toml` text and checks the name and the entry file name.
    pub fn parse(text: &str) -> Result<Self> {
        let m: PluginManifest = toml::from_str(text).map_err(|e| PluginError::Manifest(e.to_string()))?;
        if !valid_name(&m.name) {
            return Err(PluginError::Manifest(format!("bad plugin name {:?} (use a-z, 0-9, _ and -)", m.name)));
        }
        if let Some(entry) = &m.entry {
            if !valid_file_name(entry) || !entry.ends_with(".lua") {
                return Err(PluginError::Manifest(format!("bad entry {entry:?} (a .lua file in the plugin folder)")));
            }
        }
        if let Some(lib) = &m.native {
            if !valid_file_name(lib) || !(lib.ends_with(".so") || lib.ends_with(".dylib")) {
                return Err(PluginError::Manifest(format!("bad native {lib:?} (a .so file in the plugin folder)")));
            }
            if m.entry.is_some() {
                return Err(PluginError::Manifest("a plugin has either entry (Lua) or native, not both".into()));
            }
        }
        Ok(m)
    }

    /// True when the manifest asks for `p`.
    pub fn allows(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }

    /// The Lua entry file name.
    pub fn entry_file(&self) -> &str {
        self.entry.as_deref().unwrap_or("main.lua")
    }

    /// Time limit for one Lua call; `page_turn` selects the tighter page-turn default.
    pub fn time_limit(&self, page_turn: bool) -> Duration {
        let ms = match self.time_limit_ms {
            Some(ms) => ms.clamp(10, MAX_TIME_LIMIT_MS),
            None if page_turn => PAGE_TURN_LIMIT_MS,
            None => DEFAULT_TIME_LIMIT_MS,
        };
        Duration::from_millis(ms)
    }

    /// Lua heap limit in bytes.
    pub fn memory_limit(&self) -> usize {
        self.memory_mb.unwrap_or(DEFAULT_MEMORY_MB).clamp(1, MAX_MEMORY_MB) as usize * 1024 * 1024
    }

    /// True when `url`'s host is allowed by [`PluginManifest::hosts`] (any host when empty).
    pub fn allows_url(&self, url: &str) -> bool {
        hosts_allow(&self.hosts, url)
    }
}

/// True when `url`'s host is in `hosts` (an entry also allows its subdomains; any host
/// when `hosts` is empty). Not an http(s) URL: false.
pub fn hosts_allow(hosts: &[String], url: &str) -> bool {
    let Some(host) = url_host(url) else { return false };
    let host = host.as_bytes();
    hosts.is_empty()
        || hosts.iter().map(|h| h.as_bytes()).any(|h| match host.len().checked_sub(h.len()) {
            Some(0) => host.eq_ignore_ascii_case(h),
            Some(dot) => host[dot - 1] == b'.' && host[dot..].eq_ignore_ascii_case(h),
            None => false,
        })
}

/// True for a plugin name: non-empty, `[a-z0-9_-]`, at most 64 characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// True for a plain file name (no separators, not `.`/`..`, no NUL).
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0']) && name.len() <= 255
}

/// Lowercase host of an `http://` or `https://` URL.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?;
    let host = if let Some(v6) = hostport.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        hostport.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manifests() {
        let m = PluginManifest::parse("name = \"wordcount\"\nversion = \"0.1.0\"\npermissions = [\"document\", \"ui\"]\n").unwrap();
        assert!(m.allows(Permission::Document) && !m.allows(Permission::Network));
        assert_eq!(m.entry_file(), "main.lua");
        assert!(PluginManifest::parse("name = \"Bad Name\"\nversion = \"1\"").is_err());
        assert!(PluginManifest::parse("name = \"x\"\nversion = \"1\"\npermissions = [\"root\"]").is_err());
        assert!(PluginManifest::parse("nonsense").is_err());
        assert!(PluginManifest::parse("name = \"x\"\nversion = \"1\"\nentry = \"../evil.lua\"").is_err());
        assert!(PluginManifest::parse("name = \"x\"\nversion = \"1\"\nentry = \"init.txt\"").is_err());
        assert_eq!(PluginManifest::parse("name = \"x\"\nversion = \"1\"\nentry = \"init.lua\"").unwrap().entry_file(), "init.lua");
        let native = PluginManifest::parse("name = \"x\"\nversion = \"1\"\nnative = \"libx.so\"").unwrap();
        assert_eq!(native.native.as_deref(), Some("libx.so"));
        for bad in ["../libx.so", "libx.txt", "x/libx.so"] {
            assert!(PluginManifest::parse(&format!("name = \"x\"\nversion = \"1\"\nnative = {bad:?}")).is_err(), "{bad}");
        }
        assert!(PluginManifest::parse("name = \"x\"\nversion = \"1\"\nnative = \"libx.so\"\nentry = \"a.lua\"").is_err());
    }

    #[test]
    fn limits_and_hosts() {
        let mut m = PluginManifest::new("x", "1", &[Permission::Network]);
        assert_eq!(m.time_limit(false), Duration::from_millis(DEFAULT_TIME_LIMIT_MS));
        assert_eq!(m.time_limit(true), Duration::from_millis(PAGE_TURN_LIMIT_MS));
        m.time_limit_ms = Some(999_999);
        assert_eq!(m.time_limit(true), Duration::from_millis(MAX_TIME_LIMIT_MS));
        assert_eq!(m.memory_limit(), 32 << 20);
        m.memory_mb = Some(4096);
        assert_eq!(m.memory_limit(), 128 << 20);
        assert!(m.allows_url("https://anything.org/x"));
        assert!(!m.allows_url("ftp://anything.org/x"));
        m.hosts = vec!["Example.com".into()];
        assert!(m.allows_url("https://example.com/v1?q=1"));
        assert!(m.allows_url("http://api.example.com:8080/"));
        assert!(m.allows_url("https://user:pw@example.com/"));
        assert!(!m.allows_url("https://example.com.evil.org/"));
        assert!(!m.allows_url("https://notexample.com/"));
        assert_eq!(url_host("http://[::1]:80/x").as_deref(), Some("::1"));
        assert_eq!(url_host("https:///x"), None);
    }
}
