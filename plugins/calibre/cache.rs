//! The device's book list for calibre, like KOReader's `metadata.calibre`: one JSON
//! array of slim metadata objects (the fields of [`USED`]), kept in the plugin's data
//! folder (`metadata.calibre.json`).

use ink_plugin::serde_json::{self, Map, Value};
use std::path::{Path, PathBuf};

/// Metadata fields kept per book (KOReader `metadata.lua` `used_metadata`).
pub const USED: &[&str] = &["uuid", "lpath", "last_modified", "size", "title", "authors", "author_sort", "tags", "series", "series_index"];

/// Only the fields of [`USED`] (`authors` and `tags` default to `[]`, the rest to `null`).
pub fn slim(book: &Value) -> Value {
    let mut m = Map::new();
    for k in USED {
        let v = book.get(*k).cloned().unwrap_or(Value::Null);
        let v = if (*k == "authors" || *k == "tags") && v.is_null() { Value::Array(Vec::new()) } else { v };
        m.insert((*k).to_string(), v);
    }
    Value::Object(m)
}

/// Writes `data` to `path` through `<name>.json.tmp` and a rename, making the folder
/// when it is missing.
pub fn write_json(path: &Path, data: Vec<u8>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// The book list.
#[derive(Debug, Default)]
pub struct Cache {
    /// The JSON file.
    pub path: PathBuf,
    /// Books, slim metadata, in the order they arrived.
    pub books: Vec<Value>,
}

impl Cache {
    /// Reads `path`; a missing or broken file gives an empty list (logged).
    pub fn load(path: &Path) -> Cache {
        let books = match std::fs::read(path) {
            Ok(b) => match serde_json::from_slice::<Value>(&b) {
                Ok(Value::Array(a)) => a.into_iter().filter(|b| b.get("lpath").and_then(Value::as_str).is_some()).map(|b| slim(&b)).collect(),
                Ok(_) => {
                    log::warn!("calibre: {} is not a JSON array; starting an empty book list", path.display());
                    Vec::new()
                }
                Err(e) => {
                    log::warn!("calibre: {}: {e}; starting an empty book list", path.display());
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                log::warn!("calibre: cannot read {}: {e}", path.display());
                Vec::new()
            }
        };
        Cache { path: path.to_path_buf(), books }
    }

    /// Writes the list (temp file, then rename).
    pub fn save(&self) -> std::io::Result<()> {
        write_json(&self.path, serde_json::to_vec_pretty(&self.books).map_err(std::io::Error::other)?)
    }

    /// Index of the book with `lpath`.
    pub fn find(&self, lpath: &str) -> Option<usize> {
        self.books.iter().position(|b| b.get("lpath").and_then(Value::as_str) == Some(lpath))
    }

    /// Adds `book` (calibre metadata), replacing the entry with the same `lpath`.
    pub fn add(&mut self, book: &Value) {
        let Some(lpath) = book.get("lpath").and_then(Value::as_str) else { return };
        match self.find(lpath) {
            Some(i) => self.books[i] = slim(book),
            None => self.books.push(slim(book)),
        }
    }

    /// Replaces the entry with the same `lpath`, if there is one (SEND_BOOK_METADATA).
    /// Fields the new metadata leaves out or sets to null keep their old value.
    pub fn update(&mut self, book: &Value) -> bool {
        let Some(i) = book.get("lpath").and_then(Value::as_str).and_then(|l| self.find(l)) else { return false };
        let mut new = slim(book);
        for k in USED {
            if new[*k].is_null() {
                new[*k] = self.books[i][*k].clone();
            }
        }
        self.books[i] = new;
        true
    }

    /// Removes the book with `lpath`; returns its uuid.
    pub fn remove(&mut self, lpath: &str) -> Option<String> {
        let i = self.find(lpath)?;
        let b = self.books.remove(i);
        Some(b.get("uuid").and_then(Value::as_str).unwrap_or("none").to_string())
    }

    /// Drops entries whose file is not in `dir` any more; returns how many.
    pub fn prune(&mut self, dir: &Path) -> usize {
        let before = self.books.len();
        self.books.retain(|b| b.get("lpath").and_then(Value::as_str).is_some_and(|l| crate::protocol::safe_lpath(l) && dir.join(l).is_file()));
        before - self.books.len()
    }
}
