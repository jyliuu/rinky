//! Glyph outlines to coverage masks.
//!
//! An accumulation rasterizer (the signed-area method of font-rs and fontdue): every
//! outline segment adds its covered area to a float buffer, a running sum over each row
//! turns that into coverage. Curves are flattened to lines first. Outlines come from
//! `ttf-parser` (through `rustybuzz`) one glyph at a time, so large fallback fonts
//! (CJK, 10+ MB) cost nothing until a glyph is drawn; `fontdue` would outline every
//! glyph of the font when it loads.
//!
//! Synthetic styles: italic shears the outline by [`ITALIC_SLANT`]; bold widens the
//! coverage mask by [`embolden_px`] pixels (and the advance by the same amount).

use rustybuzz::ttf_parser::{Face, GlyphId, OutlineBuilder};

/// Horizontal shear of synthetic italics: tan(about 12 degrees), as FreeType uses.
pub const ITALIC_SLANT: f32 = 0.2126;

/// Synthetic styling applied to a glyph because its face lacks the weight or slant.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Synth {
    /// Embolden (widen strokes by [`embolden_px`]).
    pub bold: bool,
    /// Slant by [`ITALIC_SLANT`].
    pub italic: bool,
}

impl Synth {
    /// No synthesis.
    pub const NONE: Synth = Synth { bold: false, italic: false };
}

/// Extra stroke width (and advance) of synthetic bold at `px` pixels per em:
/// about 1/24 em, at least one pixel.
pub fn embolden_px(px: f32) -> u32 {
    ((px / 24.0).round() as u32).max(1)
}

/// A rasterized glyph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlyphMask {
    /// Bitmap width in pixels.
    pub width: usize,
    /// Bitmap height in pixels.
    pub height: usize,
    /// Column of the bitmap's left edge relative to the pen position.
    pub left: i32,
    /// Row of the bitmap's top edge relative to the baseline (negative = above it).
    pub top: i32,
    /// Coverage per pixel, row-major, 255 = full ink.
    pub coverage: Vec<u8>,
}

impl GlyphMask {
    /// Bytes held (for the cache budget).
    pub fn bytes(&self) -> usize {
        self.coverage.len() + std::mem::size_of::<Self>()
    }

    /// Sum of all coverage values divided by 255 (ink area in pixels).
    pub fn ink(&self) -> f32 {
        self.coverage.iter().map(|v| *v as f32).sum::<f32>() / 255.0
    }
}

/// Signed-area accumulator for one glyph bitmap.
struct Accumulator {
    w: usize,
    h: usize,
    a: Vec<f32>,
}

impl Accumulator {
    fn new(w: usize, h: usize) -> Self {
        // Two spare cells: a segment ending at the right edge writes one past the row.
        Accumulator { w, h, a: vec![0.0; w * h + 2] }
    }

    fn add(&mut self, i: isize, v: f32) {
        if i >= 0 && (i as usize) < self.a.len() {
            self.a[i as usize] += v;
        }
    }

    /// Adds the area of the segment `(x0, y0) -> (x1, y1)` (pixel coordinates, y down).
    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        if (y0 - y1).abs() <= f32::EPSILON {
            return;
        }
        let maxx = self.w as f32 - 0.001;
        let (x0, x1) = (x0.clamp(0.0, maxx), x1.clamp(0.0, maxx));
        let (dir, ax, ay, bx, by) = if y0 < y1 { (1.0, x0, y0, x1, y1) } else { (-1.0, x1, y1, x0, y0) };
        let dxdy = (bx - ax) / (by - ay);
        let mut x = ax;
        if ay < 0.0 {
            x -= ay * dxdy;
        }
        let ystart = ay.max(0.0) as usize;
        let yend = (by.ceil().max(0.0) as usize).min(self.h);
        for y in ystart..yend {
            let row = (y * self.w) as isize;
            let dy = ((y + 1) as f32).min(by) - (y as f32).max(ay);
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (l, r) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let lf = l.floor();
            let li = lf as isize;
            let rc = r.ceil();
            let ri = rc as isize;
            if ri <= li + 1 {
                // The segment stays inside one pixel column on this row.
                let xm = 0.5 * (x + xnext) - lf;
                self.add(row + li, d - d * xm);
                self.add(row + li + 1, d * xm);
            } else {
                let s = 1.0 / (r - l);
                let lfrac = l - lf;
                let a0 = 0.5 * s * (1.0 - lfrac) * (1.0 - lfrac);
                let rfrac = r - rc + 1.0;
                let am = 0.5 * s * rfrac * rfrac;
                self.add(row + li, d * a0);
                if ri == li + 2 {
                    self.add(row + li + 1, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - lfrac);
                    self.add(row + li + 1, d * (a1 - a0));
                    for xi in li + 2..ri - 1 {
                        self.add(row + xi, d * s);
                    }
                    let a2 = a1 + (ri - li - 3) as f32 * s;
                    self.add(row + ri - 1, d * (1.0 - a2 - am));
                }
                self.add(row + ri, d * am);
            }
            x = xnext;
        }
    }

    fn finish(self) -> Vec<u8> {
        // The running sum is one long chain of dependent float additions. Large shapes
        // (panels, keys, sheets) have long runs of 0 cells (inside or outside the
        // shape); adding 0 leaves the sum and its coverage as they are, so a block of
        // CHUNK zero cells repeats the last value without an add. The test is per block,
        // not per cell: a per-cell branch mispredicts on small glyphs, whose zero and
        // non-zero cells alternate, and made them slower than the plain sum.
        const CHUNK: usize = 16;
        let cells = &self.a[..self.w * self.h];
        let mut out = Vec::with_capacity(cells.len());
        let mut acc = 0.0f32;
        let mut sum = |out: &mut Vec<u8>, part: &[f32]| {
            for a in part {
                acc += a;
                out.push((acc.abs().min(1.0) * 255.0 + 0.5) as u8);
            }
        };
        let mut chunks = cells.chunks_exact(CHUNK);
        for c in &mut chunks {
            // Zero or minus zero in every cell (sign bit masked off).
            if c.iter().fold(0u32, |m, a| m | a.to_bits()) & 0x7fff_ffff == 0 {
                let v = out.last().copied().unwrap_or(0);
                out.resize(out.len() + CHUNK, v);
            } else {
                sum(&mut out, c);
            }
        }
        sum(&mut out, chunks.remainder());
        out
    }
}

/// Outline builder: font units to bitmap pixels, with optional shear.
struct Builder {
    acc: Accumulator,
    scale: f32,
    slant: f32,
    /// Pixel position of font-unit x = 0 / y = 0 inside the bitmap.
    ox: f32,
    oy: f32,
    start: (f32, f32),
    last: (f32, f32),
}

impl Builder {
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        ((x + self.slant * y) * self.scale - self.ox, self.oy - y * self.scale)
    }

    fn line_px(&mut self, p: (f32, f32)) {
        let l = self.last;
        self.acc.line(l.0, l.1, p.0, p.1);
        self.last = p;
    }
}

impl OutlineBuilder for Builder {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.start = p;
        self.last = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.line_px(p);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p0 = self.last;
        let p1 = self.map(x1, y1);
        let p2 = self.map(x, y);
        let dev = (p0.0 - 2.0 * p1.0 + p2.0).hypot(p0.1 - 2.0 * p1.1 + p2.1);
        let n = 1 + (3.0 * dev).sqrt().floor() as usize;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let q = (
                mt * mt * p0.0 + 2.0 * mt * t * p1.0 + t * t * p2.0,
                mt * mt * p0.1 + 2.0 * mt * t * p1.1 + t * t * p2.1,
            );
            self.line_px(q);
        }
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p0 = self.last;
        let p1 = self.map(x1, y1);
        let p2 = self.map(x2, y2);
        let p3 = self.map(x, y);
        let dev1 = (p0.0 - 2.0 * p1.0 + p2.0).hypot(p0.1 - 2.0 * p1.1 + p2.1);
        let dev2 = (p1.0 - 2.0 * p2.0 + p3.0).hypot(p1.1 - 2.0 * p2.1 + p3.1);
        let n = 1 + (3.0 * dev1.max(dev2)).sqrt().floor() as usize;
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
            let q = (a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0, a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1);
            self.line_px(q);
        }
    }

    fn close(&mut self) {
        let s = self.start;
        if s != self.last {
            self.line_px(s);
        }
    }
}

/// Rasterizes `glyph` of `face` at `px` pixels per em. Glyphs without an outline (space)
/// give an empty mask.
pub fn rasterize(face: &Face, glyph: GlyphId, px: f32, synth: Synth) -> GlyphMask {
    let Some(bbox) = face.glyph_bounding_box(glyph) else { return GlyphMask::default() };
    let upem = face.units_per_em().max(1) as f32;
    let scale = px / upem;
    let slant = if synth.italic { ITALIC_SLANT } else { 0.0 };
    let (xmin, xmax) = (bbox.x_min as f32, bbox.x_max as f32);
    let (ymin, ymax) = (bbox.y_min as f32, bbox.y_max as f32);
    // Sheared horizontal extent.
    let sx = [xmin + slant * ymin, xmin + slant * ymax, xmax + slant * ymin, xmax + slant * ymax];
    let left = (sx.iter().copied().fold(f32::MAX, f32::min) * scale).floor() as i32 - 1;
    let right = (sx.iter().copied().fold(f32::MIN, f32::max) * scale).ceil() as i32 + 1;
    let top = (-ymax * scale).floor() as i32 - 1;
    let bottom = (-ymin * scale).ceil() as i32 + 1;
    let (w, h) = ((right - left).max(1) as usize, (bottom - top).max(1) as usize);
    let mut b = Builder {
        acc: Accumulator::new(w, h),
        scale,
        slant,
        ox: left as f32,
        oy: -top as f32,
        start: (0.0, 0.0),
        last: (0.0, 0.0),
    };
    if face.outline_glyph(glyph, &mut b).is_none() {
        return GlyphMask::default();
    }
    let mut mask = GlyphMask { width: w, height: h, left, top, coverage: b.acc.finish() };
    if synth.bold {
        mask = embolden(&mask, embolden_px(px));
    }
    trim(mask)
}

/// Fills closed polygons (points in pixels, y down) into a `width` x `height` coverage
/// buffer with anti-aliased edges. Overlapping contours add up (nonzero winding); a
/// contour drawn in the opposite direction inside another cuts a hole. Used for icons,
/// circles and thick lines.
pub fn fill_contours(width: usize, height: usize, contours: &[Vec<(f32, f32)>]) -> Vec<u8> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut acc = Accumulator::new(width, height);
    for c in contours {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            acc.line(a.0, a.1, b.0, b.1);
        }
    }
    acc.finish()
}

/// Extra stroke pixels of the user's text weight `level` (0 normal, 1 darker, 2 darkest)
/// at `px` pixels per em: one step per level up to 59 px, two up to 99 px, and so on.
/// The reader ([`crate::Fonts::draw_text_extra`] via the book's fonts) and the reading
/// settings preview both use it, so the preview is as dark as the page.
pub fn weight_px(level: u8, px: f32) -> u32 {
    level as u32 * ((px / 40.0).round() as u32).max(1)
}

/// Widens a mask by `k` pixels to the right and `k / 2` upwards (max filter): synthetic bold.
pub fn embolden(m: &GlyphMask, k: u32) -> GlyphMask {
    if m.coverage.is_empty() || k == 0 {
        return m.clone();
    }
    let k = k as usize;
    let up = k / 2;
    let (w, h) = (m.width + k, m.height + up);
    let mut out = vec![0u8; w * h];
    for y in 0..m.height {
        for x in 0..m.width {
            let v = m.coverage[y * m.width + x];
            if v == 0 {
                continue;
            }
            for dy in 0..=up {
                let row = (y + up - dy) * w;
                for dx in 0..=k {
                    let o = &mut out[row + x + dx];
                    *o = (*o).max(v);
                }
            }
        }
    }
    GlyphMask { width: w, height: h, left: m.left, top: m.top - up as i32, coverage: out }
}

/// Removes empty border rows and columns.
fn trim(m: GlyphMask) -> GlyphMask {
    if m.coverage.iter().all(|v| *v == 0) {
        return GlyphMask { left: m.left, top: m.top, ..GlyphMask::default() };
    }
    let row_empty = |y: usize| m.coverage[y * m.width..(y + 1) * m.width].iter().all(|v| *v == 0);
    let col_empty = |x: usize| (0..m.height).all(|y| m.coverage[y * m.width + x] == 0);
    let y0 = (0..m.height).find(|y| !row_empty(*y)).unwrap_or(0);
    let y1 = (0..m.height).rev().find(|y| !row_empty(*y)).unwrap_or(0) + 1;
    let x0 = (0..m.width).find(|x| !col_empty(*x)).unwrap_or(0);
    let x1 = (0..m.width).rev().find(|x| !col_empty(*x)).unwrap_or(0) + 1;
    let (w, h) = (x1 - x0, y1 - y0);
    let mut cov = Vec::with_capacity(w * h);
    for y in y0..y1 {
        cov.extend_from_slice(&m.coverage[y * m.width + x0..y * m.width + x1]);
    }
    GlyphMask { width: w, height: h, left: m.left + x0 as i32, top: m.top + y0 as i32, coverage: cov }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{embedded_font_bytes, Style};

    fn face() -> Face<'static> {
        Face::parse(embedded_font_bytes(Style::Regular), 0).unwrap()
    }

    #[test]
    fn matches_fontdue_coverage() {
        let f = face();
        let fd = fontdue::Font::from_bytes(embedded_font_bytes(Style::Regular), fontdue::FontSettings::default()).unwrap();
        for c in ['A', 'g', 'O', '&', 'W', 'é'] {
            let gid = f.glyph_index(c).unwrap();
            let ours = rasterize(&f, gid, 48.0, Synth::NONE);
            let (m, theirs) = fd.rasterize(c, 48.0);
            let their_ink = theirs.iter().map(|v| *v as f32).sum::<f32>() / 255.0;
            let rel = (ours.ink() - their_ink).abs() / their_ink;
            assert!(rel < 0.02, "{c}: ink {} vs fontdue {their_ink}", ours.ink());
            // Same placement: our left edge and bottom edge match fontdue's metrics within a pixel.
            assert!((ours.left - m.xmin).abs() <= 1, "{c}: left {} vs {}", ours.left, m.xmin);
            let our_bottom = ours.top + ours.height as i32;
            assert!((our_bottom + m.ymin).abs() <= 1, "{c}: bottom {our_bottom} vs ymin {}", m.ymin);
        }
    }

    #[test]
    fn space_is_empty_and_synthetic_styles_change_ink() {
        let f = face();
        assert!(rasterize(&f, f.glyph_index(' ').unwrap(), 40.0, Synth::NONE).coverage.is_empty());
        let gid = f.glyph_index('l').unwrap();
        let plain = rasterize(&f, gid, 60.0, Synth::NONE);
        let bold = rasterize(&f, gid, 60.0, Synth { bold: true, italic: false });
        let italic = rasterize(&f, gid, 60.0, Synth { bold: false, italic: true });
        assert!(bold.ink() > plain.ink() * 1.3, "bold {} plain {}", bold.ink(), plain.ink());
        assert!(bold.width >= plain.width + embolden_px(60.0) as usize - 1);
        // The slanted 'l' leans right: its top row starts about slant * height further
        // right than upright, with about the same ink.
        let first_ink = |m: &GlyphMask| m.left + (0..m.width).find(|x| m.coverage[*x] > 128).unwrap() as i32;
        let shift = first_ink(&italic) - first_ink(&plain);
        let expect = ITALIC_SLANT * plain.height as f32;
        assert!((shift as f32 - expect).abs() <= 3.0, "shift {shift} expected {expect}");
        assert!(italic.width > plain.width);
        assert!((italic.ink() - plain.ink()).abs() / plain.ink() < 0.05);
        assert_eq!(embolden_px(12.0), 1);
        assert_eq!(embolden_px(48.0), 2);
    }

    #[test]
    fn contours_with_holes() {
        let outer = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let hole = vec![(3.0, 3.0), (3.0, 7.0), (7.0, 7.0), (7.0, 3.0)];
        let m = fill_contours(10, 10, &[outer, hole]);
        assert_eq!(m[0], 255);
        assert_eq!(m[5 * 10 + 5], 0, "hole");
        assert_eq!(m.iter().filter(|v| **v == 255).count(), 100 - 16);
        assert!(fill_contours(0, 5, &[]).is_empty());
    }

    /// `finish` skips blocks of zero cells; the result is the plain running sum's, bit for bit.
    #[test]
    fn finish_matches_the_plain_running_sum() {
        let plain = |a: &Accumulator| {
            let mut acc = 0.0f32;
            a.a[..a.w * a.h].iter().map(|v| {
                acc += v;
                (acc.abs().min(1.0) * 255.0 + 0.5) as u8
            }).collect::<Vec<u8>>()
        };
        let mut seed = 11u32;
        let mut rnd = move |m: f32| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * m
        };
        for n in 3..40 {
            let mut a = Accumulator::new(37, 23);
            // A random polygon, self-intersecting and partly off the bitmap.
            let pts: Vec<(f32, f32)> = (0..n).map(|_| (rnd(50.0) - 6.0, rnd(34.0) - 5.0)).collect();
            for i in 0..n {
                let (p, q) = (pts[i], pts[(i + 1) % n]);
                a.line(p.0, p.1, q.0, q.1);
            }
            let want = plain(&a);
            assert_eq!(a.finish(), want, "polygon of {n} points");
        }
        // A large shape: whole blocks of zero cells inside and outside it, between
        // blocks with edges; minus zero cells count as zero.
        let mut a = Accumulator::new(101, 61);
        let sq = [(3.3, 2.7), (90.6, 4.1), (88.2, 57.9), (5.1, 55.4)];
        for i in 0..4 {
            let (p, q) = (sq[i], sq[(i + 1) % 4]);
            a.line(p.0, p.1, q.0, q.1);
        }
        for i in (0..a.a.len()).step_by(7) {
            if a.a[i] == 0.0 {
                a.a[i] = -0.0;
            }
        }
        let want = plain(&a);
        assert_eq!(a.finish(), want, "large shape");
    }

    #[test]
    fn accumulator_fills_a_square() {
        let mut a = Accumulator::new(4, 4);
        // Clockwise square from (1,1) to (3,3) in pixel space (y down).
        a.line(1.0, 1.0, 3.0, 1.0);
        a.line(3.0, 1.0, 3.0, 3.0);
        a.line(3.0, 3.0, 1.0, 3.0);
        a.line(1.0, 3.0, 1.0, 1.0);
        let out = a.finish();
        let filled: Vec<usize> = out.iter().enumerate().filter(|(_, v)| **v == 255).map(|(i, _)| i).collect();
        assert_eq!(filled, vec![5, 6, 9, 10]);
        assert_eq!(out.iter().filter(|v| **v != 0 && **v != 255).count(), 0);
    }
}
