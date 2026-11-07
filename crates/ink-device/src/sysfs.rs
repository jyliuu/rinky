//! Reads and writes of `/sys` and `/proc` files below a configurable root, so the
//! battery, RTC, power, USB, hall and backlight readers run in host tests against a
//! directory tree (`SysRoot::at(tmp)`), and on the Kindle against `/`.

use std::io;
use std::path::{Path, PathBuf};

/// A filesystem root for absolute device paths such as `/sys/class/rtc/rtc0/wakealarm`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SysRoot {
    root: PathBuf,
}

impl Default for SysRoot {
    fn default() -> Self {
        SysRoot::system()
    }
}

impl SysRoot {
    /// The real root `/`.
    pub fn system() -> Self {
        SysRoot { root: PathBuf::from("/") }
    }

    /// A test root: `/sys/x` resolves to `root/sys/x`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        SysRoot { root: root.into() }
    }

    /// The host path of an absolute device path.
    pub fn path(&self, p: &str) -> PathBuf {
        self.root.join(p.trim_start_matches('/'))
    }

    /// File contents without the trailing newline.
    pub fn read(&self, p: &str) -> io::Result<String> {
        Ok(std::fs::read_to_string(self.path(p))?.trim_end_matches(['\n', '\r', ' ']).to_string())
    }

    /// File contents parsed as an integer.
    pub fn read_int(&self, p: &str) -> io::Result<i64> {
        let s = self.read(p)?;
        s.trim().parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{p}: not an integer: {s:?}")))
    }

    /// Writes `value` (sysfs attributes take the whole value in one write).
    pub fn write(&self, p: &str, value: &str) -> io::Result<()> {
        std::fs::write(self.path(p), value)
    }

    /// True when the path exists.
    pub fn exists(&self, p: &str) -> bool {
        self.path(p).exists()
    }

    /// Sorted entry names of a directory (empty when missing).
    pub fn list(&self, dir: &str) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.path(dir))
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

/// Creates files below a test root: `write_tree(root, &[("/sys/a/b", "1")])`.
pub fn write_tree(root: &Path, files: &[(&str, &str)]) -> io::Result<()> {
    for (p, v) in files {
        let path = root.join(p.trim_start_matches('/'));
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, v)?;
    }
    Ok(())
}

/// A unique empty temporary directory for tests (removed by the caller).
pub fn test_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("ink-dev-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_relative_reads_and_writes() {
        let d = test_dir("sysfs");
        write_tree(&d, &[("/sys/class/x/value", "42\n"), ("/sys/class/x/name", "abc \n")]).unwrap();
        let r = SysRoot::at(&d);
        assert_eq!(r.read_int("/sys/class/x/value").unwrap(), 42);
        assert_eq!(r.read("/sys/class/x/name").unwrap(), "abc");
        assert!(r.read_int("/sys/class/x/name").is_err());
        r.write("/sys/class/x/value", "7").unwrap();
        assert_eq!(r.read_int("/sys/class/x/value").unwrap(), 7);
        assert_eq!(r.list("/sys/class/x"), vec!["name".to_string(), "value".to_string()]);
        assert!(r.list("/nope").is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }
}
