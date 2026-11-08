//! Storage for plugins (Rust and Lua): settings, per-book data and a data directory.
//!
//! - Settings: `plugins/<name>/settings.json`, a JSON object written atomically on every
//!   [`PluginStore::set`], with a `settings.json.old` backup (refreshed at most once a
//!   minute; used when the file is corrupt). JSON (not the global TOML) because plugin
//!   values are `serde_json::Value`s, which may hold `null` and large integers.
//! - Per-book data: `BookState::plugins[<name>][<key>]` in the open book's sidecar. The
//!   store borrows the reader's [`BookState`]; the reader saves it with the position, so
//!   a plugin write and a page turn can never overwrite each other.
//! - Files: `plugins/<name>/data/`, created on first use.
//!
//! [`PluginStore`] has the same five methods as `ink_plugin::PluginStorage` (`get`, `set`,
//! `book_get`, `book_set`, `data_dir`), so the plugin host can forward to it.
//!
//! Plugin names come from manifests (also of user Lua plugins), so every path is built
//! only after [`validate_name`] accepted the name.

use crate::sidecar::BookState;
use crate::{save_with_backup, Paths, Result, StoreError};
use serde_json::{Map, Value};
use std::path::PathBuf;

/// Longest plugin name accepted.
pub const MAX_NAME: usize = 64;

/// Accepts names of 1..=64 ASCII letters, digits, `_` and `-` (so no `/`, `\`, `.`,
/// `..`, spaces or control characters can reach a path).
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty() && name.len() <= MAX_NAME && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-');
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid(format!("plugin name {name:?}: use 1-{MAX_NAME} of a-z, A-Z, 0-9, _ and -")))
    }
}

fn parse_settings(b: &[u8]) -> std::result::Result<Map<String, Value>, String> {
    serde_json::from_slice(b).map_err(|e| e.to_string())
}

/// Settings, per-book data and the data directory of one plugin.
#[derive(Debug)]
pub struct PluginStore<'a> {
    name: String,
    paths: Paths,
    settings: Map<String, Value>,
    book: Option<&'a mut BookState>,
}

impl<'a> PluginStore<'a> {
    /// Opens the store of plugin `name`, reading `plugins/<name>/settings.json` (corrupt:
    /// moved aside and its `.old` backup used; neither: empty). `book` is the open
    /// book's state, if a book is open.
    pub fn open(paths: &Paths, name: &str, book: Option<&'a mut BookState>) -> Result<Self> {
        validate_name(name)?;
        let file = paths.plugin_settings(name);
        let r = crate::load_recovering(&file, parse_settings)?;
        Ok(PluginStore { name: name.to_string(), paths: paths.clone(), settings: r.value.unwrap_or_default(), book })
    }

    /// The plugin's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A plugin setting.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.settings.get(key).cloned()
    }

    /// Sets a plugin setting (`null` removes it) and writes the settings file. A value
    /// the file could not be read back with (nested deeper than serde_json reads) is
    /// refused with [`StoreError::Invalid`]; nothing changes then.
    pub fn set(&mut self, key: &str, value: Value) -> Result<()> {
        let old = if value.is_null() { self.settings.remove(key) } else { self.settings.insert(key.to_string(), value) };
        let json = serde_json::to_vec_pretty(&self.settings).map_err(|e| StoreError::Format(e.to_string()))?;
        if let Err(e) = parse_settings(&json) {
            match old {
                Some(v) => self.settings.insert(key.to_string(), v),
                None => self.settings.remove(key),
            };
            return Err(StoreError::Invalid(format!("plugin {} setting {key:?} would not load again: {e}", self.name)));
        }
        save_with_backup(&self.paths.plugin_settings(&self.name), &json, |b| parse_settings(b).is_ok())
    }

    /// All settings.
    pub fn settings(&self) -> &Map<String, Value> {
        &self.settings
    }

    /// Per-book value for the open book (`None` without a book).
    pub fn book_get(&self, key: &str) -> Option<Value> {
        self.book.as_ref()?.plugin_get(&self.name, key).cloned()
    }

    /// Sets a per-book value for the open book (in memory; the reader saves the
    /// sidecar). Errors without an open book.
    pub fn book_set(&mut self, key: &str, value: Value) -> Result<()> {
        let book = self.book.as_mut().ok_or_else(|| StoreError::Invalid("no book is open".into()))?;
        book.plugin_set(&self.name, key, value);
        Ok(())
    }

    /// `plugins/<name>/data`, created on demand.
    pub fn data_dir(&self) -> PathBuf {
        let dir = self.paths.plugin_data(&self.name);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
}

/// `plugins/<name>/data` for `name`, created; errors for a bad name.
pub fn ensure_data_dir(paths: &Paths, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    let dir = paths.plugin_data(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use serde_json::json;

    #[test]
    fn names() {
        for ok in ["stats", "insightful", "kosync", "my-plugin_2", "A"] {
            validate_name(ok).unwrap();
        }
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", ".hidden", "with space", "nul\0", "é", &"x".repeat(65)] {
            assert!(validate_name(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn settings_book_data_and_dirs() {
        let tmp = TempDir::new("plugin");
        let paths = Paths { root: tmp.path().to_path_buf() };
        {
            let mut st = PluginStore::open(&paths, "stats", None).unwrap();
            assert_eq!(st.get("x"), None);
            st.set("x", json!({"a": [1, 2, null], "big": u64::MAX})).unwrap();
            let deep = (0..200).fold(json!(1), |v, _| json!([v]));
            assert!(matches!(st.set("x", deep), Err(StoreError::Invalid(_))));
            assert_eq!(st.get("x").unwrap()["big"], u64::MAX, "a refused value changes nothing");
            st.set("gone", json!(1)).unwrap();
            st.set("gone", Value::Null).unwrap();
            assert!(st.book_get("k").is_none());
            assert!(st.book_set("k", json!(1)).is_err());
            let d = st.data_dir();
            assert!(d.is_dir());
            assert_eq!(d, tmp.path().join("plugins/stats/data"));
        }
        // Settings persist.
        let st = PluginStore::open(&paths, "stats", None).unwrap();
        assert_eq!(st.get("x").unwrap()["big"], u64::MAX);
        assert!(st.get("gone").is_none());
        assert_eq!(st.settings().len(), 1);
        // Per-book values go into the borrowed BookState.
        let mut state = BookState::default();
        {
            let mut st = PluginStore::open(&paths, "insightful", Some(&mut state)).unwrap();
            st.book_set("chats", json!(["c1", "c2"])).unwrap();
            assert_eq!(st.book_get("chats").unwrap()[1], "c2");
        }
        assert_eq!(state.plugin_get("insightful", "chats").unwrap()[0], "c1");
        // Bad names never reach the file system.
        assert!(PluginStore::open(&paths, "../../etc", None).is_err());
        assert!(ensure_data_dir(&paths, "..").is_err());
        assert!(ensure_data_dir(&paths, "vocab").unwrap().is_dir());
        // A corrupt settings file is moved aside and the backup (the first save) is used.
        std::fs::write(paths.plugin_settings("stats"), b"{oops").unwrap();
        let st = PluginStore::open(&paths, "stats", None).unwrap();
        assert_eq!(st.get("x").unwrap()["big"], u64::MAX);
        assert_eq!(st.settings().len(), 1);
        let names: Vec<String> = std::fs::read_dir(paths.plugin_dir("stats")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().any(|n| n.starts_with("settings.json.corrupt-")), "{names:?}");
        // Both bad: the plugin starts empty.
        std::fs::write(paths.plugin_settings("stats"), b"{oops").unwrap();
        std::fs::write(crate::backup_path(&paths.plugin_settings("stats")), b"[").unwrap();
        assert!(PluginStore::open(&paths, "stats", None).unwrap().settings().is_empty());
    }
}
