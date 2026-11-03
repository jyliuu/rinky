//! A copy of what the panel shows, kept by a [`Screen`] wrapper around the device
//! screen: every rectangle sent to the panel is copied from the canvas. The app loop
//! cuts a frame that would flash to the pixels that differ from it
//! ([`ink_ui::refresh::trim_to_changes`]), so a popup over a screen that repaints the
//! whole canvas is a GL16 of the popup, not a full-screen GC16.

use ink_device::{Canvas, Rect, RefreshClass, Rotation, Screen, Size};
use std::cell::RefCell;
use std::rc::Rc;

/// What the panel shows (logical coordinates); `None` while unknown: before the first
/// full-screen update, after a rotation, a night toggle or a wake.
pub type Shown = Rc<RefCell<Option<Canvas>>>;

/// The device screen, copying every update into [`Shown`].
pub struct ShownScreen {
    inner: Box<dyn Screen>,
    shown: Shown,
}

impl ShownScreen {
    /// Wraps `inner`; the copy is shared with the app loop.
    pub fn new(inner: Box<dyn Screen>, shown: Shown) -> Self {
        ShownScreen { inner, shown }
    }

    fn record(&self, canvas: &Canvas, rect: Rect) {
        let mut s = self.shown.borrow_mut();
        let r = rect.intersect(&canvas.bounds());
        let same = s.as_ref().is_some_and(|c| c.width == canvas.width && c.height == canvas.height);
        if r == canvas.bounds() {
            match s.as_mut() {
                Some(c) if same => c.pixels.copy_from_slice(&canvas.pixels),
                _ => *s = Some(canvas.clone()),
            }
            return;
        }
        // A part of a panel whose content is not known (or of another size) tells
        // nothing about the rest.
        let Some(c) = s.as_mut().filter(|_| same) else {
            *s = None;
            return;
        };
        let w = canvas.width;
        for y in r.y..r.bottom() {
            let i = y as usize * w + r.x as usize;
            c.pixels[i..i + r.w as usize].copy_from_slice(&canvas.pixels[i..i + r.w as usize]);
        }
    }

    fn forget(&self) {
        *self.shown.borrow_mut() = None;
    }
}

impl Screen for ShownScreen {
    fn panel_size(&self) -> Size {
        self.inner.panel_size()
    }
    fn rotation(&self) -> Rotation {
        self.inner.rotation()
    }
    fn set_rotation(&mut self, rotation: Rotation) -> ink_device::Result<()> {
        if rotation != self.inner.rotation() {
            self.forget();
        }
        self.inner.set_rotation(rotation)
    }
    fn size(&self) -> Size {
        self.inner.size()
    }
    fn night_mode(&self) -> bool {
        self.inner.night_mode()
    }
    fn set_night_mode(&mut self, on: bool) -> ink_device::Result<()> {
        // The panel keeps the old polarity outside the next updates.
        if on != self.inner.night_mode() {
            self.forget();
        }
        self.inner.set_night_mode(on)
    }
    fn update(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass) -> ink_device::Result<()> {
        self.record(canvas, rect);
        self.inner.update(canvas, rect, class)
    }
    fn update_wave(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass, wave: ink_device::Wave) -> ink_device::Result<()> {
        self.record(canvas, rect);
        self.inner.update_wave(canvas, rect, class, wave)
    }
    fn update_all(&mut self, canvas: &Canvas, class: RefreshClass) -> ink_device::Result<()> {
        self.record(canvas, canvas.bounds());
        self.inner.update_all(canvas, class)
    }
    fn refresh_all(&mut self) -> ink_device::Result<()> {
        self.inner.refresh_all()
    }
    fn screenshot(&self) -> ink_device::Result<Canvas> {
        self.inner.screenshot()
    }
    fn frame_done(&mut self, canvas: &Canvas) {
        self.inner.frame_done(canvas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copy_follows_updates_and_is_forgotten_when_the_panel_changes_under_it() {
        let opts = ink_device::headless::HeadlessOptions { panel: Size::new(100, 200), ..Default::default() };
        let (dev, _probe) = ink_device::headless::open(opts);
        let shown: Shown = Rc::new(RefCell::new(None));
        let mut s = ShownScreen::new(dev.screen, shown.clone());
        let mut c = Canvas::new(100, 200);
        c.fill_rect(0, 0, 10, 10, 0);
        s.update(&c, Rect::new(0, 0, 10, 10), RefreshClass::Gl16).unwrap();
        assert!(shown.borrow().is_none(), "a part of an unknown panel");
        s.update(&c, c.bounds(), RefreshClass::Gc16).unwrap();
        assert_eq!(shown.borrow().as_ref().unwrap().pixels, c.pixels);
        c.fill_rect(50, 50, 10, 10, 0);
        c.fill_rect(80, 150, 10, 10, 0);
        s.update(&c, Rect::new(50, 50, 10, 10), RefreshClass::Du).unwrap();
        let copy = shown.borrow().clone().unwrap();
        assert_eq!(copy.get(55, 55), Some(0));
        assert_eq!(copy.get(85, 155), Some(255), "not sent yet");
        s.set_night_mode(true).unwrap();
        assert!(shown.borrow().is_none());
    }
}
