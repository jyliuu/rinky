//! Command line.

use crate::app::{providers, App};
use crate::router::Route;
use anyhow::Context as _;
use ink_device::Canvas;
use ink_store::{Paths, Settings};
use ink_text::Fonts;
use std::path::{Path, PathBuf};

/// Usage text.
pub const USAGE: &str = "usage:
  rinky render BOOK OUTDIR [PAGES] [--settings FILE]  write the first PAGES pages (default 5) as PNG
  rinky run BOOK                        read on the Kindle screen
  rinky library                         library screen on the Kindle (phase 4 stub)
  rinky sim BOOK|DIR [options]          host simulator window (minifb); DIR: library on DIR
      --ghost              simulate ghosting
      --scale F            window scale (default: fit the screen)
      --panel WxH          panel size (default 1272x1696, the PW12; e.g. 1072x1448 for a PW4)
      --script FILE        scripted input instead of the mouse; quits at the end
      --shots DIR          where `shot NAME` script steps write PNGs
      --headless --out DIR no window: write PNGs (`shot` steps; --frames: every update)
  rinky demo [--kindle | sim options]   widget gallery
  rinky selftest [--screen] [--lipc] [--device [--write] [--ntp SERVER]] [--book PATH] [--url URL]
                     [--net URL [--net-tls URL --ca PEM] [--net-scratch DIR]]
                                            device/store/net checks; exit status = failures
                                            (--net: ink-net mock server, see net-mock-server)
  rinky device COMMAND                  device-layer tests (rinky device help)
  rinky refresh                         GC16-flash what the framebuffer holds
  rinky splash TEXT [TEXT...]           white screen with one centred line per TEXT (launcher)
  rinky inject power                    press the power key (writes EV_KEY to bd71828-pwrkey)
  rinky screensaver IMAGE.png           show a PNG full screen with a GC16 flash
  rinky evdump [SECONDS]                print raw touch/power evdev events
  rinky spike-suspend [--image PNG] [--seconds N] [--suspend N]
                                            suspend/resume test screen
  rinky update [check|install] [--url URL] [--allow-http] [--force]
                                            in-app update from a GitHub release (checksum verified)
  rinky koreader-sync [--dry-run] [--ko DIR] PATH...
                                            mirror KOReader's positions, highlights, notes and
                                            bookmarks of the books under PATH into ours (the newer
                                            reader's position wins; KOReader's files are only read)
  rinky plugins [list|enable NAME|disable NAME]
                                            plugins in $RINKY_HOME/plugins; on/off at next start
  rinky export-highlights [--all | BOOK]
                                            highlights, notes and bookmarks as Markdown in
                                            $RINKY_HOME/exports/<title>.md; BOOK: file or title
  rinky perf library DIR HOME           time the library's first scan of DIR (metadata, covers)
                                            into an empty HOME, then a second open
  rinky version
environment: RINKY_TOUCH=swap,mirror_x,mirror_y  RINKY_LOG=debug|info|warn (or RUST_LOG)
             RINKY_LOG_FILE=FILE (run/library default: $RINKY_HOME/rinky.log)
             RINKY_HOME=/mnt/us/rinky (settings.toml, library.json)";

/// The install folder: `RINKY_HOME`, else `/mnt/us/rinky`.
fn home_dir() -> PathBuf {
    match std::env::var_os("RINKY_HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => Paths::kindle().root,
    }
}

/// Where `cmd` logs: `RINKY_LOG_FILE` when set (the launcher passes the file it
/// writes itself), `<home>/rinky.log` for the Kindle app commands, else stderr.
fn log_target(cmd: Option<&str>) -> crate::logging::LogTarget {
    use crate::logging::LogTarget;
    match std::env::var_os("RINKY_LOG_FILE") {
        Some(f) if !f.is_empty() => LogTarget::File(PathBuf::from(f)),
        _ if matches!(cmd, Some("run" | "library")) => LogTarget::File(home_dir().join("rinky.log")),
        _ => LogTarget::Stderr,
    }
}

/// Draws `lines` centred on a white screen and flashes it (GC16). Opens the framebuffer
/// without touching any driver flag, so it works while the framework or another
/// rinky owns the screen (launcher splash, crash note).
#[cfg(target_os = "linux")]
pub fn splash(lines: &[&str]) -> anyhow::Result<()> {
    use ink_device::{RefreshClass, Screen};
    let mut fb = ink_device::kindle::fb::KindleFb::open_passive()?;
    let size = fb.size();
    let mut canvas = Canvas::new(size.w as usize, size.h as usize);
    ink_device::font::draw_lines_centered(&mut canvas, 6, 0, lines);
    fb.update_all(&canvas, RefreshClass::Gc16)?;
    Ok(())
}

/// The panic hook's on-screen note (`crash_log`: the crash log's file name).
#[cfg(target_os = "linux")]
fn show_crash_note(crash_log: &str) {
    let _ = splash(&["rinky crashed - details in crash log", crash_log]);
}

/// Settings and paths from `RINKY_HOME` (default `/mnt/us/rinky`).
pub fn load_settings() -> (Settings, Paths) {
    let paths = Paths { root: home_dir() };
    let settings = load_settings_at(&paths);
    (settings, paths)
}

/// Reads `paths.settings()`. load_or_recover keeps a broken settings.toml aside (and
/// falls back to its .old) instead of letting the next save from a screen overwrite it
/// with defaults; a missing file gives the defaults.
fn load_settings_at(paths: &Paths) -> Settings {
    match Settings::load_or_recover(&paths.settings()) {
        Ok((s, warning)) => {
            if let Some(w) = warning {
                log::warn!("{w}");
            }
            s
        }
        Err(e) => {
            log::warn!("settings: {e}; using defaults");
            Settings::default()
        }
    }
}

/// Settings for `sim DIR`: the saved settings.toml under `paths` (rotation, fonts, ...)
/// with the library folder replaced by `dir`. Before, `sim DIR` started from the
/// defaults but still saved over the file (verifier finding, b-verify).
fn sim_library_settings(paths: &Paths, dir: &Path) -> Settings {
    Settings { library_dirs: vec![dir.to_path_buf()], ..load_settings_at(paths) }
}

/// Options shared by `sim` and `demo`.
#[derive(Clone, Debug, Default)]
struct SimArgs {
    ghost: bool,
    scale: Option<f32>,
    script: Option<PathBuf>,
    shots: Option<PathBuf>,
    headless: bool,
    out: Option<PathBuf>,
    frames: bool,
    kindle: bool,
    panel: Option<ink_device::Size>,
}

fn parse_sim_args(args: &[String]) -> anyhow::Result<(Vec<String>, SimArgs)> {
    let mut rest = Vec::new();
    let mut s = SimArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or_else(|| anyhow::anyhow!("{name} needs a value"));
        match a.as_str() {
            "--ghost" => s.ghost = true,
            "--headless" => s.headless = true,
            "--frames" => s.frames = true,
            "--kindle" => s.kindle = true,
            "--scale" => {
                let v = val("--scale")?;
                let f: f32 = v.parse().map_err(|e| anyhow::anyhow!("--scale {v}: {e}"))?;
                anyhow::ensure!(f.is_finite() && f > 0.0 && f <= 8.0, "--scale {v}: must be a number in (0, 8]");
                s.scale = Some(f);
            }
            "--panel" => {
                let v = val("--panel")?;
                let (w, h) = v.split_once('x').ok_or_else(|| anyhow::anyhow!("--panel {v}: expected WxH"))?;
                let (w, h): (u32, u32) = (w.parse()?, h.parse()?);
                anyhow::ensure!((200..=4000).contains(&w) && (200..=4000).contains(&h), "--panel {v}: out of range");
                s.panel = Some(ink_device::Size::new(w, h));
            }
            "--script" => s.script = Some(PathBuf::from(val("--script")?)),
            "--shots" => s.shots = Some(PathBuf::from(val("--shots")?)),
            "--out" => s.out = Some(PathBuf::from(val("--out")?)),
            other if other.starts_with("--") => anyhow::bail!("unknown option {other}"),
            other => rest.push(other.to_string()),
        }
    }
    Ok((rest, s))
}

/// The value after option `name`; an error when the command line ends there.
fn option_value<'a>(it: &mut impl Iterator<Item = &'a String>, name: &str) -> anyhow::Result<&'a String> {
    it.next().ok_or_else(|| anyhow::anyhow!("{name} needs a value"))
}

fn script_steps(path: &Option<PathBuf>) -> anyhow::Result<Option<Vec<ink_device::script::Step>>> {
    match path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
            Ok(Some(ink_device::script::parse(&text)?))
        }
        None => Ok(None),
    }
}

fn open_kindle() -> anyhow::Result<ink_device::Device> {
    open_kindle_with(true)
}

/// `device_sleep`: the device layer handles the power key, cover and inactivity
/// (screensaver + suspend); `false` delivers `Key::Power` to the caller (spike).
fn open_kindle_with(device_sleep: bool) -> anyhow::Result<ink_device::Device> {
    #[cfg(target_os = "linux")]
    {
        let mut opts = ink_device::kindle::KindleOptions { power_events: true, ..Default::default() };
        if !device_sleep {
            opts.sleep = None;
        }
        Ok(ink_device::kindle::open(opts)?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = device_sleep;
        anyhow::bail!("this needs the Kindle framebuffer; use `rinky sim` on the host")
    }
}

/// The host simulator's battery: a fixed 76 %, not charging (the host has no
/// `/sys/class/power_supply/bd71827_bat`), so the battery widgets draw in the simulator.
struct SimBattery;

impl ink_device::services::Battery for SimBattery {
    fn state(&self) -> ink_device::Result<ink_device::services::BatteryState> {
        Ok(ink_device::services::BatteryState { percent: 76, charging: false })
    }
}

fn open_sim_device(s: &SimArgs) -> anyhow::Result<ink_device::Device> {
    if s.kindle {
        return open_kindle();
    }
    let mut device = open_host_device(s)?;
    device.battery = Box::new(SimBattery);
    Ok(device)
}

fn open_host_device(s: &SimArgs) -> anyhow::Result<ink_device::Device> {
    if s.headless {
        let defaults = ink_device::headless::HeadlessOptions::default();
        let opts = ink_device::headless::HeadlessOptions {
            ghosting: s.ghost,
            out_dir: s.out.clone().or_else(|| s.shots.clone()),
            save_frames: s.frames,
            script: script_steps(&s.script)?.unwrap_or_default(),
            panel: s.panel.unwrap_or(defaults.panel),
        };
        return Ok(ink_device::headless::open(opts).0);
    }
    #[cfg(not(target_arch = "arm"))]
    {
        let opts = ink_device::sim::SimOptions {
            panel: s.panel,
            scale: s.scale,
            ghosting: s.ghost,
            script: script_steps(&s.script)?,
            shots_dir: s.shots.clone().or_else(|| s.out.clone()),
            ..Default::default()
        };
        Ok(ink_device::sim::open(opts)?)
    }
    #[cfg(target_arch = "arm")]
    anyhow::bail!("the simulator window is not available on the Kindle; use --headless")
}

/// Writes the first `count` pages of `book` as PNG (prototype-compatible output).
pub fn render_pngs(book: &Path, out: &Path, count: usize) -> anyhow::Result<Vec<PathBuf>> {
    render_pngs_with(book, out, count, &Settings::default())
}

/// [`render_pngs`] with the reading settings of `settings` (`render BOOK OUT PAGES --settings FILE`).
pub fn render_pngs_with(book: &Path, out: &Path, count: usize, settings: &Settings) -> anyhow::Result<Vec<PathBuf>> {
    let settings = settings.clone();
    let mut doc = crate::screens::reader::open_book(&providers(), &settings, book, 1272, 1696)?;
    std::fs::create_dir_all(out).with_context(|| format!("OUTDIR {}", out.display()))?;
    let mut canvas = Canvas::new(1272, 1696);
    let mut fonts = Fonts::load();
    let total = doc.page_count().pages;
    let mut files = Vec::new();
    for i in 0..count.min(total) {
        crate::screens::reader::render_page(doc.as_mut(), i, &mut canvas, &mut fonts)?;
        let file = out.join(format!("page-{:04}.png", i + 1));
        canvas.save_png(&file).with_context(|| format!("{}", file.display()))?;
        println!("{}", file.display());
        files.push(file);
    }
    Ok(files)
}

/// End of `evdump [SECONDS]` (default 10 s) counted from `now`; an error, not an
/// overflow panic, for a number that is not a u64 or lies beyond what `Instant` holds.
#[cfg(any(target_os = "linux", test))]
fn evdump_deadline(now: std::time::Instant, arg: Option<&String>) -> anyhow::Result<std::time::Instant> {
    let seconds: u64 = match arg {
        Some(v) => v.parse().map_err(|e| anyhow::anyhow!("evdump SECONDS {v}: {e}"))?,
        None => 10,
    };
    now.checked_add(std::time::Duration::from_secs(seconds))
        .ok_or_else(|| anyhow::anyhow!("evdump SECONDS {seconds}: too large"))
}

#[cfg(target_os = "linux")]
fn evdump(deadline: std::time::Instant) -> anyhow::Result<()> {
    use ink_device::kindle::input::list_devices;
    use std::io::Read;
    let devs = list_devices();
    println!("{devs:?}");
    let mut files: Vec<(String, std::fs::File)> = devs
        .iter()
        .filter(|d| d.has_abs || d.name.contains("pwrkey"))
        .filter_map(|d| std::fs::File::open(format!("/dev/input/{}", d.event)).ok().map(|f| (d.name.clone(), f)))
        .collect();
    // Non-blocking reads in a polling loop keep this simple for a debug tool.
    for (_, f) in &files {
        use std::os::fd::AsRawFd;
        // SAFETY: fcntl on a descriptor we own.
        unsafe {
            let flags = libc::fcntl(f.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(f.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    while std::time::Instant::now() < deadline {
        for (name, f) in files.iter_mut() {
            let mut buf = [0u8; 16 * 64];
            if let Ok(n) = f.read(&mut buf) {
                for e in buf[..n].as_chunks::<16>().0.iter().map(|e| ink_device::evdev::RawEvent::from_bytes(e)) {
                    println!("{name}: {:>10} ms type {} code {} value {}", e.time.as_millis(), e.ty, e.code, e.value);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

/// Entry point; returns the process exit status.
///
/// The app commands (`run`, `library`, `sim`, `demo`) get the panic hook, the
/// fatal-signal note and a signal thread for SIGTERM/SIGINT/SIGHUP, and end with one
/// `exit: reason=... status=...` log line ([`crate::logging::log_exit`]).
pub fn main(args: Vec<String>) -> i32 {
    ink_device::trace::init();
    let cmd = args.get(1).map(String::as_str);
    let app = matches!(cmd, Some("run" | "library" | "sim" | "demo"));
    if app {
        // Before any thread starts: every thread inherits the mask, so only the
        // signal thread receives these.
        ink_device::trace::block_exit_signals();
    }
    crate::logging::init(log_target(cmd));
    if app {
        crate::logging::set_home(&home_dir());
        crate::logging::install_panic_hook();
        ink_device::trace::install_fatal_handlers();
        ink_device::trace::spawn_exit_signal_thread(crate::logging::on_exit_signal);
        #[cfg(target_os = "linux")]
        if matches!(cmd, Some("run" | "library")) {
            crate::logging::set_crash_screen(show_crash_note);
        }
    }
    let code = match run(&args) {
        Ok(code) => code,
        Err(e) => {
            if app {
                crate::logging::set_exit_reason(format!("error: {e:#}"), false);
                log::error!("{e:#}");
            }
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "rinky: {e:#}");
            1
        }
    };
    // An exit signal stopped the app loop (the screens closed): the status says so, as
    // when the signal thread exits the process itself (scripts/rinky.sh: 143).
    let code = match ink_device::trace::stop_requested() {
        Some(sig) if app => 128 + sig,
        _ => code,
    };
    if app {
        crate::logging::log_exit(code);
    }
    code
}

/// Settings level, startup report: the common start of the app commands.
fn app_start(device: &ink_device::Device, settings: &Settings, paths: &Paths) {
    crate::logging::apply_settings(settings);
    crate::logging::startup_report(device, settings, paths);
}

/// The book files under `p` (a file, or a directory searched recursively; `.sdr`
/// folders, hidden entries and non-book files skipped), sorted.
fn files_under(p: &Path, out: &mut Vec<PathBuf>) {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if p.is_dir() {
        if name.ends_with(".sdr") || name.starts_with('.') {
            return;
        }
        let Ok(rd) = std::fs::read_dir(p) else { return };
        let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        v.sort();
        for e in v {
            files_under(&e, out);
        }
    } else if p.is_file() && !name.starts_with('.') && is_book_file(p) {
        out.push(p.to_path_buf());
    }
}

/// A file type a reader opens (not a launcher script or a sidecar).
fn is_book_file(p: &Path) -> bool {
    const BOOKS: &[&str] = &["epub", "kepub", "pdf", "mobi", "azw", "azw3", "prc", "fb2", "txt", "html", "htm", "cbz", "zip", "djvu", "xps", "docx", "rtf"];
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| BOOKS.contains(&e.to_ascii_lowercase().as_str()))
}

/// `koreader-sync`: [`ink_store::koreader::sync_book`] for every file under the given
/// paths that KOReader has data for, then the library index takes the new progress.
/// Prints one line per book; exit status 1 when a book failed.
fn koreader_sync(args: &[String]) -> anyhow::Result<i32> {
    use ink_store::koreader::{self, KoreaderDirs};
    let mut dry_run = false;
    let mut ko = KoreaderDirs::kindle();
    let mut roots = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "--ko" => ko = KoreaderDirs { root: PathBuf::from(option_value(&mut it, "--ko")?) },
            _ => roots.push(PathBuf::from(a)),
        }
    }
    if roots.is_empty() {
        anyhow::bail!("koreader-sync needs a PATH\n{USAGE}");
    }
    let mut files = Vec::new();
    for r in &roots {
        files_under(r, &mut files);
    }
    let (_, paths) = load_settings();
    let mut index = ink_store::LibraryIndex::load(&paths.library()).ok();
    let (mut synced, mut failed, mut indexed) = (0, 0, 0);
    for f in &files {
        match koreader::sync_book(f, Some(&ko), dry_run) {
            Ok(Some(r)) => {
                synced += 1;
                let s = r.summary;
                println!(
                    "{}: position {} ({} newer), +{} highlights/notes, +{} bookmarks",
                    f.display(),
                    if s.position { "from KOReader" } else { "kept" },
                    if r.koreader_newer { "KOReader" } else { "rinky" },
                    s.highlights,
                    s.bookmarks
                );
                log::info!("koreader-sync {}: {:?} from {} (dry run {dry_run})", f.display(), s, r.source.display());
                if !dry_run {
                    if let (Some(ix), Ok(state)) = (index.as_mut(), ink_store::BookState::load(f)) {
                        indexed += usize::from(ix.update_from_state(f, &state));
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                failed += 1;
                println!("{}: error: {e}", f.display());
            }
        }
    }
    if !dry_run && indexed > 0 {
        if let Some(ix) = &index {
            ix.save(&paths.library())?;
        }
    }
    println!("{synced} books synced{}, {indexed} library entries updated, {failed} failed", if dry_run { " (dry run, nothing written)" } else { "" });
    Ok(i32::from(failed > 0))
}

fn run(args: &[String]) -> anyhow::Result<i32> {
    let cmd = args.get(1).map(String::as_str);
    let rest = if args.len() > 2 { &args[2..] } else { &[] };
    match cmd {
        Some("render") if rest.len() >= 2 => {
            let count = match rest.get(2) {
                Some(v) => v.parse().map_err(|e| anyhow::anyhow!("PAGES {v}: {e}"))?,
                None => 5,
            };
            // Optional `--settings FILE`: a settings.toml whose [reader] section is used.
            let settings = match rest.iter().position(|a| a == "--settings") {
                Some(i) => {
                    let f = rest.get(i + 1).ok_or_else(|| anyhow::anyhow!("--settings needs a FILE"))?;
                    Settings::load(Path::new(f)).map_err(|e| anyhow::anyhow!("--settings {f}: {e}"))?
                }
                None => Settings::default(),
            };
            render_pngs_with(Path::new(&rest[0]), Path::new(&rest[1]), count, &settings)?;
            Ok(0)
        }
        Some("run") if !rest.is_empty() => {
            let (settings, paths) = load_settings();
            let device = open_kindle()?;
            app_start(&device, &settings, &paths);
            App::new(device, settings, paths).run(Route::Reader(PathBuf::from(&rest[0])))?;
            Ok(0)
        }
        Some("library") => {
            let (settings, paths) = load_settings();
            let device = open_kindle()?;
            app_start(&device, &settings, &paths);
            App::new(device, settings, paths).run(Route::Library)?;
            Ok(0)
        }
        Some("sim") => {
            let (rest, s) = parse_sim_args(rest)?;
            let Some(book) = rest.first() else { anyhow::bail!("sim needs a BOOK\n{USAGE}") };
            let device = open_sim_device(&s)?;
            let paths = Paths { root: std::env::temp_dir().join("rinky-sim") };
            // A folder opens the library screen on it (library agent, b-library.md).
            crate::logging::set_home(&paths.root);
            if Path::new(book).is_dir() {
                let settings = sim_library_settings(&paths, Path::new(book));
                app_start(&device, &settings, &paths);
                App::new(device, settings, paths).run(Route::Library)?;
                return Ok(0);
            }
            app_start(&device, &Settings::default(), &paths);
            App::new(device, Settings::default(), paths).run(Route::Reader(PathBuf::from(book)))?;
            Ok(0)
        }
        Some("demo") => {
            let (_, s) = parse_sim_args(rest)?;
            let device = open_sim_device(&s)?;
            let paths = Paths { root: std::env::temp_dir().join("rinky-sim") };
            crate::logging::set_home(&paths.root);
            App::new(device, Settings::default(), paths).run(Route::Demo)?;
            Ok(0)
        }
        Some("selftest") => {
            let mut opts = crate::selftest::SelftestOptions::default();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--screen" => opts.screen = true,
                    "--lipc" => opts.lipc = true,
                    "--device" => opts.device = true,
                    "--write" => opts.device_write = true,
                    "--ntp" => opts.ntp = Some(option_value(&mut it, "--ntp")?.clone()),
                    "--book" => opts.book = Some(PathBuf::from(option_value(&mut it, "--book")?)),
                    "--url" => opts.url = Some(option_value(&mut it, "--url")?.clone()),
                    "--net" => opts.net = Some(option_value(&mut it, "--net")?.clone()),
                    "--net-tls" => opts.net_tls = Some(option_value(&mut it, "--net-tls")?.clone()),
                    "--ca" => opts.ca = Some(PathBuf::from(option_value(&mut it, "--ca")?)),
                    "--net-scratch" => opts.net_scratch = Some(PathBuf::from(option_value(&mut it, "--net-scratch")?)),
                    other => anyhow::bail!("unknown selftest option {other}"),
                }
            }
            if opts.book.is_none() {
                opts.book = std::env::current_exe().ok().and_then(|e| crate::selftest::default_book(&e));
            }
            Ok(crate::selftest::run(&opts) as i32)
        }
        Some("splash") => {
            let lines: Vec<&str> = rest.iter().map(String::as_str).collect();
            anyhow::ensure!(!lines.is_empty(), "splash needs TEXT");
            #[cfg(target_os = "linux")]
            {
                splash(&lines)?;
                Ok(0)
            }
            #[cfg(not(target_os = "linux"))]
            anyhow::bail!("`splash` needs the Kindle framebuffer")
        }
        Some("refresh") => {
            #[cfg(target_os = "linux")]
            {
                use ink_device::Screen;
                ink_device::kindle::open_screen()?.refresh_all()?;
                Ok(0)
            }
            #[cfg(not(target_os = "linux"))]
            anyhow::bail!("`refresh` needs the Kindle framebuffer")
        }
        Some("inject") if rest.first().map(String::as_str) == Some("power") => {
            #[cfg(target_os = "linux")]
            {
                let dev = ink_device::kindle::input::inject_key("pwrkey", ink_device::evdev::KEY_POWER)?;
                println!("injected KEY_POWER press+release into {dev}");
                Ok(0)
            }
            #[cfg(not(target_os = "linux"))]
            anyhow::bail!("`inject` needs the Kindle input devices")
        }
        Some("screensaver") if !rest.is_empty() => {
            #[cfg(target_os = "linux")]
            {
                use ink_device::{RefreshClass, Screen};
                let img = Canvas::load_png(Path::new(&rest[0]))?;
                let mut fb = ink_device::kindle::open_screen()?;
                let size = fb.size();
                let mut canvas = Canvas::new(size.w as usize, size.h as usize);
                let (x, y) = ((size.w as i32 - img.width as i32) / 2, (size.h as i32 - img.height as i32) / 2);
                canvas.blit(&img, x, y);
                fb.update_all(&canvas, RefreshClass::Gc16)?;
                println!("screensaver {}x{} shown", img.width, img.height);
                Ok(0)
            }
            #[cfg(not(target_os = "linux"))]
            anyhow::bail!("`screensaver` needs the Kindle framebuffer")
        }
        Some("evdump") => {
            #[cfg(target_os = "linux")]
            {
                evdump(evdump_deadline(std::time::Instant::now(), rest.first())?)?;
                Ok(0)
            }
            #[cfg(not(target_os = "linux"))]
            anyhow::bail!("`evdump` needs the Kindle input devices")
        }
        Some("spike-suspend") => {
            let mut image = None;
            let mut limit = None;
            let mut suspend = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--image" => image = Some(PathBuf::from(option_value(&mut it, "--image")?)),
                    "--seconds" => {
                        let v = option_value(&mut it, "--seconds")?;
                        let secs: u64 = v.parse().map_err(|e| anyhow::anyhow!("--seconds {v}: {e}"))?;
                        limit = Some(std::time::Duration::from_secs(secs));
                    }
                    "--suspend" => {
                        let v = option_value(&mut it, "--suspend")?;
                        suspend = Some(v.parse().map_err(|e| anyhow::anyhow!("--suspend {v}: {e}"))?);
                    }
                    other => anyhow::bail!("unknown spike-suspend option {other}"),
                }
            }
            crate::spike::run(open_kindle_with(false)?, image.as_deref(), limit, suspend)?;
            Ok(0)
        }
        Some("koreader-sync") => koreader_sync(rest),
        Some("perf") if rest.len() == 3 && rest[0] == "library" => {
            crate::perf::library_bench(Path::new(&rest[1]), Path::new(&rest[2]))?;
            Ok(0)
        }
        Some("plugins") => {
            let (settings, paths) = load_settings();
            crate::plugin_host::cli(rest, crate::plugins::compiled_in(), &settings, &paths)
        }
        Some("export-highlights") => {
            let (_, paths) = load_settings();
            crate::export::cli(rest, &paths)
        }
        Some("device") => Ok(ink_device::devtool::main(rest)),
        Some("update") => crate::update::cli(rest),
        Some("version") | Some("--version") => {
            println!("rinky {}", env!("CARGO_PKG_VERSION"));
            println!("commit {}", option_env!("RINKY_COMMIT").unwrap_or("unknown"));
            println!("built {}", option_env!("RINKY_BUILD_TIME").unwrap_or("unknown"));
            Ok(0)
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_args() {
        let a: Vec<String> = ["book.epub", "--ghost", "--script", "s.txt", "--headless", "--out", "o"].iter().map(|s| s.to_string()).collect();
        let (rest, s) = parse_sim_args(&a).unwrap();
        assert_eq!(rest, vec!["book.epub"]);
        assert!(s.ghost && s.headless);
        assert_eq!(s.script, Some(PathBuf::from("s.txt")));
        assert!(parse_sim_args(&["--bogus".to_string()]).is_err());
        assert!(parse_sim_args(&["--scale".to_string()]).is_err());
        for bad in ["NaN", "inf", "-1", "0", "100", "x"] {
            assert!(parse_sim_args(&["--scale".to_string(), bad.to_string()]).is_err(), "--scale {bad}");
        }
        assert_eq!(parse_sim_args(&["--scale".to_string(), "0.5".to_string()]).unwrap().1.scale, Some(0.5));
    }

    #[test]
    fn sim_dir_reads_the_saved_settings() {
        let root = std::env::temp_dir().join(format!("rinky-cli-simset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths { root: root.clone() };
        let dir = Path::new("/books");
        // No file: defaults with the folder.
        let s = sim_library_settings(&paths, dir);
        assert_eq!(s.library_dirs, vec![PathBuf::from("/books")]);
        assert_eq!(s.device.rotation, Settings::default().device.rotation);
        // A saved rotation and folder: the rotation is kept, the folder is replaced.
        let mut saved = Settings::default();
        saved.device.rotation = 1;
        saved.library_dirs = vec![PathBuf::from("/elsewhere")];
        std::fs::create_dir_all(&root).unwrap();
        saved.save(&paths.settings()).unwrap();
        let s = sim_library_settings(&paths, dir);
        assert_eq!(s.device.rotation, 1);
        assert_eq!(s.library_dirs, vec![PathBuf::from("/books")]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn args(list: &[&str]) -> Vec<String> {
        std::iter::once("rinky").chain(list.iter().copied()).map(String::from).collect()
    }

    fn err(list: &[&str]) -> String {
        format!("{:#}", run(&args(list)).expect_err(&format!("{list:?} must fail")))
    }

    #[test]
    fn options_without_a_value_are_errors() {
        assert!(err(&["selftest", "--book"]).contains("--book needs a value"));
        assert!(err(&["selftest", "--url"]).contains("--url needs a value"));
        assert!(err(&["selftest", "--bogus"]).contains("unknown selftest option"));
        assert!(err(&["spike-suspend", "--seconds"]).contains("--seconds needs a value"));
        assert!(err(&["spike-suspend", "--suspend"]).contains("--suspend needs a value"));
        assert!(err(&["spike-suspend", "--image"]).contains("--image needs a value"));
        assert!(err(&["spike-suspend", "--seconds", "x"]).contains("--seconds x"));
        assert!(err(&["render", "a.epub", "out", "x"]).contains("invalid digit"));
    }

    #[test]
    fn bad_books_are_errors_not_panics() {
        let dir = std::env::temp_dir().join(format!("ink-cli-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dir.epub")).unwrap();
        std::fs::write(dir.join("empty.epub"), b"").unwrap();
        std::fs::write(dir.join("junk.epub"), b"not a zip").unwrap();
        std::fs::write(dir.join("empty.txt"), b"").unwrap();
        let out = dir.join("out");
        for book in ["dir.epub", "empty.epub", "junk.epub", "missing.epub", "empty.txt"] {
            let p = dir.join(book);
            let r = std::panic::catch_unwind(|| render_pngs(&p, &out, 1));
            assert!(matches!(r, Ok(Err(_))), "{book}: expected an error, got {:?}", r.map(|x| x.map(|f| f.len())));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn evdump_seconds_never_overflow() {
        let now = std::time::Instant::now();
        let arg = |v: &str| Some(v.to_string());
        assert_eq!(evdump_deadline(now, None).unwrap() - now, std::time::Duration::from_secs(10));
        assert_eq!(evdump_deadline(now, arg("3").as_ref()).unwrap() - now, std::time::Duration::from_secs(3));
        let max = format!("{:#}", evdump_deadline(now, arg(&u64::MAX.to_string()).as_ref()).unwrap_err());
        assert!(max.contains("too large"), "{max}");
        for bad in ["-1", "x", "18446744073709551616", ""] {
            assert!(evdump_deadline(now, arg(bad).as_ref()).is_err(), "evdump {bad:?}");
        }
    }

    #[test]
    fn render_errors_name_the_path() {
        let dir = std::env::temp_dir().join(format!("ink-cli-outfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let book = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/synthetic.epub");
        let e = format!("{:#}", render_pngs(&book, &file, 1).unwrap_err());
        assert!(e.contains(&file.display().to_string()), "{e}");
        assert!(err(&["render", "a.epub", "out", "x"]).contains("PAGES x"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn usage_and_unknown_commands() {
        assert_eq!(run(&["rinky".into()]).unwrap(), 2);
        assert_eq!(run(&["rinky".into(), "version".into()]).unwrap(), 0);
        assert!(USAGE.contains("export-highlights [--all | BOOK]"));
    }
}
