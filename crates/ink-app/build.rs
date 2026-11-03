//! Build information for the startup log line: `RINKY_COMMIT` (short git commit,
//! or the value of that variable when the packager sets it) and `RINKY_BUILD_TIME`
//! (UTC, when this script last ran: at a new commit or a clean build).

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

/// Days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

fn main() {
    println!("cargo:rerun-if-env-changed=RINKY_COMMIT");
    let commit = std::env::var("RINKY_COMMIT").ok().or_else(|| git(&["rev-parse", "--short=12", "HEAD"]));
    // Run again when HEAD moves (a commit or a checkout).
    for p in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", p]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]).and_then(|r| git(&["rev-parse", "--git-path", &r])) {
        println!("cargo:rerun-if-changed={r}");
    }
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    println!("cargo:rustc-env=RINKY_COMMIT={}", commit.unwrap_or_else(|| "unknown".into()));
    println!("cargo:rustc-env=RINKY_BUILD_TIME={y:04}-{mo:02}-{d:02} {:02}:{:02} UTC", t / 3600, t / 60 % 60);
}
