//! Font registry of the app: which text font families are installed, the UI font, and
//! drawing a family name in its own font.
//!
//! - At startup [`start`] reads `cache/fonts.json` (families found last time, keyed by
//!   the modification times of the font folders) and publishes it at once
//!   ([`ink_text::registry::set_installed`]), then a background thread compares the
//!   folder times and scans again only when they changed. Folders: the Kindle's system
//!   fonts (`/usr/java/lib/fonts`: Bookerly, Amazon Ember, Caecilia, ...; only loaded at
//!   runtime, never copied or packaged), `/mnt/us/fonts`, `/mnt/us/rinky/fonts`, and
//!   the folders in `RINKY_FONT_DIRS` (colon-separated, for tests and the host).
//!   `/mnt/us/koreader` is never read.
//! - Body text: `ink-html` adds every installed family to its font book, so a layout with
//!   `font_family = "Bookerly"` sets the book in Bookerly. [`reading_default`] is the
//!   family for books without their own choice.
//! - UI font: `settings.ui.font_family` (empty: the built-in Noto Sans, [`UI_BUILTIN`];
//!   "Noto Serif": the embedded reading faces). Bold UI text (titles, headings) is drawn
//!   in the family's regular face, so titles are light. Parsing a face
//!   for the UI costs about 0.5 s, so [`request_ui_font`] loads it on a thread and the
//!   app loop swaps it in ([`take_ui_fonts`]). Every UI font gets the fallback faces of
//!   [`default_ui_fallbacks`] for characters it lacks (IPA in dictionary pronunciations).
//! - [`preview_face`] / [`draw_family_text`]: a family's regular face drawn on the shaped
//!   path (mapped file, glyphs outlined on first use), for pickers and previews.

use ink_store::{Paths, Settings};
use ink_text::registry::{self, FaceRef, FontFamily};
use ink_text::{FaceData, FaceId, Fonts, Style};
use ink_ui::PaintCx;
use ink_device::Rect;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Name of the built-in family (embedded Noto Serif); `ink-html` maps it to its default.
pub const BUILTIN: &str = "Noto Serif";

/// Name of the built-in UI family: Noto Sans, embedded (OFL, `fonts/LICENSE-Noto`). The
/// UI draws bold and italic text in its regular face (light titles); its bold and italic
/// faces set text whose emphasis matters ([`emphasis_fonts`]).
pub const UI_BUILTIN: &str = "Noto Sans";
static UI_REGULAR: &[u8] = include_bytes!("../../../fonts/ui/NotoSans-Regular.ttf");
/// Its bold and italic faces, for UI text whose emphasis matters ([`emphasis_fonts`]).
static UI_BOLD: &[u8] = include_bytes!("../../../fonts/ui/NotoSans-Bold.ttf");
static UI_ITALIC: &[u8] = include_bytes!("../../../fonts/ui/NotoSans-Italic.ttf");

/// Family used for books when the user chose none and the Kindle has it.
pub const PREFERRED_READING: &str = "Bookerly";

/// Environment variable with extra font folders (colon-separated).
pub const ENV_DIRS: &str = "RINKY_FONT_DIRS";

/// Cache format version.
const CACHE_VERSION: u64 = 1;

/// The folders scanned for text fonts, in order (earlier folders win a family name).
pub fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = ink_platform::current().font_dirs();
    if let Ok(v) = std::env::var(ENV_DIRS) {
        dirs.extend(v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    // Never KOReader's folders.
    let koreader = ink_platform::current().koreader_dir();
    dirs.retain(|d| !d.starts_with(&koreader));
    dirs.dedup();
    dirs
}

/// Modification time of `dir` in nanoseconds since 1970 (`None`: missing folder).
fn dir_mtime(dir: &Path) -> Option<u64> {
    let t = std::fs::metadata(dir).ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64)
}

/// The folder key of the cache: each folder with its modification time.
fn dir_key(dirs: &[PathBuf]) -> Vec<(String, Option<u64>)> {
    dirs.iter().map(|d| (d.display().to_string(), dir_mtime(d))).collect()
}

fn face_json(f: &Option<FaceRef>) -> Value {
    match f {
        Some(f) => json!({ "path": f.path.display().to_string(), "index": f.index, "weight": f.weight }),
        None => Value::Null,
    }
}

/// The cache file's text.
pub fn cache_json(key: &[(String, Option<u64>)], families: &[FontFamily]) -> String {
    let dirs: Vec<Value> = key.iter().map(|(p, m)| json!({ "path": p, "mtime": m })).collect();
    let fams: Vec<Value> =
        families.iter().map(|f| json!({ "name": f.name, "faces": f.faces.iter().map(face_json).collect::<Vec<_>>() })).collect();
    json!({ "version": CACHE_VERSION, "dirs": dirs, "families": fams }).to_string()
}

/// Folder keys of a font cache: (folder, modification time).
pub type DirKeys = Vec<(String, Option<u64>)>;

/// Reads a cache file's text: (folder key, families). `None` when it is not a cache of
/// this version.
pub fn parse_cache(text: &str) -> Option<(DirKeys, Vec<FontFamily>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    if v.get("version")?.as_u64()? != CACHE_VERSION {
        return None;
    }
    let key = v.get("dirs")?.as_array()?.iter().map(|d| Some((d.get("path")?.as_str()?.to_string(), d.get("mtime")?.as_u64()))).collect::<Option<Vec<_>>>()?;
    let face = |f: &Value| -> Option<FaceRef> {
        Some(FaceRef {
            path: PathBuf::from(f.get("path")?.as_str()?),
            index: f.get("index")?.as_u64()? as u32,
            weight: f.get("weight")?.as_u64()? as u16,
        })
    };
    let mut families = Vec::new();
    for f in v.get("families")?.as_array()? {
        let faces = f.get("faces")?.as_array()?;
        let mut fam = FontFamily { name: f.get("name")?.as_str()?.to_string(), ..FontFamily::default() };
        for (slot, x) in faces.iter().take(4).enumerate() {
            fam.faces[slot] = face(x);
        }
        if fam.regular().is_some() {
            families.push(fam);
        }
    }
    Some((key, families))
}

/// Scans `dirs` and groups the faces into families (logged with the time taken).
pub fn scan(dirs: &[PathBuf]) -> Vec<FontFamily> {
    let t = Instant::now();
    let entries = registry::scan_fonts(dirs);
    let families = registry::group_families(&entries);
    log::info!("fonts: scanned {} faces in {} folders in {:?}: {} text families", entries.len(), dirs.len(), t.elapsed(), families.len());
    for f in &families {
        log::info!("fonts: family '{}' faces {}", f.name, f.styles());
    }
    families
}

/// True while the first scan of this run has not published anything yet.
static SCANNING: AtomicBool = AtomicBool::new(false);
/// Signalled when the scan thread is done.
static SCAN_DONE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Starts the font registry: publishes the cached families at once (when the cache is
/// there), checks and rescans the folders on a thread, then loads the UI font named in
/// `settings.ui.font_family` (if any) on that thread.
pub fn start(paths: &Paths, settings: &Settings) {
    let dirs = font_dirs();
    let cache = paths.cache().join("fonts.json");
    let t = Instant::now();
    let cached = std::fs::read_to_string(&cache).ok().and_then(|s| parse_cache(&s));
    if let Some((_, fams)) = &cached {
        registry::set_installed(fams.clone());
        log::info!("fonts: {} families from {} in {:?}", fams.len(), cache.display(), t.elapsed());
    } else {
        SCANNING.store(true, Ordering::SeqCst);
        log::info!("fonts: no font cache at {}, scanning", cache.display());
    }
    let ui = settings.ui.font_family.clone();
    let spawned = std::thread::Builder::new().name("font-scan".into()).spawn(move || {
        let key = dir_key(&dirs);
        match cached {
            Some((old, _)) if old == key => log::info!("fonts: font folders unchanged, cache kept"),
            _ => {
                let fams = scan(&dirs);
                registry::set_installed(fams.clone());
                if let Some(parent) = cache.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match ink_store::write_atomic(&cache, cache_json(&key, &fams).as_bytes()) {
                    Ok(()) => log::info!("fonts: cache written to {}", cache.display()),
                    Err(e) => log::warn!("fonts: cannot write {}: {e}", cache.display()),
                }
            }
        }
        SCANNING.store(false, Ordering::SeqCst);
        *SCAN_DONE.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        SCAN_DONE.1.notify_all();
        if !ui.is_empty() {
            load_ui_now(&ui, UI_GEN.load(Ordering::SeqCst));
        }
    });
    if let Err(e) = spawned {
        log::warn!("fonts: cannot start the font scan thread: {e}");
        SCANNING.store(false, Ordering::SeqCst);
    }
}

/// Waits (at most `max`) for the first scan when no cached families were published yet,
/// so the first book opened after installing finds Bookerly. Returns at once otherwise.
pub fn wait_first_scan(max: Duration) {
    if !SCANNING.load(Ordering::SeqCst) {
        return;
    }
    let t = Instant::now();
    let guard = SCAN_DONE.0.lock().unwrap_or_else(|e| e.into_inner());
    let _ = SCAN_DONE.1.wait_timeout_while(guard, max, |done| !*done);
    log::info!("fonts: waited {:?} for the first font scan", t.elapsed());
}

/// Installed family names, sorted (without the built-in family).
pub fn family_names() -> Vec<String> {
    registry::installed().iter().map(|f| f.name.clone()).collect()
}

/// Families the interface picker lists: the built-in Noto Sans and Noto Serif, then the
/// installed families with a bold and an italic face (dictionary entries and other
/// UI text with emphasis need both), and `current` (the family in use) in any case.
pub fn ui_picker_names(current: &str) -> Vec<String> {
    ui_list(&registry::installed(), current)
}

fn ui_list(installed: &[FontFamily], current: &str) -> Vec<String> {
    let mut v = vec![BUILTIN.to_string(), UI_BUILTIN.to_string()];
    for f in installed {
        let complete = f.faces[1].is_some() && f.faces[2].is_some();
        let listed = v.iter().any(|n| n.eq_ignore_ascii_case(&f.name));
        if !listed && (complete || f.name.eq_ignore_ascii_case(current.trim())) {
            v.push(f.name.clone());
        }
    }
    v
}

/// Families a picker lists: the built-in family first, then the installed ones.
pub fn picker_names() -> Vec<String> {
    let mut v = vec![BUILTIN.to_string()];
    v.extend(family_names().into_iter().filter(|n| !n.eq_ignore_ascii_case(BUILTIN)));
    v
}

/// The reading font for books without their own: `settings.reader.font_family`, else
/// Bookerly when installed, else the built-in font (`None`).
pub fn reading_default(settings: &Settings) -> Option<String> {
    let chosen = settings.reader.font_family.trim();
    if !chosen.is_empty() {
        return (!chosen.eq_ignore_ascii_case(BUILTIN)).then(|| chosen.to_string());
    }
    registry::installed_family(PREFERRED_READING).map(|f| f.name)
}

// ---- UI font ----

/// Generation of the newest UI font request (older results are dropped).
static UI_GEN: AtomicU64 = AtomicU64::new(0);
/// A UI font load is running.
static UI_PENDING: AtomicBool = AtomicBool::new(false);
/// A loaded UI font waiting for the app loop: (generation, family name, fonts).
static UI_READY: Mutex<Option<(u64, String, Fonts)>> = Mutex::new(None);

/// One entry of the UI fallback chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiFallback {
    /// A font file (skipped when missing).
    File(PathBuf),
    /// The embedded Noto Serif Regular (left out when the UI font is the built-in one).
    Builtin,
}

/// Fallback faces of the UI fonts, in order, for characters the UI font lacks (IPA in
/// dictionary pronunciations, combining double macrons and breves, math and dingbats):
/// Noto Sans Regular shipped in rinky's `fonts` folder (all IPA of the Oxford
/// dictionaries, a sans face like most UI fonts), the embedded Noto Serif, then the
/// system's symbol fonts ([`ink_platform::Platform::symbol_fonts`]).
pub fn default_ui_fallbacks() -> Vec<UiFallback> {
    let p = ink_platform::current();
    let mut v = vec![UiFallback::File(p.install_root().join("fonts/NotoSans-Regular.ttf")), UiFallback::Builtin];
    v.extend(p.symbol_fonts().iter().map(|f| UiFallback::File(PathBuf::from(f))));
    v
}

/// Environment variable replacing [`default_ui_fallbacks`]: colon-separated font files, the word
/// `builtin` for the embedded Noto Serif (tests and the host).
pub const ENV_UI_FALLBACKS: &str = "RINKY_UI_FALLBACKS";

/// The UI fallback chain: [`ENV_UI_FALLBACKS`] when set, else [`default_ui_fallbacks`].
pub fn ui_fallback_chain() -> Vec<UiFallback> {
    let parse = |s: &str| if s == "builtin" { UiFallback::Builtin } else { UiFallback::File(PathBuf::from(s)) };
    match std::env::var(ENV_UI_FALLBACKS) {
        Ok(v) => v.split(':').filter(|s| !s.is_empty()).map(parse).collect(),
        Err(_) => default_ui_fallbacks(),
    }
}

/// Appends the [`ui_fallback_chain`] to `fonts` as fallback faces (`builtin`: the UI font
/// is the embedded Noto Serif, so the chain's Noto Serif entry is left out). Files are
/// mapped, only their headers are read. Returns the number of faces added.
pub fn add_ui_fallbacks(fonts: &mut Fonts, builtin: bool) -> usize {
    add_fallback_chain(fonts, &ui_fallback_chain(), builtin)
}

/// Appends the faces of `chain` to `fonts` (see [`add_ui_fallbacks`]).
pub fn add_fallback_chain(fonts: &mut Fonts, chain: &[UiFallback], builtin: bool) -> usize {
    let t = Instant::now();
    let mut added = Vec::new();
    for entry in chain {
        let face = match entry {
            UiFallback::Builtin if builtin => continue,
            UiFallback::Builtin => FaceData::from_static(ink_text::embedded_font_bytes(Style::Regular), 0),
            UiFallback::File(p) if !p.is_file() => {
                log::info!("fonts: UI fallback {} missing, skipped", p.display());
                continue;
            }
            UiFallback::File(p) => FaceData::open(p, 0),
        };
        match face {
            Ok(f) => {
                added.push(f.info.label());
                fonts.add_fallback(f);
            }
            Err(e) => log::warn!("fonts: UI fallback {entry:?}: {e}"),
        }
    }
    log::info!("fonts: {} UI fallback faces in {:?}: {}", added.len(), t.elapsed(), added.join(", "));
    added.len()
}

/// The embedded Noto Sans UI fonts with the UI fallback faces.
pub fn builtin_ui_fonts() -> Fonts {
    family_fonts("", false).expect("embedded UI font")
}

/// The UI family `name` with its bold and italic faces, for UI text whose emphasis
/// matters (a dictionary entry). The interface picker offers only families that have
/// both ([`ui_picker_names`]); a face missing from one chosen before is its regular
/// face. Falls back to the built-in family.
pub fn emphasis_fonts(name: &str) -> Fonts {
    family_fonts(name, true).unwrap_or_else(|| family_fonts("", true).expect("embedded UI font"))
}

/// Fonts of UI family `name` (empty or [`UI_BUILTIN`]: the embedded Noto Sans;
/// [`BUILTIN`]: the embedded Noto Serif) with the UI fallback faces
/// ([`add_ui_fallbacks`]). With `emphasis` the bold slot holds the family's bold face;
/// without, bold text is drawn in the regular face (light titles), and so is italic
/// text in Noto Sans. A missing bold or italic face is the regular one. `None` when the
/// family is not installed or does not parse.
fn family_fonts(name: &str, emphasis: bool) -> Option<Fonts> {
    let name = name.trim();
    let (faces, noto_serif) = if name.is_empty() || name.eq_ignore_ascii_case(UI_BUILTIN) {
        (if emphasis { [UI_REGULAR, UI_BOLD, UI_ITALIC] } else { [UI_REGULAR; 3] }, false)
    } else if name.eq_ignore_ascii_case(BUILTIN) {
        let face = ink_text::embedded_font_bytes;
        ([face(Style::Regular), face(if emphasis { Style::Bold } else { Style::Regular }), face(Style::Italic)], true)
    } else {
        let fam = registry::installed_family(name)?;
        let map = |f: Option<&FaceRef>| f.and_then(|f| ink_text::face::map_file(&f.path).ok());
        let r = map(fam.regular())?;
        let b = if emphasis { map(fam.face(true, false)).unwrap_or(r) } else { r };
        ([r, b, map(fam.face(false, true)).unwrap_or(r)], false)
    };
    match Fonts::from_static(faces) {
        Ok(mut f) => {
            add_ui_fallbacks(&mut f, noto_serif);
            Some(f)
        }
        Err(e) => {
            log::warn!("fonts: UI font '{name}': {e}");
            None
        }
    }
}

/// The UI family in use (empty: the built-in one), as last put in place by
/// [`take_ui_fonts`].
pub fn ui_family() -> String {
    UI_FAMILY.lock().map(|n| n.clone()).unwrap_or_default()
}

static UI_FAMILY: Mutex<String> = Mutex::new(String::new());

/// UI fonts for family `name` (see [`family_fonts`]): bold text uses the regular face
/// (light titles). `None` when the family is not installed or does not parse.
pub fn ui_fonts(name: &str) -> Option<Fonts> {
    family_fonts(name, false)
}

fn load_ui_now(name: &str, generation: u64) {
    let t = Instant::now();
    let fonts = ui_fonts(name);
    log::info!("fonts: UI font '{}' {} in {:?}", name, if fonts.is_some() { "loaded" } else { "not available" }, t.elapsed());
    if let Some(f) = fonts {
        if UI_GEN.load(Ordering::SeqCst) == generation {
            *UI_READY.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, name.to_string(), f));
        }
    }
    if UI_GEN.load(Ordering::SeqCst) == generation {
        UI_PENDING.store(false, Ordering::SeqCst);
    }
}

/// Loads the UI font `name` on a thread (empty: the built-in font); the app loop picks it
/// up with [`take_ui_fonts`].
pub fn request_ui_font(name: &str) {
    let generation = UI_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    UI_PENDING.store(true, Ordering::SeqCst);
    log::info!("fonts: loading UI font '{name}'");
    let name = name.to_string();
    if let Err(e) = std::thread::Builder::new().name("ui-font".into()).spawn(move || {
        wait_first_scan(Duration::from_secs(5));
        load_ui_now(&name, generation);
    }) {
        log::warn!("fonts: cannot start the UI font thread: {e}");
        UI_PENDING.store(false, Ordering::SeqCst);
    }
}

/// True while a UI font is loading (the app loop then wakes up to pick it up).
pub fn ui_font_pending() -> bool {
    UI_PENDING.load(Ordering::SeqCst) || UI_READY.lock().map(|r| r.is_some()).unwrap_or(false)
}

/// The UI font loaded for the newest request, once.
pub fn take_ui_fonts() -> Option<(String, Fonts)> {
    let mut slot = UI_READY.lock().unwrap_or_else(|e| e.into_inner());
    let (generation, name, fonts) = slot.take()?;
    if generation != UI_GEN.load(Ordering::SeqCst) {
        return None;
    }
    *UI_FAMILY.lock().unwrap_or_else(|e| e.into_inner()) = name.clone();
    Some((name, fonts))
}

// ---- Drawing a family name in its own font ----

thread_local! {
    /// Family name -> (face id in the UI fonts, its file; `None`: embedded).
    static PREVIEW: RefCell<HashMap<String, (FaceId, Option<PathBuf>)>> = RefCell::new(HashMap::new());
}

/// True when face `id` of `fonts` is the regular face of the family stored as `path`
/// (`None`: the embedded face of `family`, Noto Serif or Noto Sans).
fn face_is(fonts: &Fonts, id: FaceId, path: &Option<PathBuf>, family: &str) -> bool {
    fonts.face_data(id).is_some_and(|d| d.info.path == *path && (path.is_some() || d.info.family.eq_ignore_ascii_case(family)))
}

/// The face of `fonts` that draws family `family` (its regular face, added to `fonts`
/// on first use; checked by file, so a swapped UI font gets the face again). `None` for
/// a family that is not installed. A hit costs one map lookup (no allocation), so
/// previews may call it per frame.
pub fn preview_face(fonts: &mut Fonts, family: &str) -> Option<FaceId> {
    let builtin = family.trim().is_empty() || family.eq_ignore_ascii_case(BUILTIN);
    let ui_builtin = family.eq_ignore_ascii_case(UI_BUILTIN) && registry::installed_family(family).is_none();
    let embedded = if ui_builtin { UI_BUILTIN } else { BUILTIN };
    let hit = PREVIEW.with(|p| p.borrow().get(family).and_then(|(id, path)| face_is(fonts, *id, path, embedded).then_some(*id)));
    if hit.is_some() {
        return hit;
    }
    let want: Option<PathBuf> = if builtin || ui_builtin { None } else { Some(registry::installed_family(family)?.regular()?.path.clone()) };
    // The UI font may be the family itself.
    let id = if face_is(fonts, 0, &want, embedded) {
        0
    } else {
        let face = match &want {
            None if ui_builtin => FaceData::from_static(UI_REGULAR, 0),
            None => FaceData::from_static(ink_text::embedded_font_bytes(Style::Regular), 0),
            Some(p) => FaceData::open(p, 0),
        };
        match face {
            Ok(f) => fonts.add_face(f),
            Err(e) => {
                log::warn!("fonts: preview of '{family}': {e}");
                return None;
            }
        }
    };
    PREVIEW.with(|p| p.borrow_mut().insert(family.to_string(), (id, want)));
    Some(id)
}

/// Draws `text` in family `family` at `px` with its left edge at `x`, vertically centred
/// in `row`, inside `clip` and the paint clip; gray `value`. Falls back to the UI font.
/// Returns the text width.
#[allow(clippy::too_many_arguments)]
pub fn draw_family_text(cx: &mut PaintCx, family: &str, px: f32, x: f32, row: Rect, text: &str, value: u8, clip: Rect) -> f32 {
    let clip = cx.clip().map_or(clip, |c| c.intersect(&clip));
    match preview_face(cx.fonts, family) {
        Some(id) => {
            let run = cx.fonts.shape_in(id, px, text);
            let (asc, desc) = cx.fonts.face_vmetrics(id, px);
            let baseline = row.y as f32 + (row.h as f32 - (asc - desc)) / 2.0 + asc;
            cx.fonts.draw_run_clipped(cx.canvas, &run, x, baseline, value, clip);
            run.width
        }
        None => {
            let baseline = cx.baseline_in(row, px, Style::Regular);
            let end = cx.draw_text_clipped(Style::Regular, px, x, baseline, text, value, clip);
            end - x
        }
    }
}

/// Width of `text` in family `family` at `px` (UI font when not installed).
pub fn family_text_width(fonts: &mut Fonts, family: &str, px: f32, text: &str) -> f32 {
    match preview_face(fonts, family) {
        Some(id) => fonts.shape_in(id, px, text).width,
        None => fonts.shaped_width(Style::Regular, px, text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_round_trip_and_defaults() {
        let fam = FontFamily {
            name: "Bookerly".into(),
            faces: [Some(FaceRef { path: "/usr/java/lib/fonts/Bookerly-Regular.ttf".into(), index: 0, weight: 400 }), None, None, None],
        };
        let key = vec![("/usr/java/lib/fonts".to_string(), Some(7)), ("/mnt/us/fonts".to_string(), None)];
        let (k, f) = parse_cache(&cache_json(&key, std::slice::from_ref(&fam))).unwrap();
        assert_eq!((k, f), (key, vec![fam]));
        assert!(parse_cache("{\"version\": 99}").is_none() && parse_cache("nope").is_none());
        assert!(font_dirs().iter().all(|d| !d.starts_with("/mnt/us/koreader")));
        let mut s = Settings::default();
        s.reader.font_family = "Literata".into();
        assert_eq!(reading_default(&s).as_deref(), Some("Literata"));
        s.reader.font_family = BUILTIN.into();
        assert_eq!(reading_default(&s), None);
    }

    /// Titles are light: bold UI text is the regular face, in the built-in Noto Sans and
    /// in the embedded Noto Serif.
    #[test]
    fn ui_bold_text_uses_the_regular_face() {
        for (name, mut f) in [("Noto Sans", builtin_ui_fonts()), ("Noto Serif", ui_fonts(BUILTIN).unwrap())] {
            let t = "Library · Settings";
            assert_eq!(f.text_width(Style::Bold, t, 56.0), f.text_width(Style::Regular, t, 56.0), "{name}");
            let (bold, _) = render(&mut f, &[(Style::Bold, t)]);
            let (regular, _) = render(&mut f, &[(Style::Regular, t)]);
            assert!(bold.pixels == regular.pixels, "{name}: bold draws differently");
        }
        // The reading fonts keep their bold face.
        let mut book = Fonts::load();
        assert_ne!(book.text_width(Style::Bold, "Library", 56.0), book.text_width(Style::Regular, "Library", 56.0));
    }

    /// The interface list: the built-in families, installed ones with a bold and an
    /// italic face, and the one in use.
    #[test]
    fn the_interface_list_has_families_with_bold_and_italic() {
        let face = |w| Some(FaceRef { path: "/f.ttf".into(), index: 0, weight: w });
        let fam = |name: &str, b: bool, i: bool| FontFamily { name: name.into(), faces: [face(400), b.then(|| face(700)).flatten(), i.then(|| face(400)).flatten(), None] };
        let installed = [fam("Bookerly", true, true), fam("Caecilia", true, false), fam("Futura", false, false), fam("Noto Sans", true, true)];
        assert_eq!(ui_list(&installed, ""), ["Noto Serif", "Noto Sans", "Bookerly"]);
        // A family chosen before stays listed, so its check mark shows.
        assert_eq!(ui_list(&installed, "futura"), ["Noto Serif", "Noto Sans", "Bookerly", "Futura"]);
    }

    /// Text whose emphasis matters gets the family's real bold and italic faces.
    #[test]
    fn emphasis_fonts_have_real_bold_and_italic() {
        for name in ["", UI_BUILTIN, BUILTIN] {
            let mut f = emphasis_fonts(name);
            let t = "Household";
            let (regular, _) = render(&mut f, &[(Style::Regular, t)]);
            for style in [Style::Bold, Style::Italic] {
                let run = f.shape(style, 40.0, t);
                assert!(run.glyphs.iter().all(|g| g.synth == ink_text::raster::Synth::NONE), "{name} {style:?}: synthesized");
                assert!(render(&mut f, &[(style, t)]).0.pixels != regular.pixels, "{name} {style:?}: drawn as regular");
            }
        }
    }

    /// Dictionary pronunciations (OALD8 and NOAD StarDict) and symbols from their bodies.
    const IPA_SAMPLE: &str = "/ˌɪntəˈnæʃənl/ ðə θɪŋ ʒ ŋ ɜː ɑː ɒ ʊə ˈnʌðə(r) ɡʊd ɔɪ ʃ ɐ ʀ ʏ ɲ ɣ nʲ";
    const MARK_SAMPLE: &str = "|bo͝ok| |mo͞on| |o͟o| ŋ͡ɡ n̩ k͜w q́ ā ă ä";
    const SYMBOL_SAMPLE: &str = "▪ ◇ ↑ ✗ ✓ ≠ ∫ √ ∞ − ♯ ♭ ▶";

    /// Shapes `lines` with `fonts` (UI style per line), draws them on a canvas, returns
    /// the canvas and every (line, cluster text) drawn as `.notdef`.
    fn render(fonts: &mut Fonts, lines: &[(Style, &str)]) -> (ink_device::Canvas, Vec<String>) {
        let px = 40.0;
        let mut canvas = ink_device::Canvas::new(1400, 80 * lines.len() + 20);
        let mut tofu = Vec::new();
        for (k, (style, text)) in lines.iter().enumerate() {
            let run = fonts.shape(*style, px, text);
            for g in run.glyphs.iter().filter(|g| g.id == 0) {
                tofu.push(format!("{style:?} {:?}", text[g.cluster..].chars().next().unwrap_or(' ')));
            }
            fonts.draw_run(&mut canvas, &run, 20.0, 60.0 + 80.0 * k as f32);
        }
        (canvas, tofu)
    }

    #[test]
    fn ui_fallbacks_draw_ipa_marks_and_symbols() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // A UI font without IPA (Droid Sans Mono), the chain with the shipped Noto Sans,
        // the built-in Noto Serif and a missing file (skipped).
        let mono = repo.join("crates/ink-html/fonts/DroidSansMono.ttf");
        let chain = [UiFallback::File(repo.join("fonts/ui/NotoSans-Regular.ttf")), UiFallback::Builtin, UiFallback::File("/nonexistent/x.ttf".into())];
        let mut fonts = Fonts::from_files([mono.as_path(), mono.as_path(), mono.as_path()]).unwrap();
        assert_eq!(add_fallback_chain(&mut fonts, &chain, false), 2);
        let lines = [(Style::Italic, IPA_SAMPLE), (Style::Italic, MARK_SAMPLE), (Style::Regular, IPA_SAMPLE), (Style::Bold, MARK_SAMPLE)];
        let (_, tofu) = render(&mut fonts, &lines);
        assert!(tofu.is_empty(), "notdef: {tofu:?}");
        // The built-in UI font leaves the chain's Noto Serif out.
        let mut b = Fonts::load();
        assert_eq!(add_fallback_chain(&mut b, &chain, true), 1);
        assert!(render(&mut b, &lines).1.is_empty());
        assert_eq!(ui_fallback_chain().len(), std::env::var(ENV_UI_FALLBACKS).map_or(default_ui_fallbacks().len(), |v| v.split(':').filter(|s| !s.is_empty()).count()));
    }

    /// Renders the samples with a real UI font into a PNG. Run by hand:
    /// `INK_TEST_UI_FONT=Futura-Medium.ttf INK_TEST_UI_FONT_ITALIC=Futura-MediumOblique.ttf
    /// INK_TEST_SYMBOL_FONTS=Kindle_Symbol.ttf:code2000.ttf INK_TEST_PNG=out.png
    /// cargo test -p ink-app ui_font_render_png -- --ignored`
    #[test]
    #[ignore]
    fn ui_font_render_png() {
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (reg, ita) = (PathBuf::from(var("INK_TEST_UI_FONT")), PathBuf::from(var("INK_TEST_UI_FONT_ITALIC")));
        let mut chain = vec![UiFallback::File(repo.join("fonts/ui/NotoSans-Regular.ttf")), UiFallback::Builtin];
        if let Ok(s) = std::env::var("INK_TEST_SYMBOL_FONTS") {
            chain.extend(s.split(':').filter(|s| !s.is_empty()).map(|s| UiFallback::File(s.into())));
        }
        let lines = [
            (Style::Italic, IPA_SAMPLE),
            (Style::Italic, MARK_SAMPLE),
            (Style::Regular, IPA_SAMPLE),
            (Style::Regular, MARK_SAMPLE),
            (Style::Regular, SYMBOL_SAMPLE),
        ];
        // Without fallbacks (before), then with the chain (after), one PNG each.
        let mut before = Fonts::from_files([reg.as_path(), reg.as_path(), ita.as_path()]).unwrap();
        let (canvas, tofu) = render(&mut before, &lines);
        let out = PathBuf::from(var("INK_TEST_PNG"));
        canvas.save_png(&out.with_extension("before.png")).unwrap();
        println!("before: {} notdef glyphs: {tofu:?}", tofu.len());
        let mut after = Fonts::from_files([reg.as_path(), reg.as_path(), ita.as_path()]).unwrap();
        let n = add_fallback_chain(&mut after, &chain, false);
        let (canvas, tofu) = render(&mut after, &lines);
        canvas.save_png(&out).unwrap();
        println!("after ({n} fallback faces): {} notdef glyphs: {tofu:?}", tofu.len());
        let faces: Vec<String> = (0..after.face_count() as FaceId).filter_map(|i| after.face_info(i).map(|f| format!("{i}={}", f.label()))).collect();
        println!("faces: {}", faces.join(", "));
        assert!(tofu.is_empty(), "notdef: {tofu:?}");
    }
}
