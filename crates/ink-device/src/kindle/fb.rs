//! Kindle e-ink framebuffer: `/dev/fb0` mmap + the lab126 refresh ioctls.
//!
//! Four controller interfaces ([`Epdc`], picked by [`Model`]): the MediaTek one of the
//! PW5 and newer (`struct mxcfb_update_data_mtk`, 96 bytes; everything below was
//! measured on the PW12 with it), and the i.MX ones of older Kindles: Rex (PW4, 80
//! bytes), Zelda (Oasis 2 and 3, 88 bytes) and the original `mxcfb_update_data` (Voyage,
//! PW3, Oasis 1, 72 bytes). On i.MX, night mode inverts in software with the day
//! waveforms, there is no page-turn sweep and no fast mode. The sweep is also off on the
//! MTK models KOReader does not animate (`Model::swipe`). An unknown Kindle's
//! interface is found by sending a GC16 PARTIAL of an unchanged corner with each.
//!
//! Struct layouts and request numbers come from KOReader's
//! `base/ffi/mxcfb_kindle_h.lua` and `framebuffer_mxcfb.lua` / `framebuffer_linux.lua`:
//! - night mode is global: the `grayscale` field of `fb_var_screeninfo` set to
//!   `GRAYSCALE_8BIT_INVERTED` (2) with `FBIOPUT_VSCREENINFO`; the MTK driver then
//!   inverts in hardware and picks its night waveforms; page updates (REAGL) and UI
//!   updates (GL16) use the night REAGL waveform `GLKW16` (9) as a FULL update, flashes
//!   use `GCK16` (8), DU stays DU (the driver picks its night variant). GL16 PARTIAL
//!   over the inverted image left grey streaks and ghost copies of old rows on the PW12
//!   (photo 2026-11-01); `RINKY_NIGHT_UI=auto` sends `WAVEFORM_MODE_AUTO` PARTIAL
//!   instead (what KOReader's `ui` updates do), `=gl16` the old GL16 PARTIAL;
//! - "fast mode" (`MXCFB_SET_UPDATE_FLAGS_MTK`) stops the driver from silently
//!   turning partial updates into REAGL; KOReader turns it off again on exit;
//! - rotation is done in software (as KOReader does on Kindle): pixels are rotated by
//!   [`copy_to_panel`] and refresh rectangles are in panel coordinates, `vinfo.rotate`
//!   is never touched, so the framework comes back as it was.
//!
//! Update markers: FULL updates (REAGL, GC16, night waveforms) are not waited for
//! when they are sent; before pixels are written into a panel rectangle, earlier FULL
//! updates there are *completed* first ([`InFlightUpdates`]). Updates elsewhere are
//! not waited for. On the PW12 (5.19.5) a FULL REAGL takes about 200 ms, which every
//! page turn used to spend inside the frame; `RINKY_FULL_WAIT=1` waits right after
//! sending again.
//!
//! `MXCFB_WAIT_FOR_UPDATE_SUBMISSION` before overlapping writes is off by default: on
//! the PW12 each call blocked 100-130 ms (device log 2026-11-01, 133 waits), so a
//! slider or list drag spent most of every frame there. `RINKY_SUBMIT_WAIT=1`
//! turns it on (an experiment). `RINKY_WFM_AUTO=1` sends day GL16 as
//! `WAVEFORM_MODE_AUTO` (experiment E1).
//!
//! Every change of a global flag is journaled first ([`crate::journal`]) so the
//! launcher can undo it after a crash (`rinky device restore`).

use super::model::{Epdc, Model};
use crate::platform::Platform;
use crate::canvas::Canvas;
use crate::geom::{Rect, Rotation, Size};
use crate::journal::{Entry, Journal};
use crate::sysfs::SysRoot;
use crate::screen::{copy_to_panel, InFlightUpdates, RefreshClass, Screen, Waits, Wave};
use crate::{DeviceError, Result};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::time::Instant;

const FBIOGET_VSCREENINFO: u32 = 0x4600;
const FBIOPUT_VSCREENINFO: u32 = 0x4601;
const FBIOGET_FSCREENINFO: u32 = 0x4602;
const MXCFB_SEND_UPDATE_MTK: u32 = 0x4060_462e;
/// `_IOW('F', 0x2E, struct mxcfb_update_data_rex)` (80 bytes).
const MXCFB_SEND_UPDATE_REX: u32 = 0x4050_462e;
/// `_IOW('F', 0x2E, struct mxcfb_update_data_zelda)` (88 bytes).
const MXCFB_SEND_UPDATE_ZELDA: u32 = 0x4058_462e;
/// `_IOW('F', 0x2E, struct mxcfb_update_data)` (72 bytes).
const MXCFB_SEND_UPDATE: u32 = 0x4048_462e;
const MXCFB_WAIT_FOR_UPDATE_COMPLETE: u32 = 0xc008_462f;
/// Kobo `HWTCON_SEND_UPDATE`: `_IOW('F', 0x2E, struct hwtcon_update_data)` (36 bytes).
const HWTCON_SEND_UPDATE: u32 = 0x4024_462e;
const VAR_BPP: usize = 6;
/// Kobo i.MX waveform numbers (`mxcfb_kobo_h.lua`) that differ from the MTK ones.
const KOBO_WAVEFORM_GL16: u32 = 5;
const KOBO_WAVEFORM_REAGL: u32 = 6;
const KOBO_WAVEFORM_GCK16: u32 = 9;
const KOBO_WAVEFORM_GLKW16: u32 = 10;
/// Kobo i.MX `EPDC_FLAG_ENABLE_INVERSION`: hardware night mode, per update.
const KOBO_EPDC_FLAG_ENABLE_INVERSION: u32 = 1;
/// `_IOW('F', 0x37, u32)`: wait until the driver submitted the update with this marker
/// (KOReader `mxcfb_kindle_h.lua`, FBInk `wait_for_submission_kindle`).
const MXCFB_WAIT_FOR_UPDATE_SUBMISSION: u32 = 0x4004_4637;
const MXCFB_SET_UPDATE_FLAGS_MTK: u32 = 0x4004_463b;

const VAR_GRAYSCALE: usize = 7;
const VAR_ACTIVATE: usize = 21;
const VAR_XRES: usize = 0;
const VAR_YRES: usize = 1;
const VAR_ROTATE: usize = 34;
/// `GRAYSCALE_8BIT`.
pub const GRAYSCALE_8BIT: u32 = 1;
/// `GRAYSCALE_8BIT_INVERTED` (hardware night mode).
pub const GRAYSCALE_8BIT_INVERTED: u32 = 2;

const UPDATE_MODE_PARTIAL: u32 = 0;
const UPDATE_MODE_FULL: u32 = 1;
const TEMP_USE_AMBIENT: u32 = 0x1000;
/// `TEMP_USE_AUTO`, what KOReader sends with the original `mxcfb_update_data`.
const TEMP_USE_AUTO: u32 = 0x1001;
/// The original i.MX interface's REAGL waveform (`WAVEFORM_MODE_REAGL`).
pub const LEGACY_WAVEFORM_REAGL: u32 = 8;
/// The original i.MX interface's `WAVEFORM_MODE_GC16_FAST` (KOReader's `ui` mode there).
pub const LEGACY_WAVEFORM_GC16_FAST: u32 = 3;
/// MTK waveform numbers (`MTK_WAVEFORM_MODE_*`).
pub const WAVEFORM_DU: u32 = 1;
/// GC16.
pub const WAVEFORM_GC16: u32 = 2;
/// GL16.
pub const WAVEFORM_GL16: u32 = 3;
/// GLR16 (REAGL).
pub const WAVEFORM_GLR16: u32 = 4;
/// GCK16: night-mode flash.
pub const WAVEFORM_GCK16: u32 = 8;
/// GLKW16: night-mode partial (REAGL-like, needs FULL).
pub const WAVEFORM_GLKW16: u32 = 9;
/// `WAVEFORM_MODE_AUTO`: the driver picks (KOReader's `ui` mode on MTK).
pub const WAVEFORM_AUTO: u32 = 257;
const EPDC_FLAG_FORCE_MONOCHROME: u32 = 2;
/// `MTK_EPDC_FLAG_ENABLE_SWIPE`: the EPDC sweeps this one update across its rectangle
/// in `swipe_data.direction` (the Kindle's page-turn wave).
pub const EPDC_FLAG_ENABLE_SWIPE: u32 = 65536;
/// `MTK_SWIPE_DOWN`, `_UP`, `_LEFT`, `_RIGHT` (panel-native directions).
pub const SWIPE_DOWN: u32 = 0;
/// Toward panel y = 0.
pub const SWIPE_UP: u32 = 1;
/// Toward panel x = 0.
pub const SWIPE_LEFT: u32 = 2;
/// Toward the right panel edge.
pub const SWIPE_RIGHT: u32 = 3;
/// `swipe_data.steps` (as KOReader and the stock reader); the driver crashes on an area
/// narrower or lower than this, so smaller updates never swipe.
pub const SWIPE_STEPS: u32 = 12;
const UPDATE_FLAGS_FAST_MODE: u32 = 0x8000_0000;
const UPDATE_FLAGS_MODE_FAST_FLAG: u32 = 1;

fn ioctl(fd: i32, req: u32, arg: *mut libc::c_void) -> std::io::Result<()> {
    // SAFETY: the caller passes a buffer of the size encoded in `req`.
    let r = unsafe { libc::ioctl(fd, req as _, arg) };
    if r < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Waveform and update mode for a refresh class, day or night (hardware night flag).
///
/// Night: REAGL and GL16 become the night REAGL waveform GLKW16, which must be FULL
/// (KOReader `night_is_reagl`); it does not flash. GC16 becomes the night flash GCK16.
/// DU stays DU PARTIAL (the driver picks DUNM while the flag is set).
pub fn waveform_for(class: RefreshClass, night: bool) -> (u32, u32) {
    waveform_with(class, night, NightUi::Glkw16)
}

/// How night-mode GL16 updates are sent (`RINKY_NIGHT_UI`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NightUi {
    /// GLKW16 FULL (default).
    Glkw16,
    /// `WAVEFORM_MODE_AUTO` PARTIAL: the driver picks (KOReader `ui`).
    Auto,
    /// GL16 PARTIAL (v3/refresh; grey streaks on the PW12).
    Gl16,
}

impl NightUi {
    fn from_env() -> Self {
        match std::env::var("RINKY_NIGHT_UI").as_deref() {
            Ok("auto") => NightUi::Auto,
            Ok("gl16") => NightUi::Gl16,
            _ => NightUi::Glkw16,
        }
    }
}

/// [`waveform_for`] with the night GL16 choice.
pub fn waveform_with(class: RefreshClass, night: bool, night_ui: NightUi) -> (u32, u32) {
    match (class, night) {
        (RefreshClass::Gc16, false) => (WAVEFORM_GC16, UPDATE_MODE_FULL),
        (RefreshClass::Reagl, false) => (WAVEFORM_GLR16, UPDATE_MODE_FULL),
        (RefreshClass::Gl16, false) => (WAVEFORM_GL16, UPDATE_MODE_PARTIAL),
        (RefreshClass::Du, _) => (WAVEFORM_DU, UPDATE_MODE_PARTIAL),
        (RefreshClass::Gc16, true) => (WAVEFORM_GCK16, UPDATE_MODE_FULL),
        (RefreshClass::Reagl, true) => (WAVEFORM_GLKW16, UPDATE_MODE_FULL),
        (RefreshClass::Gl16, true) => match night_ui {
            NightUi::Glkw16 => (WAVEFORM_GLKW16, UPDATE_MODE_FULL),
            NightUi::Auto => (WAVEFORM_AUTO, UPDATE_MODE_PARTIAL),
            NightUi::Gl16 => (WAVEFORM_GL16, UPDATE_MODE_PARTIAL),
        },
    }
}

/// [`waveform_with`] for a controller. The i.MX ones get the day waveforms KOReader uses
/// there (night mode is software inversion on them, so `night` is false in practice).
pub fn waveform_epdc(epdc: Epdc, class: RefreshClass, night: bool, night_ui: NightUi) -> (u32, u32) {
    match epdc {
        Epdc::Mtk => waveform_with(class, night, night_ui),
        // ZELDA_GLR16 and ZELDA_GL16 have the MTK numbers (4, 3); GC16 and DU too.
        Epdc::Zelda | Epdc::Rex => waveform_with(class, false, night_ui),
        // Kobo i.MX (KOReader `refresh_kobo_mk7` and `mxc_update`): REAGL is FULL; at
        // night (inversion flag) the eclipse waveforms, GLKW16 partial and GCK16 flash.
        Epdc::KoboMxcfb => match (class, night) {
            (RefreshClass::Gc16, false) => (WAVEFORM_GC16, UPDATE_MODE_FULL),
            (RefreshClass::Gc16, true) => (KOBO_WAVEFORM_GCK16, UPDATE_MODE_FULL),
            (RefreshClass::Reagl, false) => (KOBO_WAVEFORM_REAGL, UPDATE_MODE_FULL),
            (RefreshClass::Gl16, false) => (KOBO_WAVEFORM_GL16, UPDATE_MODE_PARTIAL),
            (RefreshClass::Reagl | RefreshClass::Gl16, true) => (KOBO_WAVEFORM_GLKW16, UPDATE_MODE_PARTIAL),
            (RefreshClass::Du, _) => (WAVEFORM_DU, UPDATE_MODE_PARTIAL),
        },
        // Kobo hwtcon: MTK numbers; REAGL must be FULL (KOReader `refresh_kobo_mtk`).
        Epdc::KoboMtk => match class {
            RefreshClass::Gc16 => (WAVEFORM_GC16, UPDATE_MODE_FULL),
            RefreshClass::Reagl => (WAVEFORM_GLR16, UPDATE_MODE_FULL),
            RefreshClass::Gl16 => (WAVEFORM_GL16, UPDATE_MODE_PARTIAL),
            RefreshClass::Du => (WAVEFORM_DU, UPDATE_MODE_PARTIAL),
        },
        Epdc::Legacy => match class {
            RefreshClass::Gc16 => (WAVEFORM_GC16, UPDATE_MODE_FULL),
            RefreshClass::Reagl => (LEGACY_WAVEFORM_REAGL, UPDATE_MODE_FULL),
            RefreshClass::Gl16 => (LEGACY_WAVEFORM_GC16_FAST, UPDATE_MODE_PARTIAL),
            RefreshClass::Du => (WAVEFORM_DU, UPDATE_MODE_PARTIAL),
        },
    }
}

/// The refresh ioctl of a controller.
pub fn send_update_request(epdc: Epdc) -> u32 {
    match epdc {
        Epdc::Mtk => MXCFB_SEND_UPDATE_MTK,
        Epdc::Rex => MXCFB_SEND_UPDATE_REX,
        Epdc::Zelda => MXCFB_SEND_UPDATE_ZELDA,
        Epdc::Legacy | Epdc::KoboMxcfb => MXCFB_SEND_UPDATE,
        Epdc::KoboMtk => HWTCON_SEND_UPDATE,
    }
}

/// The refresh struct for a controller, in u32 words (the ioctl copies as many bytes
/// as its request number says: 72, 80, 88 or 96).
///
/// - `mxcfb_update_data`: [0..4] region, [4] waveform, [5] mode, [6] marker, [7]
///   hist_bw, [8] hist_gray, [9] temp, [10] flags, [11..18] alt_buffer_data.
/// - `_rex`: [0..7] as above up to the marker, [7] temp, [8] flags, [9] dither_mode,
///   [10] quant_bit, [11..18] alt_buffer_data, [18] hist_bw, [19] hist_gray; `_zelda`
///   adds [20] ts_pxp, [21] ts_epdc.
/// - `_mtk`: see [`update_data`].
///
/// History waveforms as KOReader: DU for black and white, GC16_FAST (legacy) or GC16
/// for gray, and the REAGL waveform for both when that is the update's own.
pub fn update_data_epdc(epdc: Epdc, r: Rect, waveform: u32, mode: u32, marker: u32, swipe: Option<u32>) -> [u32; 24] {
    if epdc == Epdc::Mtk {
        return update_data(r, waveform, mode, marker, swipe);
    }
    let mut d = [0u32; 24];
    if epdc == Epdc::KoboMxcfb {
        // {rect; waveform_mode, update_mode, update_marker; temp; flags; dither_mode;
        // quant_bit; alt_buffer_data}: no dithering (EPDC_FLAG_USE_DITHERING_PASSTHROUGH).
        d[0] = r.y as u32;
        d[1] = r.x as u32;
        d[2] = r.w;
        d[3] = r.h;
        d[4] = waveform;
        d[5] = mode;
        d[6] = marker;
        d[7] = TEMP_USE_AMBIENT;
        return d;
    }
    if epdc == Epdc::KoboMtk {
        // {rect; waveform_mode, update_mode, update_marker, flags; dither_mode}
        d[0] = r.y as u32;
        d[1] = r.x as u32;
        d[2] = r.w;
        d[3] = r.h;
        d[4] = waveform;
        d[5] = mode;
        d[6] = marker;
        return d;
    }
    d[0] = r.y as u32;
    d[1] = r.x as u32;
    d[2] = r.w;
    d[3] = r.h;
    d[4] = waveform;
    d[5] = mode;
    d[6] = marker;
    let flags = if waveform == WAVEFORM_DU { EPDC_FLAG_FORCE_MONOCHROME } else { 0 };
    if epdc == Epdc::Legacy {
        let (bw, gray) = match waveform {
            LEGACY_WAVEFORM_REAGL => (waveform, waveform),
            WAVEFORM_GC16 => (WAVEFORM_DU, WAVEFORM_GC16),
            _ => (WAVEFORM_DU, LEGACY_WAVEFORM_GC16_FAST),
        };
        d[7] = bw;
        d[8] = gray;
        d[9] = TEMP_USE_AUTO;
        d[10] = flags;
    } else {
        d[7] = TEMP_USE_AMBIENT;
        d[8] = flags;
        let (bw, gray) = if waveform == WAVEFORM_GLR16 { (waveform, waveform) } else { (WAVEFORM_DU, WAVEFORM_GC16) };
        d[18] = bw;
        d[19] = gray;
    }
    d
}

/// The `swipe_data.direction` for a logical wave on a rotated screen.
pub fn swipe_direction(wave: Wave, rotation: Rotation, panel: Size) -> u32 {
    match wave.panel_step(rotation, panel) {
        (-1, 0) => SWIPE_LEFT,
        (1, 0) => SWIPE_RIGHT,
        (0, -1) => SWIPE_UP,
        _ => SWIPE_DOWN,
    }
}

/// The fixed part of `struct mxcfb_update_data_mtk` (KOReader `mxcfb_kindle_h.lua`):
/// 24 u32 words, 96 bytes. [0..4] update_region top, left, width, height; [4]
/// waveform_mode; [5] update_mode; [6] update_marker; [7] temp; [8] flags; [9]
/// dither_mode; [10] quant_bit; [11..18] alt_buffer_data (phys_addr, width, height,
/// alt_update_region); [18] swipe_data.direction; [19] swipe_data.steps; [20]
/// hist_bw_waveform_mode; [21] hist_gray_waveform_mode; [22] ts_pxp; [23] ts_epdc.
pub fn update_data(r: Rect, waveform: u32, mode: u32, marker: u32, swipe: Option<u32>) -> [u32; 24] {
    let mut d = [0u32; 24];
    d[0] = r.y as u32; // top
    d[1] = r.x as u32; // left
    d[2] = r.w;
    d[3] = r.h;
    d[4] = waveform;
    d[5] = mode;
    d[6] = marker;
    d[7] = TEMP_USE_AMBIENT;
    d[8] = if waveform == WAVEFORM_DU { EPDC_FLAG_FORCE_MONOCHROME } else { 0 };
    // The driver crashes when the area is smaller than the number of steps.
    if let Some(dir) = swipe.filter(|_| r.w >= SWIPE_STEPS && r.h >= SWIPE_STEPS) {
        d[8] |= EPDC_FLAG_ENABLE_SWIPE;
        d[18] = dir;
    }
    d[19] = SWIPE_STEPS;
    let (bw, gray) = if waveform == WAVEFORM_GLR16 { (waveform, waveform) } else { (WAVEFORM_DU, WAVEFORM_GC16) };
    d[20] = bw;
    d[21] = gray;
    d
}

fn env_on(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// The mapped framebuffer.
pub struct KindleFb {
    /// The model this runs on.
    pub model: Model,
    /// Its refresh interface (probed for an unknown model).
    pub epdc: Epdc,
    /// `rotate` as the framework left it, when rinky turned the panel upright.
    original_rotate: Option<u32>,
    /// The bits per pixel before rinky switched a Kobo framebuffer (16 or 32 bpp) to 8.
    original_bpp: Option<u32>,
    /// The MTK `mdp_src_format` before rinky set `Y8` (Kobo).
    original_mdp: Option<String>,
    /// The Kobo EPDC wake file is missing or refused a write: not tried again.
    epdc_wake_missing: bool,
    file: File,
    map: *mut u8,
    map_len: usize,
    width: usize,
    height: usize,
    stride: usize,
    yoffset: usize,
    marker: u32,
    rotation: Rotation,
    /// Night mode requested.
    night: bool,
    /// The hardware grayscale flag is set by us (inversion happens in the driver).
    hw_night: bool,
    /// Use the hardware flag for night mode (falls back to software if refused).
    pub prefer_hw_night: bool,
    original_grayscale: u32,
    journal: Option<Journal>,
    /// Opened only to look (screenshots, info): no flag is changed, nothing restored.
    passive: bool,
    /// Number of updates sent (diagnostics).
    pub updates: u64,
    /// Last waveform and mode sent (diagnostics).
    pub last_waveform: (u32, u32),
    /// Updates sent and not known to be finished.
    in_flight: InFlightUpdates,
    /// Ask for `MXCFB_WAIT_FOR_UPDATE_SUBMISSION` before overlapping writes (opt-in,
    /// `RINKY_SUBMIT_WAIT=1`; turned off for good when the driver does not know the
    /// request).
    pub submit_wait: bool,
    /// A submission wait has worked once (an EINVAL after that is a stale marker).
    submit_wait_ok: bool,
    /// EINVAL answers in a row before any success.
    submit_wait_einval: u32,
    /// Do not wait for FULL updates right after sending; wait before the next write
    /// over the same rectangle instead (default; `RINKY_FULL_WAIT=1` turns it off).
    pub defer_full_wait: bool,
    /// Send day GL16 as `WAVEFORM_MODE_AUTO` (experiment E1, `RINKY_WFM_AUTO=1`).
    pub gl16_auto: bool,
    /// Night GL16 waveform (`RINKY_NIGHT_UI`).
    pub night_ui: NightUi,
}

impl KindleFb {
    /// Opens and maps `/dev/fb0`. Fails unless it is an 8 bpp framebuffer.
    pub fn open() -> Result<Self> {
        Self::open_with(Some(Journal::default()))
    }

    /// Opens without changing any driver flag (screenshots and diagnostics while
    /// another rinky process owns the screen).
    pub fn open_passive() -> Result<Self> {
        Self::open_inner(None, true)
    }

    /// Opens with a restore journal (`None` = do not journal).
    pub fn open_with(journal: Option<Journal>) -> Result<Self> {
        Self::open_inner(journal, false)
    }

    fn open_inner(journal: Option<Journal>, passive: bool) -> Result<Self> {
        let model = Model::detect(&SysRoot::system());
        let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
        let fd = file.as_raw_fd();

        let mut var = [0u32; 40];
        ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr().cast())?;
        let kobo = model.platform == Platform::Kobo;
        // A Kobo of unknown number: the driver's name tells MTK from i.MX.
        let model_epdc = if kobo && model.is_unknown() {
            let name = std::fs::read_to_string("/sys/class/graphics/fb0/name").unwrap_or_default();
            if name.contains("hwtcon") { Epdc::KoboMtk } else { Epdc::KoboMxcfb }
        } else {
            model.epdc
        };
        let mut original_rotate = None;
        if !passive {
            let was = var[VAR_ROTATE];
            match model.upright {
                // Kobo panels are mounted rotated: the one value that is upright (FBInk's
                // rotation map), whatever Nickel or the accelerometer left.
                Some(r) if r != was => {
                    if let Some(j) = &journal {
                        let _ = j.record(Entry::Fb { key: "rotate".into(), value: was as i64 });
                    }
                    match set_rotate(fd, r).and_then(|()| ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr().cast())) {
                        Ok(()) => {
                            log::info!("fb0: rotate {was} -> {r} (upright)");
                            original_rotate = Some(was);
                        }
                        Err(e) => log::warn!("fb0: rotate {r}: {e}"),
                    }
                }
                Some(_) => {}
                // An Oasis or Scribe can be left in landscape by the framework
                // (accelerometer): turn the panel upright, as rinky rotates in software.
                None if var[VAR_XRES] > var[VAR_YRES] => {
                    if let Some(j) = &journal {
                        let _ = j.record(Entry::Fb { key: "rotate".into(), value: was as i64 });
                    }
                    match make_upright(fd, &mut var) {
                        Some(r) => {
                            log::info!("fb0: was landscape (rotate {was}); now rotate {r}");
                            original_rotate = Some(was);
                        }
                        None => log::warn!("fb0: landscape (rotate {was}) and no rotate value turns it upright"),
                    }
                }
                None => {}
            }
        }
        let mut fix = [0u8; 80];
        ioctl(fd, FBIOGET_FSCREENINFO, fix.as_mut_ptr().cast())?;
        let mut original_bpp = None;
        let mut original_mdp = None;
        let bpp = var[VAR_BPP];
        // Kobos boot at 16 (i.MX) or 32 bpp (MTK): 8 bpp gray, as KOReader (`fbdepth -d 8`).
        if kobo && bpp != 8 && !passive {
            if let Some(j) = &journal {
                let _ = j.record(Entry::Fb { key: "bpp".into(), value: bpp as i64 });
            }
            if model_epdc == Epdc::KoboMtk {
                // The MTK driver resets bits_per_pixel to its MDP source format unless
                // that is switched first (FBInk `set_fbinfo`).
                original_mdp = mdp_src_format();
                set_mdp_src_format("Y8");
            }
            var[VAR_BPP] = 8;
            var[VAR_GRAYSCALE] = GRAYSCALE_8BIT;
            // red, green, blue {offset 0, length 8, msb_right 0}; no transparency.
            var[8..20].copy_from_slice(&[0, 8, 0, 0, 8, 0, 0, 8, 0, 0, 0, 0]);
            var[VAR_ACTIVATE] = 0;
            ioctl(fd, FBIOPUT_VSCREENINFO, var.as_mut_ptr().cast())?;
            ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr().cast())?;
            ioctl(fd, FBIOGET_FSCREENINFO, fix.as_mut_ptr().cast())?;
            log::info!("fb0: switched from {bpp} to {} bpp", var[VAR_BPP]);
            original_bpp = Some(bpp);
        }
        let (xres, yres, yoffset, bpp) = (var[0] as usize, var[1] as usize, var[5] as usize, var[6]);
        let smem_len = u32::from_ne_bytes([fix[20], fix[21], fix[22], fix[23]]) as usize;
        let stride = u32::from_ne_bytes([fix[44], fix[45], fix[46], fix[47]]) as usize;
        if bpp != 8 {
            return Err(DeviceError::Backend(format!("expected 8 bpp framebuffer, got {bpp}")));
        }
        log::info!(
            "fb0: {xres}x{yres} stride {stride} yoffset {yoffset} rotate {} grayscale {} smem {smem_len}",
            var[VAR_ROTATE],
            var[VAR_GRAYSCALE]
        );

        // SAFETY: mapping the framebuffer memory the driver reports.
        let map = unsafe {
            libc::mmap(std::ptr::null_mut(), smem_len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0)
        };
        if map == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error().into());
        }
        let original = var[VAR_GRAYSCALE];
        let epdc = if model.is_unknown() && !kobo && !passive { probe_epdc(fd).unwrap_or(Epdc::Mtk) } else { model_epdc };
        log::info!("model: {}; refresh interface {}", model.describe(), epdc.name());
        let fb = KindleFb {
            model,
            epdc,
            original_rotate,
            original_bpp,
            original_mdp,
            epdc_wake_missing: false,
            file,
            map: map.cast(),
            map_len: smem_len,
            width: xres,
            height: yres,
            stride,
            yoffset,
            marker: 0,
            rotation: Rotation::Portrait,
            night: false,
            hw_night: original == GRAYSCALE_8BIT_INVERTED,
            // The grayscale flag is how the MTK driver does night mode; KOReader inverts
            // per update on i.MX (EPDC_FLAG_ENABLE_INVERSION), rinky in software there.
            prefer_hw_night: matches!(epdc, Epdc::Mtk | Epdc::KoboMxcfb),
            // A previous run that died in night mode left the flag set: day is 1.
            original_grayscale: if original == GRAYSCALE_8BIT_INVERTED { GRAYSCALE_8BIT } else { original },
            journal,
            passive,
            updates: 0,
            last_waveform: (0, 0),
            in_flight: InFlightUpdates::new(),
            submit_wait: env_on("RINKY_SUBMIT_WAIT"),
            submit_wait_ok: false,
            submit_wait_einval: 0,
            defer_full_wait: !env_on("RINKY_FULL_WAIT"),
            gl16_auto: env_on("RINKY_WFM_AUTO"),
            night_ui: NightUi::from_env(),
        };
        if passive {
            return Ok(fb);
        }
        if fb.hw_night {
            log::warn!("fb0: grayscale flag was left inverted; resetting to day");
        }
        let mut fb = fb;
        if fb.hw_night {
            let _ = fb.set_grayscale(fb.original_grayscale);
            fb.hw_night = false;
        }
        // Stop the driver from silently turning partial updates into REAGL ones.
        if fb.epdc == Epdc::Mtk {
            if let Some(j) = &fb.journal {
                let _ = j.record(Entry::Fb { key: "fastmode".into(), value: 0 });
            }
            if let Err(e) = set_fast_mode(fb.file.as_raw_fd(), true) {
                log::warn!("MXCFB_SET_UPDATE_FLAGS_MTK failed: {e}");
            }
        }
        Ok(fb)
    }

    fn var(&self) -> Result<[u32; 40]> {
        let mut var = [0u32; 40];
        ioctl(self.file.as_raw_fd(), FBIOGET_VSCREENINFO, var.as_mut_ptr().cast())?;
        Ok(var)
    }

    /// The page-turn sweep is used (MTK models that take it, [`Model::swipe`]).
    pub fn page_wave(&self) -> bool {
        self.epdc == Epdc::Mtk && self.model.swipe
    }

    /// The driver's `grayscale` field now.
    pub fn grayscale(&self) -> Result<u32> {
        Ok(self.var()?[VAR_GRAYSCALE])
    }

    /// The driver's `rotate` field now (never changed by rinky).
    pub fn hw_rotate(&self) -> Result<u32> {
        Ok(self.var()?[VAR_ROTATE])
    }

    /// Sets `grayscale` with a read-modify-write of `fb_var_screeninfo`.
    pub fn set_grayscale(&mut self, value: u32) -> Result<()> {
        let mut var = self.var()?;
        var[VAR_GRAYSCALE] = value;
        var[VAR_ACTIVATE] = 0; // FB_ACTIVATE_NOW
        ioctl(self.file.as_raw_fd(), FBIOPUT_VSCREENINFO, var.as_mut_ptr().cast())
            .map_err(|e| DeviceError::Backend(format!("FBIOPUT_VSCREENINFO grayscale {value}: {e}")))?;
        let back = self.grayscale()?;
        if back != value {
            return Err(DeviceError::Backend(format!("grayscale {value} not kept (driver reports {back})")));
        }
        Ok(())
    }

    /// True when night mode is done by the driver (grayscale flag), not in software.
    pub fn hw_night(&self) -> bool {
        self.hw_night
    }

    fn visible(&mut self) -> &mut [u8] {
        let start = self.yoffset * self.stride;
        let len = (self.height * self.stride).min(self.map_len.saturating_sub(start));
        // SAFETY: start + len lies inside the mapping (checked above); the mapping lives
        // as long as self.
        unsafe { std::slice::from_raw_parts_mut(self.map.add(start), len) }
    }

    /// Waits as `w` says. Failures are logged, never returned: a missed wait costs at
    /// most a visual glitch.
    fn wait(&mut self, w: Waits) {
        let fd = self.file.as_raw_fd();
        for m in w.submission {
            if !self.submit_wait {
                break;
            }
            let t = Instant::now();
            let mut marker = m;
            match ioctl(fd, MXCFB_WAIT_FOR_UPDATE_SUBMISSION, (&mut marker as *mut u32).cast()) {
                Ok(()) => {
                    self.submit_wait_ok = true;
                    self.submit_wait_einval = 0;
                }
                Err(e) => {
                    // ENOTTY: unknown request. EINVAL: unknown request, or a marker the
                    // driver already dropped; only 3 in a row before any success turn
                    // the waits off (a failed ioctl returns at once, retrying is cheap).
                    let code = e.raw_os_error();
                    if code == Some(libc::EINVAL) && !self.submit_wait_ok {
                        self.submit_wait_einval += 1;
                    }
                    if code == Some(libc::ENOTTY) || self.submit_wait_einval >= 3 {
                        log::warn!("MXCFB_WAIT_FOR_UPDATE_SUBMISSION not supported ({e}); submission waits off");
                        self.submit_wait = false;
                    } else {
                        log::debug!("MXCFB_WAIT_FOR_UPDATE_SUBMISSION {m}: {e}");
                    }
                }
            }
            let us = t.elapsed().as_micros();
            if us > 100_000 {
                log::warn!("fb0: submission wait for marker {m} took {us} us");
            } else {
                log::debug!("fb0: submission wait marker {m} {us} us");
            }
        }
        for m in w.completion {
            if !self.model.reliable_wait {
                // The driver's wait can hang for 5 s (Libra 2): a short pause instead,
                // as KOReader's `stub_mxc_wait_for_update_complete`.
                std::thread::sleep(std::time::Duration::from_micros(2500));
                continue;
            }
            let t = Instant::now();
            let mut marker = [m, 0u32];
            if let Err(e) = ioctl(fd, MXCFB_WAIT_FOR_UPDATE_COMPLETE, marker.as_mut_ptr().cast()) {
                log::debug!("MXCFB_WAIT_FOR_UPDATE_COMPLETE {m}: {e}");
            }
            log::debug!("fb0: deferred completion wait marker {m} {} us", t.elapsed().as_micros());
        }
    }

    /// Kobo: wakes the EPDC before a refresh, as KOReader does (`power_state` on the
    /// i.MX models that have it, `fiti_power 1` on MTK). Kindles need nothing.
    fn wake_epdc(&mut self) {
        let (path, cmd) = match self.epdc {
            Epdc::KoboMxcfb => ("/sys/class/graphics/fb0/power_state", "1,0"),
            Epdc::KoboMtk => ("/proc/hwtcon/cmd", "fiti_power 1"),
            _ => return,
        };
        if !self.epdc_wake_missing {
            if let Err(e) = std::fs::write(path, cmd) {
                log::info!("fb0: {path}: {e}; not written again");
                self.epdc_wake_missing = true;
            }
        }
    }

    /// Waits for every update still in flight (before screenshots, sleep or exit).
    pub fn wait_idle(&mut self) {
        let w = self.in_flight.drain(Instant::now());
        self.wait(w);
    }

    fn send_update(&mut self, r: Rect, waveform: u32, mode: u32, swipe: Option<u32>) -> Result<()> {
        // Bogus 1-pixel regions can upset the EPDC (KOReader discards them too).
        if r.w <= 1 || r.h <= 1 {
            return Ok(());
        }
        // Normally already done by the caller before writing pixels; cheap when so.
        let w = self.in_flight.before(r, Instant::now());
        self.wait(w);
        self.marker = self.marker.wrapping_add(1).max(1);
        let swipe = swipe.filter(|_| self.page_wave());
        let mut d = update_data_epdc(self.epdc, r, waveform, mode, self.marker, swipe);
        if self.epdc == Epdc::KoboMxcfb && self.hw_night {
            d[8] |= KOBO_EPDC_FLAG_ENABLE_INVERSION;
        }
        let flags = match self.epdc {
            Epdc::Legacy => d[10],
            Epdc::KoboMtk => d[7],
            _ => d[8],
        };
        self.wake_epdc();
        let fd = self.file.as_raw_fd();
        let t = Instant::now();
        ioctl(fd, send_update_request(self.epdc), d.as_mut_ptr().cast())
            .map_err(|e| DeviceError::Backend(format!("MXCFB_SEND_UPDATE ({}): {e}", self.epdc.name())))?;
        let submit_us = t.elapsed().as_micros();
        self.updates += 1;
        self.last_waveform = (waveform, mode);
        let full = mode == UPDATE_MODE_FULL;
        if full && !self.defer_full_wait && self.model.reliable_wait {
            let mut marker = [self.marker, 0u32];
            if let Err(e) = ioctl(fd, MXCFB_WAIT_FOR_UPDATE_COMPLETE, marker.as_mut_ptr().cast()) {
                log::warn!("MXCFB_WAIT_FOR_UPDATE_COMPLETE failed: {e}");
            }
            let complete_us = t.elapsed().as_micros();
            log::debug!("fb0: update {:?} wf {waveform} mode {mode} flags {flags:#x} swipe {} marker {} submit_us {submit_us} complete_us {complete_us}", r, d[18], self.marker);
        } else {
            self.in_flight.sent(r, self.marker, full, Instant::now());
            log::debug!("fb0: update {:?} wf {waveform} mode {mode} flags {flags:#x} swipe {} marker {} submit_us {submit_us}", r, d[18], self.marker);
        }
        Ok(())
    }

    /// Refreshes a panel rectangle with a refresh class (night waveforms while the
    /// hardware night flag is set; software inversion uses the day waveforms).
    pub fn refresh_panel(&mut self, r: Rect, class: RefreshClass) -> Result<()> {
        self.refresh_panel_swipe(r, class, None)
    }

    /// [`KindleFb::refresh_panel`] with the page-turn wave (`swipe`: a
    /// `swipe_data.direction`). The waveform is the same as without it: with the night
    /// flag set the driver keeps its night waveforms during the sweep (KOReader
    /// `framebuffer_mxcfb.lua`: the grayscale flag makes it "use the proper waveform mode
    /// during animated swipes"), so GLKW16/GCK16 stay as chosen.
    pub fn refresh_panel_swipe(&mut self, r: Rect, class: RefreshClass, swipe: Option<u32>) -> Result<()> {
        let (mut waveform, mode) = waveform_epdc(self.epdc, class, self.hw_night, self.night_ui);
        if self.gl16_auto && class == RefreshClass::Gl16 && !self.hw_night && self.epdc == Epdc::Mtk {
            waveform = WAVEFORM_AUTO;
        }
        self.send_update(r, waveform, mode, swipe)
    }

    /// Raw framebuffer contents (panel orientation, not inverted).
    pub fn snapshot(&self) -> Canvas {
        let mut c = Canvas::new(self.width, self.height);
        for y in 0..self.height {
            let off = (self.yoffset + y) * self.stride;
            if off + self.width > self.map_len {
                break;
            }
            // SAFETY: bounds checked against the mapping length above.
            let row = unsafe { std::slice::from_raw_parts(self.map.add(off), self.width) };
            c.pixels[y * self.width..(y + 1) * self.width].copy_from_slice(row);
        }
        c
    }

    /// Writes a whole panel-orientation canvas (raw values) and refreshes it.
    pub fn write_panel(&mut self, c: &Canvas, class: RefreshClass) -> Result<()> {
        if c.width != self.width || c.height != self.height {
            return Err(DeviceError::Backend(format!("panel canvas {}x{} != {}x{}", c.width, c.height, self.width, self.height)));
        }
        let all = Rect::from_size(Size::new(self.width as u32, self.height as u32));
        let waits = self.in_flight.before(all, Instant::now());
        self.wait(waits);
        let (w, stride) = (self.width, self.stride);
        let dst = self.visible();
        for y in 0..c.height {
            dst[y * stride..y * stride + w].copy_from_slice(&c.pixels[y * w..(y + 1) * w]);
        }
        let r = Rect::from_size(Size::new(self.width as u32, self.height as u32));
        self.refresh_panel(r, class)
    }

    /// The value to store so the panel shows `v` (software night inversion applied).
    pub fn panel_value(&self, v: u8) -> u8 {
        if self.night && !self.hw_night {
            255 - v
        } else {
            v
        }
    }

    /// Software inversion in effect (night mode without the hardware flag).
    pub fn sw_invert(&self) -> bool {
        self.night && !self.hw_night
    }

    /// Undoes global flags (night flag, fast mode) and clears their journal lines.
    pub fn restore_flags(&mut self) {
        if self.passive {
            return;
        }
        if self.hw_night && self.epdc != Epdc::KoboMxcfb {
            if let Err(e) = self.set_grayscale(self.original_grayscale) {
                log::warn!("restoring grayscale: {e}");
            }
        }
        self.hw_night = false;
        if self.epdc == Epdc::Mtk {
            if let Err(e) = set_fast_mode(self.file.as_raw_fd(), false) {
                log::warn!("fast mode off: {e}");
            }
        }
        if let Some(was) = self.original_rotate.take() {
            if let Err(e) = set_rotate(self.file.as_raw_fd(), was) {
                log::warn!("restoring rotate {was}: {e}");
            }
        }
        if let Some(was) = self.original_bpp.take() {
            if let Some(f) = self.original_mdp.take() {
                set_mdp_src_format(&f);
            }
            let r = self.var().and_then(|mut var| {
                var[VAR_BPP] = was;
                var[VAR_GRAYSCALE] = 0;
                var[VAR_ACTIVATE] = 0;
                ioctl(self.file.as_raw_fd(), FBIOPUT_VSCREENINFO, var.as_mut_ptr().cast()).map_err(Into::into)
            });
            if let Err(e) = r {
                log::warn!("restoring {was} bpp: {e}");
            }
        }
        if let Some(j) = &self.journal {
            let _ = j.forget(&Entry::Fb { key: "grayscale".into(), value: 0 });
            let _ = j.forget(&Entry::Fb { key: "fastmode".into(), value: 0 });
            let _ = j.forget(&Entry::Fb { key: "rotate".into(), value: 0 });
            let _ = j.forget(&Entry::Fb { key: "bpp".into(), value: 0 });
        }
    }
}

fn set_fast_mode(fd: i32, on: bool) -> std::io::Result<()> {
    let mut flags = UPDATE_FLAGS_FAST_MODE | if on { UPDATE_FLAGS_MODE_FAST_FLAG } else { 0 };
    ioctl(fd, MXCFB_SET_UPDATE_FLAGS_MTK, (&mut flags as *mut u32).cast())
}

/// Kobo MTK: the hwtcon device folders in sysfs.
fn hwtcon_dirs() -> Vec<std::path::PathBuf> {
    let Ok(dir) = std::fs::read_dir("/sys/devices/platform") else { return Vec::new() };
    dir.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".hwtcon")).map(|e| e.path()).collect()
}

/// Kobo MTK: the pixel format hwtcon reads the framebuffer as now.
fn mdp_src_format() -> Option<String> {
    let d = hwtcon_dirs().into_iter().next()?;
    std::fs::read_to_string(d.join("mdp_src_format")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Kobo MTK: the pixel format hwtcon reads the framebuffer as (`Y8`, `ABGR32`).
fn set_mdp_src_format(format: &str) {
    for d in hwtcon_dirs() {
        if let Err(e) = std::fs::write(d.join("mdp_src_format"), format) {
            log::warn!("mdp_src_format {format}: {e}");
        }
    }
}

fn set_rotate(fd: i32, rotate: u32) -> std::io::Result<()> {
    let mut var = [0u32; 40];
    ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr().cast())?;
    var[VAR_ROTATE] = rotate;
    var[VAR_ACTIVATE] = 0;
    ioctl(fd, FBIOPUT_VSCREENINFO, var.as_mut_ptr().cast())
}

/// Tries the `rotate` values until the panel is portrait; returns the one that is.
/// `var` holds the driver's state afterwards.
fn make_upright(fd: i32, var: &mut [u32; 40]) -> Option<u32> {
    let was = var[VAR_ROTATE];
    for r in [0, 2, 1, 3].into_iter().filter(|r| *r != was) {
        if set_rotate(fd, r).is_ok() && ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr().cast()).is_ok() && var[VAR_XRES] < var[VAR_YRES]
        {
            return Some(r);
        }
    }
    let _ = set_rotate(fd, was);
    let _ = ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr().cast());
    None
}

/// Finds an unknown Kindle's refresh interface: the first one whose ioctl takes a
/// GC16 PARTIAL of the unchanged top-left 16 x 16 pixels (nothing visible changes).
/// The request number encodes the struct size, so a wrong one is refused.
fn probe_epdc(fd: i32) -> Option<Epdc> {
    let r = Rect::new(0, 0, 16, 16);
    for (i, epdc) in Epdc::PROBE_ORDER.into_iter().enumerate() {
        let mut d = update_data_epdc(epdc, r, WAVEFORM_GC16, UPDATE_MODE_PARTIAL, 0x7fff_0000 + i as u32, None);
        match ioctl(fd, send_update_request(epdc), d.as_mut_ptr().cast()) {
            Ok(()) => {
                log::info!("fb0: unknown model; the {} refresh ioctl works", epdc.name());
                return Some(epdc);
            }
            Err(e) => log::info!("fb0: probe {}: {e}", epdc.name()),
        }
    }
    log::warn!("fb0: no refresh ioctl accepted; assuming MTK");
    None
}

/// Puts the framebuffer flags back after a crash (`rinky device restore`):
/// grayscale to day (or the journaled value), the framework's rotation, fast mode off
/// (MTK). Returns what it did.
pub fn restore_after_crash(entries: &[Entry]) -> Result<Vec<String>> {
    let file = OpenOptions::new().read(true).write(true).open("/dev/fb0")?;
    let fd = file.as_raw_fd();
    let mut done = Vec::new();
    let mut var = [0u32; 40];
    ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr().cast())?;
    let want = entries
        .iter()
        .find_map(|e| match e {
            Entry::Fb { key, value } if key == "grayscale" => Some(*value as u32),
            _ => None,
        })
        .unwrap_or(GRAYSCALE_8BIT);
    // The grayscale flag means something only at 8 bpp: a Kobo that rinky already put
    // back to 32 bpp must keep 0 (the MTK driver turns its pixel format to Y8 otherwise).
    if var[VAR_BPP] == 8 && var[VAR_GRAYSCALE] != want {
        var[VAR_GRAYSCALE] = want;
        var[VAR_ACTIVATE] = 0;
        ioctl(fd, FBIOPUT_VSCREENINFO, var.as_mut_ptr().cast())?;
        done.push(format!("grayscale -> {want}"));
    }
    if let Some(r) = entries.iter().find_map(|e| match e {
        Entry::Fb { key, value } if key == "rotate" => Some(*value as u32),
        _ => None,
    }) {
        set_rotate(fd, r)?;
        done.push(format!("rotate -> {r}"));
    }
    // Kobo: the bit depth Nickel expects (and on MTK the pixel format with it).
    if let Some(bpp) = entries.iter().find_map(|e| match e {
        Entry::Fb { key, value } if key == "bpp" => Some(*value as u32),
        _ => None,
    }) {
        if bpp == 32 && !hwtcon_dirs().is_empty() {
            set_mdp_src_format("ABGR32");
        }
        ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr().cast())?;
        var[VAR_BPP] = bpp;
        var[VAR_GRAYSCALE] = 0;
        var[VAR_ACTIVATE] = 0;
        ioctl(fd, FBIOPUT_VSCREENINFO, var.as_mut_ptr().cast())?;
        done.push(format!("bpp -> {bpp}"));
    }
    if Model::detect(&SysRoot::system()).epdc == Epdc::Mtk {
        set_fast_mode(fd, false)?;
        done.push("fast mode off".into());
    }
    Ok(done)
}

impl Screen for KindleFb {
    fn panel_size(&self) -> Size {
        Size::new(self.width as u32, self.height as u32)
    }

    fn rotation(&self) -> Rotation {
        self.rotation
    }

    fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
        self.rotation = rotation;
        Ok(())
    }

    fn night_mode(&self) -> bool {
        self.night
    }

    fn set_night_mode(&mut self, on: bool) -> Result<()> {
        if on == self.night {
            return Ok(());
        }
        // Flip what is on the framebuffer now, so the next partial update matches.
        let was_sw = self.sw_invert();
        if self.epdc == Epdc::KoboMxcfb {
            // A flag on each update, nothing global to set.
            self.hw_night = on;
        } else if on && self.prefer_hw_night {
            if let Some(j) = &self.journal {
                let _ = j.record(Entry::Fb { key: "grayscale".into(), value: self.original_grayscale as i64 });
            }
            match self.set_grayscale(GRAYSCALE_8BIT_INVERTED) {
                Ok(()) => self.hw_night = true,
                Err(e) => {
                    log::warn!("hardware night mode refused ({e}); inverting in software");
                    self.hw_night = false;
                }
            }
        } else if !on && self.hw_night {
            self.set_grayscale(self.original_grayscale)?;
            self.hw_night = false;
        }
        self.night = on;
        if was_sw != self.sw_invert() {
            for p in self.visible().iter_mut() {
                *p = 255 - *p;
            }
        }
        // What the driver holds now: the waveforms follow `hw_night`, the pixels
        // `sw_invert`; both must agree with the flag.
        let flag = self.grayscale().unwrap_or(u32::MAX);
        let how = match (on, self.hw_night) {
            (false, _) => "day",
            (true, true) if self.epdc == Epdc::KoboMxcfb => "inversion flag on each update, night waveforms",
            (true, true) => "hardware grayscale flag, night waveforms",
            (true, false) => "software inversion, day waveforms",
        };
        log::info!("night mode {} ({how}; grayscale flag {flag})", if on { "on" } else { "off" });
        if self.epdc != Epdc::KoboMxcfb && self.hw_night != (flag == GRAYSCALE_8BIT_INVERTED) {
            log::warn!("night mode: grayscale flag {flag} does not match hardware night {}", self.hw_night);
        }
        Ok(())
    }

    fn update(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass) -> Result<()> {
        self.update_swipe(canvas, rect, class, None)
    }

    fn update_wave(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass, wave: Wave) -> Result<()> {
        let dir = swipe_direction(wave, self.rotation, self.panel_size());
        self.update_swipe(canvas, rect, class, Some(dir))
    }

    fn refresh_all(&mut self) -> Result<()> {
        let r = Rect::from_size(self.panel_size());
        self.refresh_panel(r, RefreshClass::Gc16)
    }

    /// What the panel shows (inverted back when the driver inverts in hardware).
    fn screenshot(&self) -> Result<Canvas> {
        let mut c = self.snapshot();
        if self.hw_night {
            for p in &mut c.pixels {
                *p = 255 - *p;
            }
        }
        Ok(c)
    }
}

impl KindleFb {
    /// [`Screen::update`] with an optional page-turn wave (`swipe_data.direction`).
    fn update_swipe(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass, swipe: Option<u32>) -> Result<()> {
        let (rotation, invert, panel, stride) = (self.rotation, self.sw_invert(), self.panel_size(), self.stride);
        // Let the driver take over earlier updates of this area before overwriting it.
        let logical = rect.intersect(&canvas.bounds()).intersect(&Rect::from_size(rotation.logical_size(panel)));
        if !logical.is_empty() {
            let waits = self.in_flight.before(rotation.rect_to_panel(logical, panel), Instant::now());
            self.wait(waits);
        }
        let changed = copy_to_panel(canvas, rect, rotation, invert, panel, self.visible(), stride);
        if changed.is_empty() {
            return Ok(());
        }
        self.refresh_panel_swipe(changed, class, swipe)
    }
}

impl Drop for KindleFb {
    fn drop(&mut self) {
        if !self.passive {
            self.wait_idle();
        }
        self.restore_flags();
        // SAFETY: unmapping the region mapped in open().
        unsafe {
            libc::munmap(self.map.cast(), self.map_len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_waveforms() {
        assert_eq!(waveform_for(RefreshClass::Gc16, true), (WAVEFORM_GCK16, UPDATE_MODE_FULL));
        assert_eq!(waveform_for(RefreshClass::Reagl, true), (WAVEFORM_GLKW16, UPDATE_MODE_FULL));
        // UI updates use the night REAGL waveform too (GL16 PARTIAL left streaks).
        assert_eq!(waveform_for(RefreshClass::Gl16, true), (WAVEFORM_GLKW16, UPDATE_MODE_FULL));
        assert_eq!(waveform_with(RefreshClass::Gl16, true, NightUi::Auto), (WAVEFORM_AUTO, UPDATE_MODE_PARTIAL));
        assert_eq!(waveform_with(RefreshClass::Gc16, true, NightUi::Auto), (WAVEFORM_GCK16, UPDATE_MODE_FULL));
        assert_eq!(waveform_for(RefreshClass::Du, true), (WAVEFORM_DU, UPDATE_MODE_PARTIAL));
        assert_eq!(waveform_for(RefreshClass::Gl16, false), (WAVEFORM_GL16, UPDATE_MODE_PARTIAL));
        assert_eq!(waveform_for(RefreshClass::Reagl, false), (WAVEFORM_GLR16, UPDATE_MODE_FULL));
    }

    #[test]
    fn imx_layouts_and_waveforms() {
        // Request numbers carry the struct sizes KOReader's headers give.
        let size = |req: u32| (req >> 16) & 0x3fff;
        assert_eq!(size(send_update_request(Epdc::Legacy)), 72);
        assert_eq!(size(send_update_request(Epdc::Rex)), 80);
        assert_eq!(size(send_update_request(Epdc::Zelda)), 88);
        assert_eq!(size(send_update_request(Epdc::Mtk)), 96);
        let r = Rect::new(10, 20, 300, 400);
        // Original i.MX: history before temp and flags.
        let (wf, mode) = waveform_epdc(Epdc::Legacy, RefreshClass::Reagl, false, NightUi::Glkw16);
        assert_eq!((wf, mode), (LEGACY_WAVEFORM_REAGL, UPDATE_MODE_FULL));
        let d = update_data_epdc(Epdc::Legacy, r, wf, mode, 5, Some(SWIPE_LEFT));
        assert_eq!(&d[..11], &[20, 10, 300, 400, 8, 1, 5, 8, 8, TEMP_USE_AUTO, 0]);
        assert!(d[11..].iter().all(|w| *w == 0), "no swipe outside MTK");
        let d = update_data_epdc(Epdc::Legacy, r, WAVEFORM_DU, UPDATE_MODE_PARTIAL, 6, None);
        assert_eq!((d[7], d[8], d[10]), (WAVEFORM_DU, LEGACY_WAVEFORM_GC16_FAST, EPDC_FLAG_FORCE_MONOCHROME));
        assert_eq!(waveform_epdc(Epdc::Legacy, RefreshClass::Gl16, false, NightUi::Glkw16), (3, UPDATE_MODE_PARTIAL));
        // Rex / Zelda: temp and flags first, history after alt_buffer_data.
        let (wf, mode) = waveform_epdc(Epdc::Rex, RefreshClass::Reagl, false, NightUi::Glkw16);
        let d = update_data_epdc(Epdc::Rex, r, wf, mode, 7, None);
        assert_eq!(&d[4..11], &[WAVEFORM_GLR16, UPDATE_MODE_FULL, 7, TEMP_USE_AMBIENT, 0, 0, 0]);
        assert_eq!((d[18], d[19]), (WAVEFORM_GLR16, WAVEFORM_GLR16));
        let d = update_data_epdc(Epdc::Zelda, r, WAVEFORM_GL16, UPDATE_MODE_PARTIAL, 8, None);
        assert_eq!((d[18], d[19], d[20], d[21]), (WAVEFORM_DU, WAVEFORM_GC16, 0, 0));
        // No night waveforms on i.MX (night mode is software inversion there).
        assert_eq!(waveform_epdc(Epdc::Zelda, RefreshClass::Gc16, true, NightUi::Glkw16), (WAVEFORM_GC16, UPDATE_MODE_FULL));
        // MTK is unchanged.
        assert_eq!(update_data_epdc(Epdc::Mtk, r, 4, 1, 9, None), update_data(r, 4, 1, 9, None));
    }

    #[test]
    fn kobo_layouts_and_waveforms() {
        let size = |req: u32| (req >> 16) & 0x3fff;
        assert_eq!(send_update_request(Epdc::KoboMxcfb), 0x4048_462e);
        assert_eq!(size(send_update_request(Epdc::KoboMtk)), 36);
        let r = Rect::new(10, 20, 300, 400);
        let d = update_data_epdc(Epdc::KoboMxcfb, r, KOBO_WAVEFORM_REAGL, UPDATE_MODE_FULL, 7, None);
        assert_eq!(&d[..11], &[20, 10, 300, 400, 6, 1, 7, TEMP_USE_AMBIENT, 0, 0, 0]);
        let d = update_data_epdc(Epdc::KoboMtk, r, WAVEFORM_GL16, UPDATE_MODE_PARTIAL, 8, None);
        assert_eq!(&d[..9], &[20, 10, 300, 400, 3, 0, 8, 0, 0]);
        let wf = |c, n| waveform_epdc(Epdc::KoboMxcfb, c, n, NightUi::Glkw16);
        assert_eq!(wf(RefreshClass::Reagl, false), (KOBO_WAVEFORM_REAGL, UPDATE_MODE_FULL));
        assert_eq!(wf(RefreshClass::Gl16, false), (KOBO_WAVEFORM_GL16, UPDATE_MODE_PARTIAL));
        assert_eq!(wf(RefreshClass::Gc16, true), (KOBO_WAVEFORM_GCK16, UPDATE_MODE_FULL));
        assert_eq!(wf(RefreshClass::Reagl, true), (KOBO_WAVEFORM_GLKW16, UPDATE_MODE_PARTIAL));
        assert_eq!(waveform_epdc(Epdc::KoboMtk, RefreshClass::Reagl, false, NightUi::Glkw16), (WAVEFORM_GLR16, UPDATE_MODE_FULL));
    }

    #[test]
    fn swipe_flag_direction_and_layout() {
        let full = Rect::new(0, 0, 1272, 1696);
        let d = update_data(full, WAVEFORM_GLR16, UPDATE_MODE_FULL, 7, Some(SWIPE_LEFT));
        assert_eq!(std::mem::size_of_val(&d), 96);
        assert_eq!((d[2], d[3], d[4], d[5], d[6]), (1272, 1696, WAVEFORM_GLR16, UPDATE_MODE_FULL, 7));
        assert_eq!(d[8], EPDC_FLAG_ENABLE_SWIPE);
        assert_eq!((d[18], d[19]), (SWIPE_LEFT, SWIPE_STEPS));
        // Night: the waveform stays GLKW16/GCK16 with the flag.
        let d = update_data(full, WAVEFORM_GLKW16, UPDATE_MODE_FULL, 8, Some(SWIPE_RIGHT));
        assert_eq!((d[4], d[8], d[18]), (WAVEFORM_GLKW16, EPDC_FLAG_ENABLE_SWIPE, SWIPE_RIGHT));
        // No wave: no flag; an area smaller than the steps never sweeps.
        assert_eq!(update_data(full, WAVEFORM_GL16, UPDATE_MODE_PARTIAL, 9, None)[8], 0);
        assert_eq!(update_data(Rect::new(0, 0, 11, 500), WAVEFORM_GLR16, UPDATE_MODE_FULL, 9, Some(SWIPE_LEFT))[8], 0);
        // Directions follow the rotation (logical left, as seen on the panel).
        let p = Size::new(1272, 1696);
        assert_eq!(swipe_direction(Wave::Left, Rotation::Portrait, p), SWIPE_LEFT);
        assert_eq!(swipe_direction(Wave::Right, Rotation::Portrait, p), SWIPE_RIGHT);
        assert_eq!(swipe_direction(Wave::Left, Rotation::UpsideDown, p), SWIPE_RIGHT);
        assert_eq!(swipe_direction(Wave::Left, Rotation::Clockwise, p), SWIPE_UP);
        assert_eq!(swipe_direction(Wave::Left, Rotation::CounterClockwise, p), SWIPE_DOWN);
        assert_eq!(swipe_direction(Wave::Right, Rotation::CounterClockwise, p), SWIPE_UP);
        assert_eq!(Wave::for_turn(true, false), Wave::Left);
        assert_eq!(Wave::for_turn(false, false), Wave::Right);
        assert_eq!(Wave::for_turn(true, true), Wave::Right);
    }
}
