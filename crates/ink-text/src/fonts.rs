//! Fonts, metric and glyph caches, and text drawing.
//!
//! Prototype path (unchanged math, golden pages depend on it): three embedded Noto Serif
//! faces rasterized with `fontdue`, char-by-char advances plus `horizontal_kern`
//! ([`Fonts::text_width`], [`Fonts::draw_text`]). Change it only together with
//! `scripts/golden-update.sh`.
//!
//! Shaped path (phase 1): [`Fonts::shape`] itemizes text by face (main face of the style,
//! then fallback faces in order), shapes each run with `rustybuzz` (kerning, ligatures,
//! marks), and [`Fonts::draw_run`] draws the glyphs from a bounded glyph cache filled by
//! [`crate::raster`]. Faces: ids 0..3 are the main Regular, Bold and Italic faces; ids
//! from 3 on are fallbacks ([`Fonts::add_fallback`], [`Fonts::load_fallbacks`]).

use crate::face::{FaceData, FaceInfo};
use crate::raster::{self, embolden_px, GlyphMask, Synth};
use crate::{Style, TextError};
use fontdue::{Font, FontSettings, Metrics};
use ink_device::{Canvas, Rect};
use rustybuzz::ttf_parser::{GlyphId, Tag};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

static REGULAR: &[u8] = include_bytes!("../../../fonts/NotoSerif-Regular.ttf");
static BOLD: &[u8] = include_bytes!("../../../fonts/NotoSerif-Bold.ttf");
static ITALIC: &[u8] = include_bytes!("../../../fonts/NotoSerif-Italic.ttf");

/// The font file bytes embedded in the binary for `style` (Noto Serif, OFL). Bold italic
/// has no embedded face; it returns the italic face (bold is synthesized).
pub fn embedded_font_bytes(style: Style) -> &'static [u8] {
    match style {
        Style::Regular => REGULAR,
        Style::Bold => BOLD,
        Style::Italic | Style::BoldItalic => ITALIC,
    }
}

/// Index of a face in [`Fonts`]: 0..3 main faces (regular, bold, italic), then fallbacks.
pub type FaceId = u16;

/// One shaped glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    /// Face the glyph comes from.
    pub face: FaceId,
    /// Glyph id in that face.
    pub id: u16,
    /// Byte offset of the cluster in the shaped text.
    pub cluster: usize,
    /// Pen x of the glyph origin from the run start, offsets included.
    pub x: f32,
    /// Vertical offset (positive = up), for marks.
    pub y: f32,
    /// Advance in pixels (synthetic bold widening included).
    pub advance: f32,
    /// Synthetic styling.
    pub synth: Synth,
}

/// Shaped text in one style and size.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextRun {
    /// Glyphs in visual order.
    pub glyphs: Vec<Glyph>,
    /// Total advance.
    pub width: f32,
    /// Size in pixels per em.
    pub px: f32,
}

impl TextRun {
    /// Faces used, in order of first use.
    pub fn faces(&self) -> Vec<FaceId> {
        let mut out: Vec<FaceId> = Vec::new();
        for g in &self.glyphs {
            if !out.contains(&g.face) {
                out.push(g.face);
            }
        }
        out
    }

    /// Pen x at byte offset `byte` of the text (start of the cluster containing it), for
    /// cursors and selections. The end of the text gives the run width.
    pub fn x_at(&self, byte: usize) -> f32 {
        self.glyphs.iter().filter(|g| g.cluster >= byte).map(|g| g.x).reduce(f32::min).unwrap_or(self.width)
    }
}

/// Options for [`Fonts::shape_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapeOptions {
    /// OpenType kerning (`kern`).
    pub kerning: bool,
    /// Standard and contextual ligatures (`liga`, `clig`).
    pub ligatures: bool,
    /// BCP 47 language for language-specific forms (e.g. `tr`); `None` guesses nothing.
    pub language: Option<String>,
}

impl Default for ShapeOptions {
    fn default() -> Self {
        ShapeOptions { kerning: true, ligatures: true, language: None }
    }
}

/// [`ShapeOptions`] as `rustybuzz` takes them.
#[derive(Default)]
struct Shaping {
    features: Vec<rustybuzz::Feature>,
    language: Option<rustybuzz::Language>,
}

impl Shaping {
    fn new(opts: &ShapeOptions) -> Self {
        let off = |t: &[u8; 4]| rustybuzz::Feature::new(rustybuzz::ttf_parser::Tag::from_bytes(t), 0, ..);
        let mut features = Vec::new();
        if !opts.kerning {
            features.push(off(b"kern"));
        }
        if !opts.ligatures {
            features.extend([off(b"liga"), off(b"clig")]);
        }
        Shaping { features, language: opts.language.as_deref().and_then(|l| l.parse().ok()) }
    }
}

/// The maps behind the glyph and metric caches. Their keys are a few small integers
/// (style, char or glyph id, size bits), looked up once per glyph drawn or measured;
/// the default SipHash costs more than the rest of such a lookup.
type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// FxHash's multiply-rotate mix, with the high half folded into the low bits at the end
/// (the table picks buckets by the low bits).
#[derive(Clone, Copy, Default)]
struct FastHasher(u64);

impl FastHasher {
    fn add(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for FastHasher {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.add(*b as u64);
        }
    }

    fn write_u8(&mut self, v: u8) {
        self.add(v as u64);
    }

    fn write_u16(&mut self, v: u16) {
        self.add(v as u64);
    }

    fn write_u32(&mut self, v: u32) {
        self.add(v as u64);
    }

    fn write_u64(&mut self, v: u64) {
        self.add(v);
    }

    fn write_usize(&mut self, v: usize) {
        self.add(v as u64);
    }

    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 32)
    }
}

/// Cache key of a shaped glyph bitmap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: FaceId,
    id: u16,
    px: u32,
    synth: Synth,
    /// Extra stroke pixels ([`Fonts::glyph_mask_extra`]).
    extra: u32,
}

/// Glyph bitmaps with a byte budget; least recently used entries go first.
struct GlyphCache {
    map: FastMap<GlyphKey, (GlyphMask, u64)>,
    bytes: usize,
    budget: usize,
    tick: u64,
}

impl GlyphCache {
    fn new(budget: usize) -> Self {
        GlyphCache { map: FastMap::default(), bytes: 0, budget, tick: 0 }
    }

    fn get_or_insert(&mut self, key: GlyphKey, make: impl FnOnce() -> GlyphMask) -> &GlyphMask {
        self.tick += 1;
        let tick = self.tick;
        if !self.map.contains_key(&key) {
            let m = make();
            self.bytes += m.bytes();
            self.map.insert(key, (m, tick));
            if self.bytes > self.budget {
                self.evict(key);
            }
        }
        let e = self.map.get_mut(&key).expect("just inserted");
        e.1 = tick;
        &e.0
    }

    /// Drops the oldest entries until 3/4 of the budget is used (never `keep`).
    fn evict(&mut self, keep: GlyphKey) {
        let mut ages: Vec<(u64, GlyphKey, usize)> =
            self.map.iter().filter(|(k, _)| **k != keep).map(|(k, (m, t))| (*t, *k, m.bytes())).collect();
        ages.sort_by_key(|a| a.0);
        for (_, k, b) in ages {
            if self.bytes <= self.budget * 3 / 4 {
                break;
            }
            self.map.remove(&k);
            self.bytes -= b;
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }
}

/// Metrics and coverage of a prototype-path glyph.
type ProtoGlyph = (Metrics, Vec<u8>);

/// Prototype-path glyphs of one style, size and extra weight: ASCII by index, other
/// chars hashed. A line is drawn in one style and size, so it hashes that key once
/// and finds most of its glyphs without hashing.
#[derive(Default)]
struct ProtoGlyphs {
    ascii: Vec<Option<ProtoGlyph>>,
    other: HashMap<char, ProtoGlyph>,
}

impl ProtoGlyphs {
    fn get_or_insert(&mut self, c: char, make: impl FnOnce() -> ProtoGlyph) -> &ProtoGlyph {
        if !c.is_ascii() {
            return self.other.entry(c).or_insert_with(make);
        }
        if self.ascii.is_empty() {
            self.ascii.resize_with(128, || None);
        }
        self.ascii[c as usize].get_or_insert_with(make)
    }

    fn len(&self) -> usize {
        self.ascii.iter().flatten().count() + self.other.len()
    }
}

/// Default byte budget of the shaped glyph cache.
pub const GLYPH_CACHE_BYTES: usize = 8 << 20;

/// A set of faces (main family plus fallbacks) with caches.
pub struct Fonts {
    fonts: [Arc<Font>; 3],
    /// Per main face: it has a legacy `kern` table, the only kerning `fontdue` reads.
    kern: [bool; 3],
    advance: FastMap<(Style, char, u32), f32>,
    /// Prototype-path glyphs by (style, size bits, extra weight).
    glyphs: FastMap<(Style, u32, u32), ProtoGlyphs>,
    faces: Vec<FaceData>,
    /// First fallback face with a glyph for a char (`None`: no face has it).
    fallback_for: FastMap<char, Option<FaceId>>,
    /// Faces added with [`Fonts::add_face`]: drawn only on request, never fallbacks.
    own: Vec<FaceId>,
    shaped: GlyphCache,
    /// Runs made by [`Fonts::shape`] by (style, size bits) and text: UI text is shaped
    /// again on every paint. Emptied when the next run would take it past
    /// [`RUN_CACHE_BYTES`] or a fallback face comes in.
    runs: FastMap<(Style, u32), FastMap<Box<str>, TextRun>>,
    /// Text and glyph bytes of the kept runs.
    run_bytes: usize,
}

/// Text and glyph bytes [`Fonts::shape`] keeps: a text field shapes its whole text on
/// each keystroke, so a count of runs would not bound the memory.
const RUN_CACHE_BYTES: usize = 1 << 20;

/// Slot of the main face for `style` in the per-face arrays.
fn index(style: Style) -> usize {
    Fonts::main_face(style).0 as usize
}

/// True when `c` must have a glyph in the face that draws its cluster: not whitespace,
/// a control, a zero-width (non-)joiner or a variation selector.
fn needs_glyph(c: char) -> bool {
    !(c.is_whitespace() || c.is_control() || matches!(c, '\u{200C}' | '\u{200D}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}'))
}

/// Pair kerning of `font` before `c` on the prototype path (0 at the start of the text).
/// A face without a `kern` table (`has_kern` false; all embedded faces: Noto kerns in
/// `GPOS`) skips the two char lookups `fontdue` makes for every pair.
fn kern(font: &Font, has_kern: bool, prev: Option<char>, c: char, px: f32) -> f32 {
    match prev {
        Some(p) if has_kern => font.horizontal_kern(p, c, px).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Rasterizes a prototype-path glyph: metrics and an 8-bit coverage mask, `extra`
/// pixels bolder ([`Fonts::draw_text_extra`]).
fn proto_glyph(font: &Font, style: Style, c: char, px: f32, extra: u32) -> ProtoGlyph {
    let (m, mask) = font.rasterize(c, px);
    let mut k = extra;
    if style == Style::BoldItalic {
        k += embolden_px(px);
    }
    if k == 0 {
        return (m, mask);
    }
    let g = GlyphMask { width: m.width, height: m.height, left: m.xmin, top: 0, coverage: mask };
    let b = raster::embolden(&g, k);
    let advance_width = if style == Style::BoldItalic { m.advance_width + embolden_px(px) as f32 } else { m.advance_width };
    let metrics = Metrics { width: b.width, height: b.height, advance_width, ..m };
    (metrics, b.coverage)
}

/// The `fontdue` faces `[regular, bold, italic]`. Bytes given for more than one slot (UI
/// fonts without a bold or italic face) are parsed once and shared.
fn parse_all(faces: [&[u8]; 3]) -> Result<[Arc<Font>; 3], TextError> {
    let parse = |bytes: &[u8]| Font::from_bytes(bytes, FontSettings::default()).map(Arc::new).map_err(|e| TextError::Font(e.to_string()));
    let r = parse(faces[0])?;
    let b = if faces[1] == faces[0] { r.clone() } else { parse(faces[1])? };
    let i = if faces[2] == faces[0] {
        r.clone()
    } else if faces[2] == faces[1] {
        b.clone()
    } else {
        parse(faces[2])?
    };
    Ok([r, b, i])
}

impl Fonts {
    /// The embedded Noto Serif faces.
    pub fn load() -> Self {
        Fonts::from_static([REGULAR, BOLD, ITALIC]).expect("embedded fonts")
    }

    /// Faces from TTF/OTF bytes: `[regular, bold, italic]`. The bytes are copied and kept
    /// for the rest of the program (the shaped path needs them).
    pub fn from_bytes(faces: [&[u8]; 3]) -> Result<Self, TextError> {
        let fonts = parse_all(faces)?;
        let data = faces.iter().map(|f| FaceData::from_vec(f.to_vec(), 0)).collect::<Result<_, _>>()?;
        Ok(Fonts::from_parts(fonts, data))
    }

    /// Faces from bytes that live for the whole program (embedded or memory-mapped with
    /// [`crate::face::map_file`]): `[regular, bold, italic]`. Nothing is copied.
    pub fn from_static(faces: [&'static [u8]; 3]) -> Result<Self, TextError> {
        let fonts = parse_all(faces)?;
        let data = faces.iter().map(|f| FaceData::from_static(f, 0)).collect::<Result<_, _>>()?;
        Ok(Fonts::from_parts(fonts, data))
    }

    /// Faces from font files: `[regular, bold, italic]`.
    pub fn from_files(paths: [&Path; 3]) -> Result<Self, TextError> {
        let read = |p: &Path| std::fs::read(p).map_err(|e| TextError::Font(format!("{}: {e}", p.display())));
        let (r, b, i) = (read(paths[0])?, read(paths[1])?, read(paths[2])?);
        Fonts::from_bytes([&r, &b, &i])
    }

    fn from_parts(fonts: [Arc<Font>; 3], faces: Vec<FaceData>) -> Self {
        let kern = std::array::from_fn(|i| faces[i].ttf().is_some_and(|f| f.raw_face().table(Tag::from_bytes(b"kern")).is_some()));
        Fonts {
            fonts,
            kern,
            advance: FastMap::default(),
            glyphs: FastMap::default(),
            faces,
            fallback_for: FastMap::default(),
            own: Vec::new(),
            shaped: GlyphCache::new(GLYPH_CACHE_BYTES),
            runs: FastMap::default(),
            run_bytes: 0,
        }
    }

    /// The `fontdue` face for `style` (bold italic: the italic face).
    pub fn font(&self, style: Style) -> &Font {
        &self.fonts[index(style)]
    }

    // ---- Prototype path (exact math; golden pages depend on it) ----

    /// Advance width of `c` at `px` pixels per em (cached). Bold italic adds the
    /// synthetic bold widening.
    pub fn advance(&mut self, style: Style, c: char, px: f32) -> f32 {
        let key = (style, c, px.to_bits());
        if let Some(a) = self.advance.get(&key) {
            return *a;
        }
        let mut a = self.fonts[index(style)].metrics(c, px).advance_width;
        if style == Style::BoldItalic {
            a += embolden_px(px) as f32;
        }
        self.advance.insert(key, a);
        a
    }

    /// Width of `text` with kerning.
    pub fn text_width(&mut self, style: Style, text: &str, px: f32) -> f32 {
        let mut w = 0.0;
        let mut prev: Option<char> = None;
        for c in text.chars() {
            w += kern(&self.fonts[index(style)], self.kern[index(style)], prev, c, px);
            w += self.advance(style, c, px);
            prev = Some(c);
        }
        w
    }

    /// (ascent, descent) in pixels; descent is negative.
    pub fn vmetrics(&self, style: Style, px: f32) -> (f32, f32) {
        match self.fonts[index(style)].horizontal_line_metrics(px) {
            Some(m) => (m.ascent, m.descent),
            None => (px * 0.8, -px * 0.2),
        }
    }

    /// Draws `text` with its left edge at `x` and baseline at `baseline`.
    /// Returns the pen position after the last glyph.
    pub fn draw_text(&mut self, canvas: &mut Canvas, style: Style, px: f32, x: f32, baseline: f32, text: &str) -> f32 {
        self.draw_text_extra(canvas, style, px, x, baseline, text, 0)
    }

    /// [`Fonts::draw_text`] with `extra` more pixels of stroke weight (the user's "darker
    /// text": [`raster::embolden`] on top of the face's own weight). The advance stays
    /// the same, so the line breaks do not move.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_extra(&mut self, canvas: &mut Canvas, style: Style, px: f32, x: f32, baseline: f32, text: &str, extra: u32) -> f32 {
        let mut pen = x;
        let mut prev: Option<char> = None;
        let (font, has_kern) = (&self.fonts[index(style)], self.kern[index(style)]);
        let glyphs = self.glyphs.entry((style, px.to_bits(), extra)).or_default();
        for c in text.chars() {
            pen += kern(font, has_kern, prev, c, px);
            let (m, mask) = glyphs.get_or_insert(c, || proto_glyph(font, style, c, px, extra));
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (baseline - (m.height as i32 + m.ymin) as f32).round() as i32;
            canvas.draw_mask(gx, gy, m.width, m.height, mask);
            pen += m.advance_width;
            prev = Some(c);
        }
        pen
    }

    /// Number of cached glyph bitmaps of both paths (for memory checks).
    pub fn cached_glyphs(&self) -> usize {
        self.glyphs.values().map(ProtoGlyphs::len).sum::<usize>() + self.shaped.map.len()
    }

    /// Bytes held by the shaped glyph cache.
    pub fn cached_bytes(&self) -> usize {
        self.shaped.bytes
    }

    /// Sets the byte budget of the shaped glyph cache (default [`GLYPH_CACHE_BYTES`]).
    pub fn set_cache_budget(&mut self, bytes: usize) {
        self.shaped.budget = bytes.max(1);
    }

    /// Drops all cached bitmaps and advances.
    pub fn clear_caches(&mut self) {
        self.glyphs.clear();
        self.advance.clear();
        self.shaped.clear();
        self.runs.clear();
        self.run_bytes = 0;
    }

    // ---- Faces ----

    /// Number of faces (3 main faces plus fallbacks).
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// Facts about face `id`.
    pub fn face_info(&self, id: FaceId) -> Option<&FaceInfo> {
        self.faces.get(id as usize).map(|f| &f.info)
    }

    /// The face data of `id`.
    pub fn face_data(&self, id: FaceId) -> Option<&FaceData> {
        self.faces.get(id as usize)
    }

    /// Appends a fallback face; returns its id.
    pub fn add_fallback(&mut self, face: FaceData) -> FaceId {
        self.faces.push(face);
        // Earlier misses may now be found.
        self.fallback_for.retain(|_, v| v.is_some());
        self.runs.clear();
        self.run_bytes = 0;
        (self.faces.len() - 1) as FaceId
    }

    /// Appends a face that is only drawn on request ([`Fonts::shape_in`]), e.g. a font
    /// name shown in its own font; it is never used as a fallback. Returns its id.
    pub fn add_face(&mut self, face: FaceData) -> FaceId {
        self.faces.push(face);
        let id = (self.faces.len() - 1) as FaceId;
        self.own.push(id);
        id
    }

    /// Shapes all of `text` with face `face` (no fallback faces; characters the face
    /// lacks draw its `.notdef`). An unknown face gives an empty run.
    pub fn shape_in(&mut self, face: FaceId, px: f32, text: &str) -> TextRun {
        let mut run = TextRun { px, ..TextRun::default() };
        if (face as usize) < self.faces.len() {
            self.push_shaped(&mut run, face, text, 0..text.len(), Synth::NONE, &Shaping::default());
        }
        run
    }

    /// (ascent, descent) of face `face` at `px` from its `hhea` table (descent negative);
    /// the regular face's metrics for an unknown face.
    pub fn face_vmetrics(&self, face: FaceId, px: f32) -> (f32, f32) {
        match self.faces.get(face as usize).and_then(|f| f.ttf()) {
            Some(t) => {
                let s = px / t.units_per_em().max(1) as f32;
                (t.ascender() as f32 * s, t.descender() as f32 * s)
            }
            None => self.vmetrics(Style::Regular, px),
        }
    }

    /// Appends the faces of a font file as fallbacks.
    pub fn add_fallback_file(&mut self, path: &Path) -> Result<Vec<FaceId>, TextError> {
        Ok(FaceData::open_all(path)?.into_iter().map(|f| self.add_fallback(f)).collect())
    }

    /// Scans `dirs` ([`crate::registry::scan_fonts`]) and appends usable fallback faces in
    /// [`crate::registry::fallback_order`]. Returns how many were added.
    pub fn load_fallbacks<P: AsRef<Path>>(&mut self, dirs: &[P]) -> usize {
        let entries = crate::registry::scan_fonts(dirs);
        let mut n = 0;
        for e in crate::registry::fallback_order(&entries) {
            match FaceData::open(&e.path, e.info.index) {
                Ok(f) => {
                    self.add_fallback(f);
                    n += 1;
                }
                Err(err) => log::warn!("fallback font {}: {err}", e.path.display()),
            }
        }
        n
    }

    /// Main face for `style` and the synthesis it needs.
    fn main_face(style: Style) -> (FaceId, Synth) {
        match style {
            Style::Regular => (0, Synth::NONE),
            Style::Bold => (1, Synth::NONE),
            Style::Italic => (2, Synth::NONE),
            Style::BoldItalic => (2, Synth { bold: true, italic: false }),
        }
    }

    /// Synthesis needed to show `style` with face `id`.
    fn synth_for(&self, id: FaceId, style: Style) -> Synth {
        if id < 3 {
            return Self::main_face(style).1;
        }
        let info = &self.faces[id as usize].info;
        Synth { bold: style.is_bold() && !info.bold, italic: style.is_italic() && !info.italic }
    }

    /// The face that shows `c` in `style`: the main face when it has the glyph, else the
    /// first fallback that has it, else the main face (which draws `.notdef`).
    pub fn face_for(&mut self, style: Style, c: char) -> FaceId {
        let (main, _) = Self::main_face(style);
        if self.faces[main as usize].has_char(c) || c.is_whitespace() || c.is_control() {
            return main;
        }
        if let Some(found) = self.fallback_for.get(&c) {
            return found.unwrap_or(main);
        }
        let found = self.first_fallback(|f| f.has_char(c));
        self.fallback_for.insert(c, found);
        found.unwrap_or(main)
    }

    /// The first fallback face for which `covers` holds. Faces added with
    /// [`Fonts::add_face`] are not fallbacks, and a face `rustybuzz` cannot parse would
    /// drop the text, so both are skipped.
    fn first_fallback(&self, covers: impl Fn(&FaceData) -> bool) -> Option<FaceId> {
        (3..self.faces.len())
            .find(|i| !self.own.contains(&(*i as FaceId)) && covers(&self.faces[*i]) && self.faces[*i].shaper().is_some())
            .map(|i| i as FaceId)
    }

    /// The face that shows grapheme cluster `g` (a base char plus combining marks, e.g.
    /// `o` + U+035E in a pronunciation) in `style`: [`Fonts::face_for`] its first char
    /// when that face also has every mark; else the first fallback face that has all
    /// chars of the cluster; else [`Fonts::face_for`] the first char. A one-char cluster
    /// costs the same as [`Fonts::face_for`].
    pub fn face_for_cluster(&mut self, style: Style, g: &str) -> FaceId {
        let mut chars = g.chars();
        let first = chars.next().unwrap_or(' ');
        let face = self.face_for(style, first);
        if chars.as_str().is_empty() {
            return face;
        }
        let covers = |f: &FaceData| g.chars().filter(|c| needs_glyph(*c)).all(|c| f.has_char(c));
        if covers(&self.faces[face as usize]) {
            return face;
        }
        self.first_fallback(covers).unwrap_or(face)
    }

    // ---- Shaped path ----

    /// Shapes `text` with kerning and ligatures.
    pub fn shape(&mut self, style: Style, px: f32, text: &str) -> TextRun {
        if let Some(run) = self.kept_run(style, px, text) {
            return run.clone();
        }
        let key = (style, px.to_bits());
        let run = self.shape_with(style, px, text, &ShapeOptions::default());
        let bytes = text.len() + run.glyphs.len() * std::mem::size_of::<Glyph>();
        if self.run_bytes + bytes > RUN_CACHE_BYTES {
            self.runs.clear();
            self.run_bytes = 0;
        }
        self.runs.entry(key).or_default().insert(text.into(), run.clone());
        self.run_bytes += bytes;
        run
    }

    /// The run [`Fonts::shape`] kept for `text`, if any.
    fn kept_run(&self, style: Style, px: f32, text: &str) -> Option<&TextRun> {
        self.runs.get(&(style, px.to_bits())).and_then(|m| m.get(text))
    }

    /// Shapes `text`: splits it into runs per face (grapheme clusters stay together),
    /// shapes each run with `rustybuzz`, and lays the runs out left to right.
    pub fn shape_with(&mut self, style: Style, px: f32, text: &str, opts: &ShapeOptions) -> TextRun {
        let mut run = TextRun { px, ..TextRun::default() };
        // Itemize.
        let mut items: Vec<(usize, usize, FaceId)> = Vec::new();
        for (i, g) in text.grapheme_indices(true) {
            let c = g.chars().next().unwrap_or(' ');
            let face = if c.is_whitespace() || c.is_control() {
                items.last().map(|l| l.2).unwrap_or_else(|| Self::main_face(style).0)
            } else {
                self.face_for_cluster(style, g)
            };
            match items.last_mut() {
                Some(last) if last.2 == face => last.1 = i + g.len(),
                _ => items.push((i, i + g.len(), face)),
            }
        }
        let shaping = Shaping::new(opts);
        for (start, end, fid) in items {
            let synth = self.synth_for(fid, style);
            self.push_shaped(&mut run, fid, text, start..end, synth, &shaping);
        }
        run
    }

    /// Shapes `text[range]` with face `fid` and appends the glyphs to `run` at its end
    /// (clusters as byte offsets into `text`). Synthetic bold widens every advancing
    /// glyph. A face `rustybuzz` cannot parse adds nothing.
    fn push_shaped(&self, run: &mut TextRun, fid: FaceId, text: &str, range: Range<usize>, synth: Synth, shaping: &Shaping) {
        let Some(face) = self.faces[fid as usize].shaper() else { return };
        let scale = run.px / face.units_per_em().max(1) as f32;
        let extra = if synth.bold { embolden_px(run.px) as f32 } else { 0.0 };
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(&text[range.clone()]);
        if let Some(l) = &shaping.language {
            buf.set_language(l.clone());
        }
        buf.guess_segment_properties();
        let out = rustybuzz::shape(face, &shaping.features, buf);
        for (info, pos) in out.glyph_infos().iter().zip(out.glyph_positions()) {
            let advance = pos.x_advance as f32 * scale;
            let advance = if advance > 0.0 { advance + extra } else { advance };
            run.glyphs.push(Glyph {
                face: fid,
                id: info.glyph_id as u16,
                cluster: range.start + info.cluster as usize,
                x: run.width + pos.x_offset as f32 * scale,
                y: pos.y_offset as f32 * scale,
                advance,
                synth,
            });
            run.width += advance;
        }
    }

    /// Width of shaped `text`.
    pub fn shaped_width(&mut self, style: Style, px: f32, text: &str) -> f32 {
        match self.kept_run(style, px, text) {
            Some(run) => run.width,
            None => self.shape(style, px, text).width,
        }
    }

    /// The cached bitmap of one shaped glyph with `extra` more pixels of stroke weight
    /// (the user's "darker text", as [`Fonts::draw_text_extra`] on the prototype path).
    pub fn glyph_mask_extra(&mut self, face: FaceId, id: u16, px: f32, synth: Synth, extra: u32) -> &GlyphMask {
        let key = GlyphKey { face, id, px: px.to_bits(), synth, extra };
        let data = &self.faces[face as usize];
        self.shaped.get_or_insert(key, || match data.ttf() {
            Some(f) => {
                let m = raster::rasterize(f, GlyphId(id), px, synth);
                if extra == 0 {
                    m
                } else {
                    raster::embolden(&m, extra)
                }
            }
            None => GlyphMask::default(),
        })
    }

    /// Draws a shaped run in black with its origin at `x` and baseline at `baseline`;
    /// returns the pen position after it.
    pub fn draw_run(&mut self, canvas: &mut Canvas, run: &TextRun, x: f32, baseline: f32) -> f32 {
        self.draw_run_value(canvas, run, x, baseline, 0)
    }

    /// Draws a shaped run blending toward gray `value` (0 = black ink, 255 = white text on
    /// a dark background).
    pub fn draw_run_value(&mut self, canvas: &mut Canvas, run: &TextRun, x: f32, baseline: f32, value: u8) -> f32 {
        let all = canvas.bounds();
        self.draw_run_clipped(canvas, run, x, baseline, value, all)
    }

    /// Like [`Fonts::draw_run_value`], but only pixels inside `clip` change: glyph parts
    /// outside it are not drawn. Inside `clip` the result is the same pixel for pixel.
    pub fn draw_run_clipped(&mut self, canvas: &mut Canvas, run: &TextRun, x: f32, baseline: f32, value: u8, clip: Rect) -> f32 {
        self.draw_run_clipped_extra(canvas, run, x, baseline, value, clip, 0)
    }

    /// [`Fonts::draw_run_clipped`] with `extra` more pixels of stroke weight
    /// ([`Fonts::glyph_mask_extra`]); the pen advance stays the same.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_run_clipped_extra(&mut self, canvas: &mut Canvas, run: &TextRun, x: f32, baseline: f32, value: u8, clip: Rect, extra: u32) -> f32 {
        // Where pixels may change: `clip`, on the canvas, inside the canvas's own clip.
        let mut clip = clip.intersect(&canvas.bounds());
        if let Some(c) = canvas.clip() {
            clip = clip.intersect(&c);
        }
        if clip.is_empty() {
            return x + run.width;
        }
        for g in &run.glyphs {
            let m = self.glyph_mask_extra(g.face, g.id, run.px, g.synth, extra);
            let gx = (x + g.x).round() as i32 + m.left;
            let gy = (baseline - g.y).round() as i32 + m.top;
            blend_mask(canvas, gx, gy, m, value, clip);
        }
        x + run.width
    }
}

/// Blends `value` into the canvas with the coverage of `m` placed at `(x, y)`, only
/// inside `clip` (which lies on the canvas). Row slices, not a bounds-checked get/set
/// per pixel: every line of UI text goes through here on each paint. With value 0 this
/// is exactly `Canvas::draw_mask`'s darkening. Zero coverage leaves a pixel as it is
/// (`p * 255 / 255`), so the loop needs no branch for it and vectorizes.
fn blend_mask(canvas: &mut Canvas, x: i32, y: i32, m: &GlyphMask, value: u8, clip: Rect) {
    let area = Rect::new(x, y, m.width as u32, m.height as u32).intersect(&clip);
    if area.is_empty() {
        return;
    }
    let (n, v) = (area.w as usize, value as u32);
    for py in area.y..area.bottom() {
        let mi = (py - y) as usize * m.width + (area.x - x) as usize;
        let ci = py as usize * canvas.width + area.x as usize;
        for (p, &a) in canvas.pixels[ci..ci + n].iter_mut().zip(&m.coverage[mi..mi + n]) {
            let a = a as u32;
            *p = ((*p as u32 * (255 - a) + v * a) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_font() -> std::path::PathBuf {
        crate::testdirs::data().join("InkTestFallback.ttf")
    }

    #[test]
    fn measure_and_draw() {
        let mut f = Fonts::load();
        let w = f.text_width(Style::Regular, "Romeo", 40.0);
        assert!(w > 80.0 && w < 160.0, "{w}");
        assert!(f.text_width(Style::Bold, "Romeo", 40.0) > w);
        let (asc, desc) = f.vmetrics(Style::Regular, 40.0);
        assert!(asc > 0.0 && desc < 0.0);
        let mut c = Canvas::new(300, 80);
        let end = f.draw_text(&mut c, Style::Italic, 40.0, 10.0, 60.0, "Juliet");
        assert!((end - 10.0 - f.text_width(Style::Italic, "Juliet", 40.0)).abs() < 0.01);
        assert!(c.pixels.iter().any(|p| *p < 64));
        assert!(f.cached_glyphs() >= 5);
        f.clear_caches();
        assert_eq!(f.cached_glyphs(), 0);
    }

    #[test]
    fn faces_given_twice_are_parsed_once() {
        let f = Fonts::from_static([REGULAR, REGULAR, ITALIC]).unwrap();
        assert!(std::ptr::eq(f.font(Style::Regular), f.font(Style::Bold)));
        assert!(!std::ptr::eq(f.font(Style::Regular), f.font(Style::Italic)));
        assert_eq!(f.font(Style::Italic).metrics('a', 40.0), Fonts::load().font(Style::Italic).metrics('a', 40.0));
    }

    #[test]
    fn bad_font_bytes_are_an_error() {
        assert!(Fonts::from_bytes([b"nope", REGULAR, REGULAR]).is_err());
    }

    #[test]
    fn prototype_math_is_unchanged() {
        // Values measured with the phase 0 code; golden pages rely on them.
        let mut f = Fonts::load();
        let w = f.text_width(Style::Regular, "AVATAR fi", 37.0);
        let mut pen = 0.0;
        let mut prev = None;
        for c in "AVATAR fi".chars() {
            if let Some(p) = prev {
                pen += f.font(Style::Regular).horizontal_kern(p, c, 37.0).unwrap_or(0.0);
            }
            pen += f.font(Style::Regular).metrics(c, 37.0).advance_width;
            prev = Some(c);
        }
        assert_eq!(w, pen);
    }

    #[test]
    fn faces_with_a_kern_table_kern_on_the_prototype_path() {
        let p = crate::testdirs::data().join("kern/InkTestKern.ttf");
        let mut f = Fonts::from_files([p.as_path(), p.as_path(), p.as_path()]).unwrap();
        let font = f.font(Style::Regular).clone();
        let kern = font.horizontal_kern('日', '本', 40.0).unwrap();
        assert!((kern + 10.0).abs() < 0.01, "250 of 1000 units at 40 px: {kern}");
        let plain = font.metrics('日', 40.0).advance_width + font.metrics('本', 40.0).advance_width;
        assert_eq!(f.text_width(Style::Regular, "日本", 40.0), plain + kern);
        let mut c = Canvas::new(200, 80);
        assert_eq!(f.draw_text(&mut c, Style::Regular, 40.0, 10.0, 60.0, "日本"), 10.0 + plain + kern);
    }

    #[test]
    fn extra_weight_darkens_text_without_moving_it() {
        let ink = |extra| {
            let mut c = Canvas::new(300, 80);
            let mut f = Fonts::load();
            let end = f.draw_text_extra(&mut c, Style::Regular, 40.0, 10.0, 60.0, "Juliet", extra);
            (c.pixels.iter().map(|p| 255 - *p as u32).sum::<u32>(), end)
        };
        let (a, ea) = ink(0);
        let (b, eb) = ink(1);
        let (c, ec) = ink(2);
        assert!(b as f32 > a as f32 * 1.15 && c > b, "ink {a} {b} {c}");
        assert!(ea == eb && eb == ec, "the pen advance is unchanged: {ea} {eb} {ec}");
        // Level 0 is the plain path, pixel for pixel.
        let mut c0 = Canvas::new(300, 80);
        let mut c1 = Canvas::new(300, 80);
        let mut f = Fonts::load();
        f.draw_text(&mut c0, Style::Regular, 40.0, 10.0, 60.0, "Juliet");
        f.draw_text_extra(&mut c1, Style::Regular, 40.0, 10.0, 60.0, "Juliet", 0);
        assert_eq!(c0.pixels, c1.pixels);
    }

    #[test]
    fn extra_weight_darkens_shaped_runs_like_the_prototype_path() {
        let mut f = Fonts::load();
        let run = f.shape(Style::Regular, 40.0, "Juliet");
        let clip = Rect::new(0, 0, 300, 80);
        let ink = |f: &mut Fonts, extra| {
            let mut c = Canvas::new(300, 80);
            let end = f.draw_run_clipped_extra(&mut c, &run, 10.0, 60.0, 0, clip, extra);
            (c.pixels.iter().map(|p| 255 - *p as u32).sum::<u32>(), end, c.pixels)
        };
        let (a, ea, p0) = ink(&mut f, 0);
        let (b, eb, _) = ink(&mut f, raster::weight_px(1, 40.0));
        let (c, ec, _) = ink(&mut f, raster::weight_px(2, 40.0));
        assert!(b as f32 > a as f32 * 1.15 && c > b, "ink {a} {b} {c}");
        assert!(ea == eb && eb == ec, "the pen advance is unchanged: {ea} {eb} {ec}");
        // No extra weight is the plain call, pixel for pixel.
        let mut plain = Canvas::new(300, 80);
        f.draw_run_clipped(&mut plain, &run, 10.0, 60.0, 0, clip);
        assert_eq!(plain.pixels, p0);
    }

    #[test]
    fn synthetic_bold_italic_on_the_prototype_path() {
        let mut f = Fonts::load();
        let i = f.text_width(Style::Italic, "Juliet", 40.0);
        let bi = f.text_width(Style::BoldItalic, "Juliet", 40.0);
        assert!((bi - i - 6.0 * embolden_px(40.0) as f32).abs() < 0.01, "{bi} {i}");
        let ink = |style| {
            let mut c = Canvas::new(300, 80);
            let mut f = Fonts::load();
            f.draw_text(&mut c, style, 40.0, 10.0, 60.0, "Juliet");
            c.pixels.iter().map(|p| 255 - *p as u32).sum::<u32>()
        };
        assert!(ink(Style::BoldItalic) as f32 > ink(Style::Italic) as f32 * 1.2);
    }

    #[test]
    fn shaping_applies_kerning_and_ligatures() {
        let mut f = Fonts::load();
        // Kerning: "AV" is narrower than A + V.
        let kerned = f.shape(Style::Regular, 40.0, "AV").width;
        let plain = f.shape_with(Style::Regular, 40.0, "AV", &ShapeOptions { kerning: false, ..Default::default() }).width;
        assert!(kerned < plain - 1.0, "kerned {kerned} plain {plain}");
        // Ligature: "fi" becomes one glyph covering both chars.
        let fi = f.shape(Style::Regular, 40.0, "fi");
        assert_eq!(fi.glyphs.len(), 1, "{fi:?}");
        assert_eq!(fi.glyphs[0].cluster, 0);
        let split = f.shape_with(Style::Regular, 40.0, "fi", &ShapeOptions { ligatures: false, ..Default::default() });
        assert_eq!(split.glyphs.len(), 2);
        // The shaped width of plain text is close to the prototype measure.
        let a = f.shaped_width(Style::Regular, 40.0, "Romeo and Juliet");
        let b = f.text_width(Style::Regular, "Romeo and Juliet", 40.0);
        assert!((a - b).abs() < 2.0, "shaped {a} prototype {b}");
    }

    #[test]
    fn combining_marks_are_positioned() {
        let mut f = Fonts::load();
        // q + combining acute (no precomposed form): one cluster of two glyphs, the
        // accent sits above the x-height.
        let run = f.shape(Style::Regular, 40.0, "q\u{301}");
        assert_eq!(run.glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(), vec![0, 0]);
        assert_eq!(run.glyphs[1].advance, 0.0);
        // e + combining acute composes to the precomposed glyph.
        assert_eq!(f.shape(Style::Regular, 40.0, "e\u{301}").glyphs.len(), 1);
        let mut c = Canvas::new(80, 80);
        f.draw_run(&mut c, &run, 10.0, 60.0);
        let dark_rows: Vec<usize> = (0..80).filter(|y| c.pixels[y * 80..(y + 1) * 80].iter().any(|v| *v < 128)).collect();
        assert!(*dark_rows.first().unwrap() < 30, "accent above the x-height: {dark_rows:?}");
    }

    #[test]
    fn ipa_and_combining_marks_use_a_fallback_that_has_them() {
        // Main faces without IPA (Droid Sans Mono has Latin, no ɪ ʊ ʌ ɒ, no U+035E).
        let mono = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ink-html/fonts/DroidSansMono.ttf");
        let mut f = Fonts::from_files([mono.as_path(), mono.as_path(), mono.as_path()]).unwrap();
        // OALD and NOAD pronunciations: IPA letters, a double macron and a double breve
        // over Latin letters the main face has, a tie bar.
        let text = "ˈnʌðə(r) ˌɪntəˈnæʃənl ɡʊd bɒt ɜː o\u{35e}o  bo\u{35d}ok ŋ\u{361}ɡ ə\u{35f}";
        let before = f.shape(Style::Italic, 40.0, text);
        assert!(before.glyphs.iter().any(|g| g.id == 0), "the main face alone draws .notdef");
        f.add_fallback_file(&crate::testdirs::repo_fonts().join("ui/NotoSans-Regular.ttf")).unwrap();
        for style in [Style::Regular, Style::Italic, Style::Bold] {
            let run = f.shape(style, 40.0, text);
            let tofu: Vec<&str> = run.glyphs.iter().filter(|g| g.id == 0).map(|g| &text[g.cluster..]).collect();
            assert!(tofu.is_empty(), "{style:?}: .notdef at {tofu:?}");
            // The marks are shaped with their base letter in the fallback face.
            for mark in ['\u{35e}', '\u{35d}', '\u{35f}'] {
                let at = text.find(mark).unwrap();
                let base = text[..at].char_indices().last().unwrap().0;
                let faces: Vec<FaceId> = run.glyphs.iter().filter(|g| g.cluster == base).map(|g| g.face).collect();
                assert_eq!(faces, vec![3, 3], "{style:?} U+{:04X}", mark as u32);
            }
            // ˈ (bytes 0..2) comes from the fallback, the plain Latin n after it from the
            // main face.
            let face_at = |b: usize| run.glyphs.iter().find(|g| g.cluster == b).map(|g| g.face);
            assert_eq!((face_at(0), face_at(2)), (Some(3), Some(Fonts::main_face(style).0)));
        }
        // A cluster the main face covers stays there (one char, or base plus a mark).
        assert_eq!(f.face_for_cluster(Style::Regular, "o"), 0);
        assert_eq!(f.face_for_cluster(Style::Regular, "o\u{35e}"), 3);
    }

    #[test]
    fn fallback_faces_cover_cjk_and_symbols() {
        let mut f = Fonts::load();
        let text = "Ink 日本語 ★→";
        let before = f.shape(Style::Regular, 40.0, text);
        assert_eq!(before.faces(), vec![0], "no fallback yet: notdef from the main face");
        let ids = f.add_fallback_file(&test_font()).unwrap();
        assert_eq!(ids, vec![3]);
        let run = f.shape(Style::Regular, 40.0, text);
        assert_eq!(run.faces(), vec![0, 3]);
        // 日本語, the space after it (spaces stay in the current run) and ★→.
        let cjk: Vec<&Glyph> = run.glyphs.iter().filter(|g| g.face == 3).collect();
        assert_eq!(cjk.len(), 6);
        // Every fallback glyph is drawn from its own outline (not .notdef = glyph 0).
        assert!(cjk.iter().all(|g| g.id != 0));
        assert_eq!(run.glyphs.iter().filter(|g| g.face == 0 && g.id == 0).count(), 0);
        let mut c = Canvas::new(600, 80);
        let end = f.draw_run(&mut c, &run, 10.0, 60.0);
        assert!((end - 10.0 - run.width).abs() < 0.01);
        // Each CJK glyph is 1 em wide in the test font.
        assert!((cjk[0].advance - 40.0).abs() < 0.01);
        assert!(f.cached_bytes() > 0);
        assert_eq!(f.face_info(3).unwrap().family, "InkTest Fallback");
        // One face on request: no fallback, plain advances, clusters into the text.
        let own = f.shape_in(3, 40.0, "日本");
        assert_eq!(own.glyphs.iter().map(|g| (g.face, g.cluster)).collect::<Vec<_>>(), vec![(3, 0), (3, 3)]);
        assert!((own.width - 80.0).abs() < 0.01 && (own.glyphs[1].x - 40.0).abs() < 0.01, "{own:?}");
        assert!(f.shape_in(0, 40.0, "日").glyphs.iter().all(|g| g.face == 0 && g.id == 0), "the main face draws .notdef");
        assert_eq!(f.shape_in(99, 40.0, "x"), TextRun { px: 40.0, ..TextRun::default() });
    }

    #[test]
    fn synthetic_styles_for_fallback_faces() {
        let mut f = Fonts::load();
        f.add_fallback_file(&test_font()).unwrap();
        let regular = f.shape(Style::Regular, 40.0, "日");
        let bold = f.shape(Style::Bold, 40.0, "日");
        let italic = f.shape(Style::Italic, 40.0, "日");
        let both = f.shape(Style::BoldItalic, 40.0, "日");
        assert_eq!(regular.glyphs[0].synth, Synth::NONE);
        assert_eq!(bold.glyphs[0].synth, Synth { bold: true, italic: false });
        assert_eq!(italic.glyphs[0].synth, Synth { bold: false, italic: true });
        assert_eq!(both.glyphs[0].synth, Synth { bold: true, italic: true });
        assert!(bold.width > regular.width);
        let ink = |f: &mut Fonts, run: &TextRun| f.glyph_mask_extra(run.glyphs[0].face, run.glyphs[0].id, 40.0, run.glyphs[0].synth, 0).ink();
        let (r, b) = (ink(&mut f, &regular), ink(&mut f, &bold));
        assert!(b > r * 1.2, "bold ink {b} regular {r}");
        // Main faces: bold italic is the italic face plus synthetic bold.
        let bi = f.shape(Style::BoldItalic, 40.0, "a");
        assert_eq!((bi.glyphs[0].face, bi.glyphs[0].synth), (2, Synth { bold: true, italic: false }));
    }

    #[test]
    fn glyph_cache_respects_its_budget() {
        let mut f = Fonts::load();
        f.set_cache_budget(20_000);
        let mut c = Canvas::new(1200, 100);
        for px in [30.0, 31.0, 32.0, 33.0, 34.0] {
            let run = f.shape(Style::Regular, px, "The quick brown fox jumps over the lazy dog");
            f.draw_run(&mut c, &run, 0.0, 80.0);
        }
        assert!(f.cached_bytes() <= 20_000, "{}", f.cached_bytes());
        assert!(f.cached_glyphs() > 0);
    }

    #[test]
    fn shaped_runs_are_kept_up_to_a_bound() {
        let mut f = Fonts::load();
        let run = f.shape(Style::Regular, 40.0, "Juliet");
        assert_eq!(f.shape(Style::Regular, 40.0, "Juliet"), run);
        assert_eq!(f.shape_with(Style::Regular, 40.0, "Juliet", &ShapeOptions::default()), run);
        assert_ne!(f.shape(Style::Bold, 40.0, "Juliet"), run);
        assert_ne!(f.shape(Style::Regular, 41.0, "Juliet"), run);
        // Typing into a text field shapes the whole text on each keystroke: 400 chars would
        // keep 2.6 MB of runs under a bound by count.
        let mut typed = String::new();
        for i in 0..400 {
            typed.push(char::from(b'a' + (i % 26) as u8));
            f.shape(Style::Regular, 20.0, &typed);
            assert!(f.run_bytes <= RUN_CACHE_BYTES, "{} bytes kept after {} chars", f.run_bytes, typed.len());
        }
    }

    #[test]
    fn run_positions_for_cursors() {
        let mut f = Fonts::load();
        let run = f.shape(Style::Regular, 40.0, "abc");
        assert_eq!(run.x_at(0), 0.0);
        assert!(run.x_at(1) > 0.0 && run.x_at(2) > run.x_at(1));
        assert_eq!(run.x_at(3), run.width);
    }

    #[test]
    fn white_text_blends_on_black() {
        let mut f = Fonts::load();
        let mut c = Canvas::new(200, 80);
        c.clear(0);
        let run = f.shape(Style::Bold, 40.0, "Hi");
        f.draw_run_value(&mut c, &run, 10.0, 60.0, 255);
        assert!(c.pixels.iter().any(|p| *p > 200));
    }

    #[test]
    fn clipped_runs_match_inside_and_leave_the_rest() {
        let mut f = Fonts::load();
        let run = f.shape(Style::Italic, 48.0, "Clipped jg Ångström");
        for value in [0u8, 90] {
            let mut full = Canvas::new(500, 100);
            full.clear(230);
            f.draw_run_value(&mut full, &run, 5.0, 60.0, value);
            let clip = Rect::new(60, 30, 200, 40);
            let mut cut = Canvas::new(500, 100);
            cut.clear(230);
            f.draw_run_clipped(&mut cut, &run, 5.0, 60.0, value, clip);
            for y in 0..100 {
                for x in 0..500 {
                    let want = if clip.contains(ink_device::Point::new(x, y)) { full.get(x, y) } else { Some(230) };
                    assert_eq!(cut.get(x, y), want, "({x}, {y}) value {value}");
                }
            }
            // The canvas's own clip limits drawing the same way.
            let mut limited = Canvas::new(500, 100);
            limited.clear(230);
            limited.set_clip(Some(clip));
            f.draw_run_value(&mut limited, &run, 5.0, 60.0, value);
            assert_eq!(limited.pixels, cut.pixels, "value {value}");
            // A clip off the canvas draws nothing and does not panic.
            f.draw_run_clipped(&mut cut, &run, -1e6, 1e6, value, Rect::new(-50, -50, 10, 10));
        }
    }
}
