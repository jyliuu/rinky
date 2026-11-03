//! Seeded random UI test ("monkey"): random but plausible input scripts run through
//! the headless simulator (`rinky sim DIR --headless --script`).
//!
//! One seed makes `SEGMENTS` scripts of `SEG_EVENTS` events each. Each script is one
//! run of the binary on the same state directory (`$TMPDIR/rinky-sim`), like the
//! launcher starting the app again after an exit (power key, back out of the library).
//! After each run the harness checks:
//! - exit status 0 and an `exit: reason=` line in the log (no panic, no signal);
//! - no `crash-*.log` in the state directory, no "panicked" in the log;
//! - no hang: the log file must grow at least every `HANG` while the process runs
//!   (the script feeds events without pause, so a silent process is stuck);
//! - afterwards `settings.toml`, the library index and every book's sidecar load.
//!
//! On a failure the test prints the seed, the segment, and the directory that holds
//! the segment scripts and a copy of the state from before the failing segment.
//!
//! Environment (all optional):
//! The app runs under `sandbox-exec` with network access denied (macOS).
//! - `MONKEY_BIN`: the `rinky` binary to run (copy it first for long runs);
//! - `MONKEY_WORK`: work directory (default `$TMPDIR/ink-monkey`);
//! - `MONKEY_SEEDS=A..B` (default `1..2`), `MONKEY_SEG_EVENTS` (default 250),
//!   `MONKEY_SEGMENTS` (default 8): so 8 x 250 = 2000 events per seed;
//! - `MONKEY_LOGS=DIR`: copy every run's log to `DIR/seed-N-seg-K.log`;
//! - `MONKEY_STRICT_COVERAGE=1`: a frame with pixels outside every update rect (never shown) is a failure too;
//! - `MONKEY_STYLE=2`: more bursts of taps in one place (typing, lists);
//! - `MONKEY_KEEP=1`: keep the work directory of passing seeds (the last log is in `state/run.log`);
//! - `MONKEY_HANG_SECS` (default 12);
//! - `MONKEY_MINIMISE=DIR/seg-K.txt`: shrink that script (state from `DIR/state-before-K`)
//!   with ddmin and print the smallest failing script.
//!
//! Daily: `cargo test -p ink-app --test monkey` (3 seeds x 100 events, release not needed).
//! Long: `cargo test --release -p ink-app --test monkey -- --ignored --nocapture`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const W: i32 = 1272;
const H: i32 = 1696;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> i32 {
        (self.next() % n.max(1)) as i32
    }
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u64)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// A point somewhere on the panel; often in a zone where controls live.
fn point(r: &mut Rng) -> (i32, i32) {
    match r.below(10) {
        0 => (r.range(0, W - 1), r.range(0, 90)),            // top edge zone
        1 => (r.range(0, W - 1), r.range(H - 90, H - 1)),    // bottom edge zone
        2 => (r.range(0, 60), r.range(0, H - 1)),            // left edge
        3 => (r.range(W - 60, W - 1), r.range(0, H - 1)),    // right edge
        4 => (r.range(0, W - 1), r.range(H - 700, H - 200)), // keyboard area
        5 => (r.range(W / 3, 2 * W / 3), r.range(H / 3, 2 * H / 3)), // middle
        _ => (r.range(0, W - 1), r.range(0, H - 1)),
    }
}

/// Style 2 only: 3 to 12 taps inside a 320 x 320 box (typing, list taps).
fn burst(r: &mut Rng, out: &mut String) {
    let (cx, cy) = point(r);
    for _ in 0..r.range(3, 12) {
        let (x, y) = ((cx + r.range(-160, 160)).clamp(0, W - 1), (cy + r.range(-160, 160)).clamp(0, H - 1));
        let _ = if r.chance(20) { writeln!(out, "quicktap {x} {y}") } else { writeln!(out, "tap {x} {y}") };
    }
}

fn gen_line(r: &mut Rng, out: &mut String, style: u8) {
    let mut n = r.below(100);
    if style == 2 && n < 30 {
        return burst(r, out);
    }
    if style == 2 && n < 45 {
        n = 0; // plain tap
    }
    let (x, y) = point(r);
    let _ = match n {
        0..=44 => writeln!(out, "tap {x} {y}"),
        45..=51 => writeln!(out, "quicktap {x} {y}"),
        52..=57 => writeln!(out, "hold {x} {y} {}", r.range(300, 2500)),
        58..=69 => {
            let (x2, y2) = point(r);
            writeln!(out, "swipe {x} {y} {x2} {y2}")
        }
        70..=73 => writeln!(out, "swipe {} 5 {} {}", r.range(0, W - 1), r.range(0, W - 1), r.range(300, 900)), // control center
        74..=77 => writeln!(out, "swipe {} {} {} {}", r.range(0, W - 1), H - 5, r.range(0, W - 1), r.range(H - 900, H - 300)), // Aa
        78..=79 => writeln!(out, "swipe {} {y} {} {y}", r.range(0, 40), r.range(300, 1000)), // from the left edge
        80..=81 => writeln!(out, "swipe {} {y} {} {y}", W - 5, r.range(200, 900)),
        82..=83 => {
            let (x2, y2) = point(r);
            writeln!(out, "twotap {x} {y} {x2} {y2}")
        }
        84..=86 => writeln!(out, "pinch {x} {y} {} {}", r.range(80, 600), r.range(80, 900)),
        87..=90 => {
            let k = ["next", "prev", "menu", "next", "prev", "back"][r.below(6) as usize];
            writeln!(out, "key {k}")
        }
        91..=92 => {
            let l = ["goingToScreenSaver", "outOfScreenSaver", "wakeupFromSuspend", "readyToSuspend", "suspending", "resuming"][r.below(6) as usize];
            writeln!(out, "power {l}")
        }
        93 if r.chance(15) => writeln!(out, "key power"),
        93..=94 => writeln!(out, "wait {}", r.range(10, 300)),
        // raw finger: down, a few moves, up (or lost)
        95..=96 => {
            let _ = writeln!(out, "down {x} {y}");
            for _ in 0..r.range(1, 4) {
                let (mx, my) = point(r);
                let _ = writeln!(out, "move {mx} {my}");
            }
            let (ux, uy) = point(r);
            if r.chance(85) {
                writeln!(out, "up {ux} {uy}")
            } else {
                Ok(())
            }
        }
        _ => writeln!(out, "wait {}", r.range(10, 5000)),
    };
}

/// One script: `events` random steps. `key power` is rare because it leaves the app.
pub fn gen_script(seed: u64, segment: u64, events: usize, style: u8) -> String {
    let mut r = Rng(seed.wrapping_mul(0x2545F4914F6CDD1D) ^ segment.wrapping_mul(0x9E3779B1) ^ 0xA5A5);
    let mut s = format!("# monkey seed {seed} segment {segment} events {events}\n");
    for _ in 0..events {
        gen_line(&mut r, &mut s, style);
    }
    s
}

#[derive(Debug)]
struct Failure {
    what: String,
}

fn bin() -> PathBuf {
    std::env::var_os("MONKEY_BIN").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_rinky")))
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).into_iter().flatten().flatten() {
        let (p, q) = (e.path(), to.join(e.file_name()));
        if p.is_dir() {
            copy_dir(&p, &q);
        } else {
            let _ = std::fs::copy(&p, &q);
        }
    }
}

/// A library folder with one book of each format.
fn make_books(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let t = repo().join("testdata");
    for f in ["juliet.epub", "synthetic.epub", "formats/alice.txt", "formats/sample.fb2", "formats/epub3-nav.epub", "formats/alice-kf8.azw3"] {
        let name = Path::new(f).file_name().unwrap();
        std::fs::copy(t.join(f), dir.join(name)).unwrap_or_else(|e| panic!("{f}: {e}"));
    }
    std::fs::write(dir.join("fixture.pdf"), ink_mupdf::testgen::fixture_pdf()).unwrap();
    std::fs::write(dir.join("comic.cbz"), ink_mupdf::testgen::fixture_cbz(12)).unwrap();
}

/// Runs the binary on `script`; `state` is the TMPDIR (the app keeps `rinky-sim` in it).
fn run_script(script: &Path, books: &Path, state: &Path, hang: Duration, limit: Duration) -> Result<(), Failure> {
    std::fs::create_dir_all(state).unwrap();
    let log = state.join("run.log");
    let _ = std::fs::remove_file(&log);
    let shots = state.join("shots");
    // No network for the app or its plugins (calibre, kosync, OPDS look up host names):
    // on macOS the binary runs under a sandbox profile that denies network access.
    let sandbox = Path::new("/usr/bin/sandbox-exec").exists().then_some(["-p", "(version 1)(allow default)(deny network*)"]);
    let mut cmd = match sandbox {
        Some(profile) => {
            let mut c = Command::new("/usr/bin/sandbox-exec");
            c.args(profile).arg(bin());
            c
        }
        None => Command::new(bin()),
    };
    let mut child = cmd
        .args(["sim"])
        .arg(books)
        .args(["--headless", "--script"])
        .arg(script)
        .arg("--out")
        .arg(&shots)
        .env("TMPDIR", state)
        .env("RINKY_LOG_FILE", &log)
        .env("RUST_BACKTRACE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rinky");
    let start = Instant::now();
    let (mut last_size, mut last_growth) = (0u64, Instant::now());
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        let size = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
        if size != last_size {
            last_size = size;
            last_growth = Instant::now();
        }
        if last_growth.elapsed() > hang || start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            let tail = tail(&log, 12);
            return Err(Failure { what: format!("HANG: no log output for {:?} (run {:?})\n{tail}", last_growth.elapsed(), start.elapsed()) });
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = std::fs::remove_dir_all(&shots);
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    if let Some(keep) = std::env::var_os("MONKEY_LOGS") {
        let _ = std::fs::create_dir_all(&keep);
        let name = script.parent().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy().into_owned();
        let seg = script.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let _ = std::fs::write(Path::new(&keep).join(format!("{name}-{seg}.log")), &text);
    }
    let root = state.join("rinky-sim");
    let crashes: Vec<String> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("crash-"))
        .collect();
    if !crashes.is_empty() || text.contains("panicked at") || !status.success() {
        return Err(Failure { what: format!("CRASH: status {status:?}, crash files {crashes:?}\n{}", tail(&log, 40)) });
    }
    if std::env::var_os("MONKEY_STRICT_COVERAGE").is_some() && text.contains("drawn outside every update rect") {
        let line = text.lines().find(|l| l.contains("drawn outside every update rect")).unwrap_or("");
        return Err(Failure { what: format!("UNCOVERED: {line}") });
    }
    if !text.contains("exit: reason=") {
        return Err(Failure { what: format!("EXIT WITHOUT REASON: status {status:?}\n{}", tail(&log, 12)) });
    }
    check_files(&root, books).map_err(|what| Failure { what })
}

fn tail(p: &Path, n: usize) -> String {
    let t = std::fs::read_to_string(p).unwrap_or_default();
    let lines: Vec<&str> = t.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// settings.toml, the library index and every sidecar must load.
fn check_files(root: &Path, books: &Path) -> Result<(), String> {
    let paths = ink_store::Paths { root: root.to_path_buf() };
    if paths.settings().exists() {
        ink_store::Settings::load(&paths.settings()).map_err(|e| format!("UNREADABLE settings.toml: {e}"))?;
    }
    if paths.library().exists() {
        ink_store::LibraryIndex::load(&paths.library()).map_err(|e| format!("UNREADABLE library index: {e}"))?;
    }
    for e in std::fs::read_dir(books).unwrap().flatten() {
        let p = e.path();
        if p.is_file() && ink_store::sidecar::sidecar_path(&p).exists() {
            ink_store::BookState::load(&p).map_err(|e| format!("UNREADABLE sidecar of {}: {e}", p.display()))?;
        }
    }
    Ok(())
}

struct Cfg {
    work: PathBuf,
    seg_events: usize,
    segments: u64,
    hang: Duration,
    style: u8,
}

impl Cfg {
    fn from_env(default_events: usize, default_segments: u64) -> Cfg {
        let get = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
        let work = std::env::var_os("MONKEY_WORK").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("ink-monkey"));
        Cfg {
            work,
            seg_events: get("MONKEY_SEG_EVENTS").map(|v| v as usize).unwrap_or(default_events),
            segments: get("MONKEY_SEGMENTS").unwrap_or(default_segments),
            hang: Duration::from_secs(get("MONKEY_HANG_SECS").unwrap_or(12)),
            style: get("MONKEY_STYLE").unwrap_or(1) as u8,
        }
    }
}

/// Runs one seed; on a failure keeps the directory and returns the message.
fn run_seed(cfg: &Cfg, seed: u64) -> Result<(), String> {
    let dir = cfg.work.join(format!("seed-{seed}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let books = dir.join("books");
    make_books(&books);
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    for seg in 0..cfg.segments {
        let script = dir.join(format!("seg-{seg}.txt"));
        std::fs::write(&script, gen_script(seed, seg, cfg.seg_events, cfg.style)).unwrap();
        copy_dir(&state, &dir.join(format!("state-before-{seg}")));
        let _ = std::fs::remove_dir_all(state.join("rinky-sim").join("shots"));
        if let Err(f) = run_script(&script, &books, &state, cfg.hang, Duration::from_secs(600)) {
            return Err(format!("seed {seed} segment {seg}: {}\n  reproduce: dir {} (seg-{seg}.txt, state-before-{seg}); MONKEY_MINIMISE={}", f.what, dir.display(), script.display()));
        }
    }
    if std::env::var_os("MONKEY_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

fn seeds_from_env(default: (u64, u64)) -> Vec<u64> {
    let (a, b) = std::env::var("MONKEY_SEEDS")
        .ok()
        .and_then(|v| v.split_once("..").and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))))
        .unwrap_or(default);
    (a..b).collect()
}

fn run_many(cfg: &Cfg, seeds: &[u64]) {
    let mut failed = Vec::new();
    for &s in seeds {
        match run_seed(cfg, s) {
            Ok(()) => println!("monkey seed {s}: ok ({} events)", cfg.seg_events as u64 * cfg.segments),
            Err(e) => {
                println!("monkey FAIL {e}");
                failed.push(s);
            }
        }
    }
    assert!(failed.is_empty(), "monkey: failing seeds {failed:?} (work dir {})", cfg.work.display());
}

/// Daily: 3 seeds x 100 events x 3 segments.
#[test]
#[ignore = "slow: monkey, 2 min in debug; run with `cargo test --release -p ink-app --test monkey -- --ignored monkey_daily` (8 s)"]
fn monkey_daily() {
    let cfg = Cfg::from_env(100, 3);
    run_many(&cfg, &seeds_from_env((1, 4)));
}

/// Long: `MONKEY_SEEDS=1..200` (default 1..21), 8 segments of 250 events per seed.
#[test]
#[ignore = "slow: monkey"]
fn monkey_long() {
    let cfg = Cfg::from_env(250, 8);
    run_many(&cfg, &seeds_from_env((1, 21)));
}

fn failure_kind(f: &Failure) -> &str {
    f.what.split(':').next().unwrap_or("")
}

/// ddmin over script lines. `MONKEY_MINIMISE=<dir>/seg-K.txt`.
#[test]
#[ignore = "slow: monkey (tool: set MONKEY_MINIMISE)"]
fn monkey_minimise() {
    let Some(path) = std::env::var_os("MONKEY_MINIMISE").map(PathBuf::from) else { return };
    let dir = path.parent().unwrap().to_path_buf();
    let seg = path.file_stem().unwrap().to_string_lossy().trim_start_matches("seg-").to_string();
    let before = dir.join(format!("state-before-{seg}"));
    let books = dir.join("books");
    let work = dir.join("min");
    let cfg = Cfg::from_env(0, 0);
    let text = std::fs::read_to_string(&path).unwrap();
    // A step that has more than one line (down/move/up) stays one unit only by luck;
    // lines are independent here, a lone `move` is a legal script line.
    let lines: Vec<String> = text.lines().filter(|l| !l.starts_with('#')).map(String::from).collect();
    let mut n_runs = 0;
    let mut kind: Option<String> = None;
    let mut test = |subset: &[String]| -> bool {
        n_runs += 1;
        let s = work.join("state");
        copy_dir(&before, &s);
        let script = work.join("min.txt");
        std::fs::write(&script, subset.join("\n") + "\n").unwrap();
        match run_script(&script, &books, &s, cfg.hang, Duration::from_secs(300)) {
            Ok(()) => false,
            Err(f) => {
                let k = failure_kind(&f).to_string();
                match &kind {
                    None => {
                        kind = Some(k);
                        true
                    }
                    Some(want) => *want == k,
                }
            }
        }
    };
    assert!(test(&lines), "the full script does not fail");
    let mut cur = lines;
    let mut n = 2;
    while cur.len() >= 2 {
        let chunk = cur.len().div_ceil(n);
        let mut reduced = false;
        let mut i = 0;
        while i < cur.len() {
            let mut cand = cur.clone();
            cand.drain(i..(i + chunk).min(cur.len()));
            if !cand.is_empty() && test(&cand) {
                cur = cand;
                n = (n - 1).max(2);
                reduced = true;
                break;
            }
            i += chunk;
        }
        if !reduced {
            if n >= cur.len() {
                break;
            }
            n = (n * 2).min(cur.len());
        }
    }
    println!("minimised to {} lines after {n_runs} runs ({:?}):\n{}", cur.len(), kind, cur.join("\n"));
    std::fs::write(dir.join("minimised.txt"), cur.join("\n") + "\n").unwrap();
}
