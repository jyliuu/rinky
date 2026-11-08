//! SIGTERM after a page turn: the app loop closes the reader, which writes the position
//! to the sidecar, before the process exits with 143 (`rinky-ctl.sh stop` and
//! `restart`, the launcher, the watchdog send SIGTERM).
#![cfg(unix)]

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn sidecar_position(book: &Path) -> Option<String> {
    ink_store::BookState::load(book).ok().and_then(|s| s.position)
}

#[test]
fn sigterm_right_after_a_page_turn_writes_the_position() {
    let dir = std::env::temp_dir().join(format!("ink-signal-flush-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let book = dir.join("juliet.epub");
    std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/juliet.epub"), &book).unwrap();
    // One page turn, then no input for 30 s of real time (the loop waits as on the device).
    let script = dir.join("turn.script");
    std::fs::write(&script, "tap 1000 900\nsleep 30000\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rinky"))
        .args(["sim", "--headless", "--script"])
        .arg(&script)
        .arg(&book)
        .env("RINKY_LOG", "debug")
        // The sim keeps its state in $TMPDIR/rinky-sim; this run keeps its own.
        .env("TMPDIR", &dir)
        .env_remove("RINKY_LOG_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let mut log = Vec::new();
    let start = Instant::now();
    let mut turned = false;
    while start.elapsed() < Duration::from_secs(60) {
        let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) else { continue };
        let hit = line.contains("reader position page 2 ");
        log.push(line);
        if hit {
            turned = true;
            break;
        }
    }
    if !turned {
        let _ = child.kill();
        panic!("no page turn logged:\n{}", log.join("\n"));
    }
    let first = sidecar_position(&book);
    // At once: the idle save (2 s) has not run yet.
    let sent = Instant::now();
    let st = Command::new("kill").args(["-TERM", &pid.to_string()]).status().unwrap();
    assert!(st.success());
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if sent.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("still running 10 s after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let took = sent.elapsed();
    while let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) {
        log.push(line);
    }
    let log = log.join("\n");
    assert_eq!(status.code(), Some(143), "exit status after SIGTERM\n{log}");
    let after = sidecar_position(&book);
    assert!(after.is_some() && after != first, "sidecar position {after:?} (was {first:?} before SIGTERM)\n{log}");
    assert!(log.contains("exit: reason=signal SIGTERM status=143"), "{log}");
    // The signal thread logs this line when it gives up on the loop after STOP_GRACE
    // (2 s) and exits by itself; a wall-clock bound would also fail on a loaded machine.
    assert!(!log.contains("did not stop within"), "the loop stopped by itself, not the fallback (after {took:?})\n{log}");
    let _ = std::fs::remove_dir_all(&dir);
}
