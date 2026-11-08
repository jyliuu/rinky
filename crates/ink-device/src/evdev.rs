//! Platform-independent evdev decoding: raw `input_event` records to [`TouchFrame`]s,
//! the `/proc/bus/input/devices` parser and the per-controller touch profiles.
//!
//! The Kindle backend reads 16-byte ARM32 `struct input_event` records
//! (`tv_sec: i32, tv_usec: i32, type: u16, code: u16, value: i32`) and feeds them here.
//! Keeping this free of system calls lets host unit tests replay recorded sequences,
//! including the PW12 chip's "touch-down and touch-up in the same SYN frame" taps.
//!
//! ## Kernel value cache and the "(0, 0) first touch"
//!
//! The input core drops an `ABS_MT_POSITION_*` value that equals the value cached for
//! that slot (`input_handle_abs_event`). A new contact at exactly the same raw position
//! as the previous one (any process, any time) therefore arrives as
//! `TRACKING_ID n, TOUCH_MAJOR m, SYN` without a position. A decoder that starts with
//! position (0, 0) reports such a tap at (0, 0). Recorded on the emulator
//! (`fts_ts`, two taps at 636,700 by two processes): the second tap had no
//! `ABS_MT_POSITION_X/Y`. The backend seeds the decoder with the kernel's cached slot
//! values (`EVIOCGMTSLOTS`, see [`TouchDecoder::seed_slot`]) when it opens the device,
//! after a resume and after `SYN_DROPPED`.

use crate::input::{Timestamp, TouchContact, TouchFrame, TouchPhase};
use std::time::Duration;

/// `EV_SYN`.
pub const EV_SYN: u16 = 0;
/// `EV_KEY`.
pub const EV_KEY: u16 = 1;
/// `EV_ABS`.
pub const EV_ABS: u16 = 3;
/// `SYN_REPORT`.
pub const SYN_REPORT: u16 = 0;
/// `SYN_DROPPED`: the kernel's buffer overflowed; state must be re-read.
pub const SYN_DROPPED: u16 = 3;
/// `ABS_X` (single-touch devices).
pub const ABS_X: u16 = 0x00;
/// `ABS_Y` (single-touch devices).
pub const ABS_Y: u16 = 0x01;
/// `ABS_MT_SLOT`.
pub const ABS_MT_SLOT: u16 = 0x2f;
/// `ABS_MT_POSITION_X`.
pub const ABS_MT_POSITION_X: u16 = 0x35;
/// `ABS_MT_POSITION_Y`.
pub const ABS_MT_POSITION_Y: u16 = 0x36;
/// `ABS_MT_TRACKING_ID`.
pub const ABS_MT_TRACKING_ID: u16 = 0x39;
/// `BTN_TOUCH`.
pub const BTN_TOUCH: u16 = 0x14a;
/// `KEY_POWER`.
pub const KEY_POWER: u16 = 116;
/// Size of one ARM32 `struct input_event`.
pub const EVENT_SIZE: usize = 16;
/// Number of multi-touch slots the decoder tracks.
pub const SLOTS: usize = 10;

/// One decoded `struct input_event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawEvent {
    /// Kernel timestamp.
    pub time: Timestamp,
    /// `type`.
    pub ty: u16,
    /// `code`.
    pub code: u16,
    /// `value`.
    pub value: i32,
}

impl RawEvent {
    /// Decodes one 16-byte ARM32 record (native little-endian).
    pub fn from_bytes(e: &[u8]) -> RawEvent {
        let sec = i32::from_le_bytes([e[0], e[1], e[2], e[3]]).max(0) as u64;
        let usec = i32::from_le_bytes([e[4], e[5], e[6], e[7]]).max(0) as u64;
        RawEvent {
            time: Timestamp(Duration::from_secs(sec) + Duration::from_micros(usec)),
            ty: u16::from_le_bytes([e[8], e[9]]),
            code: u16::from_le_bytes([e[10], e[11]]),
            value: i32::from_le_bytes([e[12], e[13], e[14], e[15]]),
        }
    }

    /// Encodes as a 16-byte ARM32 record (used to inject events, e.g. the power key).
    pub fn to_bytes(&self) -> [u8; EVENT_SIZE] {
        let mut b = [0u8; EVENT_SIZE];
        b[0..4].copy_from_slice(&(self.time.0.as_secs() as i32).to_le_bytes());
        b[4..8].copy_from_slice(&(self.time.0.subsec_micros() as i32).to_le_bytes());
        b[8..10].copy_from_slice(&self.ty.to_le_bytes());
        b[10..12].copy_from_slice(&self.code.to_le_bytes());
        b[12..16].copy_from_slice(&self.value.to_le_bytes());
        b
    }

    /// Parses one line of `rinky evdump` output
    /// (`NAME: MS ms type T code C value V`), for recorded test streams.
    pub fn parse_evdump(line: &str) -> Option<RawEvent> {
        let rest = line.split_once(": ").map(|(_, r)| r).unwrap_or(line);
        let w: Vec<&str> = rest.split_whitespace().collect();
        let pos = |key: &str| w.iter().position(|t| *t == key).and_then(|i| w.get(i + 1));
        let ms: u64 = w.first()?.parse().ok()?;
        Some(RawEvent {
            time: Timestamp::from_millis(ms),
            ty: pos("type")?.parse().ok()?,
            code: pos("code")?.parse().ok()?,
            value: pos("value")?.parse().ok()?,
        })
    }
}

/// One entry of `/proc/bus/input/devices`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DevInfo {
    /// `N: Name=`.
    pub name: String,
    /// `eventN` handler.
    pub event: String,
    /// Has absolute axes (a touchscreen).
    pub has_abs: bool,
    /// Reports `ABS_MT_POSITION_X` (a multi-touch screen).
    pub has_mt: bool,
    /// The `B: KEY=` bitmap as printed (see [`DevInfo::has_key`]).
    pub key_bits: String,
}

impl DevInfo {
    /// The device can send this key code.
    pub fn has_key(&self, code: u16) -> bool {
        bitmap_has(&self.key_bits, code as usize)
    }
}

/// Parses the text of `/proc/bus/input/devices` (bitmap words as an ARM32 kernel
/// prints them: 32-bit hex words, most significant first).
pub fn parse_devices(text: &str) -> Vec<DevInfo> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut d = DevInfo::default();
        for line in block.lines() {
            if let Some(n) = line.strip_prefix("N: Name=") {
                d.name = n.trim_matches('"').to_string();
            } else if let Some(h) = line.strip_prefix("H: Handlers=") {
                if let Some(ev) = h.split_whitespace().find(|t| t.starts_with("event")) {
                    d.event = ev.to_string();
                }
            } else if let Some(a) = line.strip_prefix("B: ABS=") {
                d.has_abs = a.trim() != "0";
                d.has_mt = bitmap_has(a, ABS_MT_POSITION_X as usize);
            } else if let Some(k) = line.strip_prefix("B: KEY=") {
                d.key_bits = k.trim().to_string();
            }
        }
        if !d.event.is_empty() {
            out.push(d);
        }
    }
    out
}

fn bitmap_has(words: &str, bit: usize) -> bool {
    let words: Vec<&str> = words.split_whitespace().collect();
    let idx = bit / 32;
    if idx >= words.len() {
        return false;
    }
    let w = words[words.len() - 1 - idx];
    u32::from_str_radix(w, 16).map(|v| v & (1 << (bit % 32)) != 0).unwrap_or(false)
}

/// How a touch controller reports positions and which way it is mounted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchProfile {
    /// Profile name (`pt_mt`, `fts_ts`, `cyttsp`, `generic`).
    pub name: &'static str,
    /// Device-name substrings this profile matches.
    pub device_names: &'static [&'static str],
    /// Raw units are panel pixels (no scaling), if the reported range fits the panel.
    pub pixels: bool,
    /// Axis swap and mirroring.
    pub transform: TouchTransform,
}

/// Known controllers, most specific first.
///
/// - `pt_mt`: Parade TrueTouch on the real PW12 (`/etc/upstart/pt5`, i2c `2-0024`).
///   KOReader drives it without any transform on the PW6 (`frontend/device/kindle`:
///   no `touch_switch_xy`/`touch_mirrored_x`), so positions are panel pixels.
/// - `fts_ts`: FocalTech in the emulator (FT5536G model, raw 0..1272 x 0..1696 for a
///   1272 x 1696 panel); pixels. Scaling by `max + 1` here caused the phase 0
///   1-pixel offset (tap 400,1200 logged as 399,1199).
/// - `cyttsp`: older Parade/Cypress drivers on earlier Kindles; pixels.
pub const TOUCH_PROFILES: [TouchProfile; 4] = [
    TouchProfile::upright("pt_mt", &["pt_mt"], true),
    TouchProfile::upright("fts_ts", &["fts_ts"], true),
    TouchProfile::upright("cyttsp", &["cyttsp"], true),
    TouchProfile::upright("generic", &[], false),
];

impl TouchProfile {
    /// A controller mounted upright (no swap or mirror).
    const fn upright(name: &'static str, device_names: &'static [&'static str], pixels: bool) -> Self {
        TouchProfile { name, device_names, pixels, transform: TouchTransform::NONE }
    }

    /// The profile for a device name; `generic` (scale by the reported range) otherwise.
    pub fn for_device(name: &str) -> TouchProfile {
        TOUCH_PROFILES
            .iter()
            .find(|p| p.device_names.iter().any(|n| name.contains(n)))
            .copied()
            .unwrap_or(TOUCH_PROFILES[TOUCH_PROFILES.len() - 1])
    }

    /// A profile by its name.
    pub fn by_name(name: &str) -> Option<TouchProfile> {
        TOUCH_PROFILES.iter().find(|p| p.name == name).copied()
    }
}

/// How raw touch coordinates map onto the upright screen: swap the axes first, then
/// mirror (KOReader's `touch_switch_xy`, `touch_mirrored_x`, `touch_mirrored_y`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TouchTransform {
    /// Raw x is the screen's y and the other way round.
    pub swap: bool,
    /// Flip x (after the swap).
    pub mirror_x: bool,
    /// Flip y (after the swap).
    pub mirror_y: bool,
}

impl TouchTransform {
    /// No swap, no mirror.
    pub const NONE: TouchTransform = TouchTransform { swap: false, mirror_x: false, mirror_y: false };
}

/// Parsed `RINKY_TOUCH` value: comma-separated tokens.
///
/// | Token | Meaning |
/// | --- | --- |
/// | `swap`, `mirror_x`, `mirror_y` | axis transforms ([`TouchTransform`]); any of these (or `none`) replaces the profile's transforms |
/// | `none` | no swap or mirror |
/// | `pixels` / `scale` | raw units are pixels / scale by the reported range |
/// | `profile=NAME` | use profile `pt_mt`, `fts_ts`, `cyttsp` or `generic` |
/// | `device=TEXT` | touch device whose name contains TEXT |
/// | `range=X0:X1,Y0:Y1` is not supported; use `scale` with the driver's own range |
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TouchOverride {
    /// Replacement transforms, when any transform token is given.
    pub transforms: Option<TouchTransform>,
    /// Forced coordinate mode.
    pub pixels: Option<bool>,
    /// Forced profile.
    pub profile: Option<String>,
    /// Forced device name substring.
    pub device: Option<String>,
    /// Tokens that were not understood.
    pub unknown: Vec<String>,
}

impl TouchOverride {
    /// Parses the variable's value (empty = no override).
    pub fn parse(s: &str) -> TouchOverride {
        let mut o = TouchOverride::default();
        let (mut swap, mut mx, mut my, mut any) = (false, false, false, false);
        for tok in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            match tok {
                "swap" | "swap_xy" => (swap, any) = (true, true),
                "mirror_x" => (mx, any) = (true, true),
                "mirror_y" => (my, any) = (true, true),
                "none" => any = true,
                "pixels" => o.pixels = Some(true),
                "scale" => o.pixels = Some(false),
                t => {
                    if let Some(p) = t.strip_prefix("profile=") {
                        o.profile = Some(p.to_string());
                    } else if let Some(d) = t.strip_prefix("device=") {
                        o.device = Some(d.to_string());
                    } else {
                        o.unknown.push(t.to_string());
                    }
                }
            }
        }
        if any {
            o.transforms = Some(TouchTransform { swap, mirror_x: mx, mirror_y: my });
        }
        o
    }
}

/// Picks the touchscreen: `device=` override, then a known profile name, then any
/// multi-touch device, then any device with absolute axes.
pub fn choose_touch_device<'a>(devs: &'a [DevInfo], ov: &TouchOverride) -> Option<&'a DevInfo> {
    if let Some(name) = &ov.device {
        return devs.iter().find(|d| d.name.contains(name.as_str()));
    }
    devs.iter()
        .find(|d| d.has_abs && TouchProfile::for_device(&d.name).name != "generic")
        .or_else(|| devs.iter().find(|d| d.has_mt))
        .or_else(|| devs.iter().find(|d| d.has_abs))
}

/// Maps raw touch-controller units to panel pixels (with the `RINKY_TOUCH` options).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchMapping {
    /// Raw X range (min, max) from `EVIOCGABS`.
    pub range_x: (i32, i32),
    /// Raw Y range (min, max).
    pub range_y: (i32, i32),
    /// Panel size in pixels (width, height).
    pub screen: (i32, i32),
    /// Swap raw X and Y before scaling, then mirror the result.
    pub transform: TouchTransform,
    /// Raw units are pixels: subtract the range minimum, no scaling, clamp to the panel.
    pub pixels: bool,
}

impl TouchMapping {
    /// Identity mapping for a panel of `w` x `h` whose controller reports pixels.
    pub fn identity(w: i32, h: i32) -> Self {
        TouchMapping { range_x: (0, w - 1), range_y: (0, h - 1), screen: (w, h), transform: TouchTransform::NONE, pixels: true }
    }

    /// The mapping for a controller with `profile` and the reported ranges on a
    /// `w` x `h` panel. A pixel profile falls back to scaling when the reported range
    /// does not fit the panel (more than 2 units off in either direction).
    pub fn for_profile(profile: &TouchProfile, range_x: (i32, i32), range_y: (i32, i32), w: i32, h: i32) -> Self {
        let (rx, ry) = if profile.transform.swap { (range_y, range_x) } else { (range_x, range_y) };
        let fits = |(lo, hi): (i32, i32), size: i32| (hi - lo - (size - 1)).abs() <= 2;
        TouchMapping {
            range_x,
            range_y,
            screen: (w, h),
            transform: profile.transform,
            pixels: profile.pixels && fits(rx, w) && fits(ry, h),
        }
    }

    /// Applies an `RINKY_TOUCH` override.
    pub fn with_override(mut self, ov: &TouchOverride) -> Self {
        self.transform = ov.transforms.unwrap_or(self.transform);
        self.pixels = ov.pixels.unwrap_or(self.pixels);
        self
    }

    /// Applies the comma-separated options `swap,mirror_x,mirror_y,pixels,scale`
    /// (`RINKY_TOUCH`; see [`TouchOverride`]).
    pub fn with_options(self, opts: &str) -> Self {
        self.with_override(&TouchOverride::parse(opts))
    }

    /// Raw controller position to panel pixel.
    pub fn to_screen(&self, (rx, ry): (i32, i32)) -> (i32, i32) {
        let t = self.transform;
        let (rx, ry, rangex, rangey) = if t.swap { (ry, rx, self.range_y, self.range_x) } else { (rx, ry, self.range_x, self.range_y) };
        let (w, h) = self.screen;
        let map = |v: i32, (lo, hi): (i32, i32), size: i32| -> i32 {
            // i64: the controller's reported range or a raw value near i32::MAX must not
            // overflow (a panic in debug builds, a wrong pixel in release).
            let p = if self.pixels || hi <= lo {
                i64::from(v) - i64::from(lo)
            } else {
                (i64::from(v) - i64::from(lo)) * i64::from(size) / (i64::from(hi) - i64::from(lo) + 1)
            };
            p.clamp(0, i64::from(size - 1).max(0)) as i32
        };
        let (x, y) = (map(rx, rangex, w), map(ry, rangey, h));
        (if t.mirror_x { w - 1 - x } else { x }, if t.mirror_y { h - 1 - y } else { y })
    }

    /// Short description for logs and selftest output.
    pub fn describe(&self) -> String {
        let t = self.transform;
        format!(
            "x {:?} y {:?} -> {}x{} {}{}{}{}",
            self.range_x,
            self.range_y,
            self.screen.0,
            self.screen.1,
            if self.pixels { "pixels" } else { "scaled" },
            if t.swap { " swap" } else { "" },
            if t.mirror_x { " mirror_x" } else { "" },
            if t.mirror_y { " mirror_y" } else { "" },
        )
    }
}

#[derive(Clone, Copy, Default)]
struct SlotState {
    /// Raw position.
    pos: (i32, i32),
    /// Contact is down (as of the last SYN).
    down: bool,
    /// Went down since the last SYN.
    began: bool,
    /// Went up since the last SYN.
    ended: bool,
    /// Position changed since the last SYN.
    moved: bool,
    /// Raw position when the contact ended (`TRACKING_ID -1`).
    end_pos: (i32, i32),
    /// A contact that ended in this frame before a new one started on the same slot
    /// (protocol B allows `TRACKING_ID -1` and a new id in one frame): its phase (`Up`,
    /// or `DownUp` if it also began in this frame) and raw position.
    prior: Option<(TouchPhase, (i32, i32))>,
}

/// Kernel state of one multi-touch slot, as read with `EVIOCGMTSLOTS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotSeed {
    /// `ABS_MT_TRACKING_ID` (-1 = no contact).
    pub tracking_id: i32,
    /// Raw `ABS_MT_POSITION_X`.
    pub x: i32,
    /// Raw `ABS_MT_POSITION_Y`.
    pub y: i32,
}

/// Turns raw evdev events into [`TouchFrame`]s (multi-touch protocol B, plus
/// single-touch `BTN_TOUCH`/`ABS_X`/`ABS_Y` devices).
pub struct TouchDecoder {
    mapping: TouchMapping,
    slot: usize,
    slots: [SlotState; SLOTS],
    /// The device sent `ABS_MT_TRACKING_ID` (protocol B): its `BTN_TOUCH` pointer
    /// emulation is then ignored (it describes the whole device, not the current slot).
    has_tracking: bool,
    /// The device sent `ABS_MT_POSITION_*`: its `ABS_X`/`ABS_Y` emulation is ignored.
    has_mt_pos: bool,
    /// A `TRACKING_ID -1` was seen. Until then a `BTN_TOUCH 0` ends every contact:
    /// Kobo's "snow" panels (Clara HD and BW, Forma, Libra H2O) never send the -1
    /// (KOReader `handleTouchEvSnow`).
    saw_release: bool,
    /// Between `SYN_DROPPED` and the next `SYN_REPORT`: everything is discarded.
    dropping: bool,
    /// A `SYN_DROPPED` was handled; the backend should re-seed from the kernel.
    resync: bool,
}

impl TouchDecoder {
    /// A decoder using `mapping` to convert positions.
    pub fn new(mapping: TouchMapping) -> Self {
        TouchDecoder {
            mapping,
            slot: 0,
            slots: [SlotState::default(); SLOTS],
            has_tracking: false,
            has_mt_pos: false,
            saw_release: false,
            dropping: false,
            resync: false,
        }
    }

    /// The mapping in use.
    pub fn mapping(&self) -> &TouchMapping {
        &self.mapping
    }

    /// Sets the slot the kernel will report on without an `ABS_MT_SLOT` event
    /// (`EVIOCGABS(ABS_MT_SLOT).value`).
    pub fn set_current_slot(&mut self, slot: i32) {
        self.slot = (slot.max(0) as usize).min(SLOTS - 1);
    }

    /// Seeds one slot with the kernel's cached values without producing events.
    /// A contact that we think is down but the kernel says is up gets an `Up` in the
    /// next frame; a contact that is down in the kernel but unknown here is adopted
    /// silently (its touch-down was missed).
    pub fn seed_slot(&mut self, slot: usize, seed: SlotSeed) {
        if slot >= SLOTS {
            return;
        }
        let s = &mut self.slots[slot];
        s.pos = (seed.x, seed.y);
        let kernel_down = seed.tracking_id >= 0;
        if s.down && !kernel_down {
            s.ended = true;
        } else if !s.down && kernel_down {
            s.down = true;
        }
        if kernel_down {
            self.has_tracking = true;
        }
        self.has_mt_pos = true;
    }

    /// Seeds a single-touch (`ABS_X`/`ABS_Y`) device's cached position.
    pub fn seed_single(&mut self, x: i32, y: i32) {
        self.slots[0].pos = (x, y);
    }

    /// True once after a `SYN_DROPPED` was handled: re-seed from the kernel.
    pub fn take_resync(&mut self) -> bool {
        std::mem::take(&mut self.resync)
    }

    /// Feeds one raw event; returns a frame at `SYN_REPORT` when any contact changed.
    pub fn feed(&mut self, ev: RawEvent) -> Option<TouchFrame> {
        if ev.ty == EV_SYN && ev.code == SYN_DROPPED {
            self.dropping = true;
            return None;
        }
        if self.dropping {
            if ev.ty == EV_SYN && ev.code == SYN_REPORT {
                self.dropping = false;
                self.resync = true;
                for s in &mut self.slots {
                    s.began = false;
                    s.moved = false;
                }
            }
            return None;
        }
        match (ev.ty, ev.code) {
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                self.has_tracking = true;
                if ev.value < 0 {
                    self.saw_release = true;
                }
            }
            (EV_ABS, ABS_MT_POSITION_X) | (EV_ABS, ABS_MT_POSITION_Y) => self.has_mt_pos = true,
            (EV_KEY, BTN_TOUCH) if self.has_tracking => {
                if ev.value == 0 && !self.saw_release {
                    for s in self.slots.iter_mut().filter(|s| s.down || s.began) {
                        s.ended = true;
                        s.end_pos = s.pos;
                    }
                }
                return None;
            }
            (EV_ABS, ABS_X) | (EV_ABS, ABS_Y) if self.has_mt_pos => return None,
            _ => {}
        }
        match (ev.ty, ev.code) {
            (EV_ABS, ABS_MT_SLOT) => self.slot = (ev.value.max(0) as usize).min(SLOTS - 1),
            (EV_ABS, ABS_MT_POSITION_X) | (EV_ABS, ABS_X) => {
                let s = &mut self.slots[self.slot];
                s.pos.0 = ev.value;
                s.moved = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) | (EV_ABS, ABS_Y) => {
                let s = &mut self.slots[self.slot];
                s.pos.1 = ev.value;
                s.moved = true;
            }
            (EV_ABS, ABS_MT_TRACKING_ID) | (EV_KEY, BTN_TOUCH) => {
                // BTN_TOUCH only gets here on devices without tracking ids; it means slot 0.
                let pressed = if ev.ty == EV_ABS { ev.value >= 0 } else { ev.value != 0 };
                let slot = if ev.ty == EV_KEY { 0 } else { self.slot };
                let s = &mut self.slots[slot];
                if pressed && s.ended && s.prior.is_none() {
                    // The slot's contact ended in this frame and a new one starts: close
                    // the old one where it ended, then start the new one.
                    s.prior = Some((if s.down { TouchPhase::Up } else { TouchPhase::DownUp }, s.end_pos));
                    s.down = false;
                    s.ended = false;
                    s.began = true;
                } else if pressed && !s.down && !s.began {
                    s.began = true;
                } else if !pressed && (s.down || s.began) {
                    s.ended = true;
                    s.end_pos = s.pos;
                }
            }
            (EV_SYN, SYN_REPORT) => return self.sync(ev.time),
            _ => {}
        }
        None
    }

    /// Emits a frame for transitions caused by [`TouchDecoder::seed_slot`] (lost lifts).
    pub fn flush_seeded(&mut self, time: Timestamp) -> Option<TouchFrame> {
        self.sync(time)
    }

    fn sync(&mut self, time: Timestamp) -> Option<TouchFrame> {
        let mut contacts = Vec::new();
        for (i, s) in self.slots.iter_mut().enumerate() {
            if let Some((phase, pos)) = s.prior.take() {
                let (x, y) = self.mapping.to_screen(pos);
                contacts.push(TouchContact { slot: i as u8, x, y, phase });
            }
            let phase = match (s.down, s.began, s.ended) {
                (false, true, true) => Some(TouchPhase::DownUp),
                (false, true, false) => Some(TouchPhase::Down),
                (true, _, true) => Some(TouchPhase::Up),
                (true, false, false) if s.moved => Some(TouchPhase::Move),
                _ => None,
            };
            if let Some(phase) = phase {
                let (x, y) = self.mapping.to_screen(s.pos);
                contacts.push(TouchContact { slot: i as u8, x, y, phase });
            }
            s.down = match phase {
                Some(TouchPhase::Down) => true,
                Some(TouchPhase::Up) | Some(TouchPhase::DownUp) => false,
                _ => s.down,
            };
            s.began = false;
            s.ended = false;
            s.moved = false;
        }
        (!contacts.is_empty()).then_some(TouchFrame { time, contacts })
    }
}

/// What the app has been told about each touch slot, so that it never gets a `Move`
/// or `Up` for a contact it does not have down, and every contact it has down can be
/// closed with an `Up` (when the screensaver or the USB lock screen takes the panel,
/// touches are not delivered, so a lift that comes then would be lost).
#[derive(Clone, Debug, Default)]
pub struct AppContacts {
    down: [Option<(i32, i32)>; SLOTS],
    last_time: Timestamp,
}

impl AppContacts {
    /// The part of `frame` to deliver: `Down`/`DownUp` always (closing a contact the
    /// app still has on that slot first), `Move`/`Up` only for contacts it has down.
    pub fn filter(&mut self, frame: TouchFrame) -> Option<TouchFrame> {
        let mut out = Vec::with_capacity(frame.contacts.len());
        for c in frame.contacts {
            let i = (c.slot as usize).min(SLOTS - 1);
            match c.phase {
                TouchPhase::Down | TouchPhase::DownUp => {
                    if let Some((x, y)) = self.down[i].take() {
                        out.push(TouchContact { slot: c.slot, x, y, phase: TouchPhase::Up });
                    }
                    if c.phase == TouchPhase::Down {
                        self.down[i] = Some((c.x, c.y));
                    }
                    out.push(c);
                }
                TouchPhase::Move => {
                    if self.down[i].is_some() {
                        self.down[i] = Some((c.x, c.y));
                        out.push(c);
                    }
                }
                TouchPhase::Up => {
                    if self.down[i].take().is_some() {
                        out.push(c);
                    }
                }
            }
        }
        self.last_time = frame.time;
        (!out.is_empty()).then_some(TouchFrame { time: frame.time, contacts: out })
    }

    /// An `Up` (at the last delivered position) for every contact the app has down.
    pub fn close_all(&mut self) -> Option<TouchFrame> {
        let contacts: Vec<TouchContact> = (0..SLOTS)
            .filter_map(|i| self.down[i].take().map(|(x, y)| TouchContact { slot: i as u8, x, y, phase: TouchPhase::Up }))
            .collect();
        (!contacts.is_empty()).then_some(TouchFrame { time: self.last_time, contacts })
    }

    /// Contacts the app has down.
    pub fn count(&self) -> usize {
        self.down.iter().filter(|d| d.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ms: u64, ty: u16, code: u16, value: i32) -> RawEvent {
        RawEvent { time: Timestamp::from_millis(ms), ty, code, value }
    }

    fn run(events: &[RawEvent]) -> Vec<TouchFrame> {
        let mut d = TouchDecoder::new(TouchMapping::identity(1272, 1696));
        events.iter().filter_map(|e| d.feed(*e)).collect()
    }

    #[test]
    fn snow_lift_is_btn_touch() {
        // Kobo Clara BW: tracking id 0 at every touch, never -1; BTN_TOUCH 0 lifts.
        let tap = |t: u64, x: i32| {
            [
                ev(t, EV_KEY, BTN_TOUCH, 1),
                ev(t, EV_ABS, ABS_MT_TRACKING_ID, 0),
                ev(t, EV_ABS, ABS_MT_POSITION_X, x),
                ev(t, EV_ABS, ABS_MT_POSITION_Y, 200),
                ev(t, EV_SYN, 2, 0), // SYN_MT_REPORT
                ev(t, EV_SYN, SYN_REPORT, 0),
                ev(t + 90, EV_ABS, ABS_MT_POSITION_X, x),
                ev(t + 90, EV_KEY, BTN_TOUCH, 0),
                ev(t + 90, EV_SYN, SYN_REPORT, 0),
            ]
        };
        let events: Vec<RawEvent> = tap(0, 100).into_iter().chain(tap(1000, 300)).collect();
        let phases: Vec<(TouchPhase, i32)> =
            run(&events).iter().flat_map(|f| f.contacts.iter().map(|c| (c.phase, c.x))).collect();
        assert_eq!(phases, [(TouchPhase::Down, 100), (TouchPhase::Up, 100), (TouchPhase::Down, 300), (TouchPhase::Up, 300)]);
    }

    #[test]
    fn mt_tap_down_move_up() {
        let frames = run(&[
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 5),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 100),
            ev(0, EV_ABS, ABS_MT_POSITION_Y, 200),
            ev(0, EV_SYN, SYN_REPORT, 0),
            ev(10, EV_ABS, ABS_MT_POSITION_X, 110),
            ev(10, EV_SYN, SYN_REPORT, 0),
            ev(20, EV_ABS, ABS_MT_TRACKING_ID, -1),
            ev(20, EV_SYN, SYN_REPORT, 0),
        ]);
        let phases: Vec<_> = frames.iter().map(|f| (f.contacts[0].phase, f.contacts[0].x)).collect();
        assert_eq!(phases, vec![(TouchPhase::Down, 100), (TouchPhase::Move, 110), (TouchPhase::Up, 110)]);
    }

    #[test]
    fn app_contacts_closed_at_sleep_stay_closed_until_a_new_touch() {
        let f = |t: u64, cs: &[(u8, TouchPhase, i32, i32)]| TouchFrame {
            time: Timestamp::from_millis(t),
            contacts: cs.iter().map(|&(slot, phase, x, y)| TouchContact { slot, x, y, phase }).collect(),
        };
        let mut a = AppContacts::default();
        assert!(a.close_all().is_none());
        assert_eq!(a.filter(f(0, &[(0, TouchPhase::Down, 600, 800)])), Some(f(0, &[(0, TouchPhase::Down, 600, 800)])));
        assert!(a.filter(f(10, &[(0, TouchPhase::Move, 601, 800)])).is_some());
        // Going to sleep: the held contact is closed where it was last seen.
        assert_eq!(a.close_all(), Some(f(10, &[(0, TouchPhase::Up, 601, 800)])));
        assert_eq!(a.count(), 0);
        // The finger is still down after the wake: its moves and its lift are dropped.
        assert!(a.filter(f(20, &[(0, TouchPhase::Move, 605, 800)])).is_none());
        assert!(a.filter(f(30, &[(0, TouchPhase::Up, 605, 800)])).is_none());
        // A new tap is delivered normally.
        assert_eq!(a.filter(f(40, &[(0, TouchPhase::DownUp, 300, 300)])), Some(f(40, &[(0, TouchPhase::DownUp, 300, 300)])));
        // A Down on a slot the app still has down closes the old contact first.
        a.filter(f(50, &[(1, TouchPhase::Down, 10, 10)]));
        assert_eq!(
            a.filter(f(60, &[(1, TouchPhase::Down, 20, 20)])),
            Some(f(60, &[(1, TouchPhase::Up, 10, 10), (1, TouchPhase::Down, 20, 20)]))
        );
        assert_eq!(a.count(), 1);
    }

    #[test]
    fn tap_and_new_touch_on_one_slot_in_one_frame() {
        // A whole tap and the start of the next contact in one frame: DownUp, then Down.
        let frames = run(&[
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 1),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 300),
            ev(0, EV_ABS, ABS_MT_POSITION_Y, 400),
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, -1),
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 2),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 900),
            ev(0, EV_SYN, SYN_REPORT, 0),
            ev(30, EV_ABS, ABS_MT_TRACKING_ID, -1),
            ev(30, EV_SYN, SYN_REPORT, 0),
        ]);
        let got: Vec<_> = frames.iter().flat_map(|f| f.contacts.iter().map(|c| (c.phase, c.x, c.y))).collect();
        assert_eq!(got, vec![(TouchPhase::DownUp, 300, 400), (TouchPhase::Down, 900, 400), (TouchPhase::Up, 900, 400)]);
    }

    #[test]
    fn down_and_up_in_one_frame_is_one_downup_contact() {
        let frames = run(&[
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 9),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 300),
            ev(0, EV_ABS, ABS_MT_POSITION_Y, 400),
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, -1),
            ev(0, EV_SYN, SYN_REPORT, 0),
        ]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].contacts, vec![TouchContact { slot: 0, x: 300, y: 400, phase: TouchPhase::DownUp }]);
    }

    #[test]
    fn single_touch_btn_touch_device() {
        let frames = run(&[
            ev(0, EV_KEY, BTN_TOUCH, 1),
            ev(0, EV_ABS, ABS_X, 10),
            ev(0, EV_ABS, ABS_Y, 20),
            ev(0, EV_SYN, SYN_REPORT, 0),
            ev(5, EV_KEY, BTN_TOUCH, 0),
            ev(5, EV_SYN, SYN_REPORT, 0),
        ]);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].contacts[0].phase, TouchPhase::Up);
    }

    #[test]
    fn mt_pointer_emulation_is_ignored() {
        let frames = run(&[
            ev(0, EV_ABS, ABS_MT_SLOT, 1),
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 3),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 700),
            ev(0, EV_KEY, BTN_TOUCH, 1),
            ev(0, EV_ABS, ABS_X, 1),
            ev(0, EV_SYN, SYN_REPORT, 0),
        ]);
        assert_eq!(frames[0].contacts, vec![TouchContact { slot: 1, x: 700, y: 0, phase: TouchPhase::Down }]);
    }

    #[test]
    fn protocol_a_uses_btn_touch() {
        let frames = run(&[
            ev(0, EV_KEY, BTN_TOUCH, 1),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 40),
            ev(0, EV_ABS, ABS_MT_POSITION_Y, 50),
            ev(0, EV_SYN, SYN_REPORT, 0),
            ev(9, EV_KEY, BTN_TOUCH, 0),
            ev(9, EV_SYN, SYN_REPORT, 0),
        ]);
        let phases: Vec<_> = frames.iter().map(|f| f.contacts[0].phase).collect();
        assert_eq!(phases, vec![TouchPhase::Down, TouchPhase::Up]);
    }

    #[test]
    fn two_slots() {
        let frames = run(&[
            ev(0, EV_ABS, ABS_MT_SLOT, 0),
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 1),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 10),
            ev(0, EV_ABS, ABS_MT_SLOT, 1),
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 2),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 500),
            ev(0, EV_SYN, SYN_REPORT, 0),
        ]);
        assert_eq!(frames[0].contacts.len(), 2);
        assert_eq!(frames[0].contacts[1].slot, 1);
    }

    #[test]
    fn mapping_options_and_record_bytes() {
        let m = TouchMapping::identity(100, 200).with_options("swap,mirror_x");
        assert_eq!(m.transform, TouchTransform { swap: true, mirror_x: true, mirror_y: false });
        let e = ev(1234, EV_KEY, KEY_POWER, 1);
        assert_eq!(RawEvent::from_bytes(&e.to_bytes()), e);
    }

    /// The emulator recording from `out/evidence/device/touch-repeat-raw.txt`: the
    /// second tap at 636,700 (a new `rinky evdump` process) carries no position.
    const SECOND_TAP_SAME_SPOT: &str = "\
fts_ts: 1792868250600 ms type 3 code 57 value 0
fts_ts: 1792868250600 ms type 3 code 48 value 33
fts_ts: 1792868250600 ms type 1 code 330 value 1
fts_ts: 1792868250600 ms type 0 code 0 value 0
fts_ts: 1792868250693 ms type 3 code 57 value -1
fts_ts: 1792868250693 ms type 1 code 330 value 0
fts_ts: 1792868250693 ms type 0 code 0 value 0";

    fn fts_mapping() -> TouchMapping {
        TouchMapping::for_profile(&TouchProfile::for_device("fts_ts"), (0, 1272), (0, 1696), 1272, 1696)
    }

    fn recorded(text: &str) -> Vec<RawEvent> {
        text.lines().filter_map(RawEvent::parse_evdump).collect()
    }

    #[test]
    fn recorded_repeat_tap_without_seed_lands_at_origin() {
        // This is the phase 0 bug: a fresh decoder reports the tap at (0, 0).
        let mut d = TouchDecoder::new(fts_mapping());
        let frames: Vec<_> = recorded(SECOND_TAP_SAME_SPOT).into_iter().filter_map(|e| d.feed(e)).collect();
        assert_eq!(frames[0].contacts[0], TouchContact { slot: 0, x: 0, y: 0, phase: TouchPhase::Down });
    }

    #[test]
    fn recorded_repeat_tap_with_kernel_seed_lands_where_tapped() {
        let mut d = TouchDecoder::new(fts_mapping());
        // What EVIOCGMTSLOTS returned before the tap: slot 0 idle, last position 636,700.
        d.set_current_slot(0);
        d.seed_slot(0, SlotSeed { tracking_id: -1, x: 636, y: 700 });
        assert!(d.flush_seeded(Timestamp::default()).is_none(), "seeding an idle slot must not emit events");
        let frames: Vec<_> = recorded(SECOND_TAP_SAME_SPOT).into_iter().filter_map(|e| d.feed(e)).collect();
        let got: Vec<_> = frames.iter().map(|f| (f.contacts[0].x, f.contacts[0].y, f.contacts[0].phase)).collect();
        assert_eq!(got, vec![(636, 700, TouchPhase::Down), (636, 700, TouchPhase::Up)]);
    }

    #[test]
    fn current_slot_seed_matters_when_kernel_omits_abs_mt_slot() {
        let mut d = TouchDecoder::new(TouchMapping::identity(1272, 1696));
        d.set_current_slot(1);
        d.seed_slot(1, SlotSeed { tracking_id: -1, x: 10, y: 20 });
        let frames: Vec<_> = [
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 4),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 50),
            ev(0, EV_SYN, SYN_REPORT, 0),
        ]
        .into_iter()
        .filter_map(|e| d.feed(e))
        .collect();
        assert_eq!(frames[0].contacts[0], TouchContact { slot: 1, x: 50, y: 20, phase: TouchPhase::Down });
    }

    #[test]
    fn syn_dropped_discards_until_report_and_asks_for_resync() {
        let mut d = TouchDecoder::new(TouchMapping::identity(1272, 1696));
        let frames: Vec<_> = [
            ev(0, EV_ABS, ABS_MT_TRACKING_ID, 1),
            ev(0, EV_ABS, ABS_MT_POSITION_X, 100),
            ev(0, EV_ABS, ABS_MT_POSITION_Y, 100),
            ev(0, EV_SYN, SYN_REPORT, 0),
            ev(5, EV_SYN, SYN_DROPPED, 0),
            ev(5, EV_ABS, ABS_MT_POSITION_X, 999),
            ev(5, EV_SYN, SYN_REPORT, 0),
        ]
        .into_iter()
        .filter_map(|e| d.feed(e))
        .collect();
        assert_eq!(frames.len(), 1, "events between SYN_DROPPED and SYN_REPORT are dropped");
        assert!(d.take_resync());
        assert!(!d.take_resync());
        // The kernel says the finger lifted meanwhile: the seed produces the lost Up.
        d.seed_slot(0, SlotSeed { tracking_id: -1, x: 120, y: 100 });
        let f = d.flush_seeded(Timestamp::from_millis(6)).unwrap();
        assert_eq!(f.contacts, vec![TouchContact { slot: 0, x: 120, y: 100, phase: TouchPhase::Up }]);
    }

    #[test]
    fn profiles_by_device_name() {
        assert_eq!(TouchProfile::for_device("pt_mt").name, "pt_mt");
        assert_eq!(TouchProfile::for_device("fts_ts").name, "fts_ts");
        assert_eq!(TouchProfile::for_device("cyttsp5_mt").name, "cyttsp");
        assert_eq!(TouchProfile::for_device("some_other_ts").name, "generic");
        assert!(!TouchProfile::by_name("generic").unwrap().pixels);
    }

    #[test]
    fn extreme_raw_values_and_ranges_do_not_overflow() {
        // A controller that reports a range from i32::MIN to i32::MAX, and raw values at
        // both ends: i32 arithmetic would overflow (a panic in a debug build).
        let mut m = TouchMapping::identity(1272, 1696);
        m.range_x = (i32::MIN, i32::MAX);
        m.range_y = (i32::MIN, i32::MAX);
        for v in [i32::MIN, -1, 0, 1, i32::MAX] {
            let (x, y) = m.to_screen((v, v));
            assert!((0..1272).contains(&x) && (0..1696).contains(&y), "{v}: {x},{y}");
        }
        m.pixels = true;
        let (x, _) = m.to_screen((i32::MAX, i32::MIN));
        assert_eq!(x, 1271);
    }

    #[test]
    fn fts_pixels_fix_the_one_pixel_offset() {
        // Phase 0 scaled 0..1272 onto 1272 pixels: 400 -> 399. Pixel mode keeps 400.
        let m = fts_mapping();
        assert!(m.pixels);
        assert_eq!(m.to_screen((400, 1200)), (400, 1200));
        assert_eq!(m.to_screen((1272, 1696)), (1271, 1695), "clamped to the panel");
        let scaled = m.with_options("scale");
        assert_eq!(scaled.to_screen((400, 1200)), (399, 1199));
    }

    #[test]
    fn pt_mt_pixels_when_range_fits_and_scaled_otherwise() {
        let p = TouchProfile::for_device("pt_mt");
        let m = TouchMapping::for_profile(&p, (0, 1271), (0, 1695), 1272, 1696);
        assert!(m.pixels);
        assert_eq!(m.to_screen((1271, 0)), (1271, 0));
        // A controller that reports 12-bit units is scaled even with a pixel profile.
        let m = TouchMapping::for_profile(&p, (0, 4095), (0, 4095), 1272, 1696);
        assert!(!m.pixels);
        assert_eq!(m.to_screen((2048, 2048)), (636, 848));
    }

    #[test]
    fn override_replaces_transforms_and_keeps_mode() {
        let swap_mirror_x = TouchTransform { swap: true, mirror_x: true, mirror_y: false };
        let p = TouchProfile { transform: swap_mirror_x, ..TouchProfile::for_device("pt_mt") };
        let m = TouchMapping::for_profile(&p, (0, 1695), (0, 1271), 1272, 1696);
        assert!(m.transform == swap_mirror_x && m.pixels);
        // swap: raw (y, x) -> x from raw y; mirror_x flips.
        assert_eq!(m.to_screen((100, 10)), (1271 - 10, 100));
        let o = TouchOverride::parse("mirror_y");
        let m2 = m.with_override(&o);
        assert!(m2.transform == TouchTransform { mirror_y: true, ..TouchTransform::NONE } && m2.pixels);
        assert_eq!(m.with_options("none").transform, TouchTransform::NONE);
        let o = TouchOverride::parse(" profile=fts_ts , device=pt , bogus,scale");
        assert_eq!(o.profile.as_deref(), Some("fts_ts"));
        assert_eq!(o.device.as_deref(), Some("pt"));
        assert_eq!(o.pixels, Some(false));
        assert_eq!(o.unknown, vec!["bogus".to_string()]);
        assert_eq!(o.transforms, None);
        assert_eq!(TouchOverride::parse(""), TouchOverride::default());
    }

    /// Synthetic: the real PW12 device list was not recorded (only `pt_mt` on event1 is
    /// known from the user's logs); the ABS bits are the emulator's plus pressure.
    const PT_MT_DEVICES: &str = "I: Bus=0019 Vendor=0001 Product=0001 Version=0100\nN: Name=\"bd71828-pwrkey\"\nH: Handlers=event0 ktch \nB: EV=3\n\n\
        I: Bus=0018\nN: Name=\"pt_mt\"\nH: Handlers=event1 ktch \nB: ABS=2658000 3\n\n\
        I: Bus=0018 Vendor=1949 Product=0417\nN: Name=\"gesture_tap\"\nH: Handlers=event2 ktch \nB: EV=3\n";
    const EMU_DEVICES: &str = "I: Bus=0019\nN: Name=\"bd71828-pwrkey\"\nH: Handlers=event0 ktch \nB: EV=3\n\n\
        I: Bus=0018\nN: Name=\"fts_ts\"\nH: Handlers=event1 ktch \nB: ABS=2618000 0\n\n\
        I: Bus=0018 Vendor=1949 Product=0417\nN: Name=\"gesture_tap\"\nH: Handlers=event2 ktch \nB: EV=3\n";

    #[test]
    fn key_bitmaps() {
        // An Oasis-like key device: KEY_PAGEUP (104), KEY_PAGEDOWN (109), KEY_POWER (116)
        // are bits 8, 13 and 20 of the fourth word.
        let devs = parse_devices("N: Name=\"gpio-keys\"\nH: Handlers=kbd event2\nB: EV=3\nB: KEY=102100 0 0 0\n");
        assert!(devs[0].has_key(104) && devs[0].has_key(109) && devs[0].has_key(116));
        assert!(!devs[0].has_key(105) && !devs[0].has_key(65) && !devs[0].has_key(300));
        assert!(!parse_devices(PT_MT_DEVICES)[0].has_key(116), "no KEY= line");
    }

    #[test]
    fn device_list_parsing_and_choice() {
        for (text, name) in [(PT_MT_DEVICES, "pt_mt"), (EMU_DEVICES, "fts_ts")] {
            let devs = parse_devices(text);
            assert_eq!(devs.len(), 3);
            assert!(devs[1].has_abs && devs[1].has_mt, "{name}");
            assert!(!devs[0].has_mt);
            let t = choose_touch_device(&devs, &TouchOverride::default()).unwrap();
            assert_eq!(t.name, name);
            assert_eq!(t.event, "event1");
            let forced = choose_touch_device(&devs, &TouchOverride::parse("device=gesture"));
            assert_eq!(forced.unwrap().name, "gesture_tap");
        }
        let unknown = "N: Name=\"weird_touch\"\nH: Handlers=event3\nB: ABS=1 0\n";
        let devs = parse_devices(unknown);
        assert!(devs[0].has_abs && !devs[0].has_mt);
        assert_eq!(choose_touch_device(&devs, &TouchOverride::default()).unwrap().name, "weird_touch");
    }

    #[test]
    fn evdump_lines_parse() {
        let e = RawEvent::parse_evdump("fts_ts: 1792868242704 ms type 3 code 53 value 636").unwrap();
        assert_eq!((e.ty, e.code, e.value), (3, 53, 636));
        assert!(RawEvent::parse_evdump("[DevInfo { name }]").is_none());
    }
}
