//! Which e-reader rinky runs on, and everything that follows from that alone: where
//! the user's files and rinky's own files are, which fonts the system has, what the
//! device is called in messages and which release package belongs to it.
//!
//! The binary is the same on every e-reader; this crate is the one place that tells
//! them apart. `RINKY_PLATFORM=kindle|kobo` overrides the detection: a Kobo (or a
//! Tolino, which Kobo makes) has `/mnt/onboard/.kobo`, anything else is a Kindle.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};

/// The e-reader family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// Amazon Kindle, firmware 5.x (jailbroken; started from KUAL).
    Kindle,
    /// Rakuten Kobo or Tolino (started from NickelMenu).
    Kobo,
}

impl Platform {
    /// `RINKY_PLATFORM`, else a Kobo when `/mnt/onboard/.kobo` exists, else a Kindle.
    pub fn detect() -> Platform {
        match std::env::var("RINKY_PLATFORM").ok().as_deref().map(str::trim) {
            Some("kobo") => Platform::Kobo,
            Some("kindle") => Platform::Kindle,
            _ if Path::new("/mnt/onboard/.kobo").is_dir() => Platform::Kobo,
            _ => Platform::Kindle,
        }
    }

    /// `kindle` or `kobo` (logs, `RINKY_PLATFORM`).
    pub fn id(self) -> &'static str {
        match self {
            Platform::Kindle => "kindle",
            Platform::Kobo => "kobo",
        }
    }

    /// The device's name in messages ("Exit to Kindle").
    pub fn device_name(self) -> &'static str {
        match self {
            Platform::Kindle => "Kindle",
            Platform::Kobo => "Kobo",
        }
    }

    /// The user storage, what a computer sees over USB.
    pub fn user_root(self) -> &'static str {
        match self {
            Platform::Kindle => "/mnt/us",
            Platform::Kobo => "/mnt/onboard",
        }
    }

    /// Where rinky is installed and keeps its settings, library index and log.
    pub fn install_root(self) -> PathBuf {
        match self {
            Platform::Kindle => PathBuf::from("/mnt/us/rinky"),
            Platform::Kobo => PathBuf::from("/mnt/onboard/.adds/rinky"),
        }
    }

    /// Where KOReader is installed (rinky reads its positions and highlights).
    pub fn koreader_dir(self) -> PathBuf {
        match self {
            Platform::Kindle => PathBuf::from("/mnt/us/koreader"),
            Platform::Kobo => PathBuf::from("/mnt/onboard/.adds/koreader"),
        }
    }

    /// The library's folders on a fresh install.
    pub fn default_library_dirs(self) -> Vec<PathBuf> {
        match self {
            Platform::Kindle => vec![PathBuf::from("/mnt/us/Books"), PathBuf::from("/mnt/us/documents")],
            // Books can be anywhere on a Kobo's drive; hidden folders are skipped.
            Platform::Kobo => vec![PathBuf::from("/mnt/onboard")],
        }
    }

    /// Font folders, in order (earlier folders win a family name): the system's own
    /// fonts (read on the device, never copied), the user's fonts, rinky's folder.
    pub fn font_dirs(self) -> Vec<PathBuf> {
        let (system, user) = match self {
            Platform::Kindle => ("/usr/java/lib/fonts", "/mnt/us/fonts"),
            Platform::Kobo => ("/usr/local/Kobo/fonts", "/mnt/onboard/fonts"),
        };
        vec![PathBuf::from(system), PathBuf::from(user), self.install_root().join("fonts")]
    }

    /// System fonts used as UI fallbacks, in order, for characters the UI font lacks.
    /// Kindle: `Kindle_Symbol` for arrows, ▪, suits; the firmware's sans faces for
    /// Chinese (simplified, then traditional), Japanese and Korean, so CJK text in
    /// menus, cards and titles is not drawn as boxes; `code2000` last for ∫ ≠ ◇ ✗ and
    /// the rest. Kobo: none known (its firmware fonts are not named here).
    pub fn symbol_fonts(self) -> &'static [&'static str] {
        match self {
            Platform::Kindle => &[
                "/usr/java/lib/fonts/Kindle_Symbol.ttf",
                "/usr/java/lib/fonts/STHeitiMedium.ttf",
                "/usr/java/lib/fonts/STHeitiTC.ttf",
                "/usr/java/lib/fonts/TBGothicMed_213.ttf",
                "/usr/java/lib/fonts/NotoSansKR-Regular.otf",
                "/usr/java/lib/fonts/code2000.ttf",
            ],
            Platform::Kobo => &[],
        }
    }

    /// The release asset this platform installs and updates from:
    /// `rinky-<version><suffix>`.
    pub fn package_suffix(self) -> &'static str {
        match self {
            Platform::Kindle => "-kindlehf.tar.gz",
            Platform::Kobo => "-kobo.tar.gz",
        }
    }
}

/// rinky's runtime folder on the tmpfs (`RINKY_RUN`, default `/tmp/rinky`): the
/// launcher's lock and restart files, the restore journal, the heartbeat.
pub fn run_dir() -> PathBuf {
    std::env::var_os("RINKY_RUN").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp/rinky"))
}

/// The platform this process runs on: [`Platform::detect`], once.
pub fn current() -> Platform {
    static CURRENT: std::sync::OnceLock<Platform> = std::sync::OnceLock::new();
    *CURRENT.get_or_init(Platform::detect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_platform() {
        assert_eq!(Platform::Kobo.install_root(), PathBuf::from("/mnt/onboard/.adds/rinky"));
        assert_eq!(Platform::Kindle.font_dirs()[2], PathBuf::from("/mnt/us/rinky/fonts"));
        assert_eq!(Platform::Kobo.font_dirs()[0], PathBuf::from("/usr/local/Kobo/fonts"));
        assert!(Platform::Kobo.koreader_dir().starts_with(Platform::Kobo.user_root()));
        assert_ne!(Platform::Kindle.package_suffix(), Platform::Kobo.package_suffix());
    }

    /// The Kindle's UI fallbacks: symbols first, then the CJK sans faces (simplified
    /// Chinese, traditional Chinese, Japanese, Korean), the catch-all code2000 last.
    #[test]
    fn kindle_ui_fallbacks_put_cjk_before_code2000() {
        let names: Vec<&str> = Platform::Kindle.symbol_fonts().iter().map(|p| p.rsplit('/').next().unwrap()).collect();
        assert_eq!(names, ["Kindle_Symbol.ttf", "STHeitiMedium.ttf", "STHeitiTC.ttf", "TBGothicMed_213.ttf", "NotoSansKR-Regular.otf", "code2000.ttf"]);
        assert!(Platform::Kindle.symbol_fonts().iter().all(|p| p.starts_with("/usr/java/lib/fonts/")));
        assert!(Platform::Kobo.symbol_fonts().is_empty());
    }
}
