//! Page-space geometry: rectangles in points and MuPDF-style affine matrices.
//!
//! Page space is MuPDF's: points (1/72 inch), origin at the top-left corner of the page,
//! y grows downwards, the page's own `/Rotate` already applied. Screen space is canvas
//! pixels. A [`Matrix`] maps page space to screen space.

use ink_device::Rect;
use ink_doc::Quad;

/// A rectangle with float coordinates (`x1`/`y1` exclusive).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FRect {
    /// Left.
    pub x0: f32,
    /// Top.
    pub y0: f32,
    /// Right.
    pub x1: f32,
    /// Bottom.
    pub y1: f32,
}

impl FRect {
    /// Builds a rectangle.
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        FRect { x0, y0, x1, y1 }
    }

    /// Width (0 when empty).
    pub fn w(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    /// Height (0 when empty).
    pub fn h(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    /// True when it has no area (also when a coordinate is NaN).
    pub fn is_empty(&self) -> bool {
        !(self.x1 > self.x0 && self.y1 > self.y0)
    }

    /// Centre point.
    pub fn center(&self) -> (f32, f32) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    /// Common part (may be empty).
    pub fn intersect(&self, o: &FRect) -> FRect {
        FRect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }

    /// Smallest rectangle containing both; an empty side is ignored.
    pub fn union(&self, o: &FRect) -> FRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        FRect::new(self.x0.min(o.x0), self.y0.min(o.y0), self.x1.max(o.x1), self.y1.max(o.y1))
    }

    /// True when `(x, y)` lies inside (left/top inclusive).
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// Distance from `(x, y)` to the rectangle (0 inside), with vertical distance weighted
    /// `vy` times (so a point picks the right text line first).
    pub fn distance(&self, x: f32, y: f32, vy: f32) -> f32 {
        let dx = if x < self.x0 { self.x0 - x } else if x > self.x1 { x - self.x1 } else { 0.0 };
        let dy = if y < self.y0 { self.y0 - y } else if y > self.y1 { y - self.y1 } else { 0.0 };
        (dx * dx + (dy * vy) * (dy * vy)).sqrt()
    }

    /// Grows by `d` on every side.
    pub fn inflate(&self, d: f32) -> FRect {
        FRect::new(self.x0 - d, self.y0 - d, self.x1 + d, self.y1 + d)
    }

    /// Integer pixel rectangle covering this one (floor/ceil). Coordinates are clamped to
    /// +-2^29 pixels, so `x + w` always fits an `i32`; an empty or NaN rectangle gives an
    /// empty `Rect`.
    pub fn to_rect(&self) -> Rect {
        if self.is_empty() {
            return Rect::default();
        }
        const LIM: f32 = (1 << 29) as f32;
        let x0 = self.x0.floor().clamp(-LIM, LIM) as i32;
        let y0 = self.y0.floor().clamp(-LIM, LIM) as i32;
        let x1 = self.x1.ceil().clamp(-LIM, LIM) as i32;
        let y1 = self.y1.ceil().clamp(-LIM, LIM) as i32;
        Rect::new(x0, y0, x1.saturating_sub(x0).max(0) as u32, y1.saturating_sub(y0).max(0) as u32)
    }

    /// True when every coordinate is a finite number.
    pub fn is_finite(&self) -> bool {
        self.x0.is_finite() && self.y0.is_finite() && self.x1.is_finite() && self.y1.is_finite()
    }

    /// As an `ink_doc::Quad` (axis aligned).
    pub fn to_quad(&self) -> Quad {
        Quad { x0: self.x0, y0: self.y0, x1: self.x1, y1: self.y1 }
    }

    /// From an `ink_doc::Quad`.
    pub fn from_quad(q: &Quad) -> FRect {
        FRect::new(q.x0.min(q.x1), q.y0.min(q.y1), q.x0.max(q.x1), q.y0.max(q.y1))
    }

    /// From a pixel rectangle.
    pub fn from_rect(r: Rect) -> FRect {
        FRect::new(r.x as f32, r.y as f32, r.x as f32 + r.w as f32, r.y as f32 + r.h as f32)
    }
}

/// Affine matrix in MuPDF's convention: `x' = a x + c y + e`, `y' = b x + d y + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    /// Coefficients `a, b, c, d, e, f`.
    pub m: [f32; 6],
}

impl Default for Matrix {
    fn default() -> Self {
        Matrix::IDENTITY
    }
}

impl Matrix {
    /// The identity.
    pub const IDENTITY: Matrix = Matrix { m: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] };

    /// Uniform scale.
    pub fn scale(s: f32) -> Matrix {
        Matrix { m: [s, 0.0, 0.0, s, 0.0, 0.0] }
    }

    /// Translation.
    pub fn translate(tx: f32, ty: f32) -> Matrix {
        Matrix { m: [1.0, 0.0, 0.0, 1.0, tx, ty] }
    }

    /// Clockwise rotation by `deg` degrees (y points down, as in MuPDF's `fz_rotate`).
    /// Multiples of 90 are exact.
    pub fn rotate(deg: i32) -> Matrix {
        match deg.rem_euclid(360) {
            0 => Matrix::IDENTITY,
            90 => Matrix { m: [0.0, 1.0, -1.0, 0.0, 0.0, 0.0] },
            180 => Matrix { m: [-1.0, 0.0, 0.0, -1.0, 0.0, 0.0] },
            270 => Matrix { m: [0.0, -1.0, 1.0, 0.0, 0.0, 0.0] },
            d => {
                let (s, c) = (d as f32).to_radians().sin_cos();
                Matrix { m: [c, s, -s, c, 0.0, 0.0] }
            }
        }
    }

    /// `self` first, then `then` (MuPDF's `fz_concat(self, then)`).
    pub fn then(&self, then: &Matrix) -> Matrix {
        let [a, b, c, d, e, f] = self.m;
        let [a2, b2, c2, d2, e2, f2] = then.m;
        Matrix {
            m: [
                a * a2 + b * c2,
                a * b2 + b * d2,
                c * a2 + d * c2,
                c * b2 + d * d2,
                e * a2 + f * c2 + e2,
                e * b2 + f * d2 + f2,
            ],
        }
    }

    /// Inverse (identity for a degenerate matrix).
    pub fn invert(&self) -> Matrix {
        let [a, b, c, d, e, f] = self.m;
        let det = a * d - b * c;
        if det.abs() < 1e-12 {
            return Matrix::IDENTITY;
        }
        let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
        Matrix { m: [ia, ib, ic, id, -(e * ia + f * ic), -(e * ib + f * id)] }
    }

    /// Maps a point.
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.m;
        (a * x + c * y + e, b * x + d * y + f)
    }

    /// Bounding box of a mapped rectangle.
    pub fn map_rect(&self, r: &FRect) -> FRect {
        let pts = [self.apply(r.x0, r.y0), self.apply(r.x1, r.y0), self.apply(r.x0, r.y1), self.apply(r.x1, r.y1)];
        let mut out = FRect::new(f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in pts {
            out.x0 = out.x0.min(x);
            out.y0 = out.y0.min(y);
            out.x1 = out.x1.max(x);
            out.y1 = out.y1.max(y);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    #[test]
    fn rotation_and_inverse() {
        // A point at the top-right of a 100 x 50 page lands at the bottom-right after a
        // clockwise quarter turn (x' = -y, y' = x), shifted back into view.
        let r = Matrix::rotate(90);
        assert!(close(r.apply(100.0, 0.0), (0.0, 100.0)));
        let page = FRect::new(0.0, 0.0, 100.0, 50.0);
        let b = r.map_rect(&page);
        assert_eq!((b.w(), b.h()), (50.0, 100.0));
        let m = r.then(&Matrix::scale(2.0)).then(&Matrix::translate(-b.x0 * 2.0, -b.y0 * 2.0));
        assert!(close(m.apply(0.0, 0.0), (100.0, 0.0)));
        let inv = m.invert();
        assert!(close(inv.apply(100.0, 0.0), (0.0, 0.0)));
        for deg in [0, 90, 180, 270, 30] {
            let m = Matrix::rotate(deg).then(&Matrix::scale(1.5)).then(&Matrix::translate(7.0, -3.0));
            let p = m.apply(12.0, 34.0);
            assert!(close(m.invert().apply(p.0, p.1), (12.0, 34.0)), "{deg}");
        }
    }

    #[test]
    fn rect_ops() {
        let a = FRect::new(0.0, 0.0, 10.0, 10.0);
        let b = FRect::new(5.0, 5.0, 20.0, 20.0);
        assert_eq!(a.intersect(&b), FRect::new(5.0, 5.0, 10.0, 10.0));
        assert_eq!(a.union(&b), FRect::new(0.0, 0.0, 20.0, 20.0));
        assert!(FRect::default().is_empty());
        assert_eq!(FRect::default().union(&a), a);
        assert!(a.contains(0.0, 9.9) && !a.contains(10.0, 5.0));
        assert_eq!(a.distance(5.0, 5.0, 4.0), 0.0);
        assert_eq!(a.distance(5.0, 12.0, 4.0), 8.0);
        assert_eq!(FRect::new(0.2, 0.7, 10.1, 9.0).to_rect(), Rect::new(0, 0, 11, 9));
        // NaN, infinite and huge rectangles: no overflow, NaN is empty.
        assert!(FRect::new(f32::NAN, 0.0, 10.0, 10.0).is_empty());
        assert!(FRect::new(f32::NAN, f32::NAN, f32::NAN, f32::NAN).to_rect().is_empty());
        let huge = FRect::new(f32::NEG_INFINITY, -1e30, f32::INFINITY, 1e30).to_rect();
        assert_eq!((huge.x, huge.y, huge.w, huge.h), (-(1 << 29), -(1 << 29), 1 << 30, 1 << 30));
        assert_eq!(huge.intersect(&Rect::new(0, 0, 100, 50)), Rect::new(0, 0, 100, 50));
        assert!(!FRect::new(0.0, 0.0, f32::INFINITY, 1.0).is_finite());
    }
}
