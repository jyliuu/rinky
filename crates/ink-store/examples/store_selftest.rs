//! Runs the ink-store selftest and prints one PASS/FAIL line per check.
//!
//! ```text
//! store_selftest [--scratch DIR] [--koreader-test-dir DIR] [--library-books N] [--keep] [BOOK...]
//! ```
//!
//! `--scratch` defaults to `$TMPDIR/ink-store-selftest`; on the Kindle pass a directory on
//! `/mnt/us` to test the user store (FUSE `fuse.fsp` on 5.19.5). `--keep` leaves the files.
//! Exit status: the number of failed checks.

use ink_store::selftest::{run, SelftestOptions};
use std::path::PathBuf;

fn main() {
    let mut opts = SelftestOptions { scratch: std::env::temp_dir().join("ink-store-selftest"), ..Default::default() };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--scratch" => opts.scratch = PathBuf::from(args.next().expect("--scratch DIR")),
            "--koreader-test-dir" => opts.koreader_test_dir = Some(PathBuf::from(args.next().expect("--koreader-test-dir DIR"))),
            "--library-books" => opts.library_books = Some(args.next().and_then(|n| n.parse().ok()).expect("--library-books N")),
            "--keep" => opts.keep = true,
            "-h" | "--help" => {
                println!("store_selftest [--scratch DIR] [--koreader-test-dir DIR] [--library-books N] [--keep] [BOOK...]");
                return;
            }
            _ => opts.books.push(PathBuf::from(a)),
        }
    }
    println!("ink-store {} selftest, scratch {}", env!("CARGO_PKG_VERSION"), opts.scratch.display());
    let checks = run(&opts);
    let failed = checks.iter().filter(|c| !c.ok).count();
    for c in &checks {
        println!("{c}");
    }
    println!("store selftest: {} passed, {failed} failed", checks.len() - failed);
    std::process::exit(failed as i32);
}
