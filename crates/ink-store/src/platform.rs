//! Kindle or Kobo, for the default paths. The same rule as `ink_device::platform`
//! (this crate does not depend on that one): `RINKY_PLATFORM=kindle|kobo`, else a Kobo
//! when `/mnt/onboard/.kobo` exists.

use std::path::PathBuf;

/// Running on a Kobo.
pub fn on_kobo() -> bool {
    match std::env::var("RINKY_PLATFORM").ok().as_deref().map(str::trim) {
        Some("kobo") => true,
        Some("kindle") => false,
        _ => std::path::Path::new("/mnt/onboard/.kobo").is_dir(),
    }
}

/// The user storage: `/mnt/us` on a Kindle, `/mnt/onboard` on a Kobo.
pub fn user_root() -> &'static str {
    if on_kobo() {
        "/mnt/onboard"
    } else {
        "/mnt/us"
    }
}

/// Where rinky is installed: `/mnt/us/rinky`, or `/mnt/onboard/.adds/rinky` on a Kobo.
pub fn install_root() -> PathBuf {
    if on_kobo() {
        PathBuf::from("/mnt/onboard/.adds/rinky")
    } else {
        PathBuf::from("/mnt/us/rinky")
    }
}

/// The e-reader's name in messages ("Kindle", "Kobo").
pub fn device_name() -> &'static str {
    if on_kobo() {
        "Kobo"
    } else {
        "Kindle"
    }
}

/// Where the library looks for books by default.
pub fn default_library_dirs() -> Vec<PathBuf> {
    if on_kobo() {
        vec![PathBuf::from("/mnt/onboard")]
    } else {
        vec![PathBuf::from("/mnt/us/Books"), PathBuf::from("/mnt/us/documents")]
    }
}
