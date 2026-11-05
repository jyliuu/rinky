//! Host simulator: a `minifb` window that looks like an e-ink panel.
//!
//! - The panel is 1272 x 1696 (PW12). The window is scaled down to fit the screen
//!   (box filter), or by `SimOptions::scale`.
//! - Mouse = one finger: left button down/drag/up become touch frames in panel pixels.
//! - Keys: Right, Page Down, Space = next page; Left, Page Up = previous page;
//!   P = power; H = home; M = menu; Escape, Backspace = back; Q or closing the window = quit.
//! - E-ink look: 16 gray levels (2 for DU), optional ghosting ([`crate::eink`]).
//! - With `SimOptions::script`, input comes from the script instead of the mouse and the
//!   window closes when the script ends, so test runs never leave a window open.
//!
//! The window must live on the main thread (a macOS requirement).

use crate::canvas::Canvas;
use crate::eink::EinkPanel;
use crate::geom::{Rect, Rotation, Size};
use crate::input::{Input, InputEvent, Key, KeyEvent, Timestamp, TouchContact, TouchFrame, TouchPhase};
use crate::screen::{copy_to_panel, RefreshClass, Screen};
use crate::script::{Played, ScriptPlayer, Step};
use crate::services::{SystemClock, Unsupported};
use crate::{Device, DeviceError, DeviceInfo, Result};
use minifb::{KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Options for [`open`].
#[derive(Clone, Debug, Default)]
pub struct SimOptions {
    /// Panel size; default 1272 x 1696.
    pub panel: Option<Size>,
    /// Window scale (1.0 = one window pixel per panel pixel); default fits the screen.
    pub scale: Option<f32>,
    /// Simulate ghosting.
    pub ghosting: bool,
    /// Scripted input instead of the mouse; the run ends with the script.
    pub script: Option<Vec<Step>>,
    /// Directory for `shot NAME` script steps.
    pub shots_dir: Option<PathBuf>,
    /// Pause between scripted events so the window can be watched (default 150 ms).
    pub script_delay: Option<Duration>,
}

struct Shared {
    window: Window,
    fb: Canvas,
    panel: EinkPanel,
    rotation: Rotation,
    night: bool,
    scale: f32,
    win_size: (usize, usize),
    buffer: Vec<u32>,
    shots_dir: Option<PathBuf>,
}

impl Shared {
    /// Box-filters the panel into the window buffer and shows it.
    fn present(&mut self) -> Result<()> {
        let (ww, wh) = self.win_size;
        let src = &self.panel.pixels;
        let inv = 1.0 / self.scale;
        for oy in 0..wh {
            let y0 = (oy as f32 * inv) as usize;
            let y1 = (((oy + 1) as f32 * inv) as usize).clamp(y0 + 1, src.height);
            for ox in 0..ww {
                let x0 = (ox as f32 * inv) as usize;
                let x1 = (((ox + 1) as f32 * inv) as usize).clamp(x0 + 1, src.width);
                let mut sum = 0u32;
                for y in y0..y1 {
                    for x in x0..x1 {
                        sum += src.pixels[y * src.width + x] as u32;
                    }
                }
                let v = sum / ((y1 - y0) * (x1 - x0)) as u32;
                // Slightly warm paper tone, like the real panel.
                let (r, g, b) = (v * 245 / 255, v * 243 / 255, v * 235 / 255);
                self.buffer[oy * ww + ox] = (r << 16) | (g << 8) | b;
            }
        }
        self.window
            .update_with_buffer(&self.buffer, ww, wh)
            .map_err(|e| DeviceError::Backend(format!("minifb: {e}")))
    }
}

impl Shared {
    /// Writes what the window shows (scaled, paper-tinted RGB) as a PNG.
    fn save_window(&self, path: &std::path::Path) -> Result<()> {
        let (w, h) = self.win_size;
        let mut rgb = Vec::with_capacity(w * h * 3);
        for px in &self.buffer {
            rgb.extend_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, *px as u8]);
        }
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut enc = png::Encoder::new(file, w as u32, h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().map_err(std::io::Error::other)?;
        wr.write_image_data(&rgb).map_err(std::io::Error::other)?;
        Ok(())
    }
}

struct SimScreen(Rc<RefCell<Shared>>);

impl Screen for SimScreen {
    fn panel_size(&self) -> Size {
        self.0.borrow().fb.size()
    }

    fn rotation(&self) -> Rotation {
        self.0.borrow().rotation
    }

    fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
        self.0.borrow_mut().rotation = rotation;
        Ok(())
    }

    fn night_mode(&self) -> bool {
        self.0.borrow().night
    }

    fn set_night_mode(&mut self, on: bool) -> Result<()> {
        self.0.borrow_mut().night = on;
        Ok(())
    }

    fn update(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass) -> Result<()> {
        let mut s = self.0.borrow_mut();
        let s = &mut *s;
        let panel = s.fb.size();
        let stride = s.fb.width;
        let changed = copy_to_panel(canvas, rect, s.rotation, s.night, panel, &mut s.fb.pixels, stride);
        if class == RefreshClass::Gc16 && changed == s.fb.bounds() {
            // Show the black flash of a full GC16 update.
            let mut black = Canvas::new(s.fb.width, s.fb.height);
            black.clear(0);
            std::mem::swap(&mut s.panel.pixels, &mut black);
            s.present()?;
            std::thread::sleep(Duration::from_millis(60));
            std::mem::swap(&mut s.panel.pixels, &mut black);
        }
        s.panel.apply(&s.fb, changed, class);
        log::info!("sim update {}: {changed:?} {class:?}", s.panel.updates);
        s.present()
    }

    fn refresh_all(&mut self) -> Result<()> {
        let mut s = self.0.borrow_mut();
        let s = &mut *s;
        let r = s.fb.bounds();
        s.panel.apply(&s.fb, r, RefreshClass::Gc16);
        s.present()
    }

    fn screenshot(&self) -> Result<Canvas> {
        Ok(self.0.borrow().panel.pixels.clone())
    }
}

struct SimInput {
    shared: Rc<RefCell<Shared>>,
    start: Instant,
    queue: VecDeque<InputEvent>,
    mouse_down: Option<(i32, i32)>,
    player: Option<ScriptPlayer>,
    script_delay: Duration,
}

impl SimInput {
    fn now(&self) -> Timestamp {
        Timestamp(self.start.elapsed())
    }

    fn poll_window(&mut self) {
        let time = self.now();
        let mut s = self.shared.borrow_mut();
        s.window.update();
        if !s.window.is_open() {
            self.queue.push_back(InputEvent::Quit);
            return;
        }
        if self.player.is_some() {
            return;
        }
        let scale = s.scale;
        let pos = s.window.get_mouse_pos(MouseMode::Clamp).map(|(x, y)| ((x / scale) as i32, (y / scale) as i32));
        let down = s.window.get_mouse_down(MouseButton::Left);
        let contact = |(x, y): (i32, i32), phase| TouchContact { slot: 0, x, y, phase };
        match (self.mouse_down, down, pos) {
            (None, true, Some(p)) => {
                self.mouse_down = Some(p);
                self.queue.push_back(InputEvent::Touch(TouchFrame { time, contacts: vec![contact(p, TouchPhase::Down)] }));
            }
            (Some(last), true, Some(p)) if p != last => {
                self.mouse_down = Some(p);
                self.queue.push_back(InputEvent::Touch(TouchFrame { time, contacts: vec![contact(p, TouchPhase::Move)] }));
            }
            (Some(last), false, _) => {
                self.mouse_down = None;
                let p = pos.unwrap_or(last);
                self.queue.push_back(InputEvent::Touch(TouchFrame { time, contacts: vec![contact(p, TouchPhase::Up)] }));
            }
            _ => {}
        }
        for (keys, pressed) in [(s.window.get_keys_pressed(KeyRepeat::Yes), true), (s.window.get_keys_released(), false)] {
            for k in keys {
                let key = match k {
                    minifb::Key::Right | minifb::Key::PageDown | minifb::Key::Space => Key::PageNext,
                    minifb::Key::Left | minifb::Key::PageUp => Key::PagePrev,
                    minifb::Key::P => Key::Power,
                    minifb::Key::H => Key::Home,
                    minifb::Key::M => Key::Menu,
                    minifb::Key::Escape | minifb::Key::Backspace => Key::Back,
                    minifb::Key::Q => {
                        self.queue.push_back(InputEvent::Quit);
                        continue;
                    }
                    _ => continue,
                };
                self.queue.push_back(InputEvent::Key(KeyEvent { key, pressed, time }));
            }
        }
    }
}

impl Input for SimInput {
    fn coalesce_moves(&self) -> bool {
        // The mouse in the window, not a script.
        self.player.is_none()
    }

    fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<InputEvent>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if crate::trace::stop_requested().is_some() {
                return Ok(Some(InputEvent::Quit));
            }
            self.poll_window();
            if let Some(ev) = self.queue.pop_front() {
                return Ok(Some(ev));
            }
            if let Some(player) = &mut self.player {
                std::thread::sleep(self.script_delay);
                match player.next_item() {
                    Played::Event(ev) => return Ok(Some(ev)),
                    Played::Sleep(d) => {
                        let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
                        if player.sleep(d, left) {
                            return Ok(None);
                        }
                        continue;
                    }
                    Played::Shot(name) => {
                        let s = self.shared.borrow();
                        if let Some(dir) = &s.shots_dir {
                            std::fs::create_dir_all(dir)?;
                            s.panel.pixels.save_png(&dir.join(format!("{name}.png")))?;
                            s.save_window(&dir.join(format!("{name}.window.png")))?;
                        }
                        continue;
                    }
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    }
}

#[cfg(target_os = "macos")]
fn screen_height() -> Option<u32> {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayPixelsHigh(display: u32) -> usize;
    }
    // SAFETY: both are plain CoreGraphics queries without arguments that need care.
    let h = unsafe { CGDisplayPixelsHigh(CGMainDisplayID()) };
    (h > 0).then_some(h as u32)
}

#[cfg(not(target_os = "macos"))]
fn screen_height() -> Option<u32> {
    None
}

/// Opens the simulator window. Returns an error when no window can be created
/// (for example without a display).
pub fn open(opts: SimOptions) -> Result<Device> {
    let panel = opts.panel.unwrap_or(Size::new(1272, 1696));
    let scale = opts.scale.unwrap_or_else(|| match screen_height() {
        Some(h) => ((h.saturating_sub(120)) as f32 / panel.h as f32).clamp(0.25, 1.0),
        None => 0.5,
    });
    let win_size = ((panel.w as f32 * scale) as usize, (panel.h as f32 * scale) as usize);
    let mut window = Window::new(
        "rinky sim (P power, arrows turn, Q quit)",
        win_size.0,
        win_size.1,
        WindowOptions::default(),
    )
    .map_err(|e| DeviceError::Backend(format!("minifb: {e}")))?;
    window.set_target_fps(60);
    let shared = Rc::new(RefCell::new(Shared {
        window,
        fb: Canvas::new(panel.w as usize, panel.h as usize),
        panel: EinkPanel::new(panel, opts.ghosting),
        rotation: Rotation::Portrait,
        night: false,
        scale,
        win_size,
        buffer: vec![0; win_size.0 * win_size.1],
        shots_dir: opts.shots_dir,
    }));
    shared.borrow_mut().present()?;
    let input = SimInput {
        shared: shared.clone(),
        start: Instant::now(),
        queue: VecDeque::new(),
        mouse_down: None,
        player: opts.script.as_deref().map(ScriptPlayer::new),
        script_delay: opts.script_delay.unwrap_or(Duration::from_millis(150)),
    };
    Ok(Device {
        info: DeviceInfo { backend: "sim", model: "simulated PW12 (minifb)".into(), firmware: String::new(), page_wave: true },
        screen: Box::new(SimScreen(shared)),
        input: Box::new(input),
        light: Box::new(Unsupported),
        battery: Box::new(Unsupported),
        power: Box::new(Unsupported),
        wifi: Box::new(Unsupported),
        usb: Box::new(Unsupported),
        clock: Box::new(SystemClock),
    })
}
