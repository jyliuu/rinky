//! Damage tracking and the e-ink refresh policy.
//!
//! Widgets report [`Damage`] (a rectangle plus a [`RefreshClass`]): DU for touch feedback
//! and keys, GL16 for UI changes, REAGL for page content, GC16 when a screen appears.
//! Before an update the app merges overlapping damage ([`merge`]) and lets the
//! [`RefreshScheduler`] decide:
//!
//! - page turns ([`RefreshScheduler::page_turn`]): REAGL, a GC16 flash every N turns or
//!   after [`RefreshScheduler::request_flash`];
//! - UI updates ([`RefreshScheduler::ui`]): classes as reported, but after
//!   [`RefreshScheduler::request_ui_flash`] (or every `ui_flash_every` partial updates
//!   when set; DU/GL16 leave ghosts) the next UI update becomes one GC16 of the area that
//!   changed.
//!
//! The newer, per-frame path:
//!
//! - [`coalesce`] turns all damage of one frame into at most [`MAX_RECTS`] rectangles
//!   (touching rectangles merge, nearby ones of the same class merge, a large GL16 area
//!   becomes one full-screen update) in issue order DU, GL16, REAGL, GC16;
//! - [`Change`] names what changed on screen and [`RefreshScheduler::class_for`] picks the
//!   class for it (page turns REAGL with a GC16 every N turns or when ghost debt is due,
//!   UI GL16, tap feedback DU only on 2-level content, slider drag DU then GL16 settle).

use ink_device::{Canvas, Rect, RefreshClass};

/// A rectangle that needs to be shown with a refresh class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damage {
    /// Logical rectangle.
    pub rect: Rect,
    /// Waveform class.
    pub class: RefreshClass,
}

/// Merges overlapping damage. The merged rectangle takes the strongest class
/// (GC16 > REAGL > GL16 > DU), so a flash is never lost by merging.
pub fn merge(mut list: Vec<Damage>) -> Vec<Damage> {
    let mut out: Vec<Damage> = Vec::new();
    while let Some(mut d) = list.pop() {
        let mut changed = true;
        while changed {
            changed = false;
            let mut i = 0;
            while i < out.len() {
                if !out[i].rect.intersect(&d.rect).is_empty() {
                    let o = out.swap_remove(i);
                    d = Damage { rect: d.rect.union(&o.rect), class: d.class.max(o.class) };
                    changed = true;
                } else {
                    i += 1;
                }
            }
        }
        out.push(d);
    }
    out.sort_by_key(|d| (d.rect.y, d.rect.x));
    out
}

/// Bounding box of all damage.
pub fn bounds(list: &[Damage]) -> Rect {
    list.iter().fold(Rect::default(), |r, d| r.union(&d.rect))
}

/// Most rectangles [`coalesce`] returns for one frame (one ioctl each).
pub const MAX_RECTS: usize = 4;

/// A GL16 frame covering at least this share of the screen (in percent) becomes one
/// full-screen update: one ioctl, and a PARTIAL update drives only changed pixels.
pub const FULL_SCREEN_PERCENT: u64 = 60;

/// A GL16 frame covering at least this share of the screen (in percent) is a screen
/// change (a list jump, a tab switch, a full-screen page opening or closing): one
/// full-screen GC16 flash (GCK16 at night), as the Kindle and KOReader flash on such
/// changes. Below it, a large GL16 frame stays a non-flashing update. The settings
/// screen and the reader's sheets avoid this flash: settings list jumps are DU (the rows
/// are black and white) and whole pages REAGL ([`no_flash`]). The app loop measures the
/// share on the pixels that really changed ([`trim_to_changes`]), so a popup over a
/// screen that repaints its whole canvas never flashes.
pub const FLASH_PERCENT: u64 = 80;

fn area(r: &Rect) -> u64 {
    r.w as u64 * r.h as u64
}

/// True when `a` and `b` overlap or share an edge or corner (KOReader's
/// `openIntersectWith`: the EPDC treats touching updates as one area anyway).
fn touches(a: &Rect, b: &Rect) -> bool {
    !a.is_empty() && !b.is_empty() && a.x <= b.right() && b.x <= a.right() && a.y <= b.bottom() && b.y <= a.bottom()
}

/// Merges damage whose rectangles touch, and damage of the same class whose union is
/// at most 1.3 times their summed area. Repeats until nothing merges. The merged
/// rectangle takes the stronger class.
fn merge_close(mut list: Vec<Damage>) -> Vec<Damage> {
    loop {
        let mut merged = false;
        let mut out: Vec<Damage> = Vec::with_capacity(list.len());
        'next: for d in list {
            for o in out.iter_mut() {
                let union = o.rect.union(&d.rect);
                let near_same = o.class == d.class && area(&union) * 10 <= (area(&o.rect) + area(&d.rect)) * 13;
                if touches(&o.rect, &d.rect) || near_same {
                    *o = Damage { rect: union, class: o.class.max(d.class) };
                    merged = true;
                    continue 'next;
                }
            }
            out.push(d);
        }
        list = out;
        if !merged {
            return list;
        }
    }
}

/// Sorts damage into issue order: DU first (fastest visible feedback), then GL16, then
/// the FULL updates REAGL and GC16 (they block until done); top to bottom inside a class.
fn issue_order(list: &mut [Damage]) {
    list.sort_by_key(|d| (d.class, d.rect.y, d.rect.x));
}

/// Coalesces all damage of one frame before it goes to the panel:
///
/// 1. clips to `screen` and drops rectangles 1 px wide or high (the EPDC dislikes them);
/// 2. merges rectangles that overlap or touch; the merge takes the stronger class;
/// 3. merges rectangles of the same class whose bounding box is at most 1.3 times their
///    summed area (a DU press is never widened into GL16 because a label nearby changed);
/// 4. caps the result at [`MAX_RECTS`]: above that, each class becomes its bounding box
///    and step 2 runs again;
/// 5. when the GL16 rectangles together cover [`FLASH_PERCENT`] of the screen, the
///    frame is a screen change: one full-screen GC16 (it covers every other rectangle);
/// 6. else when they cover [`FULL_SCREEN_PERCENT`], they and the DU rectangles become
///    one full-screen GL16 (still PARTIAL). REAGL and GC16 rectangles stay as they are:
///    they drive every pixel of their rectangle, so they are never widened. DU alone is
///    never widened either (2 levels only);
/// 7. sorts into issue order DU, GL16, REAGL, GC16.
pub fn coalesce(list: Vec<Damage>, screen: Rect) -> Vec<Damage> {
    let mut out = reduce(list, screen);
    if gl16_covers(&out, screen, FLASH_PERCENT) {
        return vec![Damage { rect: screen, class: RefreshClass::Gc16 }];
    }
    if gl16_covers(&out, screen, FULL_SCREEN_PERCENT) {
        // DU and GL16 are covered by the full-screen GL16; no merge, so REAGL and GC16
        // keep their own rectangles and the result stays within MAX_RECTS.
        out.retain(|d| matches!(d.class, RefreshClass::Reagl | RefreshClass::Gc16));
        out.push(Damage { rect: screen, class: RefreshClass::Gl16 });
        issue_order(&mut out);
    }
    out
}

/// Keeps `damage` from becoming a flash in [`coalesce`]: when its GL16 rectangles cover
/// [`FLASH_PERCENT`] of `screen`, they are shown with `class` instead (REAGL, from
/// [`Change::LargePanel`]); otherwise `damage` is returned as it is.
pub fn no_flash(damage: Vec<Damage>, screen: Rect, class: RefreshClass) -> Vec<Damage> {
    if !would_flash(&damage, screen) {
        return damage;
    }
    damage.into_iter().map(|d| if d.class == RefreshClass::Gl16 { Damage { class, ..d } } else { d }).collect()
}

/// True when [`coalesce`] would turn `damage` into a full-screen GC16 (its GL16
/// rectangles cover [`FLASH_PERCENT`] of `screen`).
pub fn would_flash(damage: &[Damage], screen: Rect) -> bool {
    gl16_covers(&reduce(damage.to_vec(), screen), screen, FLASH_PERCENT)
}

/// True when the GL16 rectangles of `reduced` cover at least `percent` of `screen`.
/// After [`reduce`] the rectangles are disjoint, so their summed area is the covered
/// area.
fn gl16_covers(reduced: &[Damage], screen: Rect, percent: u64) -> bool {
    let gl16: u64 = reduced.iter().filter(|d| d.class == RefreshClass::Gl16).map(|d| area(&d.rect)).sum();
    gl16 * 100 >= area(&screen) * percent
}

/// Run before [`coalesce`] with `shown`, a copy of what the panel shows: when the
/// frame would flash ([`would_flash`]), its GL16 rectangles are cut to the pixels that
/// differ from `shown` ([`changed_bands`]). A screen that repaints the whole canvas for
/// a popup (the library's menus, a dialog, a keyboard) reports a full-screen GL16;
/// only the popup's pixels changed, so it shows as a GL16 of the popup, and its
/// dismissal as a GL16 of what the popup covered. A frame whose pixels really changed
/// over [`FLASH_PERCENT`] of the screen (a tab switch, a list page) still flashes.
/// `shown` of another size (or `None`: the panel content is not known, after a
/// rotation, night toggle or wake) keeps `damage` as it is.
pub fn trim_to_changes(damage: Vec<Damage>, screen: Rect, shown: Option<&Canvas>, canvas: &Canvas) -> Vec<Damage> {
    let Some(old) = shown.filter(|s| s.width == canvas.width && s.height == canvas.height) else { return damage };
    if !would_flash(&damage, screen) {
        return damage;
    }
    let (gl16, mut out): (Vec<Damage>, Vec<Damage>) = damage.into_iter().partition(|d| d.class == RefreshClass::Gl16);
    let cut = changed_bands(old, canvas, gl16);
    out.extend(cut);
    out
}

/// Steps 1-4 and 6 of [`coalesce`], without the full-screen promotion (for a part of
/// the screen, e.g. one plugin layer).
pub fn reduce(list: Vec<Damage>, clip: Rect) -> Vec<Damage> {
    let list: Vec<Damage> = list
        .into_iter()
        .map(|d| Damage { rect: d.rect.intersect(&clip), class: d.class })
        .filter(|d| d.rect.w > 1 && d.rect.h > 1)
        .collect();
    let mut out = merge_close(list);
    if out.len() > MAX_RECTS {
        let mut groups: Vec<Damage> = Vec::new();
        for d in out {
            match groups.iter_mut().find(|g| g.class == d.class) {
                Some(g) => g.rect = g.rect.union(&d.rect),
                None => groups.push(d),
            }
        }
        // At most four classes, so at most four rectangles.
        out = merge_close(groups);
    }
    issue_order(&mut out);
    out
}

/// Cuts `damage` down to the pixels that differ between `old` and `new` (the canvas as
/// on the panel before a redraw, and after it): each damage rect becomes the bands of
/// changed pixel rows inside it, each as wide as its changed columns. Bands less than
/// [`BAND_JOIN`] px apart join. A rect with no change is dropped. Canvases of different
/// sizes return `damage` as it is.
pub fn changed_bands(old: &Canvas, new: &Canvas, damage: Vec<Damage>) -> Vec<Damage> {
    if old.width != new.width || old.pixels.len() != new.pixels.len() {
        return damage;
    }
    let w = new.width;
    let mut out = Vec::new();
    for d in damage {
        let r = d.rect.intersect(&new.bounds());
        // (top, last changed row, min x, max x)
        let mut band: Option<(i32, i32, i32, i32)> = None;
        let n = r.w as usize;
        let push = |out: &mut Vec<Damage>, (top, last, x0, x1): (i32, i32, i32, i32)| {
            out.push(Damage { rect: Rect::new(x0, top, (x1 - x0 + 1) as u32, (last - top + 1) as u32), class: d.class });
        };
        for y in r.y..r.bottom() {
            let i = y as usize * w + r.x as usize;
            let (a, b) = (&old.pixels[i..i + n], &new.pixels[i..i + n]);
            // Most rows of a frame are unchanged: one slice compare (memcmp) settles them.
            if a == b {
                continue;
            }
            let Some(first) = a.iter().zip(b).position(|(p, q)| p != q) else { continue };
            let last = a.iter().zip(b).rposition(|(p, q)| p != q).unwrap_or(first);
            let (x0, x1) = (r.x + first as i32, r.x + last as i32);
            band = match band {
                Some((top, prev, bx0, bx1)) if y - prev <= BAND_JOIN => Some((top, y, bx0.min(x0), bx1.max(x1))),
                Some(done) => {
                    push(&mut out, done);
                    Some((y, y, x0, x1))
                }
                None => Some((y, y, x0, x1)),
            };
        }
        if let Some(done) = band {
            push(&mut out, done);
        }
    }
    out
}

/// Changed pixel rows at most this far apart are one band in [`changed_bands`].
pub const BAND_JOIN: i32 = 16;

/// True when `rect` of `canvas` holds only black and white (values below 0x20 or above
/// 0xDF), checked on every 4th row and column. DU shows such content without loss; gray
/// text anti-aliasing or a `light` 0xDD fill would be crushed to 2 levels.
pub fn is_two_level(canvas: &Canvas, rect: Rect) -> bool {
    let r = rect.intersect(&canvas.bounds());
    let mut y = r.y;
    while y < r.bottom() {
        let row = y as usize * canvas.width;
        let mut x = r.x;
        while x < r.right() {
            let v = canvas.pixels[row + x as usize];
            if (0x20..=0xDF).contains(&v) {
                return false;
            }
            x += 4;
        }
        y += 4;
    }
    true
}

/// What changed on screen. [`RefreshScheduler::class_for`] picks the refresh class, so
/// screens state the change and the policy lives in one place.
///
/// | change | class |
/// |---|---|
/// | page turn, jump (TOC, %, link, search hit) | REAGL; GC16 every N turns, on ghost debt or on an image page |
/// | relayout (font, margins) | REAGL |
/// | overlay open or replace, button release, status line, highlight saved | GL16 |
/// | overlay close (page text under it) | REAGL |
/// | full-screen overlay close | REAGL, adds ghost debt (no flash of its own) |
/// | press feedback | DU on 2-level content, else GL16 |
/// | key press, slider drag, selection drag, toast show | DU |
/// | key release / text edit while typing fast | DU; GL16 when idle |
/// | slider lift, selection lift, toast hide | GL16 (REAGL over the page) |
/// | pop to a screen | GL16 (REAGL over the page); GC16 when the popped screen was full-screen |
/// | settings page or large sheet shown whole (opened, another page or tab) | REAGL (no flash) |
/// | popup, menu, dialog, keyboard over a screen that repaints all (library) | GL16 of the changed pixels ([`trim_to_changes`]) |
/// | pop of a panel screen (control center) | GL16 of the changed pixels (the app loop turns the screen's GC16 into GL16) |
/// | screen push, night toggle, rotation, clear ghosting | GC16 (restarts the counts) |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Change {
    /// The next page of the book.
    PageTurn,
    /// A jump to another place in the book (counts as a page turn).
    Jump,
    /// The page was laid out again (font, margins): page text, no flash.
    Relayout,
    /// An overlay (menu, sheet, popup) appears or is replaced by another.
    OverlayOpen,
    /// An overlay goes away; the page comes back in its rectangle.
    OverlayClose,
    /// A full-screen overlay (go to, search, plugin viewer) goes away.
    FullOverlayClose,
    /// Touch feedback on a button, tile or row. `two_level`: the pressed state holds
    /// only black and white ([`is_two_level`]), so DU shows it exactly.
    Press {
        /// The pressed content is black and white only.
        two_level: bool,
    },
    /// A button released, or an action changed its label.
    Release,
    /// A keyboard key goes down.
    KeyPress,
    /// A keyboard key comes up. `typing`: another key came within the idle time
    /// (about 600 ms); one GL16 of the keyboard follows when typing stops.
    KeyRelease {
        /// Typing fast.
        typing: bool,
    },
    /// A text field changed. `typing` as for [`Change::KeyRelease`].
    TextEdit {
        /// Typing fast.
        typing: bool,
    },
    /// A slider moves under the finger.
    SliderDrag,
    /// The finger left the slider: settle with GL16.
    SliderLift,
    /// A text selection grows under the finger.
    SelectionDrag,
    /// The finger left the selection (its popup opens): restores anti-aliasing.
    SelectionLift,
    /// A highlight or bookmark was saved or removed.
    Mark,
    /// A status line, header or footer strip changed.
    Status,
    /// A toast appears (a black pill reads fine in 2 levels).
    ToastShow,
    /// A toast goes away. `over_page`: page text comes back under it.
    ToastHide {
        /// Page text under the toast.
        over_page: bool,
    },
    /// A new screen appears.
    ScreenEnter,
    /// A settings page or a large reader sheet (Aa) is shown whole: opened, or another
    /// page or tab. REAGL drives every pixel without a
    /// flash (and is never promoted to a full-screen GC16 like a large GL16; see
    /// [`no_flash`]).
    LargePanel,
    /// The screen above was popped; its area comes back.
    Pop {
        /// The popped screen covered the whole screen.
        full_screen: bool,
        /// Page text comes back (the reader).
        over_page: bool,
    },
    /// Night mode on or off.
    NightToggle,
    /// The screen rotated.
    Rotation,
    /// The user asked to clear ghosting.
    ClearGhosting,
}

/// Decides the class of page turns and UI updates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshScheduler {
    /// Flash every this many page turns (0 = never).
    pub flash_every: u32,
    /// Flash every this many page turns in night mode (`None` = as `flash_every`).
    pub night_flash_every: Option<u32>,
    /// Flash after this many partial UI updates (0 = never).
    pub ui_flash_every: u32,
    turns: u32,
    force: bool,
    ui_updates: u32,
    ui_force: bool,
    night: bool,
    ghost_debt: u32,
    image_flash: bool,
}

impl RefreshScheduler {
    /// A scheduler; the prototype used `flash_every = 8`. UI updates flash only on
    /// request (`ui_flash_every = 0`): a periodic flash while typing is worse than a
    /// little DU ghosting; screens request one when a large overlay closes.
    pub fn new(flash_every: u32) -> Self {
        RefreshScheduler { flash_every, ..Default::default() }
    }

    /// Class for the next page turn: [`RefreshClass::Reagl`], or [`RefreshClass::Gc16`]
    /// on every `flash_every`-th turn or after [`RefreshScheduler::request_flash`].
    ///
    /// Also flashes when ghost debt is due ([`RefreshScheduler::add_ghost_debt`]) or an
    /// image page was announced ([`RefreshScheduler::set_image_flash`]). In night mode
    /// ([`RefreshScheduler::set_night`]) the count is `night_flash_every`.
    pub fn page_turn(&mut self) -> RefreshClass {
        self.turns += 1;
        let every = self.every();
        let debt_due = every > 0 && self.ghost_debt >= (every / 2).max(1);
        if self.force || self.image_flash || debt_due || (every > 0 && self.turns >= every) {
            self.reset_page_counts();
            RefreshClass::Gc16
        } else {
            RefreshClass::Reagl
        }
    }

    fn every(&self) -> u32 {
        if self.night {
            self.night_flash_every.unwrap_or(self.flash_every)
        } else {
            self.flash_every
        }
    }

    /// Tells the scheduler whether night mode is on (selects `night_flash_every`).
    pub fn set_night(&mut self, on: bool) {
        self.night = on;
    }

    /// Adds ghost debt: closing a full-screen overlay adds 2, a DU animation turn 1.
    /// When the debt reaches half of the flash count, the next page turn flashes. With
    /// `flash_every = 0` debt never flashes.
    pub fn add_ghost_debt(&mut self, n: u32) {
        self.ghost_debt = self.ghost_debt.saturating_add(n);
    }

    /// Current ghost debt (diagnostics and tests).
    pub fn ghost_debt(&self) -> u32 {
        self.ghost_debt
    }

    /// Makes the next page turn flash because the page holds images (or its image
    /// coverage changed a lot); `false` withdraws it.
    pub fn set_image_flash(&mut self, on: bool) {
        self.image_flash = on;
    }

    /// The refresh class for a [`Change`]; page turns and jumps count toward the
    /// periodic flash, a full-screen overlay close adds ghost debt, and the GC16 changes
    /// restart the counts. See the table at [`Change`].
    pub fn class_for(&mut self, change: Change) -> RefreshClass {
        use RefreshClass::*;
        match change {
            Change::PageTurn | Change::Jump => self.page_turn(),
            Change::Relayout | Change::OverlayClose => Reagl,
            Change::FullOverlayClose => {
                self.add_ghost_debt(2);
                Reagl
            }
            Change::Press { two_level: true } => Du,
            Change::Press { two_level: false } => Gl16,
            Change::KeyPress | Change::SliderDrag | Change::SelectionDrag | Change::ToastShow => Du,
            Change::KeyRelease { typing: true } | Change::TextEdit { typing: true } => Du,
            Change::KeyRelease { typing: false } | Change::TextEdit { typing: false } => Gl16,
            Change::OverlayOpen | Change::Release | Change::SliderLift | Change::SelectionLift => Gl16,
            Change::Mark | Change::Status => Gl16,
            Change::ToastHide { over_page: true } => Reagl,
            Change::ToastHide { over_page: false } => Gl16,
            Change::Pop { full_screen: true, .. } => {
                self.reset_page_counts();
                Gc16
            }
            Change::Pop { full_screen: false, over_page: true } => Reagl,
            Change::Pop { full_screen: false, over_page: false } => Gl16,
            Change::LargePanel => Reagl,
            Change::ScreenEnter | Change::NightToggle | Change::Rotation | Change::ClearGhosting => {
                self.reset_page_counts();
                Gc16
            }
        }
    }

    /// [`RefreshScheduler::class_for`] as a [`Damage`] of `rect`.
    pub fn damage(&mut self, change: Change, rect: Rect) -> Damage {
        Damage { rect, class: self.class_for(change) }
    }

    fn reset_page_counts(&mut self) {
        self.turns = 0;
        self.force = false;
        self.ghost_debt = 0;
        self.image_flash = false;
    }

    /// Makes the next page turn flash.
    pub fn request_flash(&mut self) {
        self.force = true;
    }

    /// Makes the next UI update flash.
    pub fn request_ui_flash(&mut self) {
        self.ui_force = true;
    }

    /// Restarts the counts (after a full-screen flash done for another reason).
    pub fn reset(&mut self) {
        self.reset_page_counts();
        self.ui_updates = 0;
        self.ui_force = false;
    }

    /// Applies the UI policy to merged damage: as reported, or one GC16 of the bounding
    /// box when a flash is due. A GC16 in the damage restarts the count.
    pub fn ui(&mut self, damage: Vec<Damage>) -> Vec<Damage> {
        if damage.is_empty() {
            return damage;
        }
        if damage.iter().any(|d| d.class == RefreshClass::Gc16) {
            self.ui_updates = 0;
            self.ui_force = false;
            return damage;
        }
        let partial = damage.iter().filter(|d| matches!(d.class, RefreshClass::Du | RefreshClass::Gl16)).count() as u32;
        self.ui_updates += partial;
        if self.ui_force || (self.ui_flash_every > 0 && self.ui_updates > self.ui_flash_every) {
            self.ui_updates = 0;
            self.ui_force = false;
            return vec![Damage { rect: bounds(&damage), class: RefreshClass::Gc16 }];
        }
        damage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_bands_keep_only_changed_pixels() {
        let old = Canvas::new(100, 200);
        let mut new = old.clone();
        new.fill_rect(10, 20, 5, 4, 0); // rows 20..24, x 10..15
        new.fill_rect(40, 30, 5, 2, 0); // joins the first band (gap < 16)
        new.fill_rect(70, 150, 3, 3, 0); // a second band
        let full = vec![Damage { rect: Rect::new(0, 0, 100, 200), class: RefreshClass::Gl16 }];
        let got = changed_bands(&old, &new, full);
        assert_eq!(got, vec![
            Damage { rect: Rect::new(10, 20, 35, 12), class: RefreshClass::Gl16 },
            Damage { rect: Rect::new(70, 150, 3, 3), class: RefreshClass::Gl16 },
        ]);
        assert!(changed_bands(&old, &old, vec![Damage { rect: Rect::new(0, 0, 100, 200), class: RefreshClass::Du }]).is_empty());
    }

    fn d(x: i32, y: i32, w: u32, h: u32, class: RefreshClass) -> Damage {
        Damage { rect: Rect::new(x, y, w, h), class }
    }

    #[test]
    fn every_eighth_turn_flashes_like_the_prototype() {
        let mut s = RefreshScheduler::new(8);
        let classes: Vec<_> = (0..16).map(|_| s.page_turn()).collect();
        assert_eq!(classes.iter().filter(|c| **c == RefreshClass::Gc16).count(), 2);
        assert_eq!(classes[7], RefreshClass::Gc16);
        assert_eq!(classes[15], RefreshClass::Gc16);
        s.request_flash();
        assert_eq!(s.page_turn(), RefreshClass::Gc16);
        assert_eq!(RefreshScheduler::new(0).page_turn(), RefreshClass::Reagl);
    }

    #[test]
    fn merging_keeps_the_strongest_class() {
        let out = merge(vec![
            d(0, 0, 10, 10, RefreshClass::Du),
            d(5, 5, 10, 10, RefreshClass::Gc16),
            d(100, 100, 5, 5, RefreshClass::Gl16),
            d(14, 14, 10, 10, RefreshClass::Gl16),
        ]);
        assert_eq!(out, vec![d(0, 0, 24, 24, RefreshClass::Gc16), d(100, 100, 5, 5, RefreshClass::Gl16)]);
        assert_eq!(bounds(&out), Rect::new(0, 0, 105, 105));
    }

    const SCREEN: Rect = Rect::new(0, 0, 1272, 1696);

    #[test]
    fn coalesce_merges_touching_rects_and_drops_slivers() {
        use RefreshClass::*;
        // Two keys that share an edge become one; a 1 px line is dropped; the part
        // outside the screen is clipped.
        let out = coalesce(
            vec![d(0, 0, 100, 50, Du), d(100, 0, 100, 50, Du), d(500, 500, 1, 300, Gl16), d(1200, 1600, 200, 200, Gl16)],
            SCREEN,
        );
        assert_eq!(out, vec![d(0, 0, 200, 50, Du), d(1200, 1600, 72, 96, Gl16)]);
        // Touching rects of different classes merge into the stronger class.
        let out = coalesce(vec![d(0, 0, 100, 50, Du), d(0, 50, 100, 50, Gl16)], SCREEN);
        assert_eq!(out, vec![d(0, 0, 100, 100, Gl16)]);
    }

    #[test]
    fn coalesce_merges_near_rects_of_one_class_only() {
        use RefreshClass::*;
        // Two GL16 rows 4 px apart: union 100x104 <= 1.3 x 2 x 100x50.
        let rows = coalesce(vec![d(0, 0, 100, 50, Gl16), d(0, 54, 100, 50, Gl16)], SCREEN);
        assert_eq!(rows, vec![d(0, 0, 100, 104, Gl16)]);
        // The same gap between a DU press and a GL16 label: no merge.
        let mixed = coalesce(vec![d(0, 0, 100, 50, Du), d(0, 54, 100, 50, Gl16)], SCREEN);
        assert_eq!(mixed, vec![d(0, 0, 100, 50, Du), d(0, 54, 100, 50, Gl16)]);
        // Far apart: no merge.
        let far = coalesce(vec![d(0, 0, 100, 50, Gl16), d(0, 400, 100, 50, Gl16)], SCREEN);
        assert_eq!(far.len(), 2);
    }

    #[test]
    fn coalesce_caps_at_four_rects_and_sorts_du_first() {
        use RefreshClass::*;
        // Seven separate rects: three DU keys, three GL16 labels, one REAGL strip.
        let list: Vec<Damage> = (0..3)
            .map(|i| d(i * 200, 800, 50, 50, Gl16))
            .chain((0..3).map(|i| d(i * 200, 0, 50, 50, Du)))
            .chain([d(0, 1600, 50, 50, Reagl)])
            .collect();
        let out = coalesce(list, SCREEN);
        assert_eq!(out, vec![d(0, 0, 450, 50, Du), d(0, 800, 450, 50, Gl16), d(0, 1600, 50, 50, Reagl)]);
        // Class boxes that overlap merge again into the stronger class.
        let crossed = vec![
            d(0, 0, 50, 50, Du),
            d(400, 400, 50, 50, Du),
            d(400, 0, 50, 50, Gl16),
            d(0, 400, 50, 50, Gl16),
            d(1000, 1000, 50, 50, Gl16),
        ];
        assert_eq!(coalesce(crossed, SCREEN), vec![d(0, 0, 1050, 1050, Gl16)]);
    }

    #[test]
    fn coalesce_promotes_large_gl16_but_not_du_or_reagl() {
        use RefreshClass::*;
        let big = d(0, 480, 1272, 1216, Gl16); // the quick-light panel: 72% of the screen
        assert_eq!(coalesce(vec![big], SCREEN), vec![d(0, 0, 1272, 1696, Gl16)]);
        // The full-screen GL16 then covers a DU press elsewhere: one update.
        assert_eq!(coalesce(vec![big, d(10, 10, 50, 50, Du)], SCREEN), vec![d(0, 0, 1272, 1696, Gl16)]);
        // Below 60%: stays.
        let half = d(0, 848, 1272, 848, Gl16);
        assert_eq!(coalesce(vec![half], SCREEN), vec![half]);
        // DU and REAGL keep their rectangle.
        let du = Damage { class: Du, ..big };
        assert_eq!(coalesce(vec![du], SCREEN), vec![du]);
        let reagl = Damage { class: Reagl, ..big };
        assert_eq!(coalesce(vec![reagl], SCREEN), vec![reagl]);
        // reduce never promotes.
        assert_eq!(reduce(vec![big], SCREEN), vec![big]);
        // A REAGL rect in the same frame keeps its own rectangle (no full-screen REAGL).
        let small = d(10, 10, 50, 50, Reagl);
        assert_eq!(coalesce(vec![big, small], SCREEN), vec![d(0, 0, 1272, 1696, Gl16), small]);
        let flash = d(10, 10, 50, 50, Gc16);
        assert_eq!(coalesce(vec![big, flash], SCREEN), vec![d(0, 0, 1272, 1696, Gl16), flash]);
        // Two separate GL16 rects that together cover 63%: one full-screen update.
        let two = vec![d(0, 0, 1272, 400, Gl16), d(0, 1000, 1272, 680, Gl16)];
        assert_eq!(coalesce(two, SCREEN), vec![d(0, 0, 1272, 1696, Gl16)]);
        // Three close GL16 rects merge into one of 87% (1.3x rule): a screen change.
        let thirds = vec![d(0, 0, 1272, 400, Gl16), d(0, 500, 1272, 400, Gl16), d(0, 1000, 1272, 472, Gl16)];
        assert_eq!(coalesce(thirds, SCREEN), vec![d(0, 0, 1272, 1696, Gc16)]);
    }

    #[test]
    fn coalesce_flashes_a_screen_change() {
        use RefreshClass::*;
        // A settings list jump: the content area below the app bar (88% of the screen).
        let list = d(0, 152, 1272, 1484, Gl16);
        assert_eq!(coalesce(vec![list], SCREEN), vec![d(0, 0, 1272, 1696, Gc16)]);
        // With a DU press and a small REAGL rect in the same frame: still one flash.
        let all = vec![list, d(10, 10, 50, 50, Du), d(1100, 10, 50, 50, Reagl)];
        assert_eq!(coalesce(all, SCREEN), vec![d(0, 0, 1272, 1696, Gc16)]);
        // A large REAGL area (page text) never flashes here.
        let page = Damage { class: Reagl, ..list };
        assert_eq!(coalesce(vec![page], SCREEN), vec![page]);
        // 79%: the non-flashing full-screen GL16.
        let most = d(0, 0, 1272, 1340, Gl16);
        assert_eq!(coalesce(vec![most], SCREEN), vec![d(0, 0, 1272, 1696, Gl16)]);
    }

    #[test]
    fn a_full_screen_repaint_for_a_popup_is_cut_to_the_popup() {
        use RefreshClass::*;
        let shown = {
            let mut c = Canvas::new(1272, 1696);
            c.fill_rect(100, 300, 900, 900, 0x88); // a cover grid
            c
        };
        // A menu opens over it: the screen repaints all and reports all as GL16.
        let mut now = shown.clone();
        now.fill_rect(620, 160, 620, 700, 255);
        now.fill_rect(640, 200, 400, 40, 0);
        let all = vec![d(0, 0, 1272, 1696, Gl16)];
        assert_eq!(coalesce(all.clone(), SCREEN), vec![d(0, 0, 1272, 1696, Gc16)], "without the copy: a flash");
        let cut = coalesce(trim_to_changes(all.clone(), SCREEN, Some(&shown), &now), SCREEN);
        // The menu's pixels that changed: its text and where it covers the grid.
        assert_eq!(cut, vec![d(620, 200, 420, 660, Gl16)], "the menu only");
        // Closing: the cover grid comes back in the menu's rectangle only.
        let back = coalesce(trim_to_changes(all.clone(), SCREEN, Some(&now), &shown), SCREEN);
        assert_eq!(back, cut);
        // Nothing known about the panel, or a frame that would not flash: unchanged.
        assert_eq!(trim_to_changes(all.clone(), SCREEN, None, &now), all);
        let small = vec![d(0, 0, 100, 100, Gl16)];
        assert_eq!(trim_to_changes(small.clone(), SCREEN, Some(&shown), &shown), small);
        // A real screen change (most pixels differ) still flashes.
        let mut other = Canvas::new(1272, 1696);
        other.clear(0);
        assert_eq!(coalesce(trim_to_changes(all.clone(), SCREEN, Some(&shown), &other), SCREEN), vec![d(0, 0, 1272, 1696, Gc16)]);
        // DU, REAGL and GC16 damage is kept as it is.
        let mixed = vec![d(0, 0, 1272, 1696, Gl16), d(0, 0, 50, 50, Du), d(0, 1600, 50, 50, Gc16)];
        let got = trim_to_changes(mixed, SCREEN, Some(&shown), &now);
        assert!(got.contains(&d(0, 0, 50, 50, Du)) && got.contains(&d(0, 1600, 50, 50, Gc16)), "{got:?}");
    }

    #[test]
    fn two_level_check_samples_gray() {
        let mut c = Canvas::new(40, 40);
        c.clear(255);
        c.fill_rect(0, 0, 20, 20, 0);
        assert!(is_two_level(&c, c.bounds()));
        c.fill_rect(20, 20, 8, 8, 0xAA);
        assert!(!is_two_level(&c, c.bounds()));
        assert!(is_two_level(&c, Rect::new(0, 0, 20, 20)));
    }

    #[test]
    fn ghost_debt_and_image_pages_flash_the_next_turn() {
        let mut s = RefreshScheduler::new(8);
        assert_eq!(s.class_for(Change::FullOverlayClose), RefreshClass::Reagl);
        assert_eq!(s.ghost_debt(), 2);
        assert_eq!(s.page_turn(), RefreshClass::Reagl, "2 < 8/2");
        s.add_ghost_debt(2);
        assert_eq!(s.page_turn(), RefreshClass::Gc16, "debt 4 >= 4");
        assert_eq!(s.ghost_debt(), 0);
        // The flash restarted the count: 7 more REAGL turns, then the 8th flashes.
        for _ in 0..7 {
            assert_eq!(s.page_turn(), RefreshClass::Reagl);
        }
        assert_eq!(s.page_turn(), RefreshClass::Gc16);
        s.set_image_flash(true);
        assert_eq!(s.class_for(Change::Jump), RefreshClass::Gc16);
        assert_eq!(s.page_turn(), RefreshClass::Reagl, "consumed");
        // flash_every = 0: debt never flashes.
        let mut never = RefreshScheduler::new(0);
        never.add_ghost_debt(100);
        assert_eq!(never.page_turn(), RefreshClass::Reagl);
    }

    #[test]
    fn night_mode_has_its_own_flash_count() {
        let mut s = RefreshScheduler::new(8);
        s.night_flash_every = Some(3);
        s.set_night(true);
        let classes: Vec<_> = (0..3).map(|_| s.page_turn()).collect();
        assert_eq!(classes, vec![RefreshClass::Reagl, RefreshClass::Reagl, RefreshClass::Gc16]);
        s.set_night(false);
        assert!((0..7).all(|_| s.page_turn() == RefreshClass::Reagl));
        assert_eq!(s.page_turn(), RefreshClass::Gc16);
    }

    #[test]
    fn change_policy_picks_classes() {
        use RefreshClass::*;
        let mut s = RefreshScheduler::new(8);
        assert_eq!(s.class_for(Change::Press { two_level: true }), Du);
        assert_eq!(s.class_for(Change::Press { two_level: false }), Gl16);
        assert_eq!(s.class_for(Change::SliderDrag), Du);
        assert_eq!(s.class_for(Change::SliderLift), Gl16);
        assert_eq!(s.class_for(Change::KeyRelease { typing: true }), Du);
        assert_eq!(s.class_for(Change::KeyRelease { typing: false }), Gl16);
        assert_eq!(s.class_for(Change::OverlayOpen), Gl16);
        assert_eq!(s.class_for(Change::OverlayClose), Reagl);
        assert_eq!(s.class_for(Change::ToastHide { over_page: false }), Gl16);
        assert_eq!(s.class_for(Change::Pop { full_screen: false, over_page: true }), Reagl);
        assert_eq!(s.class_for(Change::PageTurn), Reagl);
        // A GC16 change restarts the page count.
        assert_eq!(s.class_for(Change::NightToggle), Gc16);
        assert!((0..7).all(|_| s.page_turn() == Reagl));
        let r = Rect::new(1, 2, 30, 40);
        assert_eq!(s.damage(Change::Status, r), Damage { rect: r, class: Gl16 });
    }

    #[test]
    fn ui_updates_flash_after_many_partials_or_on_request() {
        let mut s = RefreshScheduler::new(8);
        assert_eq!(s.ui_flash_every, 0, "off by default");
        s.ui_flash_every = 3;
        let key = || vec![d(10, 10, 50, 50, RefreshClass::Du)];
        assert_eq!(s.ui(key()), key());
        assert_eq!(s.ui(key()), key());
        assert_eq!(s.ui(key()), key());
        // The fourth partial update is promoted to a GC16 of its area.
        assert_eq!(s.ui(key()), vec![d(10, 10, 50, 50, RefreshClass::Gc16)]);
        assert_eq!(s.ui(key()), key());
        s.request_ui_flash();
        let two = vec![d(0, 0, 10, 10, RefreshClass::Gl16), d(100, 0, 10, 10, RefreshClass::Du)];
        assert_eq!(s.ui(two), vec![d(0, 0, 110, 10, RefreshClass::Gc16)]);
        // A screen's own GC16 restarts the count; REAGL page content does not count.
        s.ui(vec![d(0, 0, 1, 1, RefreshClass::Gc16)]);
        for _ in 0..10 {
            assert_eq!(s.ui(vec![d(0, 0, 1, 1, RefreshClass::Reagl)])[0].class, RefreshClass::Reagl);
        }
        assert!(s.ui(Vec::new()).is_empty());
        s.reset();
        assert_eq!(s.ui(key()), key());
    }
}
