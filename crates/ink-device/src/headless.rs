//! Headless backend: a simulated e-ink panel that writes PNGs, driven by a script.
//!
//! Used by tests and by `rinky sim BOOK --headless --script FILE --out DIR`.
//! Every [`Screen::update`] can be saved as `frame-NNNN.png`; the script's `shot NAME`
//! lines save `NAME.png`.

use crate::canvas::Canvas;
use crate::eink::{panel_screen_state, EinkPanel};
use crate::geom::{Rect, Rotation, Size};
use crate::input::{Input, InputEvent};
use crate::screen::{copy_to_panel, RefreshClass, Screen, Wave};
use crate::script::{Played, ScriptPlayer, Step};
use crate::services::{SystemClock, Unsupported};
use crate::{Device, DeviceInfo, Result};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Options for [`open`].
#[derive(Clone, Debug)]
pub struct HeadlessOptions {
    /// Panel size (default: PW12, 1272 x 1696).
    pub panel: Size,
    /// Simulate ghosting.
    pub ghosting: bool,
    /// Where PNGs go; `None` writes nothing.
    pub out_dir: Option<PathBuf>,
    /// Save every update as `frame-NNNN.png`.
    pub save_frames: bool,
    /// Input script.
    pub script: Vec<Step>,
}

impl Default for HeadlessOptions {
    fn default() -> Self {
        HeadlessOptions {
            panel: Size::new(1272, 1696),
            ghosting: false,
            out_dir: None,
            save_frames: false,
            script: Vec::new(),
        }
    }
}

/// One update as recorded by the headless screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateRecord {
    /// Panel rectangle.
    pub rect: Rect,
    /// Waveform class.
    pub class: RefreshClass,
    /// The page-turn wave asked for ([`Screen::update_wave`]).
    pub wave: Option<Wave>,
}

struct Shared {
    panel: EinkPanel,
    out_dir: Option<PathBuf>,
    save_frames: bool,
    log: Vec<UpdateRecord>,
    /// Frames counted by [`Screen::frame_done`].
    frames: u64,
    /// Frames whose canvas differed from the framebuffer outside every submitted
    /// update rect: (frame number, panel bounding box of those pixels, pixel count).
    uncovered: Vec<Uncovered>,
    /// What the panel should hold after a frame (reused by [`Screen::frame_done`]).
    want: Vec<u8>,
}

/// A frame whose canvas held pixels that no update rect covered (they never reached
/// the panel): see [`HeadlessProbe::uncovered`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uncovered {
    /// Frame number (1 = the first [`Screen::frame_done`]).
    pub frame: u64,
    /// Updates sent before the check (index into [`HeadlessProbe::updates`]).
    pub updates: usize,
    /// Panel bounding box of the pixels that differ.
    pub rect: Rect,
    /// Number of pixels that differ.
    pub pixels: usize,
}

impl Shared {
    fn save(&self, name: &str) -> Result<()> {
        if let Some(dir) = &self.out_dir {
            std::fs::create_dir_all(dir)?;
            self.panel.pixels.save_png(&dir.join(format!("{name}.png")))?;
        }
        Ok(())
    }
}

/// Handle to inspect a headless device from tests (panel pixels, update log).
#[derive(Clone)]
pub struct HeadlessProbe(Rc<RefCell<Shared>>);

impl HeadlessProbe {
    /// What the simulated panel shows.
    pub fn panel(&self) -> Canvas {
        self.0.borrow().panel.pixels.clone()
    }

    /// Every update so far, in order.
    pub fn updates(&self) -> Vec<UpdateRecord> {
        self.0.borrow().log.clone()
    }

    /// Frames after which the framebuffer (only ever written inside submitted update
    /// rects) differed from the app's canvas: pixels changed outside every update rect.
    /// Empty when every frame showed all it drew.
    pub fn uncovered(&self) -> Vec<Uncovered> {
        self.0.borrow().uncovered.clone()
    }

    /// Frames checked so far.
    pub fn frames(&self) -> u64 {
        self.0.borrow().frames
    }
}

struct HeadlessScreen(Rc<RefCell<Shared>>);

impl Screen for HeadlessScreen {
    panel_screen_state!();

    fn update(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass) -> Result<()> {
        let mut s = self.0.borrow_mut();
        let changed = s.panel.update(canvas, rect, class);
        s.log.push(UpdateRecord { rect: changed, class, wave: None });
        log::info!("headless update {}: {changed:?} {class:?}", s.panel.updates);
        if s.save_frames {
            s.save(&format!("frame-{:04}", s.panel.updates))?;
        }
        Ok(())
    }

    fn update_wave(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass, wave: Wave) -> Result<()> {
        self.update(canvas, rect, class)?;
        if let Some(last) = self.0.borrow_mut().log.last_mut() {
            last.wave = Some(wave);
        }
        Ok(())
    }

    fn refresh_all(&mut self) -> Result<()> {
        let mut s = self.0.borrow_mut();
        let r = s.panel.fb.bounds();
        s.panel.refresh(r, RefreshClass::Gc16);
        s.log.push(UpdateRecord { rect: r, class: RefreshClass::Gc16, wave: None });
        Ok(())
    }

    /// Compares the framebuffer with what the canvas would put there (rotation and
    /// night inversion applied): a difference is a pixel drawn outside every update
    /// rect of this and earlier frames. Recorded for [`HeadlessProbe::uncovered`].
    fn frame_done(&mut self, canvas: &Canvas) {
        let mut s = self.0.borrow_mut();
        let s = &mut *s;
        s.frames += 1;
        let (fb, size, rotation) = (&s.panel.fb, s.panel.size(), s.panel.rotation);
        if canvas.size() != rotation.logical_size(size) {
            return;
        }
        let stride = fb.width;
        s.want.resize(fb.pixels.len(), 0);
        copy_to_panel(canvas, canvas.bounds(), rotation, s.panel.night, size, &mut s.want, stride);
        if stride == 0 || s.want == fb.pixels {
            return;
        }
        let (mut n, mut x0, mut y0, mut x1, mut y1) = (0usize, usize::MAX, usize::MAX, 0, 0);
        // Whole rows are compared as slices (a memcmp even in a debug build); only rows
        // that differ are walked pixel by pixel. This runs after every frame of every
        // headless test, and a per-pixel loop over the panel dominated their run time.
        for (y, (a, b)) in s.want.chunks_exact(stride).zip(fb.pixels.chunks_exact(stride)).enumerate() {
            if a != b {
                let differs = |(p, q): (&u8, &u8)| p != q;
                let first = a.iter().zip(b).position(differs).unwrap_or(0);
                let last = a.iter().zip(b).rposition(differs).unwrap_or(0);
                n += a.iter().zip(b).filter(|&(p, q)| p != q).count();
                x0 = x0.min(first);
                y0 = y0.min(y);
                x1 = x1.max(last);
                y1 = y1.max(y);
            }
        }
        let rect = Rect::new(x0 as i32, y0 as i32, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
        log::warn!("headless frame {}: {n} pixels drawn outside every update rect, in {rect:?}", s.frames);
        s.uncovered.push(Uncovered { frame: s.frames, updates: s.log.len(), rect, pixels: n });
        if std::env::var_os("INK_COVERAGE_PANIC").is_some() {
            panic!("headless frame {}: {n} pixels drawn outside every update rect, in {rect:?}", s.frames);
        }
    }
}

struct HeadlessInput {
    shared: Rc<RefCell<Shared>>,
    player: ScriptPlayer,
}

impl Input for HeadlessInput {
    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<InputEvent>> {
        loop {
            if crate::trace::stop_requested().is_some() {
                return Ok(Some(InputEvent::Quit));
            }
            // Background work done (`crate::wake`): the loop paints once more.
            if crate::wake::take() {
                return Ok(None);
            }
            match self.player.next_item() {
                Played::Shot(name) => self.shared.borrow().save(&name)?,
                Played::Event(ev) => return Ok(Some(ev)),
                Played::Sleep(d) => {
                    if self.player.sleep(d, timeout) {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn flush(&mut self) -> Result<()> {
        // Scripted events are deliberate; never drop them.
        Ok(())
    }
}

/// Opens a headless device and a probe to inspect it.
pub fn open(opts: HeadlessOptions) -> (Device, HeadlessProbe) {
    let shared = Rc::new(RefCell::new(Shared {
        panel: EinkPanel::new(opts.panel, opts.ghosting),
        out_dir: opts.out_dir,
        save_frames: opts.save_frames,
        log: Vec::new(),
        frames: 0,
        uncovered: Vec::new(),
        want: Vec::new(),
    }));
    let device = Device {
        info: DeviceInfo { backend: "headless", model: "simulated PW12".into(), firmware: String::new(), page_wave: true },
        screen: Box::new(HeadlessScreen(shared.clone())),
        input: Box::new(HeadlessInput { shared: shared.clone(), player: ScriptPlayer::new(&opts.script) }),
        light: Box::new(Unsupported),
        battery: Box::new(Unsupported),
        power: Box::new(Unsupported),
        wifi: Box::new(Unsupported),
        usb: Box::new(Unsupported),
        clock: Box::new(SystemClock),
    };
    (device, HeadlessProbe(shared))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::parse;

    #[test]
    fn updates_and_shots() {
        let dir = std::env::temp_dir().join(format!("ink-headless-{}", std::process::id()));
        let script = parse("tap 10 10\nshot after-tap\n").unwrap();
        let (mut dev, probe) = open(HeadlessOptions {
            panel: Size::new(20, 30),
            out_dir: Some(dir.clone()),
            save_frames: true,
            script,
            ..Default::default()
        });
        let mut c = Canvas::new(20, 30);
        c.fill_rect(0, 0, 10, 10, 0);
        dev.screen.update_all(&c, RefreshClass::Gc16).unwrap();
        assert_eq!(probe.panel().get(5, 5), Some(0));
        assert!(matches!(dev.input.next_event(None).unwrap(), Some(InputEvent::Touch(_))));
        assert!(matches!(dev.input.next_event(None).unwrap(), Some(InputEvent::Touch(_))));
        assert_eq!(dev.input.next_event(None).unwrap(), Some(InputEvent::Quit));
        assert!(dir.join("frame-0001.png").exists());
        assert!(dir.join("after-tap.png").exists());
        assert_eq!(probe.updates().len(), 1);
        dev.screen.set_rotation(Rotation::Clockwise).unwrap();
        assert_eq!(dev.screen.size(), Size::new(30, 20));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pixels_outside_every_update_are_reported_with_their_box_and_count() {
        let (mut dev, probe) = open(HeadlessOptions { panel: Size::new(20, 30), ..Default::default() });
        let mut c = Canvas::new(20, 30);
        c.fill_rect(2, 3, 4, 5, 0);
        dev.screen.update(&c, Rect::new(2, 3, 4, 5), RefreshClass::Gc16).unwrap();
        dev.screen.frame_done(&c);
        assert_eq!(probe.uncovered(), vec![]);
        // Two pixels drawn but never sent, on different rows and columns.
        c.set(17, 4, 0);
        c.set(1, 25, 0);
        dev.screen.update(&c, Rect::new(2, 3, 4, 5), RefreshClass::Du).unwrap();
        dev.screen.frame_done(&c);
        assert_eq!(probe.uncovered(), vec![Uncovered { frame: 2, updates: 2, rect: Rect::new(1, 4, 17, 22), pixels: 2 }]);
    }

    #[test]
    fn frames_with_pixels_outside_every_update_are_recorded() {
        let (mut dev, probe) = open(HeadlessOptions { panel: Size::new(20, 30), ..Default::default() });
        dev.screen.set_rotation(Rotation::Clockwise).unwrap();
        let mut c = Canvas::new(30, 20);
        c.fill_rect(2, 3, 4, 5, 0);
        dev.screen.update(&c, Rect::new(2, 3, 4, 5), RefreshClass::Gl16).unwrap();
        dev.screen.frame_done(&c);
        assert!(probe.uncovered().is_empty());
        // Drawn, but only part of it sent to the panel.
        c.fill_rect(10, 10, 3, 2, 0);
        dev.screen.update(&c, Rect::new(10, 10, 1, 2), RefreshClass::Gl16).unwrap();
        dev.screen.frame_done(&c);
        let rect = Rotation::Clockwise.rect_to_panel(Rect::new(11, 10, 2, 2), Size::new(20, 30));
        assert_eq!(probe.uncovered(), vec![Uncovered { frame: 2, updates: 2, rect, pixels: 4 }]);
        assert_eq!(probe.frames(), 2);
    }
}
