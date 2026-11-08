//! Anti-aliased drawing on a [`Canvas`]: polygons, circles, rings, thick lines, arcs and
//! rounded rectangles, built as a [`Path`] and filled with [`ink_text::raster::fill_contours`].
//!
//! Every filled contour is stored with positive orientation and every hole with negative
//! orientation, so overlapping fills never cancel each other.

use ink_device::{Canvas, Rect};
use ink_text::raster::fill_contours;
use std::f32::consts::PI;

/// Closed contours in canvas pixels (y down).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    /// The contours; see the module docs for orientation.
    pub contours: Vec<Vec<(f32, f32)>>,
}

fn signed_area(pts: &[(f32, f32)]) -> f32 {
    let mut a = 0.0;
    for i in 0..pts.len() {
        let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
        a += p.0 * q.1 - q.0 * p.1;
    }
    a / 2.0
}

fn oriented(mut pts: Vec<(f32, f32)>, positive: bool) -> Vec<(f32, f32)> {
    if (signed_area(&pts) >= 0.0) != positive {
        pts.reverse();
    }
    pts
}

fn circle_points(cx: f32, cy: f32, r: f32) -> Vec<(f32, f32)> {
    let n = ((r * 1.5) as usize).clamp(12, 180);
    (0..n).map(|i| {
        let a = i as f32 / n as f32 * 2.0 * PI;
        (cx + r * a.cos(), cy + r * a.sin())
    })
    .collect()
}

/// Appends a quarter circle of radius `r` around `(cx, cy)`, clockwise on screen from
/// angle `a0`: the corner of a rounded rectangle, in a few segments for its size.
fn corner(pts: &mut Vec<(f32, f32)>, (cx, cy): (f32, f32), r: f32, a0: f32) {
    let steps = ((r / 2.0) as usize).clamp(3, 16);
    for i in 0..=steps {
        let a = a0 + i as f32 / steps as f32 * PI / 2.0;
        pts.push((cx + r * a.cos(), cy + r * a.sin()));
    }
}

fn round_rect_points(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<(f32, f32)> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    if r < 0.5 {
        return vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
    }
    let mut pts = Vec::new();
    corner(&mut pts, (x + w - r, y + r), r, -PI / 2.0);
    corner(&mut pts, (x + w - r, y + h - r), r, 0.0);
    corner(&mut pts, (x + r, y + h - r), r, PI / 2.0);
    corner(&mut pts, (x + r, y + r), r, PI);
    pts
}

/// A rectangle with only its top corners rounded (sheets attached to the bottom edge).
fn top_round_rect_points(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<(f32, f32)> {
    let r = r.min(w / 2.0).min(h).max(0.0);
    let mut pts = Vec::new();
    corner(&mut pts, (x + w - r, y + r), r, -PI / 2.0);
    pts.push((x + w, y + h));
    pts.push((x, y + h));
    corner(&mut pts, (x + r, y + r), r, PI);
    pts
}

impl Path {
    /// An empty path.
    pub fn new() -> Self {
        Self::default()
    }

    /// A filled polygon.
    pub fn polygon(&mut self, pts: &[(f32, f32)]) -> &mut Self {
        if pts.len() >= 3 {
            self.contours.push(oriented(pts.to_vec(), true));
        }
        self
    }

    /// A hole (cut out of fills it lies inside).
    pub fn hole(&mut self, pts: &[(f32, f32)]) -> &mut Self {
        if pts.len() >= 3 {
            self.contours.push(oriented(pts.to_vec(), false));
        }
        self
    }

    /// A filled rectangle.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) -> &mut Self {
        self.polygon(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)])
    }

    /// A filled rectangle with rounded corners.
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) -> &mut Self {
        self.polygon(&round_rect_points(x, y, w, h, r))
    }

    /// The outline of a rounded rectangle, `width` thick, inside the rectangle.
    pub fn round_rect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32) -> &mut Self {
        self.polygon(&round_rect_points(x, y, w, h, r));
        self.hole(&round_rect_points(x + width, y + width, w - 2.0 * width, h - 2.0 * width, (r - width).max(0.0)))
    }

    /// A filled rectangle with only the top corners rounded.
    pub fn top_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) -> &mut Self {
        self.polygon(&top_round_rect_points(x, y, w, h, r))
    }

    /// The top and side edges of [`Path::top_round_rect`], `width` thick, inside it (no
    /// bottom edge).
    pub fn top_round_rect_outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32) -> &mut Self {
        self.polygon(&top_round_rect_points(x, y, w, h, r));
        self.hole(&top_round_rect_points(x + width, y + width, w - 2.0 * width, h - width, (r - width).max(0.0)))
    }

    /// A filled circle.
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32) -> &mut Self {
        self.polygon(&circle_points(cx, cy, r))
    }

    /// A ring: circle of radius `r` (outer edge) with a stroke `width` thick.
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32) -> &mut Self {
        self.polygon(&circle_points(cx, cy, r));
        self.hole(&circle_points(cx, cy, (r - width).max(0.0)))
    }

    /// A straight stroke `width` thick with square ends.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32) -> &mut Self {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        let (nx, ny) = (-dy / len * width / 2.0, dx / len * width / 2.0);
        self.polygon(&[(x0 + nx, y0 + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (x0 - nx, y0 - ny)])
    }

    /// Connected strokes with round joins and ends.
    pub fn polyline(&mut self, pts: &[(f32, f32)], width: f32) -> &mut Self {
        for w in pts.windows(2) {
            self.line(w[0].0, w[0].1, w[1].0, w[1].1, width);
        }
        for p in pts {
            self.circle(p.0, p.1, width / 2.0);
        }
        self
    }

    /// An arc stroke from angle `a0` to `a1` (radians, 0 = right, clockwise on screen).
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, width: f32) -> &mut Self {
        let n = (((a1 - a0).abs() * r / 3.0) as usize).clamp(4, 120);
        let pts: Vec<(f32, f32)> = (0..=n)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        self.polyline(&pts, width)
    }

    /// Bounding box in whole pixels.
    pub fn bounds(&self) -> Rect {
        let mut b: Option<(f32, f32, f32, f32)> = None;
        for p in self.contours.iter().flatten() {
            b = Some(match b {
                None => (p.0, p.1, p.0, p.1),
                Some((x0, y0, x1, y1)) => (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1)),
            });
        }
        match b {
            None => Rect::default(),
            Some((x0, y0, x1, y1)) => {
                let (x0, y0) = (x0.floor() as i32, y0.floor() as i32);
                Rect::new(x0, y0, (x1.ceil() as i32 - x0 + 1) as u32, (y1.ceil() as i32 - y0 + 1) as u32)
            }
        }
    }

    /// Fills the path with gray `value`.
    pub fn fill(&self, canvas: &mut Canvas, value: u8) {
        let b = self.bounds().intersect(&canvas.bounds());
        if b.is_empty() {
            return;
        }
        let local: Vec<Vec<(f32, f32)>> =
            self.contours.iter().map(|c| c.iter().map(|p| (p.0 - b.x as f32, p.1 - b.y as f32)).collect()).collect();
        let cov = fill_contours(b.w as usize, b.h as usize, &local);
        blend(canvas, b, &cov, value);
    }
}

/// Blends `value` into `r` of the canvas with per-pixel coverage `cov` (`r.w * r.h`).
pub fn blend(canvas: &mut Canvas, r: Rect, cov: &[u8], value: u8) {
    // Row slices of the part of `r` that drawing may change, instead of a bounds-checked
    // get/set per pixel: the sheets and panels blend their full-size outlines on every
    // paint.
    let area = drawable(canvas, r);
    if area.is_empty() {
        return;
    }
    let (rw, cw, n) = (r.w as usize, canvas.width, area.w as usize);
    let v = value as u32;
    for y in area.y..area.bottom() {
        let c0 = (y - r.y) as usize * rw + (area.x - r.x) as usize;
        let p0 = y as usize * cw + area.x as usize;
        let row = &mut canvas.pixels[p0..p0 + n];
        // One formula for every coverage (0 keeps the pixel, 255 gives `value`): with no
        // branch the loop vectorizes.
        for (p, &a) in row.iter_mut().zip(&cov[c0..c0 + n]) {
            let a = a as u32;
            *p = ((*p as u32 * (255 - a) + v * a) / 255) as u8;
        }
    }
}

/// Fills a rounded rectangle.
pub fn fill_round_rect(canvas: &mut Canvas, r: Rect, radius: f32, value: u8) {
    Path::new().round_rect(r.x as f32, r.y as f32, r.w as f32, r.h as f32, radius).fill(canvas, value);
}

/// Strokes a rounded rectangle `width` thick inside `r`.
pub fn stroke_round_rect(canvas: &mut Canvas, r: Rect, radius: f32, width: f32, value: u8) {
    Path::new().round_rect_outline(r.x as f32, r.y as f32, r.w as f32, r.h as f32, radius, width).fill(canvas, value);
}

/// Draws `src` scaled to fit inside `r` (aspect kept, centred; each target pixel is the
/// mean of the source pixels it covers). Returns the rectangle actually covered.
pub fn blit_fit(canvas: &mut Canvas, src: &Canvas, r: Rect) -> Rect {
    if src.width == 0 || src.height == 0 || r.w == 0 || r.h == 0 {
        return Rect::new(r.x, r.y, 0, 0);
    }
    let scale = (r.w as f32 / src.width as f32).min(r.h as f32 / src.height as f32);
    let (w, h) = (((src.width as f32 * scale).round() as u32).clamp(1, r.w), ((src.height as f32 * scale).round() as u32).clamp(1, r.h));
    let dst = Rect::new(r.x + (r.w - w) as i32 / 2, r.y + (r.h - h) as i32 / 2, w, h);
    // Source span `(start, end, sample step)` of target column or row `i`: at most 4
    // samples per axis, enough for thumbnails and cheap for big images.
    let span = |i: u32, scale: f32, len: usize| {
        let end = (((i + 1) as f32 * scale) as usize).clamp(1, len);
        let start = ((i as f32 * scale) as usize).min(end - 1);
        (start, end, (end - start).div_ceil(4).max(1))
    };
    let (sx, sy) = (src.width as f32 / w as f32, src.height as f32 / h as f32);
    // Only the part that drawing may change; the column spans are the same on every row.
    let vis = drawable(canvas, dst);
    let (vx, vy) = ((vis.x - dst.x) as u32, (vis.y - dst.y) as u32);
    let cols: Vec<(usize, usize, usize)> = (vx..vx + vis.w).map(|x| span(x, sx, src.width)).collect();
    for y in vy..vy + vis.h {
        let (y0, y1, sty) = span(y, sy, src.height);
        let row = (dst.y + y as i32) as usize * canvas.width + vis.x as usize;
        for (p, &(x0, x1, stx)) in canvas.pixels[row..row + cols.len()].iter_mut().zip(&cols) {
            let (mut sum, mut n) = (0u32, 0u32);
            for yy in (y0..y1).step_by(sty) {
                for xx in (x0..x1).step_by(stx) {
                    sum += src.pixels[yy * src.width + xx] as u32;
                    n += 1;
                }
            }
            *p = (sum / n.max(1)) as u8;
        }
    }
    dst
}

/// Forces `r` of the canvas to two levels (black below `threshold`, else white): what a
/// DU animation frame shows, so the simulator and screenshots match the panel.
pub fn two_level(canvas: &mut Canvas, r: Rect, threshold: u8) {
    let r = drawable(canvas, r);
    for y in r.y..r.bottom() {
        let i = y as usize * canvas.width + r.x as usize;
        for p in &mut canvas.pixels[i..i + r.w as usize] {
            *p = if *p < threshold { 0 } else { 255 };
        }
    }
}

/// The part of `r` that drawing may change: on the canvas and inside its clip (what
/// `Canvas::set` writes).
fn drawable(canvas: &Canvas, r: Rect) -> Rect {
    let r = r.intersect(&canvas.bounds());
    match canvas.clip() {
        Some(c) => r.intersect(&c),
        None => r,
    }
}

/// `r` shrunk by `d` on every side.
pub fn inset(r: Rect, d: i32) -> Rect {
    Rect::new(r.x + d, r.y + d, (r.w as i32 - 2 * d).max(0) as u32, (r.h as i32 - 2 * d).max(0) as u32)
}

/// Center of a rectangle.
pub fn center(r: Rect) -> (f32, f32) {
    (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `blend` before the row-slice version (2026-11-02): get/set per pixel.
    fn blend_reference(canvas: &mut Canvas, r: Rect, cov: &[u8], value: u8) {
        for y in 0..r.h as i32 {
            for x in 0..r.w as i32 {
                let a = cov[(y * r.w as i32 + x) as usize] as u32;
                if a == 0 {
                    continue;
                }
                let (px, py) = (r.x + x, r.y + y);
                if let Some(bg) = canvas.get(px, py) {
                    let v = if a >= 255 { value as u32 } else { (bg as u32 * (255 - a) + value as u32 * a) / 255 };
                    canvas.set(px, py, v as u8);
                }
            }
        }
    }

    #[test]
    fn blend_matches_the_per_pixel_reference() {
        let mut seed = 7u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as u8
        };
        for (r, clip) in [
            (Rect::new(10, 20, 50, 40), None),
            (Rect::new(-15, -7, 60, 30), None),
            (Rect::new(80, 90, 40, 40), None),
            (Rect::new(5, 5, 90, 90), Some(Rect::new(20, 30, 40, 25))),
            (Rect::new(0, 0, 100, 100), Some(Rect::new(-10, 50, 300, 300))),
        ] {
            let mut base = Canvas::new(100, 100);
            base.pixels.iter_mut().for_each(|p| *p = rnd());
            let cov: Vec<u8> = (0..r.w * r.h).map(|i| if i % 5 == 0 { 0 } else if i % 7 == 0 { 255 } else { rnd() }).collect();
            let (mut a, mut b) = (base.clone(), base);
            a.set_clip(clip);
            b.set_clip(clip);
            blend(&mut a, r, &cov, 37);
            blend_reference(&mut b, r, &cov, 37);
            assert_eq!(a.pixels, b.pixels, "{r:?} clip {clip:?}");
        }
    }

    fn dark(c: &Canvas) -> usize {
        c.pixels.iter().filter(|v| **v < 128).count()
    }

    #[test]
    fn shapes_cover_their_area() {
        let mut c = Canvas::new(100, 100);
        Path::new().circle(50.0, 50.0, 20.0).fill(&mut c, 0);
        let area = dark(&c) as f32;
        assert!((area - PI * 400.0).abs() < 40.0, "{area}");
        let mut c = Canvas::new(100, 100);
        Path::new().ring(50.0, 50.0, 20.0, 4.0).fill(&mut c, 0);
        assert_eq!(c.get(50, 50), Some(255), "ring has a hole");
        assert!(dark(&c) > 400);
        let mut c = Canvas::new(100, 100);
        // Overlapping fills of opposite drawing direction do not cancel.
        Path::new().rect(10.0, 10.0, 50.0, 50.0).polygon(&[(30.0, 30.0), (30.0, 80.0), (80.0, 80.0), (80.0, 30.0)]).fill(&mut c, 0);
        assert_eq!(c.get(40, 40), Some(0));
    }

    #[test]
    fn lines_rounded_rects_and_clipping() {
        let mut c = Canvas::new(60, 60);
        Path::new().line(0.0, 30.0, 60.0, 30.0, 4.0).fill(&mut c, 0);
        assert_eq!(c.get(30, 30), Some(0));
        assert_eq!(c.get(30, 20), Some(255));
        stroke_round_rect(&mut c, Rect::new(5, 5, 50, 50), 10.0, 3.0, 0);
        assert_eq!(c.get(30, 6), Some(0));
        assert_eq!(c.get(6, 6), Some(255), "rounded corner");
        // Fully outside: nothing happens.
        Path::new().circle(-50.0, -50.0, 5.0).fill(&mut c, 0);
        fill_round_rect(&mut c, Rect::new(40, 40, 100, 100), 4.0, 100);
        assert_eq!(c.get(59, 59), Some(100));
        assert_eq!(inset(Rect::new(0, 0, 10, 10), 2), Rect::new(2, 2, 6, 6));
        assert_eq!(center(Rect::new(0, 0, 10, 20)), (5.0, 10.0));
        Path::new().arc(30.0, 30.0, 20.0, 0.0, PI, 3.0).fill(&mut c, 0);
    }

    #[test]
    fn blit_fit_scales_and_centres_and_two_level_thresholds() {
        let mut src = Canvas::new(40, 80);
        src.fill_rect(0, 0, 40, 40, 0); // top half black
        let mut c = Canvas::new(100, 100);
        let r = blit_fit(&mut c, &src, Rect::new(0, 0, 100, 100));
        assert_eq!(r, Rect::new(25, 0, 50, 100), "aspect kept, centred");
        assert_eq!(c.get(50, 10), Some(0));
        assert_eq!(c.get(50, 90), Some(255));
        assert_eq!(c.get(10, 10), Some(255), "outside the picture");
        assert_eq!(blit_fit(&mut c, &Canvas::new(0, 0), Rect::new(0, 0, 10, 10)).w, 0);
        // A gradient scaled down, also partly off the canvas and under a clip: the same
        // pixels where they land, nothing elsewhere.
        let grad = Canvas::from_pixels(300, 200, (0..300 * 200).map(|i| ((i % 300) * 255 / 300 + i / 300 % 7) as u8).collect());
        let mut full = Canvas::new(100, 100);
        blit_fit(&mut full, &grad, Rect::new(5, 5, 90, 90));
        let mut cut = Canvas::new(100, 100);
        cut.set_clip(Some(Rect::new(20, 30, 50, 10)));
        blit_fit(&mut cut, &grad, Rect::new(5, 5, 90, 90));
        for (i, (a, b)) in full.pixels.iter().zip(&cut.pixels).enumerate() {
            let inside = Rect::new(20, 30, 50, 10).contains(ink_device::Point::new((i % 100) as i32, (i / 100) as i32));
            assert_eq!(*b, if inside { *a } else { 255 }, "pixel {i}");
        }
        let mut edge = Canvas::new(100, 100);
        let r = blit_fit(&mut edge, &grad, Rect::new(-40, 60, 90, 90));
        assert_eq!(r, Rect::new(-40, 75, 90, 60));
        assert_eq!(edge.get(0, 80), full.get(45, 25));
        let mut g = Canvas::new(4, 1);
        for (x, v) in [0x22, 0x77, 0x99, 0xDD].iter().enumerate() {
            g.set(x as i32, 0, *v);
        }
        two_level(&mut g, Rect::new(0, 0, 10, 10), 0x80);
        assert_eq!(g.pixels, vec![0, 0, 255, 255]);
        // Only inside the canvas's clip.
        let mut g = Canvas::from_pixels(4, 1, vec![0x22, 0x77, 0x99, 0xDD]);
        g.set_clip(Some(Rect::new(1, 0, 2, 1)));
        two_level(&mut g, Rect::new(-5, -5, 20, 20), 0x80);
        assert_eq!(g.pixels, vec![0x22, 0, 255, 0xDD]);
    }
}
