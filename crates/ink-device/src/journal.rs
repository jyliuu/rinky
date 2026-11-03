//! Restore journal: every system setting rinky changes is written down first, so
//! the launcher and the watchdog can put it back after a crash or `kill -9`.
//!
//! File (default `/tmp/rinky/restore`), one line per setting, first value wins:
//!
//! ```text
//! lipc i com.lab126.volumd disableMounting 0
//! fb grayscale 1
//! ```
//!
//! `scripts/rinky.sh` replays `lipc` lines with `lipc-set-prop -i|-s` and runs
//! `rinky device restore` for the rest (framebuffer flags); a clean exit undoes the
//! changes itself and removes the lines ([`Journal::forget`]).

use std::io::Write;
use std::path::{Path, PathBuf};

/// Default journal path.
pub const DEFAULT_PATH: &str = "/tmp/rinky/restore";

/// One saved original value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A lipc integer property.
    LipcInt {
        /// Publisher.
        publisher: String,
        /// Property.
        prop: String,
        /// Value to restore.
        value: i64,
    },
    /// A lipc string property (no spaces).
    LipcStr {
        /// Publisher.
        publisher: String,
        /// Property.
        prop: String,
        /// Value to restore.
        value: String,
    },
    /// A framebuffer setting (`grayscale`, `fastmode`, `rotate`).
    Fb {
        /// Setting name.
        key: String,
        /// Value to restore.
        value: i64,
    },
}

impl Entry {
    fn key(&self) -> String {
        match self {
            Entry::LipcInt { publisher, prop, .. } | Entry::LipcStr { publisher, prop, .. } => {
                format!("lipc {publisher} {prop}")
            }
            Entry::Fb { key, .. } => format!("fb {key}"),
        }
    }

    /// The journal line.
    pub fn line(&self) -> String {
        match self {
            Entry::LipcInt { publisher, prop, value } => format!("lipc i {publisher} {prop} {value}"),
            Entry::LipcStr { publisher, prop, value } => format!("lipc s {publisher} {prop} {value}"),
            Entry::Fb { key, value } => format!("fb {key} {value}"),
        }
    }

    /// Parses a line.
    pub fn parse(line: &str) -> Option<Entry> {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["lipc", "i", p, k, v] => Some(Entry::LipcInt { publisher: p.to_string(), prop: k.to_string(), value: v.parse().ok()? }),
            ["lipc", "s", p, k, v] => Some(Entry::LipcStr { publisher: p.to_string(), prop: k.to_string(), value: v.to_string() }),
            ["fb", k, v] => Some(Entry::Fb { key: k.to_string(), value: v.parse().ok()? }),
            _ => None,
        }
    }
}

/// The journal file.
#[derive(Clone, Debug)]
pub struct Journal {
    path: PathBuf,
}

impl Default for Journal {
    fn default() -> Self {
        Journal::at(DEFAULT_PATH)
    }
}

impl Journal {
    /// A journal at `path`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Journal { path: path.into() }
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// All entries (empty when the file is missing).
    pub fn entries(&self) -> Vec<Entry> {
        std::fs::read_to_string(&self.path).map(|t| t.lines().filter_map(Entry::parse).collect()).unwrap_or_default()
    }

    /// Records the original value unless that setting is already recorded.
    pub fn record(&self, e: Entry) -> std::io::Result<()> {
        if self.entries().iter().any(|x| x.key() == e.key()) {
            return Ok(());
        }
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{}", e.line())?;
        f.sync_all()
    }

    /// Removes a setting after it was restored cleanly.
    pub fn forget(&self, e: &Entry) -> std::io::Result<()> {
        let rest: Vec<Entry> = self.entries().into_iter().filter(|x| x.key() != e.key()).collect();
        if rest.is_empty() {
            match std::fs::remove_file(&self.path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
                _ => return Ok(()),
            }
        }
        let text: String = rest.iter().map(|e| e.line() + "\n").collect();
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_first_value_and_forget() {
        let d = crate::sysfs::test_dir("journal");
        let j = Journal::at(d.join("run/restore"));
        assert!(j.entries().is_empty());
        let dm = |v| Entry::LipcInt { publisher: "com.lab126.volumd".into(), prop: "disableMounting".into(), value: v };
        j.record(dm(0)).unwrap();
        j.record(dm(1)).unwrap(); // later changes keep the first (original) value
        j.record(Entry::Fb { key: "grayscale".into(), value: 1 }).unwrap();
        j.record(Entry::LipcStr { publisher: "p".into(), prop: "s".into(), value: "x".into() }).unwrap();
        assert_eq!(
            std::fs::read_to_string(j.path()).unwrap(),
            "lipc i com.lab126.volumd disableMounting 0\nfb grayscale 1\nlipc s p s x\n"
        );
        assert_eq!(j.entries()[0], dm(0));
        j.forget(&dm(5)).unwrap();
        assert_eq!(j.entries().len(), 2);
        j.forget(&Entry::Fb { key: "grayscale".into(), value: 0 }).unwrap();
        j.forget(&Entry::LipcStr { publisher: "p".into(), prop: "s".into(), value: String::new() }).unwrap();
        assert!(!j.path().exists());
        j.forget(&dm(0)).unwrap();
        assert_eq!(Entry::parse("garbage"), None);
        std::fs::remove_dir_all(d).unwrap();
    }
}
