//! Which e-reader rinky runs on: a Kindle or a Kobo (Tolinos are Kobos inside).
//!
//! The binary is the same on both; the platform decides the user storage root, where
//! rinky is installed, and which device backend to open. `RINKY_PLATFORM=kindle|kobo`
//! overrides the detection (a Kobo has `/mnt/onboard/.kobo`). `ink-store` keeps a copy
//! of [`Platform::detect`] (it does not depend on this crate).

use std::path::PathBuf;

/// The e-reader family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// Amazon Kindle, firmware 5.x.
    Kindle,
    /// Rakuten Kobo (and Tolino), firmware 4.x and 5.x.
    Kobo,
}

impl Platform {
    /// `RINKY_PLATFORM`, else a Kobo when `/mnt/onboard/.kobo` exists.
    pub fn detect() -> Platform {
        match std::env::var("RINKY_PLATFORM").ok().as_deref().map(str::trim) {
            Some("kobo") => Platform::Kobo,
            Some("kindle") => Platform::Kindle,
            _ if std::path::Path::new("/mnt/onboard/.kobo").is_dir() => Platform::Kobo,
            _ => Platform::Kindle,
        }
    }

    /// The user storage (what a computer sees over USB).
    pub fn user_root(self) -> &'static str {
        match self {
            Platform::Kindle => "/mnt/us",
            Platform::Kobo => "/mnt/onboard",
        }
    }

    /// Where rinky is installed and keeps its data.
    pub fn install_root(self) -> PathBuf {
        match self {
            Platform::Kindle => PathBuf::from("/mnt/us/rinky"),
            Platform::Kobo => PathBuf::from("/mnt/onboard/.adds/rinky"),
        }
    }

    /// `kindle` or `kobo` (logs, the update asset name).
    pub fn name(self) -> &'static str {
        match self {
            Platform::Kindle => "kindle",
            Platform::Kobo => "kobo",
        }
    }
}
