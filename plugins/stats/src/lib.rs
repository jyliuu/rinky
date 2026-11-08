//! Reading statistics and vocabulary (phase 10).
//!
//! - [`recorder::StatsPlugin`]: the compiled-in `stats` plugin. It uses the reader hooks
//!   (`on_book_open`, `on_page_change`, `on_suspend`, `on_resume`, `on_close`) to write
//!   page records and sessions into ink-store's statistics database
//!   (`<root>/statistics.sqlite3`, [`ink_store::StatsDb`]) from a worker thread.
//! - [`report`]: the numbers of the Statistics screen (today, this week, streak, pages per
//!   hour, per-book time, a month calendar), pure functions over the database rows.
//! - [`vocab::VocabDb`]: looked-up words (`<root>/vocabulary.sqlite3`, table
//!   `vocabulary`, KOReader's vocabulary-builder columns) with flashcard reviews.
//!
//! `<root>` is the app's `Paths::root` (`/mnt/us/rinky` on the Kindle). A plugin
//! does not get `Paths`; it finds the root from its data directory, which the app puts at
//! `<root>/plugins/<name>/data` ([`root_from_data_dir`]).

use std::path::{Path, PathBuf};

pub mod recorder;
pub mod report;
pub mod vocab;

pub use recorder::StatsPlugin;
pub use rusqlite;
pub use vocab::{Lookup, VocabDb, VocabWord};

/// File name of the vocabulary database under the app root.
pub const VOCABULARY_FILE: &str = "vocabulary.sqlite3";

/// The app root for a plugin data directory `<root>/plugins/<name>/data`, or `None` when
/// `dir` does not have that shape (in-memory storage in tests, a temporary directory).
pub fn root_from_data_dir(dir: &Path) -> Option<PathBuf> {
    let data = dir.file_name()?;
    let name_dir = dir.parent()?;
    let plugins = name_dir.parent()?;
    if data != "data" || plugins.file_name()? != "plugins" {
        return None;
    }
    plugins.parent().map(Path::to_path_buf)
}

/// The statistics database for a plugin data directory: `<root>/statistics.sqlite3`
/// (the same file as `ink_store::Paths::statistics`), or `dir/statistics.sqlite3` when
/// the root cannot be found.
pub fn statistics_path_for_data_dir(dir: &Path) -> PathBuf {
    match root_from_data_dir(dir) {
        Some(root) => ink_store::Paths { root }.statistics(),
        None => dir.join("statistics.sqlite3"),
    }
}

/// `<root>/vocabulary.sqlite3`.
pub fn vocabulary_path(paths: &ink_store::Paths) -> PathBuf {
    paths.root.join(VOCABULARY_FILE)
}

/// Seconds since the Unix epoch now.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_from_plugin_data_dir() {
        let d = Path::new("/mnt/us/rinky/plugins/stats/data");
        assert_eq!(root_from_data_dir(d), Some(PathBuf::from("/mnt/us/rinky")));
        assert_eq!(statistics_path_for_data_dir(d), PathBuf::from("/mnt/us/rinky/statistics.sqlite3"));
        let tmp = Path::new("/tmp/rinky-plugins/stats/data");
        assert_eq!(root_from_data_dir(tmp), None);
        assert_eq!(statistics_path_for_data_dir(tmp), tmp.join("statistics.sqlite3"));
        assert_eq!(root_from_data_dir(Path::new("/x/plugins/stats/other")), None);
        assert_eq!(root_from_data_dir(Path::new("data")), None);
    }
}
