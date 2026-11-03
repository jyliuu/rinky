//! Verifier attacks on the `rinky` command line (p0fix verify cycle 1).
//!
//! Each test runs the built binary with hostile arguments and checks that it ends with
//! an error message and exit status 1 or 2, never a panic (exit 101) or a hang.
//! Tests that show a defect are `#[ignore = "verifier: ..."]`; run them with
//! `cargo test -p ink-app --test verify_cli -- --ignored`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rinky")
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-verify-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs the binary with `args`, kills it after `limit`; returns the output and the time.
fn run<S: AsRef<OsStr>>(args: &[S], limit: Duration) -> (Output, Duration) {
    let t = Instant::now();
    let mut child = Command::new(bin())
        .args(args)
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if t.elapsed() > limit {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    (out, t.elapsed())
}

/// Exit 1 with an `rinky: ...` message (or 2 with the usage text); no panic.
fn assert_clean_error(label: &str, out: &Output) {
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panicked at"), "{label}: panic\n{err}");
    let code = out.status.code();
    assert!(
        (code == Some(1) && err.contains("rinky: ")) || (code == Some(2) && err.contains("usage:")),
        "{label}: expected exit 1 with an error message or 2 with usage, got {code:?}\n{err}"
    );
}

#[test]
fn render_with_hostile_page_counts_is_an_error() {
    let out_dir = scratch("counts");
    let book = root().join("testdata/synthetic.epub");
    for count in ["-1", "", " 3", "1.5", "0x10", "99999999999999999999999999"] {
        let args = [OsStr::new("render"), book.as_os_str(), out_dir.as_os_str(), OsStr::new(count)];
        let (out, _) = run(&args, Duration::from_secs(60));
        assert_clean_error(&format!("render count {count:?}"), &out);
    }
    std::fs::remove_dir_all(out_dir).unwrap();
}

#[test]
fn headless_sim_and_demo_without_a_script_terminate() {
    let out_dir = scratch("noscript");
    let book = root().join("testdata/synthetic.epub");
    let cases: [Vec<&OsStr>; 2] = [
        vec![OsStr::new("sim"), book.as_os_str(), OsStr::new("--headless"), OsStr::new("--out"), out_dir.as_os_str()],
        vec![OsStr::new("demo"), OsStr::new("--headless"), OsStr::new("--out"), out_dir.as_os_str()],
    ];
    for args in cases {
        let (out, took) = run(&args, Duration::from_secs(60));
        assert!(took < Duration::from_secs(60), "{args:?}: still running after 60 s");
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    std::fs::remove_dir_all(out_dir).unwrap();
}

#[test]
fn script_that_is_a_directory_is_an_error() {
    let dir = scratch("scriptdir");
    let book = root().join("testdata/synthetic.epub");
    let args = [OsStr::new("sim"), book.as_os_str(), OsStr::new("--headless"), OsStr::new("--script"), dir.as_os_str()];
    let (out, _) = run(&args, Duration::from_secs(60));
    assert_clean_error("sim --script DIR", &out);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A book path that is not valid UTF-8 (possible on Linux: file names are bytes; the
/// launcher passes the path straight through). `std::env::args()` in main.rs panicked
/// on it (verifier finding K2); main.rs now reads `args_os()` and exits 1 with an error.
#[test]
fn non_utf8_book_path_is_an_error_not_a_panic() {
    use std::os::unix::ffi::OsStrExt;
    let out_dir = scratch("nonutf8");
    let book = OsStr::from_bytes(b"/tmp/caf\xe9-not-utf8.epub");
    let (out, _) = run(&[OsStr::new("render"), book, out_dir.as_os_str(), OsStr::new("1")], Duration::from_secs(60));
    std::fs::remove_dir_all(out_dir).unwrap();
    assert_clean_error("render with a non-UTF-8 path", &out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not valid UTF-8"), "{}", String::from_utf8_lossy(&out.stderr));
}

/// F2: the error for a missing book names the file. F4: binary data named `.txt` is
/// refused instead of being rendered as text.
#[test]
fn missing_book_is_named_and_binary_txt_is_refused() {
    let dir = scratch("f2f4");
    let missing = dir.join("nothere.epub");
    let args = [OsStr::new("render"), missing.as_os_str(), dir.as_os_str(), OsStr::new("1")];
    let (out, _) = run(&args, Duration::from_secs(60));
    assert_clean_error("missing book", &out);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("nothere.epub"), "error does not name the file: {err}");

    let bin_txt = dir.join("binary.txt");
    std::fs::write(&bin_txt, (0..64).flat_map(|_| 0u8..=255).collect::<Vec<u8>>()).unwrap();
    let pages = dir.join("pages");
    let args = [OsStr::new("render"), bin_txt.as_os_str(), pages.as_os_str(), OsStr::new("1")];
    let (out, _) = run(&args, Duration::from_secs(60));
    assert_clean_error("binary.txt", &out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a text file"));
    assert!(!pages.join("page-0001.png").exists(), "binary.txt was rendered");
    std::fs::remove_dir_all(dir).unwrap();
}
