//! The [`Screen`] trait, e-ink refresh classes and the shared rotate/invert copy.

use crate::canvas::Canvas;
use crate::geom::{Point, Rect, Rotation, Size};
use crate::Result;
use std::time::{Duration, Instant};

/// E-ink waveform class of an update. `ink-ui` picks the class per damaged rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefreshClass {
    /// DU: fast 2-level partial update, for taps, highlights and keyboard keys.
    Du,
    /// GL16: 16-level partial update without flash, for UI changes.
    #[default]
    Gl16,
    /// REAGL (MTK GLR16): page turns without a flash and with little ghosting.
    Reagl,
    /// GC16 full update with a black flash; clears ghosting. Every N pages or on request.
    Gc16,
}

impl RefreshClass {
    /// True for classes that flash the screen.
    pub fn flashes(self) -> bool {
        self == RefreshClass::Gc16
    }
}

/// Direction of the panel controller's page-turn wave (MTK "swipe" update: one update
/// that the EPDC sweeps across the rectangle), in logical screen terms: `Left` sweeps
/// from the right edge to the left (a next page), `Right` the other way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wave {
    /// Right to left (next page in a left-to-right book).
    Left,
    /// Left to right (previous page).
    Right,
}

impl Wave {
    /// The wave of a page turn: `forward` (next page) sweeps left, unless the book reads
    /// right to left.
    pub fn for_turn(forward: bool, rtl: bool) -> Self {
        if forward != rtl {
            Wave::Left
        } else {
            Wave::Right
        }
    }

    /// The direction on the panel (native portrait) as a unit step `(dx, dy)`.
    pub fn panel_step(self, rotation: Rotation, panel: Size) -> (i32, i32) {
        let dx = if self == Wave::Left { -1 } else { 1 };
        let c = Point::new(panel.w.min(panel.h) as i32 / 2, panel.w.min(panel.h) as i32 / 2);
        let a = rotation.to_panel(c, panel);
        let b = rotation.to_panel(Point::new(c.x + dx, c.y), panel);
        (b.x - a.x, b.y - a.y)
    }
}

/// A display that shows rectangles of a [`Canvas`].
///
/// The canvas passed to [`Screen::update`] is in *logical* coordinates: its size is
/// [`Screen::size`], which already accounts for [`Screen::rotation`].
pub trait Screen {
    /// Native panel size (portrait).
    fn panel_size(&self) -> Size;

    /// Current rotation.
    fn rotation(&self) -> Rotation;

    /// Changes the rotation. Later updates expect a canvas of the new [`Screen::size`].
    fn set_rotation(&mut self, rotation: Rotation) -> Result<()>;

    /// Logical size: the panel size after rotation.
    fn size(&self) -> Size {
        self.rotation().logical_size(self.panel_size())
    }

    /// Whether night mode (inverted output) is on.
    fn night_mode(&self) -> bool;

    /// Turns night mode on or off. Takes effect on the next update.
    fn set_night_mode(&mut self, on: bool) -> Result<()>;

    /// Shows `rect` (logical coordinates) of `canvas` with the given waveform class.
    /// Full-screen [`RefreshClass::Gc16`] and [`RefreshClass::Reagl`] updates wait until
    /// the panel finished, so the next frame does not race the refresh.
    ///
    /// On e-ink panels this may also block briefly *before* writing pixels: until the
    /// driver took over (submitted) an earlier update that overlaps `rect`, so the new
    /// pixels never mix into an update still being read and a short press highlight is
    /// not dropped by the following release (see [`InFlightUpdates`]).
    fn update(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass) -> Result<()>;

    /// As [`Screen::update`], for a page turn: panels that can sweep one update across
    /// the screen (MTK hardware page-turn wave) do so in direction `wave`; others make a
    /// plain update. Still one update.
    fn update_wave(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass, _wave: Wave) -> Result<()> {
        self.update(canvas, rect, class)
    }

    /// Shows the whole canvas.
    fn update_all(&mut self, canvas: &Canvas, class: RefreshClass) -> Result<()> {
        self.update(canvas, canvas.bounds(), class)
    }

    /// Re-flashes whatever the panel holds now (GC16), without new content.
    fn refresh_all(&mut self) -> Result<()>;

    /// What the panel shows now, in panel orientation.
    fn screenshot(&self) -> Result<Canvas>;

    /// Called by the app after each frame went to the panel: `canvas` is what the panel
    /// should show now. The headless screen uses it to check that every changed pixel
    /// was inside a submitted update rect ([`crate::headless::HeadlessProbe::uncovered`]);
    /// other screens ignore it.
    fn frame_done(&mut self, _canvas: &Canvas) {}
}

/// Most updates [`InFlightUpdates`] remembers; the oldest is forgotten first.
pub const MAX_IN_FLIGHT: usize = 8;

/// After this long an update is taken as submitted and finished without asking the
/// driver (the slowest waveform, GC16, takes well under a second).
pub const IN_FLIGHT_MAX_AGE: Duration = Duration::from_secs(2);

/// One panel update sent to the driver and not known to be finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InFlight {
    /// Panel rectangle.
    pub rect: Rect,
    /// Driver update marker.
    pub marker: u32,
    /// A FULL update (REAGL, GC16, night waveforms) whose completion nobody waited for.
    pub full: bool,
    /// When it was sent.
    pub sent: Instant,
}

/// The driver waits needed before the next write or update of a rectangle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Waits {
    /// Wait until these markers were submitted (partial updates: cheap).
    pub submission: Vec<u32>,
    /// Wait until these markers completed (FULL updates still running).
    pub completion: Vec<u32>,
}

impl Waits {
    /// True when nothing has to be waited for.
    pub fn is_empty(&self) -> bool {
        self.submission.is_empty() && self.completion.is_empty()
    }
}

/// Bookkeeping of the e-ink updates in flight, so a backend waits only for the earlier
/// updates that overlap the rectangle it writes next:
///
/// - an overlapping partial update (DU, GL16): wait for its *submission* only, a few ms;
/// - an overlapping FULL update not waited for yet: wait for its *completion*;
/// - updates elsewhere on the panel: no wait at all, so a page turn never waits for a
///   key highlight in another corner, and never for the completion of DU slide frames.
///
/// It only decides; the backend does the ioctls (`kindle/fb.rs`).
#[derive(Clone, Debug, Default)]
pub struct InFlightUpdates {
    list: Vec<InFlight>,
}

impl InFlightUpdates {
    /// Empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a sent update. `full`: a FULL update whose completion was not waited for.
    pub fn sent(&mut self, rect: Rect, marker: u32, full: bool, now: Instant) {
        self.list.push(InFlight { rect, marker, full, sent: now });
        if self.list.len() > MAX_IN_FLIGHT {
            self.list.remove(0);
        }
    }

    /// The waits needed before writing or updating panel rectangle `rect`. The returned
    /// markers are forgotten: the caller waits for them now.
    pub fn before(&mut self, rect: Rect, now: Instant) -> Waits {
        self.list.retain(|f| now.saturating_duration_since(f.sent) < IN_FLIGHT_MAX_AGE);
        let mut w = Waits::default();
        self.list.retain(|f| {
            if f.rect.intersect(&rect).is_empty() {
                return true;
            }
            if f.full {
                w.completion.push(f.marker);
            } else {
                w.submission.push(f.marker);
            }
            false
        });
        w
    }

    /// Forgets everything and returns what is left to wait for (before sleeping,
    /// screenshots, exit).
    pub fn drain(&mut self, now: Instant) -> Waits {
        let all = self.list.iter().fold(Rect::default(), |r, f| r.union(&f.rect));
        self.before(all, now)
    }

    /// Updates still remembered.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// True when no update is remembered.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// Copies `rect` (logical) of `canvas` into a panel-orientation buffer, applying the
/// rotation and night-mode inversion. Returns the panel rectangle that changed.
///
/// `dst` has `dst_stride` bytes per row and at least `panel.h` rows.
pub fn copy_to_panel(
    canvas: &Canvas,
    rect: Rect,
    rotation: Rotation,
    night: bool,
    panel: Size,
    dst: &mut [u8],
    dst_stride: usize,
) -> Rect {
    let rect = rect.intersect(&canvas.bounds()).intersect(&Rect::from_size(rotation.logical_size(panel)));
    if rect.is_empty() {
        return rect;
    }
    let panel_rect = rotation.rect_to_panel(rect, panel);
    // The panel is written row by row (the framebuffer may be uncached memory). Along a
    // panel row the canvas is read forwards or backwards, along a canvas row (half
    // turns) or down a canvas column (quarter turns, in bands of 64 panel columns so
    // the canvas rows they read stay in the cache).
    let n = canvas.width;
    let quarter = matches!(rotation, Rotation::Clockwise | Rotation::CounterClockwise);
    let band = if quarter { 64 } else { panel_rect.w as usize };
    for x0 in (panel_rect.x..panel_rect.right()).step_by(band) {
        let w = band.min((panel_rect.right() - x0) as usize);
        for py in panel_rect.y..panel_rect.bottom() {
            let d = py as usize * dst_stride + x0 as usize;
            let out = &mut dst[d..d + w];
            // The canvas pixel the first panel pixel shows, and the lowest address read.
            let first = rotation.from_panel(Point::new(x0, py), panel);
            let s = first.y as usize * n + first.x as usize;
            let (lo, step, backwards) = match rotation {
                Rotation::Portrait => (s, 1, false),
                Rotation::UpsideDown => (s + 1 - w, 1, true),
                Rotation::CounterClockwise => (s, n, false),
                Rotation::Clockwise => (s - (w - 1) * n, n, true),
            };
            let src = &canvas.pixels[lo..=lo + (w - 1) * step];
            match (step, backwards) {
                // memcpy, also in unoptimised test builds.
                (1, false) if !night => out.copy_from_slice(src),
                (1, false) => copy_pixels(out.iter_mut(), src.iter(), night),
                (1, true) => copy_pixels(out.iter_mut().rev(), src.iter(), night),
                (_, false) => copy_pixels(out.iter_mut(), src.iter().step_by(step), night),
                (_, true) => copy_pixels(out.iter_mut().rev(), src.iter().step_by(step), night),
            }
        }
    }
    panel_rect
}

/// Copies `src` into `out`, inverted for night mode.
fn copy_pixels<'a>(out: impl Iterator<Item = &'a mut u8>, src: impl Iterator<Item = &'a u8>, night: bool) {
    if night {
        out.zip(src).for_each(|(o, v)| *o = 255 - v);
    } else {
        out.zip(src).for_each(|(o, v)| *o = *v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_direction_on_the_panel() {
        let p = Size::new(1272, 1696);
        assert_eq!(Wave::Left.panel_step(Rotation::Portrait, p), (-1, 0));
        assert_eq!(Wave::Right.panel_step(Rotation::Portrait, p), (1, 0));
        assert_eq!(Wave::Left.panel_step(Rotation::UpsideDown, p), (1, 0));
        // Landscape: logical left is along the panel's long side.
        assert_eq!(Wave::Left.panel_step(Rotation::Clockwise, p), (0, -1));
        assert_eq!(Wave::Left.panel_step(Rotation::CounterClockwise, p), (0, 1));
        assert_eq!(Wave::for_turn(true, false), Wave::Left);
        assert_eq!(Wave::for_turn(false, false), Wave::Right);
        assert_eq!(Wave::for_turn(true, true), Wave::Right);
    }

    /// Every rotation, night or day, clipped or not, into a padded buffer: the same
    /// pixels as mapping each logical pixel on its own.
    #[test]
    fn copy_matches_a_pixel_by_pixel_copy() {
        let (panel, stride) = (Size::new(70, 150), 75);
        for rotation in Rotation::ALL {
            let l = rotation.logical_size(panel);
            let mut c = Canvas::new(l.w as usize, l.h as usize);
            c.pixels.iter_mut().enumerate().for_each(|(i, p)| *p = (i * 31 % 251) as u8);
            for rect in [c.bounds(), Rect::new(3, 5, 1, 1), Rect::new(-4, 9, 66, 70), Rect::new(10, 2, 200, 1)] {
                for night in [false, true] {
                    let mut got = vec![7u8; stride * panel.h as usize];
                    let changed = copy_to_panel(&c, rect, rotation, night, panel, &mut got, stride);
                    let mut want = vec![7u8; got.len()];
                    let r = rect.intersect(&c.bounds());
                    for y in r.y..r.bottom() {
                        for x in r.x..r.right() {
                            let p = rotation.to_panel(Point::new(x, y), panel);
                            let v = c.get(x, y).unwrap();
                            want[p.y as usize * stride + p.x as usize] = if night { 255 - v } else { v };
                        }
                    }
                    assert!(got == want, "{rotation:?} {rect:?} night {night}");
                    assert_eq!(changed, rotation.rect_to_panel(r, panel));
                }
            }
        }
    }

    #[test]
    fn copy_rotates_and_inverts() {
        let panel = Size::new(4, 6);
        let mut c = Canvas::new(6, 4); // landscape logical canvas
        c.set(0, 0, 10);
        let mut dst = vec![255u8; 24];
        let r = copy_to_panel(&c, c.bounds(), Rotation::Clockwise, true, panel, &mut dst, 4);
        assert_eq!(r, Rect::new(0, 0, 4, 6));
        // Logical (0, 0) is panel (pw - 1, 0) when rotated clockwise.
        assert_eq!(dst[3], 245);
        assert_eq!(dst[0], 0);
    }

    #[test]
    fn in_flight_waits_only_for_overlapping_updates() {
        let t0 = Instant::now();
        let mut f = InFlightUpdates::new();
        f.sent(Rect::new(0, 0, 100, 100), 1, false, t0); // key highlight, DU
        f.sent(Rect::new(500, 500, 100, 100), 2, false, t0); // elsewhere
        f.sent(Rect::new(0, 1000, 100, 100), 3, true, t0); // FULL, not waited yet
        // The key release over the same key: submission of marker 1 only.
        let w = f.before(Rect::new(10, 10, 50, 50), t0);
        assert_eq!(w, Waits { submission: vec![1], completion: vec![] });
        // Asked again: already waited for.
        assert!(f.before(Rect::new(10, 10, 50, 50), t0).is_empty());
        // A full-screen page turn: submission of the DU, completion of the FULL.
        let w = f.before(Rect::new(0, 0, 1272, 1696), t0);
        assert_eq!(w, Waits { submission: vec![2], completion: vec![3] });
        assert!(f.is_empty());
        // Old entries are dropped without a wait.
        f.sent(Rect::new(0, 0, 10, 10), 4, true, t0);
        assert!(f.before(Rect::new(0, 0, 10, 10), t0 + IN_FLIGHT_MAX_AGE).is_empty());
        // At most MAX_IN_FLIGHT entries; drain returns the rest.
        for m in 0..20 {
            f.sent(Rect::new(m * 20, 0, 10, 10), m as u32, false, t0);
        }
        assert_eq!(f.len(), MAX_IN_FLIGHT);
        assert_eq!(f.drain(t0).submission, (12..20).collect::<Vec<u32>>());
        assert!(f.is_empty());
    }
}
