//! Font families for the engine, built from `ink_text::Fonts` sets.
//!
//! A family here has up to four faces (regular, bold, italic, bold italic), each parsed on
//! first use into its own `ink_text::Fonts`, so a request for (family, bold, italic) maps
//! to a `Fonts` and `Style::Regular`. Families:
//! - `serif` (default): the embedded Noto Serif faces, with Noto Serif Bold Italic
//!   embedded in this crate;
//! - `monospace`: Droid Sans Mono (embedded in this crate, Apache 2.0);
//! - every `@font-face` family of the book (missing faces fall back to the family's
//!   regular face);
//! - system families found in the Kindle font directories (`/mnt/us/fonts`,
//!   `/usr/java/lib/fonts`), matched by file name, for the user's font choice.
//!
//! Characters a family's face has no glyph for (CJK, Hangul, many symbols) are measured
//! and drawn with fallback faces from [`FontSources::fallback_dirs`] (the Kindle font
//! directories for books; empty in most tests, so host pages do not depend on the host's
//! fonts). The fallback faces go through `ink_text`'s shaped path (memory-mapped files,
//! glyphs outlined on first use), so a 10+ MB CJK font costs nothing until it is needed.

use crate::css::FontFaceRule;
use ink_device::Canvas;
use ink_text::{embedded_font_bytes, Fonts, Style};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

static BOLD_ITALIC: &[u8] = include_bytes!("../fonts/NotoSerif-BoldItalic.ttf");
static MONO: &[u8] = include_bytes!("../fonts/DroidSansMono.ttf");
static PLACEHOLDER: &[u8] = include_bytes!("../fonts/Placeholder.ttf");

/// Family index in a [`FontBook`].
pub type FamilyId = u16;

/// A face request resolved to a family and style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceKey {
    /// Family.
    pub family: FamilyId,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
}

/// Where a family's faces come from.
enum Src {
    Serif,
    Mono,
    Bytes(FamilyBytes),
    /// An installed family (`ink_text::registry::installed`): memory-mapped files by
    /// slot (regular, bold, italic, bold italic).
    Mapped([Option<&'static [u8]>; 4]),
}

/// A family whose faces are parsed one at a time on first use. fontdue parses every glyph
/// of a face up front (about 0.5 s per Noto Serif face in the emulator), and a page often
/// needs only the regular face. Each face lives in the regular slot of its own
/// `ink_text::Fonts` (always used with `Style::Regular`); the bold and italic slots hold a
/// 924-byte placeholder font (Droid Sans Mono subset to `.notdef` and space).
struct Family {
    src: Src,
    /// Faces by [`face_index`]: `None` not parsed yet, `Some(None)` not usable.
    faces: [Option<Option<Fonts>>; 4],
    /// Faces may come from the [`prewarm_faces`] pool (the foreground book only, so the
    /// paginator threads never take the faces parsed for the first page).
    use_pool: bool,
}

/// 0 regular, 1 bold, 2 italic, 3 bold italic.
fn face_index(bold: bool, italic: bool) -> usize {
    bold as usize | (italic as usize) << 1
}

impl Family {
    fn new(src: Src) -> Self {
        Family { src, faces: [None, None, None, None], use_pool: false }
    }

    /// The font data of face `idx` when it lives for the whole program (embedded or
    /// memory-mapped): those faces can be parsed ahead ([`prewarm_faces`]).
    fn static_bytes(&self, idx: usize) -> Option<&'static [u8]> {
        match &self.src {
            Src::Serif => Some(match idx {
                0 => embedded_font_bytes(Style::Regular),
                1 => embedded_font_bytes(Style::Bold),
                2 => embedded_font_bytes(Style::Italic),
                _ => BOLD_ITALIC,
            }),
            Src::Mono => Some(MONO),
            Src::Bytes(_) => None,
            Src::Mapped(f) => {
                let bold = f[1].or(f[0]);
                match idx {
                    0 => f[0],
                    1 => bold,
                    2 => f[2].or(f[0]),
                    _ => f[3].or(bold),
                }
            }
        }
    }

    /// Font data for face `idx`, with the family's fallbacks: a missing bold or italic
    /// face uses the regular one, a missing bold italic the bold one; a book family
    /// without a regular face uses its bold or italic face as the regular one.
    fn bytes(&self, idx: usize) -> Option<&[u8]> {
        match &self.src {
            Src::Serif => Some(match idx {
                0 => embedded_font_bytes(Style::Regular),
                1 => embedded_font_bytes(Style::Bold),
                2 => embedded_font_bytes(Style::Italic),
                _ => BOLD_ITALIC,
            }),
            Src::Mono => Some(MONO),
            Src::Bytes(b) => {
                let reg = b.regular.as_ref().or(b.bold.as_ref()).or(b.italic.as_ref());
                let bold = b.bold.as_ref().or(reg);
                let pick = match idx {
                    0 => reg,
                    1 => bold,
                    2 => b.italic.as_ref().or(reg),
                    _ => b.bold_italic.as_ref().or(bold),
                };
                pick.map(|a| a.as_slice())
            }
            Src::Mapped(f) => {
                let bold = f[1].or(f[0]);
                match idx {
                    0 => f[0],
                    1 => bold,
                    2 => f[2].or(f[0]),
                    _ => f[3].or(bold),
                }
            }
        }
    }

    /// The lowest face index with the same font data as `idx` (shared data is parsed once).
    fn canonical(&self, idx: usize) -> usize {
        let p = self.bytes(idx).map(<[u8]>::as_ptr);
        (0..idx).find(|&j| self.bytes(j).map(<[u8]>::as_ptr) == p).unwrap_or(idx)
    }

    /// Parses face `idx` if needed; true when it is usable.
    fn ensure_face(&mut self, idx: usize) -> bool {
        let c = self.canonical(idx);
        if self.faces[c].is_none() {
            let t = std::time::Instant::now();
            let pooled = if self.use_pool { self.static_bytes(c).and_then(pool::take) } else { None };
            let parsed = pooled.or_else(|| {
                // Data that lives for the whole program is not copied.
                let r = match self.static_bytes(c) {
                    Some(b) => Some(Fonts::from_static([b, PLACEHOLDER, PLACEHOLDER])),
                    None => self.bytes(c).map(|b| Fonts::from_bytes([b, PLACEHOLDER, PLACEHOLDER])),
                };
                r.and_then(|r| match r {
                    Ok(f) => Some(f),
                    Err(e) => {
                        log::warn!("font face not usable: {e}");
                        None
                    }
                })
            });
            let ms = t.elapsed().as_millis();
            if ms > 200 {
                log::info!("ink-html: font face parsed in {ms} ms");
            }
            self.faces[c] = Some(parsed);
        }
        matches!(self.faces[c], Some(Some(_)))
    }

    /// The parsed face to use for `idx` (bold italic falls back to bold, then regular).
    fn resolve(&mut self, idx: usize) -> Option<usize> {
        let chain: &[usize] = match idx {
            3 => &[3, 1, 0],
            1 => &[1, 0],
            2 => &[2, 0],
            _ => &[0],
        };
        chain.iter().find(|&&i| self.ensure_face(i)).map(|&i| self.canonical(i))
    }
}

/// Font bytes of one family: regular, bold, italic, bold italic (any may be missing).
#[derive(Clone, Debug, Default)]
pub struct FamilyBytes {
    /// Regular face.
    pub regular: Option<Arc<Vec<u8>>>,
    /// Bold face.
    pub bold: Option<Arc<Vec<u8>>>,
    /// Italic face.
    pub italic: Option<Arc<Vec<u8>>>,
    /// Bold italic face.
    pub bold_italic: Option<Arc<Vec<u8>>>,
}

/// Word widths of one (face, size bits), keyed by the word.
type WordWidths = HashMap<Box<str>, f32, FxBuild>;

/// Families by lowercase name. Not `Send` across threads by design: each thread builds
/// its own book from the same [`FontSources`].
pub struct FontBook {
    families: Vec<Family>,
    by_name: HashMap<String, FamilyId, FxBuild>,
    /// The last `family` lookup (one style's names are looked up for every word).
    last_family: std::cell::RefCell<Option<(Vec<String>, FamilyId)>>,
    /// Word widths per (face, size bits), looked up by `&str` without allocating.
    widths: HashMap<(FaceKey, u32), WordWidths, FxBuild>,
    /// Words in `widths`.
    width_count: usize,
    fallback_dirs: Vec<PathBuf>,
    /// Fallback faces behind placeholder main faces: `None` not loaded yet, `Some(None)`
    /// none found.
    fallback: Option<Option<Fonts>>,
    /// Extra stroke weight of drawn text, in levels ([`FontBook::set_embolden`]).
    embolden: u8,
}

/// A run of text drawn with the family's face (`false`) or a fallback face (`true`).
type Segment = (bool, std::ops::Range<usize>);

/// `ink_text` style for a face request (fallback faces synthesize bold and italic).
fn style_of(k: FaceKey) -> Style {
    match (k.bold, k.italic) {
        (false, false) => Style::Regular,
        (true, false) => Style::Bold,
        (false, true) => Style::Italic,
        (true, true) => Style::BoldItalic,
    }
}

/// Everything needed to build a [`FontBook`] (cheap to clone, `Send + Sync`).
#[derive(Clone, Debug, Default)]
pub struct FontSources {
    /// Book (`@font-face`) and system families by lowercase name.
    pub families: Vec<(String, FamilyBytes)>,
    /// Directories searched for fallback faces (CJK and other scripts the family lacks).
    /// Empty: no fallback, missing glyphs draw the face's `.notdef` box.
    pub fallback_dirs: Vec<PathBuf>,
}

impl FontSources {
    /// Groups `@font-face` rules into families using the loaded font bytes
    /// (`fonts[path]`); `resolve(src)` turns a rule's URL into a book path.
    pub fn add_font_faces(&mut self, rules: &[(String, FontFaceRule)], fonts: &HashMap<String, Arc<Vec<u8>>>) {
        for (dir, r) in rules {
            let bytes = r.sources.iter().find_map(|src| fonts.get(&ink_doc::resolve_href(dir, src)).cloned());
            let Some(bytes) = bytes else { continue };
            // A cheap check (tables only); glyphs are parsed when the family is first used.
            if ttf_parser::Face::parse(bytes.as_slice(), 0).is_err() {
                log::warn!("@font-face {}: font data not readable (obfuscated?)", r.family);
                continue;
            }
            let name = r.family.to_ascii_lowercase();
            let fam = match self.families.iter_mut().find(|(n, _)| *n == name) {
                Some((_, f)) => f,
                None => {
                    self.families.push((name, FamilyBytes::default()));
                    &mut self.families.last_mut().unwrap().1
                }
            };
            let slot = match (r.bold, r.italic) {
                (false, false) => &mut fam.regular,
                (true, false) => &mut fam.bold,
                (false, true) => &mut fam.italic,
                (true, true) => &mut fam.bold_italic,
            };
            if slot.is_none() {
                *slot = Some(bytes);
            }
        }
    }

    /// Adds a system family `name` from font files in `dirs` whose file names start with
    /// the family name (spaces removed, case-insensitive). Returns false when no regular
    /// face was found.
    pub fn add_system_family(&mut self, name: &str, dirs: &[PathBuf]) -> bool {
        let key = name.to_ascii_lowercase();
        if self.families.iter().any(|(n, _)| *n == key) {
            return true;
        }
        let want = key.replace([' ', '-', '_'], "");
        let files = ink_text::registry::scan_font_dirs(dirs);
        let mut fam = FamilyBytes::default();
        for f in files {
            let stem = f.stem.to_ascii_lowercase();
            let compact = stem.replace([' ', '-', '_'], "");
            if !compact.starts_with(&want) {
                continue;
            }
            let rest = &compact[want.len()..];
            let bold = rest.contains("bold") || rest.contains("black") || rest.contains("heavy");
            let italic = rest.contains("italic") || rest.contains("oblique");
            let plain = rest.is_empty() || rest.contains("regular") || rest.contains("roman") || rest.contains("book") || rest.contains("normal");
            if !(bold || italic || plain) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&f.path) else { continue };
            let slot = match (bold, italic) {
                (false, false) => &mut fam.regular,
                (true, false) => &mut fam.bold,
                (false, true) => &mut fam.italic,
                (true, true) => &mut fam.bold_italic,
            };
            if slot.is_none() {
                *slot = Some(Arc::new(bytes));
            }
        }
        if fam.regular.is_none() {
            return false;
        }
        self.families.push((key, fam));
        true
    }

    /// [`FontSources::build`] for the foreground (UI thread) book: its embedded and
    /// installed faces come from the [`prewarm_faces`] pool when they were parsed ahead.
    pub fn build_foreground(&self) -> FontBook {
        let mut fb = self.build();
        for f in &mut fb.families {
            f.use_pool = true;
        }
        fb
    }

    /// Builds the per-thread font book.
    pub fn build(&self) -> FontBook {
        let mut fb = FontBook {
            families: Vec::new(),
            by_name: HashMap::default(),
            last_family: std::cell::RefCell::new(None),
            widths: HashMap::default(),
            width_count: 0,
            fallback_dirs: self.fallback_dirs.clone(),
            fallback: None,
            embolden: 0,
        };
        fb.families.push(Family::new(Src::Serif));
        fb.families.push(Family::new(Src::Mono));
        for n in ["serif", "times", "times new roman", "georgia", "noto serif", "cursive", "fantasy", "system-ui"] {
            fb.by_name.insert(n.into(), 0);
        }
        for n in ["monospace", "courier", "courier new", "droid sans mono", "consolas", "menlo", "monaco", "ui-monospace"] {
            fb.by_name.insert(n.into(), 1);
        }
        for (name, bytes) in &self.families {
            if bytes.regular.is_none() && bytes.bold.is_none() && bytes.italic.is_none() {
                continue;
            }
            fb.families.push(Family::new(Src::Bytes(bytes.clone())));
            fb.by_name.insert(name.clone(), (fb.families.len() - 1) as FamilyId);
        }
        // Installed text families (Bookerly, Amazon Ember, user fonts) after the book's
        // own: the user's reading font choice and book CSS naming them find them. They
        // replace the aliases of the built-in families ("georgia" -> serif) but never a
        // book family or a generic name. Files are mapped (cached per path); faces are
        // parsed on first use.
        const GENERIC: [&str; 8] = ["serif", "monospace", "cursive", "fantasy", "system-ui", "ui-monospace", "noto serif", "droid sans mono"];
        for fam in ink_text::registry::installed().iter() {
            let key = fam.name.to_ascii_lowercase();
            if GENERIC.contains(&key.as_str()) || fb.by_name.get(&key).is_some_and(|&id| id >= 2) {
                continue;
            }
            let map = |i: usize| fam.faces[i].as_ref().and_then(|f| ink_text::face::map_file(&f.path).ok());
            let faces = [map(0), map(1), map(2), map(3)];
            if faces[0].is_none() {
                continue;
            }
            fb.families.push(Family::new(Src::Mapped(faces)));
            fb.by_name.insert(key, (fb.families.len() - 1) as FamilyId);
        }
        fb
    }
}

/// Parses the regular, bold and italic faces of family `name` (the user's font, else
/// the default serif) on a background thread, so the foreground book's first layout
/// takes them parsed instead of parsing them itself (fontdue reads every glyph of a face
/// up front: about 10 ms per Noto Serif face on a desktop core, 0.1 s or more on the
/// PW12). Call it when a book starts opening; it returns at once. Only faces that live
/// for the whole program (embedded, installed fonts) are parsed ahead.
pub fn prewarm_faces(name: Option<&str>) {
    pool::start();
    let name = name.map(str::to_string);
    let spawned = std::thread::Builder::new().name("ink-html-faces".into()).spawn(move || {
        let _done = pool::Done;
        let fb = FontSources::default().build();
        let names = vec![name.unwrap_or_else(|| "serif".into())];
        let fam = &fb.families[fb.family(&names) as usize];
        let mut want: Vec<&'static [u8]> = Vec::new();
        for idx in 0..3 {
            if let Some(b) = fam.static_bytes(fam.canonical(idx)) {
                if !want.iter().any(|w| pool::key(w) == pool::key(b)) {
                    want.push(b);
                }
            }
        }
        pool::parse_all(&want);
    });
    if let Err(e) = spawned {
        log::warn!("ink-html: cannot start the face thread: {e}");
        pool::stop();
    }
}

/// The faces parsed ahead by [`prewarm_faces`], at most one per font data.
mod pool {
    use super::{Fonts, PLACEHOLDER};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct State {
        /// A prewarm thread runs (it has not listed its faces yet, or parses them).
        running: bool,
        /// Faces the running thread will parse.
        listed: Option<Vec<(usize, usize)>>,
        ready: Vec<((usize, usize), Fonts)>,
    }

    static STATE: Mutex<Option<State>> = Mutex::new(None);
    static CHANGED: Condvar = Condvar::new();

    /// The longest a face request waits for the prewarm thread.
    const WAIT: Duration = Duration::from_secs(5);

    pub fn key(b: &[u8]) -> (usize, usize) {
        (b.as_ptr() as usize, b.len())
    }

    fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        f(g.get_or_insert_with(State::default))
    }

    pub fn start() {
        with(|s| {
            s.running = true;
            s.listed = None;
        });
    }

    pub fn stop() {
        with(|s| {
            s.running = false;
            s.listed = None;
        });
        CHANGED.notify_all();
    }

    /// Ends the thread's run when it returns or panics.
    pub struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            stop();
        }
    }

    /// Parses `faces` (skipping ones already in the pool), each on its own thread (the
    /// first page usually needs regular, italic and bold at once).
    pub fn parse_all(faces: &[&'static [u8]]) {
        let todo: Vec<&'static [u8]> = with(|s| {
            let todo: Vec<_> = faces.iter().copied().filter(|b| !s.ready.iter().any(|(k, _)| *k == key(b))).collect();
            s.listed = Some(todo.iter().map(|b| key(b)).collect());
            todo
        });
        CHANGED.notify_all();
        let one = |b: &'static [u8]| {
            let t = Instant::now();
            let parsed = Fonts::from_static([b, PLACEHOLDER, PLACEHOLDER]);
            with(|s| {
                if let Some(l) = s.listed.as_mut() {
                    l.retain(|k| *k != key(b));
                }
                if let Ok(f) = parsed {
                    s.ready.push((key(b), f));
                }
            });
            log::debug!("ink-html: face of {} bytes parsed ahead in {} ms", b.len(), t.elapsed().as_millis());
            CHANGED.notify_all();
        };
        std::thread::scope(|scope| {
            let mut rest = todo.iter();
            let first = rest.next();
            for &b in rest {
                // A face whose thread cannot start is parsed by its first user.
                if std::thread::Builder::new().name("ink-html-face".into()).spawn_scoped(scope, move || one(b)).is_err() {
                    with(|s| {
                        if let Some(l) = s.listed.as_mut() {
                            l.retain(|k| *k != key(b));
                        }
                    });
                }
            }
            if let Some(&b) = first {
                one(b);
            }
        });
    }

    /// The parsed face for `b`: from the pool, after waiting for the prewarm thread
    /// when it will parse it (or has not said yet). `None`: parse it yourself.
    pub fn take(b: &'static [u8]) -> Option<Fonts> {
        let k = key(b);
        let deadline = Instant::now() + WAIT;
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let s = g.get_or_insert_with(State::default);
            if let Some(i) = s.ready.iter().position(|(r, _)| *r == k) {
                return Some(s.ready.swap_remove(i).1);
            }
            let pending = s.running && s.listed.as_ref().is_none_or(|l| l.contains(&k));
            let now = Instant::now();
            if !pending || now >= deadline {
                return None;
            }
            g = CHANGED.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

impl FontBook {
    /// A book with only the built-in families.
    pub fn builtin() -> Self {
        FontSources::default().build()
    }

    /// The first family of `names` this book has (default: serif).
    pub fn family(&self, names: &[String]) -> FamilyId {
        if let Some((last, id)) = self.last_family.borrow().as_ref() {
            if last.as_slice() == names {
                return *id;
            }
        }
        let id = self.family_uncached(names);
        *self.last_family.borrow_mut() = Some((names.to_vec(), id));
        id
    }

    fn family_uncached(&self, names: &[String]) -> FamilyId {
        for n in names {
            let t = n.trim();
            // Lowercase names (the usual case) are looked up without a copy.
            let lower;
            let k = if t.bytes().any(|b| b.is_ascii_uppercase()) {
                lower = t.to_ascii_lowercase();
                lower.as_str()
            } else {
                t
            };
            if let Some(&id) = self.by_name.get(k) {
                return id;
            }
            if k == "sans-serif" || k.contains("sans") {
                // No sans face is embedded; a system or book sans would have matched above.
                continue;
            }
        }
        0
    }

    /// Family id by exact (lowercase) name.
    pub fn family_by_name(&self, name: &str) -> Option<FamilyId> {
        self.by_name.get(&name.to_ascii_lowercase()).copied()
    }

    fn fonts_style(&mut self, k: FaceKey) -> (&mut Fonts, Style) {
        let mut id = (k.family as usize).min(self.families.len() - 1);
        // A family whose regular face does not parse is replaced by the default family.
        if !self.families[id].ensure_face(0) {
            id = 0;
        }
        let fam = &mut self.families[id];
        let slot = fam.resolve(face_index(k.bold, k.italic)).expect("default font family");
        match &mut fam.faces[slot] {
            Some(Some(f)) => (f, Style::Regular),
            _ => unreachable!("resolve returns a parsed face"),
        }
    }

    /// The fallback faces (loaded on first use; `None` when there are none).
    fn fallback_fonts(&mut self) -> Option<&mut Fonts> {
        if self.fallback.is_none() {
            let loaded = if self.fallback_dirs.is_empty() {
                None
            } else {
                let t = std::time::Instant::now();
                match Fonts::from_bytes([PLACEHOLDER, PLACEHOLDER, PLACEHOLDER]) {
                    Ok(mut f) => {
                        let n = f.load_fallbacks(&self.fallback_dirs);
                        log::info!("ink-html: {n} fallback font faces from {:?} in {:?}", self.fallback_dirs, t.elapsed());
                        (n > 0).then_some(f)
                    }
                    Err(e) => {
                        log::warn!("ink-html: no fallback fonts: {e}");
                        None
                    }
                }
            };
            self.fallback = Some(loaded);
        }
        self.fallback.as_mut().and_then(Option::as_mut)
    }

    /// True when `c` is drawn with a fallback face: the family's face has no glyph for
    /// it and a fallback face has one.
    fn uses_fallback(&mut self, k: FaceKey, c: char) -> bool {
        if c.is_ascii() || c.is_whitespace() || c.is_control() || self.fallback_dirs.is_empty() {
            return false;
        }
        let (f, s) = self.fonts_style(k);
        if f.font(s).lookup_glyph_index(c) != 0 {
            return false;
        }
        // Placeholder main faces: any face found is a fallback face (id 3 and up).
        self.fallback_fonts().is_some_and(|fb| fb.face_for(Style::Regular, c) >= 3)
    }

    /// `text` split into runs of the family's face and of fallback faces (one run
    /// without allocation checks for ASCII text).
    fn segments(&mut self, k: FaceKey, text: &str) -> Vec<Segment> {
        if text.is_ascii() || self.fallback_dirs.is_empty() {
            return vec![(false, 0..text.len())];
        }
        let mut out: Vec<Segment> = Vec::new();
        for (i, c) in text.char_indices() {
            let fb = self.uses_fallback(k, c);
            let end = i + c.len_utf8();
            match out.last_mut() {
                Some((f, r)) if *f == fb => r.end = end,
                _ => out.push((fb, i..end)),
            }
        }
        if out.is_empty() {
            out.push((false, 0..0));
        }
        out
    }

    /// Width of `text` without the word cache.
    fn measure(&mut self, k: FaceKey, px: f32, text: &str) -> f32 {
        let mut w = 0.0;
        for (fb, r) in self.segments(k, text) {
            let seg = &text[r];
            if fb {
                if let Some(f) = self.fallback_fonts() {
                    w += f.shaped_width(style_of(k), px, seg);
                }
            } else {
                let (f, s) = self.fonts_style(k);
                w += f.text_width(s, seg, px);
            }
        }
        w
    }

    /// Width of `text` (cached per word).
    pub fn width(&mut self, k: FaceKey, px: f32, text: &str) -> f32 {
        let fk = (k, px.to_bits());
        if text.len() <= 24 {
            if let Some(w) = self.widths.get(&fk).and_then(|m| m.get(text)) {
                return *w;
            }
        }
        let w = self.measure(k, px, text);
        if text.len() <= 24 {
            if self.width_count > 200_000 {
                self.widths.clear();
                self.width_count = 0;
            }
            self.widths.entry(fk).or_default().insert(text.into(), w);
            self.width_count += 1;
        }
        w
    }

    /// Advance of one char.
    pub fn advance(&mut self, k: FaceKey, px: f32, c: char) -> f32 {
        if self.uses_fallback(k, c) {
            let mut buf = [0u8; 4];
            let st = style_of(k);
            if let Some(f) = self.fallback_fonts() {
                return f.shaped_width(st, px, c.encode_utf8(&mut buf));
            }
        }
        let (f, s) = self.fonts_style(k);
        f.advance(s, c, px)
    }

    /// (ascent, descent) with descent negative.
    pub fn vmetrics(&mut self, k: FaceKey, px: f32) -> (f32, f32) {
        let (f, s) = self.fonts_style(k);
        f.vmetrics(s, px)
    }

    /// True when the face has a glyph for `c`.
    /// Fallback faces do not count: list markers without a glyph (`◦`, `▪`) are drawn as
    /// squares, the same on every machine whatever fonts it has.
    pub fn has_glyph(&mut self, k: FaceKey, c: char) -> bool {
        let (f, s) = self.fonts_style(k);
        f.font(s).lookup_glyph_index(c) != 0
    }

    /// Darker text: `level` 0 draws the faces as they are, 1 and 2 widen every stroke by
    /// [`FontBook::embolden_px`] pixels per level. Drawing only; widths do not change.
    pub fn set_embolden(&mut self, level: u8) {
        self.embolden = level.min(3);
    }

    /// Extra stroke pixels per embolden level at `px` pixels per em (1 up to 59 px, 2 up
    /// to 99 px, and so on).
    pub fn embolden_px(&self, px: f32) -> u32 {
        ink_text::raster::weight_px(self.embolden, px)
    }

    /// Draws `text`; returns the pen position after it.
    pub fn draw(&mut self, canvas: &mut Canvas, k: FaceKey, px: f32, x: f32, baseline: f32, text: &str) -> f32 {
        let mut pen = x;
        let extra = self.embolden_px(px);
        for (fb, r) in self.segments(k, text) {
            let seg = &text[r];
            if fb {
                if let Some(f) = self.fallback_fonts() {
                    let run = f.shape(style_of(k), px, seg);
                    pen = f.draw_run(canvas, &run, pen, baseline);
                }
            } else {
                let (f, s) = self.fonts_style(k);
                pen = f.draw_text_extra(canvas, s, px, pen, baseline, seg, extra);
            }
        }
        pen
    }

    /// Number of families.
    pub fn len(&self) -> usize {
        self.families.len()
    }

    /// Always false (the built-in families exist).
    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }
}

/// The e-reader's font directories plus `extra` (searched first).
pub fn font_dirs(extra: Option<&Path>) -> Vec<PathBuf> {
    let mut v = ink_platform::current().font_dirs();
    if let Some(e) = extra {
        v.insert(0, e.to_path_buf());
    }
    v
}

/// The regular serif face bytes (used by tests).
pub fn serif_regular() -> &'static [u8] {
    embedded_font_bytes(Style::Regular)
}

/// FxHash (rustc's hasher): much cheaper than SipHash for the short keys of the width
/// cache, which is never fed untrusted keys in a way that matters (a book can at worst
/// slow its own layout down).
#[derive(Clone, Copy, Default)]
pub struct FxHasher(u64);

const FX_K: u64 = 0x517c_c1b7_2722_0a95;

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(FX_K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for c in chunks {
            self.add(u64::from_le_bytes(*c));
        }
        if !rest.is_empty() {
            let mut b = [0u8; 8];
            b[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(b));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(i as u64);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

/// `BuildHasher` for [`FxHasher`].
pub type FxBuild = BuildHasherDefault<FxHasher>;

#[cfg(test)]
mod tests {

    #[test]
    fn prewarmed_faces_measure_like_parsed_ones() {
        prewarm_faces(None);
        let mut fg = FontSources::default().build_foreground();
        let mut bg = FontSources::default().build();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let k = FaceKey { family: 0, bold, italic };
            assert_eq!(fg.width(k, 40.0, "Prewarmed faces"), bg.width(k, 40.0, "Prewarmed faces"));
        }
        // The pool holds no face for a thread book, and a face nobody parses ahead is
        // parsed at once (no wait).
        let t = std::time::Instant::now();
        assert!(pool::take(MONO).is_none());
        assert!(t.elapsed() < std::time::Duration::from_secs(4));
    }
    use super::*;

    #[test]
    fn builtin_families_and_faces() {
        let mut fb = FontBook::builtin();
        assert_eq!(fb.family(&["Courier New".into(), "monospace".into()]), 1);
        assert_eq!(fb.family(&["Unknown".into()]), 0);
        let r = FaceKey { family: 0, bold: false, italic: false };
        let bi = FaceKey { family: 0, bold: true, italic: true };
        let m = FaceKey { family: 1, bold: false, italic: false };
        let w = fb.width(r, 30.0, "Romeo");
        assert!(fb.width(bi, 30.0, "Romeo") > w * 0.9);
        assert_eq!(fb.width(m, 30.0, "iiii"), fb.width(m, 30.0, "MMMM"), "monospace");
        let mut c = Canvas::new(200, 60);
        let end = fb.draw(&mut c, bi, 30.0, 5.0, 40.0, "Juliet");
        assert!(end > 5.0 && c.pixels.iter().any(|p| *p < 100));
        assert!(fb.has_glyph(r, 'A'));
    }

    #[test]
    fn missing_glyphs_come_from_fallback_faces() {
        // INK_MANIFEST_DIR: the test binary runs on the emulator with the files copied there.
        let crate_dir = std::env::var_os("INK_MANIFEST_DIR").map(PathBuf::from).unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into());
        let dir = crate_dir.join("../ink-text/tests/data");
        let r = FaceKey { family: 0, bold: false, italic: false };
        let text = "Juliet 日本語";
        // Without fallback dirs: Noto Serif has no CJK glyphs, .notdef boxes.
        let mut plain = FontBook::builtin();
        assert!(!plain.has_glyph(r, '日'));
        let mut boxes = Canvas::new(600, 80);
        plain.draw(&mut boxes, r, 40.0, 10.0, 60.0, text);
        // With the test fallback font (each CJK glyph 1 em wide).
        let mut fb = FontSources { fallback_dirs: vec![dir], ..Default::default() }.build();
        assert!(!fb.has_glyph(r, '日'), "has_glyph names the family's own glyphs only");
        assert!((fb.advance(r, 40.0, '日') - 40.0).abs() < 0.01);
        let latin = fb.width(r, 40.0, "Juliet ");
        assert_eq!(latin, plain.width(r, 40.0, "Juliet "), "Latin text unchanged");
        let w = fb.width(r, 40.0, text);
        assert!((w - latin - 120.0).abs() < 0.5, "width {w}, latin {latin}");
        let mut c = Canvas::new(600, 80);
        let end = fb.draw(&mut c, r, 40.0, 10.0, 60.0, text);
        assert!((end - 10.0 - w).abs() < 0.5, "pen {end}, width {w}");
        assert_ne!(c.pixels, boxes.pixels, "CJK drawn from the fallback face, not as boxes");
        // ASCII-only text never loads the fallback faces.
        let mut fb = FontSources { fallback_dirs: vec!["/nonexistent".into()], ..Default::default() }.build();
        fb.width(r, 40.0, "Romeo");
        assert!(fb.fallback.is_none());
    }

    #[test]
    fn faces_parse_one_at_a_time() {
        let parsed = |fb: &FontBook, fam: usize| fb.families[fam].faces.iter().map(|f| matches!(f, Some(Some(_)))).collect::<Vec<_>>();
        let mut fb = FontBook::builtin();
        let r = FaceKey { family: 0, bold: false, italic: false };
        fb.width(r, 30.0, "Romeo");
        assert_eq!(parsed(&fb, 0), [true, false, false, false], "only the regular face");
        fb.width(FaceKey { bold: true, ..r }, 30.0, "Romeo");
        assert_eq!(parsed(&fb, 0), [true, true, false, false]);
        // Monospace: one font for all four requests, parsed once.
        let m = FaceKey { family: 1, bold: true, italic: true };
        fb.width(m, 30.0, "x");
        assert_eq!(parsed(&fb, 1), [true, false, false, false]);
        // A book family with only a regular face: every request uses it.
        let mut s = FontSources::default();
        s.families.push(("only".into(), FamilyBytes { regular: Some(Arc::new(serif_regular().to_vec())), ..Default::default() }));
        let mut fb = s.build();
        let id = fb.family_by_name("only").unwrap();
        let k = FaceKey { family: id, bold: true, italic: true };
        assert_eq!(fb.width(k, 30.0, "Juliet"), fb.width(FaceKey { family: 0, bold: false, italic: false }, 30.0, "Juliet"));
        assert_eq!(parsed(&fb, id as usize), [true, false, false, false]);
        // Data that does not parse: the default family is used.
        let mut s = FontSources::default();
        s.families.push(("junk".into(), FamilyBytes { regular: Some(Arc::new(b"not a font".to_vec())), ..Default::default() }));
        let mut fb = s.build();
        let id = fb.family_by_name("junk").unwrap();
        let junk = fb.width(FaceKey { family: id, bold: false, italic: false }, 30.0, "Juliet");
        assert_eq!(junk, fb.width(FaceKey { family: 0, bold: false, italic: false }, 30.0, "Juliet"));
        // The placeholder parses (it fills the unused slots of every face).
        assert!(Fonts::from_bytes([PLACEHOLDER, PLACEHOLDER, PLACEHOLDER]).is_ok());
    }

    #[test]
    fn system_family_from_files() {
        // INK_MANIFEST_DIR: the test binary runs on the emulator with the files copied there.
        let crate_dir = std::env::var_os("INK_MANIFEST_DIR").map(PathBuf::from).unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into());
        let dir = crate_dir.join("../../fonts");
        let mut s = FontSources::default();
        assert!(s.add_system_family("Noto Serif", std::slice::from_ref(&dir)));
        assert!(!s.add_system_family("Nonexistent", &[dir]));
        let fb = s.build();
        assert!(fb.family_by_name("noto serif").is_some());
    }
}
