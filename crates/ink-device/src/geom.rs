//! Integer geometry shared by every crate: points, sizes, rectangles and screen rotation.
//!
//! All coordinates are pixels. The *panel* is the physical display in its native
//! portrait orientation (1272 x 1696 on the PW12). *Logical* coordinates are what the UI
//! draws in after [`Rotation`] is applied.

/// A pixel position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point {
    /// Column, growing to the right.
    pub x: i32,
    /// Row, growing downwards.
    pub y: i32,
}

impl Point {
    /// Builds a point.
    pub const fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }
}

/// A width and height in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Size {
    /// Builds a size.
    pub const fn new(w: u32, h: u32) -> Self {
        Size { w, h }
    }

    /// The same size with width and height swapped (for 90 and 270 degree rotations).
    pub const fn transposed(self) -> Self {
        Size { w: self.h, h: self.w }
    }
}

/// An axis-aligned rectangle. `w` or `h` of 0 means empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left column.
    pub x: i32,
    /// Top row.
    pub y: i32,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Rect {
    /// Builds a rectangle.
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Rect { x, y, w, h }
    }

    /// The rectangle `(0, 0, size)`.
    pub const fn from_size(size: Size) -> Self {
        Rect { x: 0, y: 0, w: size.w, h: size.h }
    }

    /// One past the right column.
    pub const fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    /// One past the bottom row.
    pub const fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    /// True when the rectangle covers no pixel.
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// True when `p` lies inside.
    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.right() && p.y < self.bottom()
    }

    /// Overlap of two rectangles (empty when they do not overlap).
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        if x1 <= x0 || y1 <= y0 {
            Rect::default()
        } else {
            Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
        }
    }

    /// Smallest rectangle containing both (an empty side is ignored).
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = self.right().max(o.right());
        let y1 = self.bottom().max(o.bottom());
        Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
    }
}

/// Screen orientation, clockwise from the panel's native portrait orientation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rotation {
    /// Native portrait (the PW12 default).
    #[default]
    Portrait,
    /// Rotated 90 degrees clockwise: landscape.
    Clockwise,
    /// Upside-down portrait.
    UpsideDown,
    /// Rotated 90 degrees counter-clockwise: landscape.
    CounterClockwise,
}

impl Rotation {
    /// All four orientations, in clockwise order.
    pub const ALL: [Rotation; 4] =
        [Rotation::Portrait, Rotation::Clockwise, Rotation::UpsideDown, Rotation::CounterClockwise];

    /// Logical screen size for a panel of `panel` size.
    pub const fn logical_size(self, panel: Size) -> Size {
        match self {
            Rotation::Portrait | Rotation::UpsideDown => panel,
            Rotation::Clockwise | Rotation::CounterClockwise => panel.transposed(),
        }
    }

    /// Maps a logical point to the panel pixel that shows it.
    pub const fn to_panel(self, p: Point, panel: Size) -> Point {
        let (pw, ph) = (panel.w as i32, panel.h as i32);
        match self {
            Rotation::Portrait => p,
            // Logical (x, y) on a landscape screen of size (ph, pw).
            Rotation::Clockwise => Point::new(pw - 1 - p.y, p.x),
            Rotation::UpsideDown => Point::new(pw - 1 - p.x, ph - 1 - p.y),
            Rotation::CounterClockwise => Point::new(p.y, ph - 1 - p.x),
        }
    }

    /// Maps a panel pixel (for example a touch position) to logical coordinates.
    pub const fn from_panel(self, p: Point, panel: Size) -> Point {
        let (pw, ph) = (panel.w as i32, panel.h as i32);
        match self {
            Rotation::Portrait => p,
            Rotation::Clockwise => Point::new(p.y, pw - 1 - p.x),
            Rotation::UpsideDown => Point::new(pw - 1 - p.x, ph - 1 - p.y),
            Rotation::CounterClockwise => Point::new(ph - 1 - p.y, p.x),
        }
    }

    /// Maps a logical rectangle to the panel rectangle that shows it.
    pub fn rect_to_panel(self, r: Rect, panel: Size) -> Rect {
        if r.is_empty() {
            return r;
        }
        let a = self.to_panel(Point::new(r.x, r.y), panel);
        let b = self.to_panel(Point::new(r.right() - 1, r.bottom() - 1), panel);
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
        Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32)
    }

    /// Rotation by a further 90 degrees clockwise.
    pub const fn next(self) -> Rotation {
        match self {
            Rotation::Portrait => Rotation::Clockwise,
            Rotation::Clockwise => Rotation::UpsideDown,
            Rotation::UpsideDown => Rotation::CounterClockwise,
            Rotation::CounterClockwise => Rotation::Portrait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: Size = Size::new(1272, 1696);

    #[test]
    fn rotation_round_trips_every_corner() {
        for r in Rotation::ALL {
            let l = r.logical_size(PANEL);
            for p in [Point::new(0, 0), Point::new(l.w as i32 - 1, 0), Point::new(5, l.h as i32 - 1)] {
                let q = r.to_panel(p, PANEL);
                assert!(Rect::from_size(PANEL).contains(q), "{r:?} {p:?} -> {q:?}");
                assert_eq!(r.from_panel(q, PANEL), p, "{r:?}");
            }
        }
    }

    #[test]
    fn rect_mapping_keeps_area() {
        let r = Rect::new(10, 20, 100, 50);
        for rot in Rotation::ALL {
            let m = rot.rect_to_panel(r, PANEL);
            assert_eq!(m.w * m.h, 5000, "{rot:?}");
        }
        assert_eq!(Rotation::Clockwise.rect_to_panel(r, PANEL), Rect::new(1272 - 70, 10, 50, 100));
    }

    #[test]
    fn intersect_and_union() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
        assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
        assert!(a.intersect(&Rect::new(20, 20, 1, 1)).is_empty());
    }
}
