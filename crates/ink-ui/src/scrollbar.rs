//! A thin vertical scroll bar for lists that move a screenful at a time.
//!
//! [`ScrollBar`] is a value: how many items there are (`total`), the first one shown
//! (`first`) and how many are shown (`shown`). Any unit works (items, rows, pixels). The
//! bar draws a `Theme::track()` track with a black thumb whose position and length are
//! the shown part of the whole; it draws nothing when everything fits.
//!
//! Layout: a screen gives the bar the region of its scrolling content *including* a
//! free column of [`SCROLLBAR_GUTTER`] pixels on the right ([`ScrollBar::track`] centres
//! the bar in that column, [`ScrollBar::content`] is the rest). The touch zone
//! ([`ScrollBar::hit_rect`], [`SCROLLBAR_HIT_W`] wide) reaches a little into the content
//! so it stays a full touch target.
//!
//! Use on e-ink (see docs/DESIGN.md, "Motion"): a swipe or a page key moves the list by
//! one screen ([`ScrollBar::paged`] for page-aligned lists); a tap or a drag on the bar
//! jumps to the item under the finger ([`ScrollBar::item_at`]). While a finger drags,
//! repaint only the track rectangle with DU ([`ScrollBar::paint`] with the previewed
//! position); on the lift, repaint the content and the bar once with GL16.

use crate::theme::space;
use crate::widget::PaintCx;
use ink_device::Rect;

/// Width of the drawn bar.
pub const SCROLLBAR_W: u32 = 14;
/// Width of the free column at the right of the content that holds the bar.
pub const SCROLLBAR_GUTTER: u32 = 48;
/// Width of the bar's touch zone (from the right edge of the region).
pub const SCROLLBAR_HIT_W: u32 = 96;
/// Smallest thumb length.
pub const SCROLLBAR_MIN_THUMB: u32 = 2 * SCROLLBAR_W;

/// Where a scrolling list is: `shown` items from `first` out of `total`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollBar {
    /// All items.
    pub total: usize,
    /// Index of the first item shown.
    pub first: usize,
    /// Items shown.
    pub shown: usize,
}

impl ScrollBar {
    /// `shown` items from `first` out of `total`.
    pub fn new(total: usize, first: usize, shown: usize) -> Self {
        ScrollBar { total, first, shown }
    }

    /// Page `page` of a list with `per` items per screen (the page is clamped).
    pub fn paged(total: usize, per: usize, page: usize) -> Self {
        let per = per.max(1);
        let page = page.min(Self::pages_of(total, per) - 1);
        let first = (page * per).min(total);
        ScrollBar { total, first, shown: per.min(total - first) }
    }

    /// Screens a list of `total` items needs at `per` items per screen (at least 1).
    pub fn pages_of(total: usize, per: usize) -> usize {
        total.div_ceil(per.max(1)).max(1)
    }

    /// True when not everything fits: the bar is drawn and reacts.
    pub fn needed(&self) -> bool {
        self.first > 0 || self.first + self.shown < self.total
    }

    /// The content part of `region`: all but the bar's column on the right.
    pub fn content(region: Rect) -> Rect {
        Rect::new(region.x, region.y, region.w.saturating_sub(SCROLLBAR_GUTTER), region.h)
    }

    /// The bar's track: centred in the right column of `region`, a little inset from
    /// its top and bottom.
    pub fn track(region: Rect) -> Rect {
        let inset = space(1).min(region.h as i32 / 4);
        let x = region.right() - (SCROLLBAR_GUTTER + SCROLLBAR_W) as i32 / 2;
        Rect::new(x, region.y + inset, SCROLLBAR_W, (region.h as i32 - 2 * inset).max(0) as u32)
    }

    /// The touch zone of the bar in `region`: its full height, [`SCROLLBAR_HIT_W`] wide
    /// at the right edge.
    pub fn hit_rect(region: Rect) -> Rect {
        let w = SCROLLBAR_HIT_W.min(region.w);
        Rect::new(region.right() - w as i32, region.y, w, region.h)
    }

    /// The thumb inside `track`.
    pub fn thumb(&self, track: Rect) -> Rect {
        let total = self.total.max(1) as u64;
        let h = track.h as u64;
        let min = (SCROLLBAR_MIN_THUMB as u64).min(h);
        let len = (h * self.shown.min(self.total) as u64 / total).max(min);
        let top = (h * self.first.min(self.total) as u64 / total).min(h - len);
        Rect::new(track.x, track.y + top as i32, track.w, len as u32)
    }

    /// The item at height `y` of `track` (clamped to the list): where a tap or a drag on
    /// the bar jumps.
    pub fn item_at(&self, track: Rect, y: i32) -> usize {
        if self.total == 0 || track.h == 0 {
            return 0;
        }
        let off = (y - track.y).clamp(0, track.h as i32 - 1) as u64;
        ((off * self.total as u64 / track.h as u64) as usize).min(self.total - 1)
    }

    /// Draws the track and the thumb into `track` (nothing when [`ScrollBar::needed`]
    /// is false). Only `track` is touched.
    pub fn paint(&self, cx: &mut PaintCx, track: Rect) {
        if !self.needed() || track.h == 0 {
            return;
        }
        let r = SCROLLBAR_W as f32 / 2.0;
        crate::draw::fill_round_rect(cx.canvas, track, r, cx.theme.track());
        crate::draw::fill_round_rect(cx.canvas, self.thumb(track), r, cx.theme.fg);
    }

    /// The drag preview, for a DU update of `track` while a finger moves on the bar:
    /// the track cleared to paper and the black thumb (pure black and white, as DU
    /// needs). The lift repaints the bar with [`ScrollBar::paint`].
    pub fn paint_drag(&self, cx: &mut PaintCx, track: Rect) {
        cx.canvas.fill_rect(track.x, track.y, track.w as i32, track.h as i32, cx.theme.bg);
        if self.needed() && track.h > 0 {
            crate::draw::fill_round_rect(cx.canvas, self.thumb(track), SCROLLBAR_W as f32 / 2.0, cx.theme.fg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paged_positions_and_thumb() {
        let s = ScrollBar::paged(20, 7, 1);
        assert_eq!((s.first, s.shown), (7, 7));
        assert!(s.needed());
        assert_eq!(ScrollBar::paged(20, 7, 9), ScrollBar::new(20, 14, 6), "the page is clamped");
        assert!(!ScrollBar::paged(5, 7, 0).needed());
        assert!(!ScrollBar::paged(0, 7, 0).needed());
        assert_eq!(ScrollBar::pages_of(0, 7), 1);
        assert_eq!(ScrollBar::pages_of(15, 7), 3);
        let track = Rect::new(0, 100, 14, 1000);
        assert_eq!(ScrollBar::new(20, 0, 10).thumb(track), Rect::new(0, 100, 14, 500));
        assert_eq!(ScrollBar::new(20, 10, 10).thumb(track), Rect::new(0, 600, 14, 500));
        // A long list keeps a visible thumb that ends at the track's bottom.
        let t = ScrollBar::new(10_000, 9_999, 1).thumb(track);
        assert_eq!((t.h, t.bottom()), (SCROLLBAR_MIN_THUMB, track.bottom()));
    }

    #[test]
    fn item_at_maps_the_track_to_the_list() {
        let s = ScrollBar::new(30, 0, 10);
        let track = Rect::new(0, 100, 14, 300);
        assert_eq!(s.item_at(track, 0), 0);
        assert_eq!(s.item_at(track, 100), 0);
        assert_eq!(s.item_at(track, 250), 15);
        assert_eq!(s.item_at(track, 399), 29);
        assert_eq!(s.item_at(track, 5000), 29);
        assert_eq!(ScrollBar::new(0, 0, 0).item_at(track, 250), 0);
    }

    #[test]
    fn geometry_in_a_region() {
        let region = Rect::new(32, 256, 1240, 1310);
        let c = ScrollBar::content(region);
        let t = ScrollBar::track(region);
        let hit = ScrollBar::hit_rect(region);
        assert_eq!(c.right(), 1272 - SCROLLBAR_GUTTER as i32);
        assert!(t.x > c.right() && t.right() < region.right(), "the bar sits in its own column");
        assert_eq!((hit.right(), hit.w, hit.h), (region.right(), SCROLLBAR_HIT_W, region.h));
        assert!(hit.x <= t.x);
    }
}
