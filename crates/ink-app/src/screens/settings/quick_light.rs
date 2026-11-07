//! Control center: the quick settings panel at the top of the screen, opened by a swipe
//! down that starts at the top edge (on every screen) or by [`open_control_center`]
//! (the reader menu's Light icon).
//!
//! The panel holds, from the top: a header with the clock, the date and the battery; a
//! brightness slider (sun icon, -/+); a warmth slider; "Auto warmth" (Off / Sunset /
//! Custom hours, applied by a timer, see [`tick_auto_warmth`]); and up to two rows of
//! tiles chosen in `settings.ui.control_buttons` ([`buttons`], names from
//! [`ink_store::prefs::CONTROL_BUTTONS`]: night mode, auto warmth, Wi-Fi, rotate, full
//! refresh, status bars, sync, screenshot, sleep, settings). Every
//! control acts at once through the device services (`flIntensity`,
//! `currentAmberLevel`, `wifid enable`, powerd's `amberSched*`). The sliders write the
//! light while they are dragged ([`should_write`]: at most every [`LIVE_EVERY`] or every
//! [`LIVE_STEPS`] steps, never queued; the Kindle backend writes on a thread, newest
//! value wins) and the final value on release; during a drag only the slider repaints,
//! with DU.
//!
//! Opening draws the whole panel at once (one GL16 update, no slide-in) with the last
//! known light levels and Wi-Fi state ([`ink_device::Light::set_cached_reads`]); a
//! background read ([`ink_device::Light::refresh_state`]) follows and only the tiles
//! and sliders that changed repaint.
//!
//! Auto warmth is stored as powerd's `Manual` schedule (sunset mode writes the computed
//! sunset and sunrise times), which the Kindle backend's own amber timer
//! (`ink_device::kindle` input loop, every 60 s) applies on every screen; the panel
//! applies a change at once. Backends without such a timer can call
//! [`tick_auto_warmth`] from their loop.
//!
//! The app opens it on every screen: on [`is_trigger`] it pushes
//! [`crate::router::Route::QuickLight`] ([`ControlCenterScreen`], route kind and screen
//! name `quicklight`). [`QuickLight`] reads the two light sliders for the panel and the
//! settings page; their actions go to [`apply`].

use super::advanced;
use super::popup::{self, Anchor, PopupCatcher, SettingsPopup};
use crate::router::{AppCtx, Route, Services, Transition};
use ink_device::clock::DateTime;
use ink_device::light::{AmberMode, AmberSchedule, AmberScheduler, DayTime};
use ink_device::{Clock, Light, Rect, RefreshClass, Rotation, Size};
use ink_store::prefs::{label, CONTROL_BUTTONS};
use ink_text::Style;
use ink_ui::draw::{fill_round_rect, stroke_round_rect, Path};
use ink_ui::{
    Damage, Direction, EventResult, Gesture, Icon, Label, PaintCx, Segmented, Slider, TouchKind, UiEvent, Widget,
    WidgetId, WidgetTree,
};
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// Most buttons the control center shows: two rows of four tiles. The settings page
/// "Control center buttons" lets the user pick at most this many.
pub const MAX_BUTTONS: usize = 8;

/// Tiles per row of the control center.
pub const BUTTON_COLS: usize = 4;

/// While a slider is dragged the light is written at most this often (or sooner when
/// the value moved [`LIVE_STEPS`] steps); the final value is written on release.
pub const LIVE_EVERY: Duration = Duration::from_millis(60);

/// See [`LIVE_EVERY`].
pub const LIVE_STEPS: u32 = 2;

/// True when a dragged slider at `value` should write the light now: nothing written
/// yet in this drag, or the value changed and [`LIVE_EVERY`] passed or it moved at
/// least [`LIVE_STEPS`] steps since the last write. Nothing is queued: a skipped value
/// is simply not written (the next one or the release writes).
pub fn should_write(last: Option<(u32, Instant)>, value: u32, now: Instant) -> bool {
    match last {
        None => true,
        Some((v, at)) => v != value && (now.saturating_duration_since(at) >= LIVE_EVERY || v.abs_diff(value) >= LIVE_STEPS),
    }
}

/// A swipe down must start this close to the top edge (logical pixels).
pub const TOP_EDGE: i32 = 120;

/// Action prefix of the brightness slider.
pub const BRIGHTNESS: &str = "ql-bright";
/// Action prefix of the warmth slider.
pub const WARMTH: &str = "ql-warm";
/// Action of the Done button and of the scrim under the panel.
pub const CLOSE: &str = "ql-close";

/// True for a swipe down that starts at the top edge.
pub fn is_trigger(ev: &UiEvent) -> bool {
    matches!(ev, UiEvent::Gesture(Gesture::Swipe { from, dir: Direction::Down, .. }) if from.y < TOP_EDGE)
}

/// Sets the light for a slider action of the panel (`ql-bright:N`, `ql-warm:N`, also the
/// settings page's `bright:N`, `warm:N`). Returns a status line, or `None` when the
/// action is not a light action.
pub fn apply(action: &str, light: &mut dyn Light) -> Option<String> {
    let (what, value) = action.split_once(':')?;
    let v: u32 = value.parse().ok()?;
    let (name, r) = match what {
        BRIGHTNESS | "bright" => ("Brightness", light.set_brightness(v)),
        WARMTH | "warm" => ("Warmth", light.set_warmth(v)),
        _ => return None,
    };
    Some(match r {
        Ok(()) => format!("{name} {v}"),
        Err(e) => format!("{name}: {e}"),
    })
}

/// The light sliders of the device.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickLight {
    /// Brightness slider; `None` when the device has no frontlight.
    pub brightness: Option<Slider>,
    /// Warmth slider; `None` when the device has no warm light.
    pub warmth: Option<Slider>,
}

impl QuickLight {
    /// Sliders at the light's current levels.
    pub fn read(light: &dyn Light) -> Self {
        let slider = |label: &str, range: (u32, u32), v: ink_device::Result<u32>, action: &str| {
            v.ok().map(|v| Slider::new(label, range.0 as f32, range.1 as f32, 1.0, v as f32, action))
        };
        QuickLight {
            brightness: slider("Brightness", light.brightness_range(), light.brightness(), BRIGHTNESS),
            warmth: slider("Warmth", light.warmth_range(), light.warmth(), WARMTH),
        }
    }

    /// Same as [`QuickLight::read`] for the services of a screen context.
    pub fn from_services(s: &Services) -> Self {
        QuickLight::read(&*s.light)
    }
}

// -- Opening ---------------------------------------------------------------------------------

/// Opens the control center over the current screen: return the result from
/// `AppScreen::handle` (the reader menu's Light icon, a plugin, a key binding).
pub fn open_control_center(_cx: &mut AppCtx) -> Transition {
    Transition::Push(Route::QuickLight)
}

/// Action of a tile: `cc-tile:<name>`.
pub const TILE: &str = "cc-tile";
/// Action of the auto warmth choice: `cc-auto:<0 off | 1 sunset | 2 custom>`.
pub const AUTO: &str = "cc-auto";
/// Actions of the custom hours: `cc-from:-1`, `cc-from:1`, `cc-to:-1`, `cc-to:1`.
pub const FROM: &str = "cc-from";
/// See [`FROM`].
pub const TO: &str = "cc-to";
/// Custom hours move in steps of this many minutes.
pub const HOUR_STEP_MIN: u16 = 30;

// -- Auto warmth -------------------------------------------------------------------------------

/// File under `Paths::root` that remembers "sunset to sunrise" (powerd only knows the
/// hours: its own `Automatic` mode uses powerd's twilight times, which the backend
/// timer does not apply and which were wrong on the emulator, so it is never written).
pub const SUNSET_FILE: &str = "auto-warmth-sunset";
/// Latitude used for sunset and sunrise. The Kindle has no location; 52 degrees north
/// is in the middle of Europe's and North America's reading population.
pub const DEFAULT_LATITUDE: f64 = 52.0;
/// Local solar noon assumed for sunset and sunrise (between winter and summer time in
/// most time zones; no longitude is known).
pub const SOLAR_NOON: DayTime = DayTime::hm(12, 30);

/// Sunrise and sunset on `year-month-day` at `latitude` (degrees north), with the sun's
/// centre 0.833 degrees below the horizon (refraction), around `noon`. Polar day and
/// night clamp to 00:00..23:59 and noon..noon.
pub fn sun_times(year: i32, month: u8, day: u8, latitude: f64, noon: DayTime) -> (DayTime, DayTime) {
    use ink_device::clock::days_from_civil;
    let n = (days_from_civil(year, month, day) - days_from_civil(year, 1, 1) + 1) as f64;
    let decl = (-23.44f64).to_radians() * (2.0 * std::f64::consts::PI / 365.0 * (n + 10.0)).cos();
    let phi = latitude.to_radians();
    let cos_w = ((-0.833f64).to_radians().sin() - phi.sin() * decl.sin()) / (phi.cos() * decl.cos());
    let half_min = (cos_w.clamp(-1.0, 1.0).acos().to_degrees() * 4.0).round() as i32;
    let half_min = half_min.min(719);
    let at = |m: i32| DayTime((m.rem_euclid(1440)) as u16);
    (at(noon.0 as i32 - half_min), at(noon.0 as i32 + half_min))
}

/// How the warmth follows the clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoMode {
    /// The warmth stays as the user sets it.
    #[default]
    Off,
    /// Warm from sunset to sunrise ([`sun_times`]).
    Sunset,
    /// Warm between the user's own hours.
    Custom,
}

impl AutoMode {
    /// Index in the panel's choice (Off, Sunset, Custom).
    pub fn index(self) -> usize {
        self as usize
    }

    /// From the panel's choice.
    pub fn from_index(i: usize) -> AutoMode {
        match i {
            1 => AutoMode::Sunset,
            2 => AutoMode::Custom,
            _ => AutoMode::Off,
        }
    }
}

/// The auto warmth settings and the timer state. The settings live in powerd's
/// `amberSched*` properties (`Light::amber_schedule`); devices without them keep the
/// values for the session only.
#[derive(Clone, Debug)]
pub struct AutoWarmth {
    /// Mode.
    pub mode: AutoMode,
    /// Custom start.
    pub start: DayTime,
    /// Custom end.
    pub end: DayTime,
    /// Warmth during the warm period (0..=24); a warmth the user picks during the period
    /// becomes the new level.
    pub level: u32,
    /// Ramp at both ends of the period.
    pub ramp: Duration,
    scheduler: AmberScheduler,
    /// The user's own warmth, seen outside the warm period.
    base: Option<u32>,
    /// The last tick was inside the warm period.
    was_in: Option<bool>,
    /// When the timer should run next.
    due: Option<Instant>,
    /// The sunset choice was read from the file.
    file_read: bool,
}

impl Default for AutoWarmth {
    fn default() -> Self {
        let d = AmberSchedule::default();
        AutoWarmth {
            mode: AutoMode::Off,
            start: d.start,
            end: d.end,
            level: d.level,
            ramp: d.ramp,
            scheduler: AmberScheduler::default(),
            base: None,
            was_in: None,
            due: None,
            file_read: false,
        }
    }
}

impl AutoWarmth {
    /// The settings stored in the device (defaults when it has none).
    pub fn from_device(light: &dyn Light) -> Self {
        let mut a = AutoWarmth::default();
        if let Ok(s) = light.amber_schedule() {
            a.mode = if s.enabled { AutoMode::Custom } else { AutoMode::Off };
            if s.start != s.end {
                (a.start, a.end) = (s.start, s.end);
            }
            a.level = s.level.min(24);
        }
        a
    }

    /// Sunset and sunrise of `date` (approximate, see [`DEFAULT_LATITUDE`]).
    pub fn sun(date: &DateTime) -> (DayTime, DayTime) {
        let (rise, set) = sun_times(date.year, date.month, date.day, DEFAULT_LATITUDE, SOLAR_NOON);
        (set, rise)
    }

    /// The schedule for `date`.
    pub fn schedule(&self, date: &DateTime) -> AmberSchedule {
        let (start, end) = match self.mode {
            AutoMode::Sunset => Self::sun(date),
            _ => (self.start, self.end),
        };
        AmberSchedule {
            enabled: self.mode != AutoMode::Off,
            mode: AmberMode::Manual,
            start,
            end,
            level: self.level,
            ramp: self.ramp,
        }
    }

    /// Stores the settings in the device (powerd); errors are logged only.
    pub fn save(&self, light: &mut dyn Light, date: &DateTime) {
        if let Err(e) = light.set_amber_schedule(&self.schedule(date)) {
            log::debug!("auto warmth: cannot store the schedule: {e}");
        }
    }

    /// Reads the sunset choice from `root/`[`SUNSET_FILE`] once (custom hours stored
    /// in powerd become "sunset" when the file is there) and refreshes the stored
    /// sunset times for today. Returns true when the mode changed.
    pub fn load_sunset(&mut self, root: &std::path::Path, light: &mut dyn Light, date: &DateTime) -> bool {
        let mut changed = false;
        if !self.file_read {
            self.file_read = true;
            if self.mode == AutoMode::Custom && root.join(SUNSET_FILE).exists() {
                self.mode = AutoMode::Sunset;
                changed = true;
            }
        }
        if self.mode == AutoMode::Sunset {
            let stored = light.amber_schedule().ok().map(|s| (s.start, s.end));
            let want = Self::sun(date);
            if stored != Some(want) {
                self.save(light, date);
            }
        }
        changed
    }

    /// Writes or removes `root/`[`SUNSET_FILE`] for the current mode.
    pub fn store_sunset(&self, root: &std::path::Path) {
        let f = root.join(SUNSET_FILE);
        let r = if self.mode == AutoMode::Sunset {
            std::fs::create_dir_all(root).and_then(|_| std::fs::write(&f, "sunset to sunrise\n"))
        } else if f.exists() {
            std::fs::remove_file(&f)
        } else {
            Ok(())
        };
        if let Err(e) = r {
            log::warn!("auto warmth: {}: {e}", f.display());
        }
    }

    /// True when the warm period is on now.
    pub fn active(&self, date: &DateTime) -> bool {
        self.mode != AutoMode::Off && self.schedule(date).in_window(date.day_time())
    }

    /// Applies the schedule at `date`: sets the warmth when the schedule's value
    /// changed. Returns how long until the warmth may change next (`None`: never, auto
    /// warmth is off or the device has no warm light).
    pub fn tick(&mut self, light: &mut dyn Light, date: &DateTime) -> Option<Duration> {
        if self.mode == AutoMode::Off {
            return None;
        }
        let current = light.warmth().ok()?;
        self.tick_from(current, light, date).1
    }

    /// [`Self::tick`] with the warmth the device had (`current`). Returns the warmth it
    /// leaves and how long until it may change next.
    fn tick_from(&mut self, current: u32, light: &mut dyn Light, date: &DateTime) -> (u32, Option<Duration>) {
        let sched = self.schedule(date);
        let t = date.day_time();
        let inside = sched.in_window(t);
        // Outside the period the warmth is the user's own, except right after the period
        // ended: then it is still the period's, and the user's comes back below.
        if !inside && self.was_in != Some(true) {
            self.base = Some(current);
        }
        self.was_in = Some(inside);
        let base = self.base.unwrap_or(current);
        let mut now = current;
        if let Some(w) = self.scheduler.tick(&sched, t, base) {
            if w != current {
                log::info!("auto warmth: {current} -> {w} at {t}");
                match light.set_warmth(w) {
                    Ok(()) => now = w,
                    Err(e) => log::warn!("auto warmth: {e}"),
                }
            }
        }
        (now, Some(sched.next_change_in(t)))
    }

    /// The user picked warmth `v` by hand: inside the warm period it becomes the period's
    /// level (stored), outside it is the user's own warmth.
    pub fn manual(&mut self, v: u32, light: &mut dyn Light, date: &DateTime) {
        if self.active(date) {
            if self.level != v {
                self.level = v.min(24);
                self.save(light, date);
            }
        } else {
            self.base = Some(v);
        }
        // The scheduler keeps its last value: the user's choice stays until the
        // schedule's value changes.
    }

    /// Changes the mode or the custom hours (`f`), stores them and applies them at once.
    /// Returns the warmth it leaves (`None`: no warm light).
    ///
    /// The warmth is read before the schedule is written: powerd 5.19.5 jumps
    /// `currentAmberLevel` to `schedAmberLevel` on a schedule write and fades back
    /// (3 -> 10 -> 9 -> 5 -> 3 within about 200 ms on the emulator), so a read after the
    /// write can see a passing value.
    pub fn change(&mut self, light: &mut dyn Light, date: &DateTime, f: impl FnOnce(&mut AutoWarmth)) -> Option<u32> {
        let before = light.warmth().ok();
        let was_active = self.active(date);
        f(self);
        self.scheduler.reset();
        self.save(light, date);
        self.due = Some(Instant::now());
        if self.mode == AutoMode::Off {
            // Back to the user's own warmth when the warm period was on.
            if let (true, Some(b)) = (was_active, self.base) {
                if light.set_warmth(b).is_ok() {
                    return Some(b);
                }
            }
            before
        } else {
            before.map(|c| self.tick_from(c, light, date).0)
        }
    }
}

thread_local! {
    static AUTO_WARMTH: RefCell<Option<AutoWarmth>> = const { RefCell::new(None) };
}

/// Runs `f` on the session's auto warmth state (read from the device on first use).
pub fn with_auto_warmth<R>(light: &mut dyn Light, f: impl FnOnce(&mut AutoWarmth, &mut dyn Light) -> R) -> R {
    AUTO_WARMTH.with(|a| {
        let mut a = a.borrow_mut();
        let a = a.get_or_insert_with(|| AutoWarmth::from_device(&*light));
        f(a, light)
    })
}

/// The auto warmth timer for a loop without the Kindle backend's own amber timer:
/// applies the schedule when it is due and returns how long the loop may sleep
/// (`timeout` shortened to the next change). Cheap when not due (no device calls). Not
/// hooked into the app loop (the Kindle backend applies powerd's schedule itself); the
/// control center calls it when it opens.
pub fn tick_auto_warmth(timeout: Option<Duration>, light: &mut dyn Light, clock: &dyn Clock) -> Option<Duration> {
    let now = Instant::now();
    let next = with_auto_warmth(light, |a, light| {
        if a.due.is_none_or(|d| d <= now) {
            let wait = a.tick(light, &clock.local_now());
            a.due = wait.map(|w| now + w);
        }
        a.due.map(|d| d.saturating_duration_since(now))
    });
    match (timeout, next) {
        (Some(t), Some(n)) => Some(t.min(n)),
        (t, n) => t.or(n),
    }
}

// -- Layout ------------------------------------------------------------------------------------

/// Where everything goes on a screen of a given size.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The whole panel (top of the screen).
    pub panel: Rect,
    /// Clock, date, battery.
    pub header: Rect,
    /// Sun icon left of the brightness slider.
    pub bright_icon: Rect,
    /// Brightness slider.
    pub bright: Rect,
    /// Icon left of the warmth slider.
    pub warm_icon: Rect,
    /// Warmth slider.
    pub warm: Rect,
    /// "Auto warmth" caption.
    pub auto_label: Rect,
    /// Off / Sunset / Custom.
    pub auto_choice: Rect,
    /// Sunset times or the custom hours.
    pub detail: Rect,
    /// Tiles in button order (`settings.ui.control_buttons`).
    pub tiles: Vec<Rect>,
    /// Status line and grab handle at the bottom.
    pub status: Rect,
    /// The Close button: [`CLOSE_SIDE`] square at the right end of the status row.
    pub close: Rect,
}

/// Side of the Close button (the design system's minimum touch target); also the height
/// of the status row.
pub const CLOSE_SIDE: u32 = 96;

/// The buttons to show: `settings.ui.control_buttons` without unknown names (see
/// [`ink_store::prefs::CONTROL_BUTTONS`]) and duplicates, at most [`MAX_BUTTONS`].
/// Plugin tiles (`plugin:<plugin>:<id>`) count only when `host` has them registered.
pub fn buttons(s: &ink_store::Settings, host: &crate::plugin_host::PluginHost) -> Vec<String> {
    crate::plugin_entries::control_chosen(s, MAX_BUTTONS, |n| crate::plugin_entries::control_known(host, n))
}

/// [`buttons`] without asking the plugin host: plugin tiles are accepted by their saved
/// id shape (for the count in the settings list, which has no host).
pub fn buttons_unchecked(s: &ink_store::Settings) -> Vec<String> {
    crate::plugin_entries::control_chosen(s, MAX_BUTTONS, |n| ink_plugin::registry::parse_saved_id(n).is_some() || CONTROL_BUTTONS.iter().any(|(b, _)| *b == n))
}

/// The layout for a screen of `size` (portrait 1272 x 1696 or landscape 1696 x 1272)
/// with `n` tiles ([`BUTTON_COLS`] per row, at most [`MAX_BUTTONS`]); the panel ends
/// below the last tile row used.
pub fn layout(size: Size, n: usize) -> Layout {
    let n = n.min(MAX_BUTTONS);
    let m = ink_ui::theme::space(4);
    let w = size.w.saturating_sub(2 * m as u32);
    let row = |y: i32, h: u32| Rect::new(m, y, w, h);
    let header = row(0, 112);
    let icon_w = 96u32;
    let slider = |y: i32| {
        let s = Rect::new(m + icon_w as i32 + 16, y, w.saturating_sub(icon_w + 16), 192);
        // The icon sits beside the track (lower half of the slider).
        (Rect::new(m, y + 96, icon_w, 96), s)
    };
    let (bright_icon, bright) = slider(120);
    let (warm_icon, warm) = slider(320);
    let auto_y = 528;
    let label_w = (w / 3).min(380);
    let auto_label = Rect::new(m, auto_y, label_w, 104);
    let auto_choice = Rect::new(m + label_w as i32 + 16, auto_y, w.saturating_sub(label_w + 16), 104);
    let detail = row(auto_y + 112, 104);
    let tiles_y = detail.bottom() + 24;
    let (cols, gap, th) = (BUTTON_COLS as u32, 16u32, 168u32);
    let tw = (w - gap * (cols - 1)) / cols;
    let tiles = (0..n as u32)
        .map(|i| Rect::new(m + ((tw + gap) * (i % cols)) as i32, tiles_y + ((th + gap) * (i / cols)) as i32, tw, th))
        .collect::<Vec<_>>();
    let rows = n.div_ceil(BUTTON_COLS) as i32;
    let tiles_h = if rows > 0 { rows * (th + gap) as i32 - gap as i32 + 8 } else { 0 };
    let status = row(tiles_y + tiles_h, CLOSE_SIDE);
    let close = Rect::new(status.right() - CLOSE_SIDE as i32, status.y, CLOSE_SIDE, CLOSE_SIDE);
    // The black edge (`rule_px`) runs below the status line.
    let panel = Rect::new(0, 0, size.w, (status.bottom() as u32 + 8).min(size.h));
    Layout { panel, header, bright_icon, bright, warm_icon, warm, auto_label, auto_choice, detail, tiles, status, close }
}

/// Where a long-press popup ([`SettingsPopup`]) may go: between the header and the
/// status line, inside the panel (closing it repaints only the panel).
pub fn picker_area(l: &Layout) -> Rect {
    let top = l.header.bottom() + 8;
    Rect::new(l.panel.x, top, l.panel.w, (l.status.y - 8 - top).max(0) as u32)
}

// -- Widgets -----------------------------------------------------------------------------------

/// Small pictures the icon set does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// An icon of the set.
    Icon(Icon),
    /// Airplane seen from above.
    Airplane,
    /// Circle half black, half white (night mode).
    Contrast,
    /// Filled sun (warmth).
    WarmSun,
}

impl Glyph {
    /// Draws the glyph `side` px large, centred in `r`.
    pub fn draw(self, cx: &mut PaintCx, r: Rect, side: u32, value: u8) {
        let side = side.min(r.w).min(r.h);
        let s = side as f32 / 24.0;
        let (ox, oy) = (r.x as f32 + (r.w - side) as f32 / 2.0, r.y as f32 + (r.h - side) as f32 / 2.0);
        let p = |x: f32, y: f32| (ox + x * s, oy + y * s);
        match self {
            Glyph::Icon(i) => cx.icon(i, r, side, value),
            Glyph::Airplane => {
                let pts: Vec<(f32, f32)> = [
                    (12.0, 1.5),
                    (13.6, 3.5),
                    (13.6, 9.0),
                    (22.5, 14.0),
                    (22.5, 16.2),
                    (13.6, 13.6),
                    (13.6, 18.6),
                    (16.5, 20.6),
                    (16.5, 22.5),
                    (12.0, 21.2),
                    (7.5, 22.5),
                    (7.5, 20.6),
                    (10.4, 18.6),
                    (10.4, 13.6),
                    (1.5, 16.2),
                    (1.5, 14.0),
                    (10.4, 9.0),
                    (10.4, 3.5),
                ]
                .iter()
                .map(|&(x, y)| p(x, y))
                .collect();
                Path::new().polygon(&pts).fill(cx.canvas, value);
            }
            Glyph::Contrast => {
                let (cx0, cy0) = p(12.0, 12.0);
                let rad = 9.5 * s;
                Path::new().ring(cx0, cy0, rad, 2.0 * s).fill(cx.canvas, value);
                // The right half filled.
                let half: Vec<(f32, f32)> = (0..=24)
                    .map(|k| {
                        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * k as f32 / 24.0;
                        (cx0 + rad * a.cos(), cy0 + rad * a.sin())
                    })
                    .collect();
                Path::new().polygon(&half).fill(cx.canvas, value);
            }
            Glyph::WarmSun => {
                let (cx0, cy0) = p(12.0, 12.0);
                Path::new().circle(cx0, cy0, 5.0 * s).fill(cx.canvas, value);
                let mut rays = Path::new();
                for k in 0..8 {
                    let a = std::f32::consts::PI * k as f32 / 4.0;
                    let (c, sn) = (a.cos(), a.sin());
                    rays.line(cx0 + 7.5 * s * c, cy0 + 7.5 * s * sn, cx0 + 10.5 * s * c, cy0 + 10.5 * s * sn, 2.0 * s);
                }
                rays.fill(cx.canvas, value);
            }
        }
    }
}

/// A glyph without interaction (beside the sliders).
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphView(pub Glyph);

impl Widget for GlyphView {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        self.0.draw(cx, b, 56, fg);
    }
}

/// Clock, date and battery.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    /// `09:40`.
    pub time: String,
    /// `Saturday 31 October`.
    pub date: String,
    /// (percent, charging); `None` without a battery.
    pub battery: Option<(u8, bool)>,
}

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

impl Header {
    /// The header for `now` and the battery.
    pub fn read(now: &DateTime, battery: &dyn ink_device::Battery) -> Self {
        let battery = battery.state().ok().map(|b| (b.percent, b.charging));
        Header {
            time: format!("{:02}:{:02}", now.hour, now.minute),
            date: format!(
                "{} {} {}",
                DAYS[now.weekday as usize % 7],
                now.day,
                MONTHS[(now.month.clamp(1, 12) - 1) as usize]
            ),
            battery,
        }
    }
}

impl Widget for Header {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        let tw = cx.text_width(Style::Bold, t.title.px, &self.time).ceil() as i32;
        cx.text_left(Rect::new(b.x, b.y, tw as u32 + 8, b.h), &self.time, t.title.px, Style::Bold, t.fg);
        let mut right = b.right();
        if let Some((percent, charging)) = self.battery {
            let text = format!("{percent}%");
            let bw = cx.text_width(Style::Bold, t.label.px, &text).ceil() as i32;
            let tr = Rect::new(right - bw, b.y, bw as u32, b.h);
            let base = cx.baseline_in(tr, t.label.px, Style::Bold);
            cx.draw_text(Style::Bold, t.label.px, tr.x as f32, base, &text, t.fg);
            let (iw, ih) = (54, 26);
            // Centred on the digits, not the header row.
            let mid = base - ink_ui::icon::DIGIT_MID * t.label.px;
            let ir = Rect::new(tr.x - 12 - iw, (mid - ih as f32 / 2.0).round() as i32, iw as u32, ih as u32);
            ink_ui::icon::draw_battery(cx.canvas, ir, percent, charging, t.fg, t.bg);
            right = ir.x - 16;
        }
        let dx = b.x + tw + 24;
        if right > dx {
            let r = Rect::new(dx, b.y + 6, (right - dx) as u32, b.h);
            let base = cx.baseline_in(r, t.caption.px, Style::Regular);
            let date = cx.ellipsize(&self.date, t.caption.px, Style::Regular, r.w as f32);
            cx.draw_text(Style::Regular, t.caption.px, r.x as f32, base, &date, t.muted);
        }
    }
}

/// Sunset times or the custom hours with -/+ buttons.
#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    /// Mode.
    pub mode: AutoMode,
    /// Custom start.
    pub start: DayTime,
    /// Custom end.
    pub end: DayTime,
    /// Today's (sunset, sunrise).
    pub sun: (DayTime, DayTime),
}

impl Detail {
    /// (from minus, from plus, to minus, to plus) and the two time boxes, inside `b`.
    pub fn buttons(b: Rect) -> ([Rect; 4], [Rect; 2]) {
        let half = b.w / 2;
        let btn = 96u32;
        let y = b.y + (b.h as i32 - btn as i32) / 2;
        let one = |x0: i32| {
            // "From" caption 110, minus, time 170, plus.
            let minus = Rect::new(x0 + 110, y, btn, btn);
            let time = Rect::new(minus.right() + 8, b.y, 170, b.h);
            let plus = Rect::new(time.right() + 8, y, btn, btn);
            (minus, time, plus)
        };
        let (fm, ft, fp) = one(b.x);
        let (tm, tt, tp) = one(b.x + half as i32);
        ([fm, fp, tm, tp], [ft, tt])
    }
}

impl Widget for Detail {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        match self.mode {
            AutoMode::Off => {
                cx.text_left(b, "Warmth stays as you set it.", t.caption.px, Style::Regular, t.muted);
            }
            AutoMode::Sunset => {
                let text = format!("Warm from sunset {} to sunrise {}", self.sun.0, self.sun.1);
                cx.text_left(b, &text, t.body.px * 0.9, Style::Regular, t.fg);
            }
            AutoMode::Custom => {
                let (btns, times) = Detail::buttons(b);
                for (i, (cap, v)) in [("From", self.start), ("To", self.end)].into_iter().enumerate() {
                    let x0 = if i == 0 { b.x } else { b.x + (b.w / 2) as i32 };
                    cx.text_left(Rect::new(x0, b.y, 104, b.h), cap, t.label.px, Style::Regular, t.fg);
                    cx.text_centered(times[i], &v.to_string(), t.body.px, Style::Bold, t.fg);
                    for (r, icon) in [(btns[2 * i], Icon::Minus), (btns[2 * i + 1], Icon::Plus)] {
                        stroke_round_rect(cx.canvas, r, r.w as f32 / 2.0, t.border_px as f32, t.fg);
                        cx.icon(icon, r, r.w / 2, t.fg);
                    }
                }
            }
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let UiEvent::Gesture(Gesture::Tap { pos }) = ev else {
            return match ev {
                UiEvent::Gesture(_) => EventResult::Handled,
                _ => EventResult::Ignored,
            };
        };
        if self.mode != AutoMode::Custom {
            return EventResult::Handled;
        }
        let (btns, _) = Detail::buttons(b);
        let acts = [(FROM, -1), (FROM, 1), (TO, -1), (TO, 1)];
        // Touch targets reach 16 px around the round buttons.
        match btns.iter().position(|r| Rect::new(r.x - 16, b.y, r.w + 32, b.h).contains(*pos)) {
            Some(i) => EventResult::Action(format!("{}:{}", acts[i].0, acts[i].1)),
            None => EventResult::Handled,
        }
    }
}

/// A quick settings tile: a toggle (`on` = `Some`) or an action (`None`).
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    /// Picture.
    pub glyph: Glyph,
    /// Name (bold).
    pub label: String,
    /// State line under the name (`On`, `Off`, the network, `Portrait`...).
    pub state: String,
    /// Toggle state; `None` for action tiles (outlined).
    pub on: Option<bool>,
    /// Reported on tap.
    pub action: String,
    /// A long press reports [`advanced::LONG`]`:<name>` (three dots in the corner).
    pub long: bool,
    /// A finger rests on it (inverted, DU): from [`ink_ui::TouchKind::Press`] on.
    pub pressed: bool,
    /// The last event changed the look (pressed state).
    changed: bool,
}

impl Tile {
    /// A tile.
    pub fn new(glyph: Glyph, label: &str, state: impl Into<String>, on: Option<bool>, action: impl Into<String>) -> Self {
        Tile { glyph, label: label.into(), state: state.into(), on, action: action.into(), long: false, pressed: false, changed: false }
    }
}

impl Widget for Tile {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        let filled = (self.on == Some(true)) != self.pressed;
        let (bg, ink) = if filled { (t.fg, t.bg) } else { (t.bg, t.fg) };
        fill_round_rect(cx.canvas, b, t.radius_lg, bg);
        if !filled {
            stroke_round_rect(cx.canvas, b, t.radius_lg, t.border_px as f32, t.fg);
        }
        self.glyph.draw(cx, Rect::new(b.x, b.y + 16, b.w, 64), 56, ink);
        let inner = |y: i32, h: u32| Rect::new(b.x + 12, b.y + y, b.w.saturating_sub(24), h);
        // A label a little too wide ("Refresh screen" in a 4-column grid) is set smaller,
        // down to the caption size, before it is cut with an ellipsis.
        let room = b.w.saturating_sub(24) as f32;
        let w = cx.text_width(Style::Bold, t.label.px, &self.label);
        let lpx = if w > room { (t.label.px * room / w).floor().max(t.caption.px) } else { t.label.px };
        let label = cx.ellipsize(&self.label, lpx, Style::Bold, room);
        cx.text_centered(inner(84, 44), &label, lpx, Style::Bold, ink);
        let state = cx.ellipsize(&self.state, t.caption.px, Style::Regular, b.w.saturating_sub(24) as f32);
        cx.text_centered(inner(128, 32), &state, t.caption.px, Style::Regular, ink);
        if self.long {
            // Three dots in the top right corner: a long press opens more options.
            for i in 0..3 {
                fill_round_rect(cx.canvas, Rect::new(b.right() - 52 + i * 14, b.y + 16, 8, 8), 4.0, ink);
            }
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Touch(t) => {
                // No feedback on touch-down: the touch may still become a swipe.
                let now = t.press_on(self.pressed, b.contains(t.pos));
                if now != self.pressed {
                    self.pressed = now;
                    self.changed = true;
                    EventResult::Handled
                } else if t.kind == TouchKind::Down {
                    // Capture the contact (for its Press) without a repaint.
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Gesture(Gesture::Tap { .. }) => {
                self.changed |= std::mem::take(&mut self.pressed);
                EventResult::Action(self.action.clone())
            }
            UiEvent::Gesture(Gesture::LongPress { .. }) if self.long => {
                self.changed |= std::mem::take(&mut self.pressed);
                let name = self.action.split_once(':').map_or("", |(_, n)| n);
                EventResult::Action(format!("{}:{name}", advanced::LONG))
            }
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        if self.pressed {
            RefreshClass::Du
        } else {
            RefreshClass::Gl16
        }
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        // An event that changed nothing (a touch-down, a quick tap) refreshes nothing.
        if std::mem::take(&mut self.changed) {
            None
        } else {
            Some(Vec::new())
        }
    }
}

/// The layer under the panel: a tap below the panel closes it ([`CLOSE`]). Every other
/// touch and gesture is ignored, so a touch on an empty part of the panel marks nothing
/// dirty (a `Scrim` would repaint and refresh the whole screen with GL16 on every touch).
#[derive(Clone, Debug, PartialEq)]
pub struct Backdrop {
    /// The panel; taps inside it do not close.
    pub panel: Rect,
}

impl Widget for Backdrop {
    fn paint(&mut self, _cx: &mut PaintCx, _b: Rect) {}

    fn opaque(&self) -> bool {
        false
    }

    /// It draws nothing: being added (full screen) must not refresh the whole screen,
    /// which would make the opening a screen-change flash.
    fn adds_damage(&self) -> bool {
        false
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Tap { pos }) if !self.panel.contains(*pos) => EventResult::Action(CLOSE.into()),
            _ => EventResult::Ignored,
        }
    }
}

/// The panel's background: paper, a black edge along the bottom, the grab handle and
/// the status line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelBack;

impl Widget for PanelBack {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        let edge = Rect::new(b.x, b.bottom() - t.rule_px, b.w, t.rule_px as u32);
        fill_round_rect(cx.canvas, edge, 0.0, t.fg);
    }
}

/// The Close button at the right end of the status row: an X icon; a tap reports
/// [`CLOSE`]. It gives no feedback (the panel closes at once).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CloseButton;

impl Widget for CloseButton {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let ink = cx.theme.fg;
        cx.icon(Icon::Close, b, 48, ink);
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Tap { .. }) => EventResult::Action(CLOSE.into()),
            _ => EventResult::Ignored,
        }
    }
}

/// The status line at the bottom of the panel (grab handle and a message). It leaves a
/// [`CLOSE_SIDE`] wide column at each end (the right one is the Close button) so the
/// text stays centred.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// Message; empty shows the closing hint.
    pub text: String,
}

impl Widget for Status {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        let handle = Rect::new(b.x + (b.w as i32 - 120) / 2, b.y + 8, 120, 12);
        fill_round_rect(cx.canvas, handle, 6.0, t.subtle);
        let side = CLOSE_SIDE;
        let r = Rect::new(b.x + side as i32, b.y + 24, b.w.saturating_sub(2 * side), b.h.saturating_sub(30));
        if self.text.is_empty() {
            cx.text_centered(r, "Swipe up to close", t.caption.px, Style::Regular, t.muted);
        } else {
            let text = cx.ellipsize(&self.text, t.caption.px, Style::Bold, r.w as f32);
            cx.text_centered(r, &text, t.caption.px, Style::Bold, t.fg);
        }
    }
}

// -- Screen ------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Ids {
    header: WidgetId,
    bright: Option<WidgetId>,
    warm: Option<WidgetId>,
    choice: WidgetId,
    detail: WidgetId,
    status: WidgetId,
}

/// The control center as a screen ([`crate::router::Route::QuickLight`]): drawn over what
/// the screen below left on the canvas; the Close button, a tap below the panel, a swipe up, Back or the
/// power key close it, and the screen below repaints in its `on_enter`.
#[derive(Default)]
pub struct ControlCenterScreen {
    tree: WidgetTree,
    ids: Option<Ids>,
    layout: Option<Layout>,
    /// Show the whole screen with GC16 after the next paint (night mode switched).
    flash_all: bool,
    /// Minute shown in the header.
    minute: Option<(u8, u8)>,
    /// The tiles: button name and widget, in panel order.
    tiles: Vec<(String, WidgetId)>,
    /// Last live write of a dragged slider: (slider, value, when).
    live: Option<(WidgetId, u32, Instant)>,
    /// The device state read the panel shows ([`Light::state_generation`]) and when
    /// the panel asked for a fresh one (the panel opens with the last known values).
    state_read: Option<(u64, Instant)>,
    /// The open long-press popup ([`advanced`], or a plugin tile's menu).
    picker: Option<OpenPicker>,
    /// When the 30 s timer work (header clock, warmth) last ran.
    slow_tick: Option<Instant>,
}

/// A tile of a plugin (`plugin:<plugin>:<id>`).
fn is_plugin_tile(name: &str) -> bool {
    ink_plugin::registry::parse_saved_id(name).is_some()
}

/// A long-press popup on the panel: its button, widgets and what its rows do.
struct OpenPicker {
    name: String,
    id: WidgetId,
    catcher: WidgetId,
    rows: PickerRows,
}

/// What the rows of an open popup do.
enum PickerRows {
    /// A built-in button's options: the choice of each row ([`advanced::choose`]).
    Builtin(Vec<advanced::Choice>),
    /// A plugin tile's menu (the plugin's menus are claimed while it is open): rows
    /// answer request `menu` of `plugin` with the item's action ([`popup::menu_pick`]).
    Plugin {
        /// Plugin name.
        plugin: String,
        /// Request id of the menu.
        menu: u32,
        /// `(label, action)` items.
        items: Vec<(String, String)>,
    },
}

/// How long the panel polls for the background read of the device state.
const STATE_WAIT: Duration = Duration::from_secs(4);
/// Poll interval while that read runs.
const STATE_POLL: Duration = Duration::from_millis(150);
/// The header clock and warmth label timer.
const SLOW_TICK: Duration = Duration::from_secs(30);
/// How often plugin tiles are re-read: a plugin's state can change on its own (the SSH
/// server's address after USB networking goes off).
const PLUGIN_POLL: Duration = Duration::from_secs(2);

/// The old name of [`ControlCenterScreen`].
pub type QuickLightScreen = ControlCenterScreen;

fn on_off(on: bool) -> &'static str {
    if on {
        "On"
    } else {
        "Off"
    }
}

impl ControlCenterScreen {
    /// The tile of button `name` (a name of [`CONTROL_BUTTONS`]) with its current state.
    fn make_tile(name: &str, cx: &mut AppCtx) -> Tile {
        let settings = cx.settings_update.as_ref().unwrap_or(cx.settings);
        let rot = crate::app::settings_rotation(settings);
        let bars_on = match crate::screens::reader::reader_bars_hidden() {
            Some(hidden) => !hidden && (settings.status_bars.top.enabled || settings.status_bars.bottom.enabled),
            None => settings.status_bars.top.enabled || settings.status_bars.bottom.enabled,
        };
        let a = format!("{TILE}:{name}");
        let caption = label(CONTROL_BUTTONS, name);
        let toggle = |glyph: Glyph, on: bool| Tile::new(glyph, caption, on_off(on), Some(on), a.clone());
        let mut tile = match name {
            "night_mode" => {
                let night = cx.screen.night_mode();
                toggle(Glyph::Contrast, night)
            }
            "auto_warmth" => {
                let has_warm = cx.services.light.warmth().is_ok();
                let on = with_auto_warmth(&mut *cx.services.light, |w, _| w.mode != AutoMode::Off);
                let mut t = toggle(Glyph::WarmSun, on);
                if !has_warm {
                    t.state = "Not available".into();
                }
                t
            }
            "wifi" => {
                let wifi = cx.services.wifi.state().ok();
                let airplane = cx.services.wifi.airplane_mode().ok();
                let wifi_on = wifi.as_ref().is_some_and(|w| w.enabled) && airplane != Some(true);
                let state = match &wifi {
                    None => "Not available".to_string(),
                    Some(w) if wifi_on => w.essid.clone().unwrap_or_else(|| "On".into()),
                    Some(_) => "Off".into(),
                };
                Tile::new(Glyph::Icon(Icon::Wifi), caption, state, Some(wifi_on), a)
            }
            "rotate" => {
                let rot_name = match rot {
                    Rotation::Portrait => "Portrait",
                    Rotation::Clockwise => "Landscape",
                    Rotation::UpsideDown => "Upside down",
                    Rotation::CounterClockwise => "Landscape left",
                };
                Tile::new(Glyph::Icon(Icon::Rotate), caption, rot_name, None, a)
            }
            "full_refresh" => Tile::new(Glyph::Icon(Icon::Star), caption, "Clear ghosting", None, a),
            "status_bars" => toggle(Glyph::Icon(Icon::Info), bars_on),
            "screenshot" => Tile::new(Glyph::Icon(Icon::Download), caption, "Save the screen", None, a),
            "sleep" => Tile::new(Glyph::Icon(Icon::Night), caption, "Now", None, a),
            "battery" => {
                let state = cx.services.battery.state().map_or_else(|_| "Not available".into(), |b| if b.charging { format!("{}%, charging", b.percent) } else { format!("{}%", b.percent) });
                Tile::new(Glyph::Icon(Icon::Battery), caption, state, None, a)
            }
            "settings" => Tile::new(Glyph::Icon(Icon::Settings), caption, "All settings", None, a),
            _ => match ink_plugin::registry::parse_saved_id(name).and_then(|(p, id)| cx.plugins.control_tile(p, id)) {
                // A tile of a plugin: icon, caption and state come from the plugin.
                Some((t, st)) => {
                    let icon = crate::plugin_host::icon_named(&t.icon).unwrap_or_else(|| {
                        log::warn!("control center: tile {name}: no icon called {:?}", t.icon);
                        Icon::Info
                    });
                    let mut tile = Tile::new(Glyph::Icon(icon), &t.label, st.text, st.on, a);
                    tile.long = t.hold.is_some();
                    return tile;
                }
                None => Tile::new(Glyph::Icon(Icon::Info), caption, "Not available", None, a),
            },
        };
        tile.long = advanced::has_options(name);
        tile
    }

    fn detail(cx: &mut AppCtx) -> Detail {
        let now = cx.services.clock.local_now();
        with_auto_warmth(&mut *cx.services.light, |a, _| Detail { mode: a.mode, start: a.start, end: a.end, sun: AutoWarmth::sun(&now) })
    }

    fn build(&mut self, cx: &mut AppCtx) {
        let size = cx.screen.size();
        let names = buttons(cx.settings_update.as_ref().unwrap_or(cx.settings), cx.plugins);
        let lay = layout(size, names.len());
        let mut tree = WidgetTree::new();
        tree.add(None, Box::new(Backdrop { panel: lay.panel }), Rect::from_size(size));
        // Only the edge strip: a removed picker repaints its own rect, not the whole panel
        // (the panel is cleared by `invalidate` on enter and resize).
        let rule = cx.theme.rule_px;
        tree.add(None, Box::new(PanelBack), Rect::new(lay.panel.x, lay.panel.bottom() - rule, lay.panel.w, rule as u32));
        let now = cx.services.clock.local_now();
        self.minute = Some((now.hour, now.minute));
        let header = tree.add(None, Box::new(Header::read(&now, &*cx.services.battery)), lay.header);
        let q = QuickLight::from_services(&cx.services);
        let mut slider = |s: Option<Slider>, icon: Rect, r: Rect, glyph: Glyph, missing: &str| match s {
            Some(s) => {
                tree.add(None, Box::new(GlyphView(glyph)), icon);
                Some(tree.add(None, Box::new(s), r))
            }
            None => {
                let px = cx.theme.caption.px;
                tree.add(None, Box::new(Label::left(missing, px)), Rect::new(icon.x, r.y, r.right() as u32 - icon.x as u32, r.h));
                None
            }
        };
        let bright = slider(q.brightness, lay.bright_icon, lay.bright, Glyph::Icon(Icon::Light), "Frontlight not available");
        let warm = slider(q.warmth, lay.warm_icon, lay.warm, Glyph::WarmSun, "Warm light not available");
        tree.add(None, Box::new(Label::left("Auto warmth", cx.theme.label.px).styled(Style::Bold)), lay.auto_label);
        let detail = Self::detail(cx);
        let choice = tree.add(
            None,
            Box::new(Segmented::new(&["Off", "Sunset", "Custom"], AUTO).with_selected(detail.mode.index())),
            lay.auto_choice,
        );
        let detail = tree.add(None, Box::new(detail), lay.detail);
        let mut tiles = Vec::with_capacity(names.len());
        for (name, r) in names.into_iter().zip(&lay.tiles) {
            let t = Self::make_tile(&name, cx);
            tiles.push((name, tree.add(None, Box::new(t), *r)));
        }
        let status = tree.add(None, Box::new(Status::default()), lay.status);
        tree.add(None, Box::new(CloseButton), lay.close);
        self.tree = tree;
        self.tiles = tiles;
        self.live = None;
        self.picker = None;
        self.ids = Some(Ids { header, bright, warm, choice, detail, status });
        self.layout = Some(lay);
    }

    /// Shows `text` in the status line and logs it (the result of a tile action).
    fn set_status(&mut self, text: impl Into<String>) {
        let text = text.into();
        log::info!("control center: {text}");
        let Some(ids) = self.ids else { return };
        if let Some(s) = self.tree.get_mut::<Status>(ids.status) {
            s.text = text;
        }
        self.tree.mark_dirty(ids.status, RefreshClass::Gl16);
    }

    fn refresh_tiles(&mut self, cx: &mut AppCtx) {
        self.refresh_tiles_where(cx, |_| true);
    }

    /// Re-reads the tiles whose name passes `which`; repaints only the changed ones.
    fn refresh_tiles_where(&mut self, cx: &mut AppCtx, which: impl Fn(&str) -> bool) {
        for (name, id) in self.tiles.clone().into_iter().filter(|(n, _)| which(n)) {
            let new = Self::make_tile(&name, cx);
            if let Some(t) = self.tree.get_mut::<Tile>(id) {
                if *t != new {
                    *t = new;
                    self.tree.mark_dirty(id, RefreshClass::Gl16);
                }
            }
        }
    }

    /// Shows the warmth the device has now (after auto warmth changed it).
    fn refresh_warmth(&mut self, cx: &mut AppCtx) {
        let Ok(v) = cx.services.light.warmth() else { return };
        self.show_warmth(v);
    }

    /// Shows the device state of the background read (only the changed tiles and
    /// sliders repaint).
    fn apply_state_read(&mut self, cx: &mut AppCtx) {
        cx.services.light.set_cached_reads(true);
        self.refresh_tiles(cx);
        self.refresh_detail(cx);
        let bright = cx.services.light.brightness().ok();
        let warm = cx.services.light.warmth().ok();
        cx.services.light.set_cached_reads(false);
        if let Some(v) = warm {
            self.show_warmth(v);
        }
        if let (Some(v), Some(id)) = (bright, self.ids.and_then(|i| i.bright)) {
            self.show_value(id, v);
        }
    }

    /// Moves the warmth slider to `v` (unless a drag is on).
    fn show_warmth(&mut self, v: u32) {
        let Some(id) = self.ids.and_then(|i| i.warm) else { return };
        self.show_value(id, v);
    }

    /// Moves slider `id` to `v` (unless a drag is on).
    fn show_value(&mut self, id: WidgetId, v: u32) {
        if let Some(s) = self.tree.get_mut::<Slider>(id) {
            if s.value != v as f32 && s.refresh_class() != RefreshClass::Du {
                s.value = v as f32;
                self.tree.mark_dirty(id, RefreshClass::Gl16);
            }
        }
    }

    fn refresh_detail(&mut self, cx: &mut AppCtx) {
        let Some(ids) = self.ids else { return };
        let d = Self::detail(cx);
        if let Some(s) = self.tree.get_mut::<Segmented>(ids.choice) {
            s.selected = d.mode.index();
        }
        if let Some(w) = self.tree.get_mut::<Detail>(ids.detail) {
            *w = d;
        }
        self.tree.mark_dirty(ids.choice, RefreshClass::Gl16);
        self.tree.mark_dirty(ids.detail, RefreshClass::Gl16);
    }

    fn change_auto(&mut self, cx: &mut AppCtx, f: impl FnOnce(&mut AutoWarmth)) {
        let now = cx.services.clock.local_now();
        let root = cx.paths.root.clone();
        let w = with_auto_warmth(&mut *cx.services.light, |a, light| {
            let w = a.change(light, &now, f);
            a.store_sunset(&root);
            w
        });
        self.refresh_detail(cx);
        // The value change() left, not a fresh read (powerd's passing value, see change()).
        if let Some(w) = w {
            self.show_warmth(w);
        }
    }

    /// Writes the light while slider `id` is dragged (see [`should_write`]); only the
    /// slider repaints (DU, marked by the tree), nothing else on the panel.
    fn live_write(&mut self, id: WidgetId, cx: &mut AppCtx) {
        let Some(ids) = self.ids else { return };
        let bright = ids.bright == Some(id);
        if !bright && ids.warm != Some(id) {
            return;
        }
        let Some(v) = self.tree.get_mut::<Slider>(id).map(|s| s.value.round().max(0.0) as u32) else { return };
        let now = Instant::now();
        let last = self.live.filter(|(l, _, _)| *l == id).map(|(_, v, at)| (v, at));
        if !should_write(last, v, now) {
            return;
        }
        let r = if bright { cx.services.light.set_brightness_live(v) } else { cx.services.light.set_warmth_live(v) };
        match r {
            Ok(()) => log::info!("control center: live {} {v} in {} us", if bright { "brightness" } else { "warmth" }, now.elapsed().as_micros()),
            Err(e) => log::warn!("control center: live {}: {e}", if bright { "brightness" } else { "warmth" }),
        }
        self.live = Some((id, v, now));
    }

    /// Changes the settings with `f` (stored and saved by the app after this event).
    fn edit(cx: &mut AppCtx, f: impl FnOnce(&mut ink_store::Settings)) {
        let mut s = cx.settings_update.take().unwrap_or_else(|| cx.settings.clone());
        f(&mut s);
        *cx.settings_update = Some(s);
    }

    /// Saves the screen as a PNG in `<root>/screenshots/` and returns its path.
    fn screenshot(cx: &mut AppCtx) -> anyhow::Result<std::path::PathBuf> {
        let t0 = Instant::now();
        let shot = cx.screen.screenshot()?;
        let n = cx.services.clock.local_now();
        let dir = cx.paths.root.join("screenshots");
        std::fs::create_dir_all(&dir)?;
        let name = format!("rinky-{:04}{:02}{:02}-{:02}{:02}{:02}.png", n.year, n.month, n.day, n.hour, n.minute, n.second);
        let path = dir.join(name);
        shot.save_png(&path)?;
        log::info!("control center: screenshot {} ({}x{}) in {} ms", path.display(), shot.width, shot.height, t0.elapsed().as_millis());
        Ok(path)
    }

    /// Shows `text` in the status line at once (before a call that blocks).
    fn say_now(&mut self, text: &str, cx: &mut AppCtx) {
        self.set_status(text);
        for d in self.tree.paint(&mut PaintCx { canvas: cx.canvas, fonts: cx.fonts, theme: cx.theme }) {
            if let Err(e) = cx.show(d.rect, d.class) {
                log::warn!("control center: show: {e}");
            }
        }
    }

    fn picker_area(&self) -> Option<Rect> {
        self.layout.as_ref().map(picker_area)
    }

    /// A long press on tile `name`: its options in the settings popup
    /// ([`advanced::options`]; a plugin tile: the plugin's menu, [`Self::plugin_hold`]).
    /// Never a page.
    fn long_press(&mut self, name: &str, cx: &mut AppCtx) -> Transition {
        log::info!("control center: long press {name}");
        // A plugin tile's name is its saved id, `plugin:<plugin>:<id>`.
        if let Some((plugin, id)) = ink_plugin::registry::parse_saved_id(name) {
            self.plugin_hold(name, plugin, id, cx);
            return Transition::None;
        }
        let Some(area) = self.picker_area() else { return Transition::None };
        if let Some(o) = advanced::options(name, advanced::max_items(area.h), cx) {
            let choices = o.items.iter().map(|i| i.choice.clone()).collect();
            self.show_picker(name, o.popup(), area, PickerRows::Builtin(choices), cx);
        }
        Transition::None
    }

    /// Shows `popup` (replacing an open one) at the top of `area`: one GL16 update of
    /// its box (and of the old box). The same menu again only repaints its changed rows.
    fn show_picker(&mut self, name: &str, mut popup: SettingsPopup, area: Rect, rows: PickerRows, cx: &mut AppCtx) {
        let r = popup.frame(area, Anchor::Top);
        if let Some(p) = self.picker.as_mut() {
            let same = p.name == name && self.tree.bounds(p.id) == Some(r);
            if let Some(old) = self.tree.get_mut::<SettingsPopup>(p.id).filter(|o| same && o.title == popup.title && o.list.rows.len() == popup.list.rows.len()) {
                // The new state of the same options: only the rows that changed repaint.
                for (i, row) in std::mem::take(&mut popup.list.rows).into_iter().enumerate() {
                    old.set_row(i, row);
                }
                old.set_note(popup.note);
                p.rows = rows;
                self.tree.mark_changed(p.id, RefreshClass::Gl16);
                log::info!("control center: popup {name} updated in place");
                return;
            }
        }
        if let Some(p) = self.picker.take() {
            self.tree.remove(p.id);
            self.tree.remove(p.catcher);
        }
        let close = popup.close_action.clone();
        let catcher = self.tree.add(None, Box::new(PopupCatcher { popup: r, action: close }), Rect::from_size(cx.screen.size()));
        let n = popup.list.rows.len();
        let id = self.tree.add(None, Box::new(popup), r);
        log::info!("control center: popup {name} with {n} rows at {r:?}");
        self.picker = Some(OpenPicker { name: name.to_string(), id, catcher, rows });
    }

    /// Removes the popup: the panel under it repaints (one GL16 update). A plugin's menu
    /// is cancelled (answer `""`) and its menus go back to the overlay.
    fn close_picker(&mut self, cx: &mut AppCtx) {
        if let Some(p) = self.picker.take() {
            log::info!("control center: popup {} closed", p.name);
            self.tree.remove(p.id);
            self.tree.remove(p.catcher);
            if let PickerRows::Plugin { plugin, menu, .. } = &p.rows {
                cx.plugins.answer(plugin, *menu, "", None);
                cx.plugins.claim_menus(None);
            }
            self.refresh_tiles(cx);
        }
    }

    /// A tap in the popup (`arg`: row index, a switch adds `:on`/`:off`; or `close`).
    fn pick(&mut self, arg: &str, cx: &mut AppCtx) -> Transition {
        let Some(p) = &self.picker else { return Transition::None };
        if arg == "close" {
            self.close_picker(cx);
            return Transition::None;
        }
        let PickerRows::Builtin(choices) = &p.rows else { return Transition::None };
        let name = p.name.clone();
        let index = arg.split(':').next().and_then(|i| i.parse::<usize>().ok());
        let Some(choice) = index.and_then(|i| choices.get(i)).cloned() else { return Transition::None };
        if let Some(text) = advanced::busy_text(&choice) {
            self.say_now(&text, cx);
        }
        match advanced::choose(&choice, cx) {
            advanced::Outcome::Done(text) => {
                self.close_picker(cx);
                self.set_status(text);
            }
            advanced::Outcome::Again(text) => {
                let again = self.picker_area().and_then(|a| advanced::options(&name, advanced::max_items(a.h), cx).map(|o| (a, o)));
                if let Some((area, o)) = again {
                    let choices = o.items.iter().map(|i| i.choice.clone()).collect();
                    self.show_picker(&name, o.popup(), area, PickerRows::Builtin(choices), cx);
                }
                self.refresh_tiles(cx);
                self.set_status(text);
            }
            advanced::Outcome::Settings(page) => return advanced::open_settings(&page),
        }
        Transition::None
    }

    /// A long press on a plugin tile with a hold action ([`ink_plugin::ControlTile::on_hold`]):
    /// calls the plugin's `on_action(<hold>)` with its menus claimed, and shows the menu
    /// it answers with (usually its settings, `settings:<page>`) in the same settings
    /// popup as the built-in buttons. Dialogs and viewers it opens go to the overlay.
    fn plugin_hold(&mut self, name: &str, plugin: &str, id: &str, cx: &mut AppCtx) {
        let Some((tile, _)) = cx.plugins.control_tile(plugin, id) else { return };
        let Some(hold) = tile.hold.clone() else { return };
        if cx.plugins.turned_off(plugin) {
            self.set_status(format!("{} is turned off (Settings, Plugins)", tile.label));
            return;
        }
        cx.plugins.claim_menus(Some(plugin));
        if !cx.plugins.trigger(plugin, &hold, None) {
            self.set_status(format!("{} failed", tile.label));
        }
        if !self.take_plugin_menus(name, cx) {
            // No menu (a dialog or nothing): the overlay shows what the plugin sends.
            cx.plugins.claim_menus(None);
        }
        self.refresh_tiles(cx);
    }

    /// Shows the newest menu the claimed plugin sent (in the popup) and applies label
    /// changes to its rows. `name`: the tile. True when a menu is shown.
    fn take_plugin_menus(&mut self, name: &str, cx: &mut AppCtx) -> bool {
        if cx.plugins.claimed_menus().is_none() {
            return false;
        }
        let got = cx.plugins.take_claimed_menus();
        self.apply_plugin_menus(name, got, cx)
    }

    /// [`Self::take_plugin_menus`] for the requests `got`.
    fn apply_plugin_menus(&mut self, name: &str, got: Vec<ink_plugin::UiRequest>, cx: &mut AppCtx) -> bool {
        let Some(plugin) = cx.plugins.claimed_menus().map(str::to_string) else { return false };
        let open = self.picker.as_ref().map(|p| p.id);
        let open_title = open.and_then(|id| self.tree.get_mut::<SettingsPopup>(id).map(|p| p.title.clone()));
        let mut menu = match self.picker.as_ref().map(|p| &p.rows) {
            Some(PickerRows::Plugin { menu, items, .. }) => Some((*menu, open_title.unwrap_or_default(), items.clone())),
            _ => None,
        };
        let mut changed = false;
        for req in got {
            match req {
                ink_plugin::UiRequest::Menu { id, title, items } => {
                    menu = Some((id, title, items));
                    changed = true;
                }
                ink_plugin::UiRequest::MenuLabel { id, action, label } => {
                    let item = menu.as_mut().filter(|(m, _, _)| *m == id).and_then(|(_, _, items)| items.iter_mut().find(|(_, a)| *a == action));
                    if let Some(item) = item {
                        item.0 = label;
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        let Some((id, title, items)) = menu else { return false };
        if !changed {
            return self.picker.is_some();
        }
        let Some(area) = self.picker_area() else { return false };
        let popup = SettingsPopup::new(title, popup::menu_rows(&items)).with_close(advanced::PICK_CLOSE);
        self.show_picker(name, popup, area, PickerRows::Plugin { plugin, menu: id, items }, cx);
        true
    }

    /// A tap on a row of a plugin's menu (`menu:<i>`): answers the menu with the item's
    /// action. The plugin's next menu (its new state) is shown in place (only changed
    /// rows repaint); without one the popup closes.
    fn plugin_pick(&mut self, a: &str, cx: &mut AppCtx) {
        let Some(p) = &self.picker else { return };
        let PickerRows::Plugin { plugin, menu, items } = &p.rows else { return };
        let Some(pick) = popup::menu_pick(a, items) else { return };
        let (name, plugin, menu) = (p.name.clone(), plugin.clone(), *menu);
        log::info!("control center: {plugin} menu {menu}: {pick}");
        if !cx.plugins.answer(&plugin, menu, &pick, None) {
            self.set_status(format!("{plugin} failed"));
        }
        let got = cx.plugins.take_claimed_menus();
        if got.iter().any(|r| matches!(r, ink_plugin::UiRequest::Menu { .. })) {
            self.apply_plugin_menus(&name, got, cx);
            self.refresh_tiles(cx);
            return;
        }
        // Answered and no new menu: close without cancelling it.
        if let Some(p) = self.picker.as_mut() {
            p.rows = PickerRows::Builtin(Vec::new());
        }
        cx.plugins.claim_menus(None);
        self.close_picker(cx);
    }


    /// A tap on a plugin tile (`plugin:<plugin>:<id>`): checks Wi-Fi when the tile needs
    /// it, calls the plugin's `on_action(<id>)` and shows the tile's new state.
    fn plugin_tile(&mut self, saved: &str, cx: &mut AppCtx) {
        let Some((plugin, id)) = ink_plugin::registry::parse_saved_id(saved) else { return };
        let Some((tile, _)) = cx.plugins.control_tile(plugin, id) else {
            self.set_status(format!("{plugin} is not available"));
            return;
        };
        if cx.plugins.turned_off(plugin) {
            self.set_status(format!("{} is turned off (Settings, Plugins)", tile.label));
            return;
        }
        if let Some(hint) = &tile.needs_wifi {
            if !cx.services.wifi.state().is_ok_and(|w| w.enabled) {
                self.set_status(hint.clone());
                return;
            }
        }
        if cx.plugins.trigger(plugin, id, None) {
            self.set_status(if tile.working.is_empty() { tile.label.clone() } else { tile.working.clone() });
        } else {
            self.set_status(format!("{} failed", tile.label));
        }
        self.refresh_tiles(cx);
    }

    fn tile(&mut self, name: &str, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        log::info!("control center: button {name}");
        match name {
            "wifi" => {
                let on = cx.services.wifi.state().is_ok_and(|w| w.enabled) && cx.services.wifi.airplane_mode().ok() != Some(true);
                match cx.services.wifi.set_enabled(!on) {
                    Ok(()) => self.set_status(if on { "Wi-Fi off" } else { "Wi-Fi on" }),
                    Err(e) => self.set_status(format!("Wi-Fi: {e}")),
                }
                self.refresh_tiles(cx);
            }
            "night_mode" => {
                let on = !cx.screen.night_mode();
                Self::edit(cx, |s| s.device.night_mode = on);
                match cx.screen.set_night_mode(on) {
                    Ok(()) => self.set_status(if on { "Night mode on" } else { "Night mode off" }),
                    Err(e) => self.set_status(format!("Night mode: {e}")),
                }
                // The whole screen inverts, the page below too.
                self.flash_all = true;
                self.refresh_tiles(cx);
            }
            "auto_warmth" => {
                if cx.services.light.warmth().is_err() {
                    self.set_status("Auto warmth: no warm light on this device");
                } else {
                    let on = with_auto_warmth(&mut *cx.services.light, |w, _| w.mode != AutoMode::Off);
                    let mode = if on { AutoMode::Off } else { AutoMode::Sunset };
                    self.change_auto(cx, |w| w.mode = mode);
                    self.set_status(if on { "Auto warmth off" } else { "Auto warmth: sunset to sunrise" });
                    self.refresh_tiles(cx);
                }
            }
            "full_refresh" => {
                // paint() returns a GC16 of the whole screen.
                self.flash_all = true;
                self.set_status("Screen refreshed");
            }
            "status_bars" if crate::screens::reader::reader_bars_hidden().is_some() => {
                // Over a book: hide or show that reader's bars (not the saved settings).
                let hidden = crate::screens::reader::request_status_bars_toggle().unwrap_or(false);
                log::info!("control center: reader status bars {}", if hidden { "hidden" } else { "shown" });
                self.set_status(if hidden { "Status bars hidden" } else { "Status bars shown" });
                self.refresh_tiles(cx);
            }
            "status_bars" => {
                let s = cx.settings_update.as_ref().unwrap_or(cx.settings);
                let on = !(s.status_bars.top.enabled || s.status_bars.bottom.enabled);
                Self::edit(cx, |s| {
                    s.status_bars.top.enabled = on;
                    s.status_bars.bottom.enabled = on;
                });
                self.set_status(if on { "Status bars on" } else { "Status bars off" });
                self.refresh_tiles(cx);
            }
            "screenshot" => match Self::screenshot(cx) {
                Ok(p) => self.set_status(format!("Saved {}", p.display())),
                Err(e) => self.set_status(format!("Screenshot failed: {e}")),
            },
            other if ink_plugin::registry::parse_saved_id(other).is_some() => self.plugin_tile(other, cx),
            "rotate" => {
                // Upright to the chosen angle, any other angle back upright (advanced::rotate_tap).
                let s = cx.settings_update.as_ref().unwrap_or(cx.settings);
                let (now, deg) = (s.device.rotation, s.ui.rotate_degrees());
                let to = advanced::rotate_tap(now, deg);
                if to == now % 4 {
                    self.set_status("Rotate angle is 0°: hold Rotate to choose one");
                    return Ok(Transition::None);
                }
                Self::edit(cx, |s| s.device.rotation = to);
                log::info!("control center: rotate to {:?} (angle {deg})", crate::app::settings_rotation(cx.settings_update.as_ref().unwrap_or(cx.settings)));
                // The app turns the screen after this event; on_resize lays the panel out.
            }
            "sleep" => {
                return Ok(match cx.services.power.sleep_now() {
                    Ok(()) => {
                        log::info!("control center: sleep now");
                        Transition::Pop
                    }
                    Err(e) => {
                        self.set_status(format!("Sleep: {e}"));
                        Transition::None
                    }
                });
            }
            "settings" => {
                log::info!("control center: open settings");
                return Ok(Transition::Replace(Route::Settings));
            }
            "battery" => {
                log::info!("control center: open the battery page");
                crate::screens::settings::open_at("mod:battery");
                return Ok(Transition::Replace(Route::Settings));
            }
            other => log::warn!("control center: unknown button {other}"),
        }
        Ok(Transition::None)
    }
}

impl crate::router::AppScreen for ControlCenterScreen {
    fn name(&self) -> &'static str {
        "quicklight"
    }

    /// A plugin menu still open is cancelled and the plugin's menus go back to the overlay.
    fn on_exit(&mut self, cx: &mut AppCtx) {
        if self.picker.as_ref().is_some_and(|p| matches!(p.rows, PickerRows::Plugin { .. })) {
            self.close_picker(cx);
        }
    }

    fn on_enter(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        let t = Instant::now();
        let now = cx.services.clock.local_now();
        let root = cx.paths.root.clone();
        // Draw at once with the last known light levels and Wi-Fi state (each device
        // read is a process on the Kindle, about 15 of them took 900 ms); a background
        // read follows and the tiles that changed repaint (apply_state_read).
        cx.services.light.set_cached_reads(true);
        with_auto_warmth(&mut *cx.services.light, |a, light| a.load_sunset(&root, light, &now));
        let _ = tick_auto_warmth(None, &mut *cx.services.light, &*cx.services.clock);
        self.build(cx);
        cx.services.light.set_cached_reads(false);
        let generation = cx.services.light.state_generation();
        cx.services.light.refresh_state();
        self.state_read = Some((generation, Instant::now()));
        log::info!("control center open (built in {} ms)", t.elapsed().as_millis());
        // One GL16 update of the panel, no slide-in frames.
        if let Some(l) = &self.layout {
            self.tree.invalidate(l.panel, RefreshClass::Gl16);
        }
        Ok(())
    }

    /// Only the panel: a pop shows the screen below again there (with the night mode
    /// toggled or the screen turned, the app shows all of it).
    fn covered_rect(&self, screen: Rect) -> Rect {
        self.layout.as_ref().map_or(screen, |l| l.panel)
    }

    fn on_resize(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        // The canvas is new and blank: show it all, the page below comes back on close.
        self.build(cx);
        let all = cx.canvas.bounds();
        self.tree.invalidate(all, RefreshClass::Gc16);
        Ok(())
    }

    fn handle(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        if self.picker.is_some() {
            // Back and a swipe up close the picker, not the panel; the timers wait (a
            // tile or slider repainted under the picker would draw over it).
            match ev {
                UiEvent::Key(k) if k.pressed && k.key == ink_device::Key::Back => {
                    self.close_picker(cx);
                    return Ok(Transition::None);
                }
                // A swipe up outside the popup closes it; inside, its list jumps.
                UiEvent::Gesture(Gesture::Swipe { from, dir: Direction::Up, .. })
                    if self.picker.as_ref().and_then(|p| self.tree.bounds(p.id)).is_none_or(|b| !b.contains(*from)) =>
                {
                    self.close_picker(cx);
                    return Ok(Transition::None);
                }
                UiEvent::Timer(_) => return Ok(Transition::None),
                _ => {}
            }
        }
        match ev {
            UiEvent::Quit => return Ok(Transition::Exit),
            UiEvent::Key(k) if k.pressed && matches!(k.key, ink_device::Key::Power | ink_device::Key::Back) => {
                return Ok(Transition::Pop)
            }
            UiEvent::Gesture(Gesture::Swipe { from, dir: Direction::Up, .. })
                if self.layout.as_ref().is_some_and(|l| l.panel.contains(*from)) =>
            {
                log::info!("control center closed (swipe)");
                return Ok(Transition::Pop);
            }
            UiEvent::Timer(_) => {
                if let Some((generation, since)) = self.state_read {
                    if cx.services.light.state_generation() != generation {
                        self.state_read = None;
                        log::info!("control center: device state read after {} ms", since.elapsed().as_millis());
                        self.apply_state_read(cx);
                    } else if since.elapsed() > STATE_WAIT {
                        self.state_read = None;
                    }
                    // A poll for the read, not the 30 s timer.
                    return Ok(Transition::None);
                }
                self.refresh_tiles_where(cx, is_plugin_tile);
                let now = cx.services.clock.local_now();
                if self.minute != Some((now.hour, now.minute)) {
                    self.minute = Some((now.hour, now.minute));
                    if let Some(ids) = self.ids {
                        let h = Header::read(&now, &*cx.services.battery);
                        if let Some(w) = self.tree.get_mut::<Header>(ids.header) {
                            if *w != h {
                                *w = h;
                                self.tree.mark_dirty(ids.header, RefreshClass::Gl16);
                            }
                        }
                    }
                }
                // The warmth on the 30 s timer, not on every plugin tile poll.
                if self.slow_tick.is_none_or(|t| t.elapsed() + PLUGIN_POLL > SLOW_TICK) {
                    self.slow_tick = Some(Instant::now());
                    self.refresh_warmth(cx);
                }
                return Ok(Transition::None);
            }
            _ => {}
        }
        let r = self.tree.dispatch(ev);
        if let (UiEvent::Touch(t), Some((id, EventResult::Handled))) = (ev, &r) {
            match t.kind {
                TouchKind::Down | TouchKind::Move | TouchKind::Hold => self.live_write(*id, cx),
                // A cancelled drag: the slider went back to its old value; write it.
                TouchKind::Cancel if self.live.is_some_and(|(l, _, _)| l == *id) => {
                    self.live = None;
                    self.live_write(*id, cx);
                    self.live = None;
                }
                _ => {}
            }
        }
        let Some((id, EventResult::Action(a))) = r else { return Ok(Transition::None) };
        if a == CLOSE {
            log::info!("control center closed");
            return Ok(Transition::Pop);
        }
        if let Some(text) = apply(&a, cx.services.light) {
            // The release (or a -/+ tap) writes the final value; the drag is over.
            if self.live.is_some_and(|(l, _, _)| l == id) {
                self.live = None;
            }
            log::info!("control center: {text}");
            if let Some(v) = a.strip_prefix(WARMTH).and_then(|v| v.strip_prefix(':')).and_then(|v| v.parse::<u32>().ok()) {
                let now = cx.services.clock.local_now();
                with_auto_warmth(&mut *cx.services.light, |w, light| w.manual(v, light, &now));
            }
            return Ok(Transition::None);
        }
        let (verb, arg) = a.split_once(':').unwrap_or((&a, ""));
        match verb {
            TILE => return self.tile(arg, cx),
            advanced::LONG => return Ok(self.long_press(arg, cx)),
            advanced::PICK => return Ok(self.pick(arg, cx)),
            popup::MENU_ROW => self.plugin_pick(&a, cx),
            AUTO => {
                let mode = AutoMode::from_index(arg.parse().unwrap_or(0));
                self.change_auto(cx, |w| w.mode = mode);
                self.set_status(match mode {
                    AutoMode::Off => "Auto warmth off",
                    AutoMode::Sunset => "Auto warmth: sunset to sunrise",
                    AutoMode::Custom => "Auto warmth: your hours",
                });
            }
            FROM | TO => {
                let d: i32 = arg.parse().unwrap_or(0);
                let shift = |t: DayTime| DayTime(((t.0 as i32 + d * HOUR_STEP_MIN as i32).rem_euclid(1440)) as u16);
                self.change_auto(cx, |w| {
                    if verb == FROM {
                        w.start = shift(w.start);
                    } else {
                        w.end = shift(w.end);
                    }
                });
            }
            _ => {}
        }
        Ok(Transition::None)
    }

    fn paint(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        // A plugin's menu that came later (after a network answer) or a label change.
        if let Some(name) = self.picker.as_ref().filter(|p| matches!(p.rows, PickerRows::Plugin { .. })).map(|p| p.name.clone()) {
            self.take_plugin_menus(&name, cx);
        }
        let damage = self.tree.paint(&mut PaintCx { canvas: cx.canvas, fonts: cx.fonts, theme: cx.theme });
        if std::mem::take(&mut self.flash_all) {
            return Ok(vec![Damage { rect: cx.canvas.bounds(), class: RefreshClass::Gc16 }]);
        }
        Ok(damage)
    }

    fn timeout(&self) -> Option<Duration> {
        // The device state read after opening; then the header clock and the warmth
        // label (the Kindle backend's amber timer applies the auto warmth schedule).
        // An open picker holds the timers back (see handle).
        if self.state_read.is_some() && self.picker.is_none() {
            return Some(STATE_POLL);
        }
        if self.tiles.iter().any(|(n, _)| is_plugin_tile(n)) {
            return Some(PLUGIN_POLL);
        }
        Some(SLOW_TICK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::settings::fakes::Fake;
    use ink_device::Point;

    #[test]
    fn trigger_needs_a_swipe_down_from_the_top_edge() {
        let swipe = |y, dir| UiEvent::Gesture(Gesture::Swipe { from: Point::new(600, y), to: Point::new(600, y + 400), dir });
        assert!(is_trigger(&swipe(10, Direction::Down)));
        assert!(!is_trigger(&swipe(400, Direction::Down)));
        assert!(!is_trigger(&swipe(10, Direction::Left)));
    }

    #[test]
    fn apply_sets_the_light_and_ignores_other_actions() {
        let mut f = Fake::default();
        assert_eq!(apply("ql-bright:20", &mut f).as_deref(), Some("Brightness 20"));
        assert_eq!(apply("warm:7", &mut f).as_deref(), Some("Warmth 7"));
        assert_eq!((f.0.borrow().brightness, f.0.borrow().warmth), (20, 7));
        assert_eq!(apply("sleep:5", &mut f), None);
        assert_eq!(apply("ql-bright:x", &mut f), None);
        let mut none = ink_device::services::Unsupported;
        assert!(apply("bright:3", &mut none).unwrap().starts_with("Brightness: "));
    }

    #[test]
    fn panel_without_a_light_has_no_sliders() {
        let q = QuickLight::read(&ink_device::services::Unsupported);
        assert!(q.brightness.is_none() && q.warmth.is_none());
        let q = QuickLight::read(&Fake::default());
        assert_eq!(q.brightness.unwrap().value, 12.0);
    }

    // -- Control center ---------------------------------------------------------------------

    use crate::app::App;
    use ink_device::headless::{self, HeadlessOptions, HeadlessProbe};
    use ink_device::script::parse;
    use ink_store::{Paths, Settings};
    use std::path::PathBuf;

    /// Portrait 1272 x 1696 (see [`layout`]) with the default six buttons (night, auto
    /// warmth, Wi-Fi, rotate / full refresh, sync): tile row 0 centres at y 852, row 1 at
    /// 1036, columns at x 177, 483, 789, 1095; brightness -/+ at (192|1192, 264), warmth at
    /// (192|1192, 464); Off / Sunset / Custom at (563|834|1105, 580); From -/+ at
    /// (190|472, 692), To -/+ at (794|1076, 692); the panel ends at y 1232.
    const OPEN: &str = "swipe 600 20 600 700\n";

    struct Cc {
        probe: HeadlessProbe,
        fake: std::rc::Rc<std::cell::RefCell<crate::screens::settings::fakes::FakeState>>,
        root: PathBuf,
    }

    fn run_cc(name: &str, script: &str) -> Cc {
        run_cc_with(name, script, Settings::default())
    }

    fn run_cc_with(name: &str, script: &str, settings: Settings) -> Cc {
        AUTO_WARMTH.with(|a| *a.borrow_mut() = None);
        let out_dir = std::env::var_os("INK_SETTINGS_OUT").map(|d| PathBuf::from(d).join(name));
        let (mut device, probe) =
            headless::open(HeadlessOptions { script: parse(&format!("{script}quit\n")).unwrap(), out_dir, ..Default::default() });
        let fake = crate::screens::settings::fakes::install(&mut device);
        let root = std::env::temp_dir().join(format!("ink-cc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        App::new(device, settings, Paths { root: root.clone() }).run(crate::router::Route::Demo).unwrap();
        Cc { probe, fake, root }
    }

    fn saved(c: &Cc) -> Settings {
        Settings::load(&c.root.join("settings.toml")).expect("settings.toml written")
    }

    fn saved_or_default(c: &Cc) -> Settings {
        Settings::load(&c.root.join("settings.toml")).unwrap_or_default()
    }

    #[test]
    fn layout_fits_portrait_and_landscape_with_touch_sized_targets() {
        for size in [Size::new(1272, 1696), Size::new(1696, 1272)] {
            let l = layout(size, MAX_BUTTONS);
            assert!(l.panel.h < size.h, "{size:?}: room below the panel to tap");
            assert!(l.tiles.iter().all(|t| t.w >= 96 && t.h >= 96 && l.panel.contains(Point::new(t.right() - 1, t.bottom() - 1))));
            assert!(l.status.bottom() <= l.panel.bottom());
            for w in l.tiles.windows(2) {
                assert!(w[0].intersect(&w[1]).is_empty());
            }
            let (btns, _) = Detail::buttons(l.detail);
            assert!(btns.iter().all(|b| b.w >= 96 && b.h >= 96 && l.detail.contains(Point::new(b.right() - 1, b.y))));
        }
        let l = layout(Size::new(1272, 1696), 6);
        let c = |r: Rect| Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
        assert_eq!((c(l.tiles[0]), c(l.tiles[4])), (Point::new(177, 852), Point::new(177, 1036)));
        assert_eq!(l.panel.h, 1232);
        // The Close button: a minimum-size square at the right end of the status row, inside the panel.
        assert!(l.close.w >= 96 && l.close.h >= 96 && l.close.right() <= l.status.right() && l.close.y == l.status.y && l.close.bottom() <= l.panel.bottom());
        // One row of tiles: a shorter panel.
        assert_eq!(layout(Size::new(1272, 1696), 3).panel.h, 1232 - 184);
        // Unknown and repeated names are skipped, at most MAX_BUTTONS.
        let mut s = Settings::default();
        let host = crate::plugin_host::PluginHost::new(Vec::new());
        s.ui.control_buttons = ["wifi", "bogus", "wifi", "sleep", "plugin:gone:x"].map(String::from).to_vec();
        assert_eq!(buttons(&s, &host), ["wifi", "sleep"]);
        assert_eq!(buttons_unchecked(&s), ["wifi", "sleep", "plugin:gone:x"]);
        s.ui.control_buttons = CONTROL_BUTTONS.iter().map(|(n, _)| n.to_string()).collect();
        assert_eq!(buttons(&s, &host).len(), MAX_BUTTONS);
    }

    #[test]
    fn live_writes_are_throttled_and_never_queued() {
        let t0 = Instant::now();
        assert!(should_write(None, 5, t0));
        assert!(!should_write(Some((5, t0)), 5, t0 + Duration::from_secs(1)), "same value");
        assert!(!should_write(Some((5, t0)), 6, t0 + Duration::from_millis(10)), "one step, too soon");
        assert!(should_write(Some((5, t0)), 7, t0 + Duration::from_millis(10)), "two steps");
        assert!(should_write(Some((5, t0)), 6, t0 + LIVE_EVERY), "time passed");
    }

    #[test]
    fn sun_times_follow_the_seasons() {
        let len = |m, d| {
            let (rise, set) = sun_times(2026, m, d, DEFAULT_LATITUDE, SOLAR_NOON);
            rise.minutes_until(set)
        };
        // 52 N: about 7 h 40 in December, 16 h 40 in June, 12 h 10 at the equinox.
        assert!((450..480).contains(&len(12, 21)), "{}", len(12, 21));
        assert!((980..1010).contains(&len(6, 21)), "{}", len(6, 21));
        assert!((720..745).contains(&len(3, 20)), "{}", len(3, 20));
        let (rise, set) = sun_times(2026, 11, 1, DEFAULT_LATITUDE, SOLAR_NOON);
        assert!(rise < SOLAR_NOON && SOLAR_NOON < set);
        // Polar night and day stay valid times.
        let (r, s) = sun_times(2026, 12, 21, 80.0, SOLAR_NOON);
        assert_eq!((r, s), (SOLAR_NOON, SOLAR_NOON));
        let (r, s) = sun_times(2026, 6, 21, 80.0, SOLAR_NOON);
        assert_eq!(r.minutes_until(s), 1438);
    }

    #[test]
    fn auto_warmth_applies_the_custom_hours_and_gives_the_warmth_back() {
        let mut f = Fake::default();
        let at = |h: i64, m: i64| DateTime::from_unix(1_793_404_800 + h * 3600 + m * 60, 0);
        let mut a = AutoWarmth::default();
        assert_eq!(a.tick(&mut f, &at(21, 0)), None, "off: no timer");
        a.change(&mut f, &at(18, 0), |a| a.mode = AutoMode::Custom);
        assert_eq!(f.0.borrow().warmth, 4, "outside 20:00-08:00 the user's warmth stays");
        // 22:00 inside the period (ramp 30 min): the level.
        assert!(a.tick(&mut f, &at(22, 0)).is_some());
        assert_eq!(f.0.borrow().warmth, 10);
        // The user picks 15 at night: kept and becomes the level.
        f.set_warmth(15).unwrap();
        a.manual(15, &mut f, &at(22, 5));
        a.tick(&mut f, &at(22, 10));
        assert_eq!((f.0.borrow().warmth, a.level), (15, 15));
        // Morning: back to the user's own warmth.
        a.tick(&mut f, &at(9, 0));
        assert_eq!(f.0.borrow().warmth, 4);
        // Turning it off in the period gives the warmth back at once.
        a.change(&mut f, &at(23, 0), |a| a.mode = AutoMode::Sunset);
        assert_eq!(f.0.borrow().warmth, 15);
        a.change(&mut f, &at(23, 1), |a| a.mode = AutoMode::Off);
        assert_eq!(f.0.borrow().warmth, 4);
        // Without a warm light there is no timer.
        let mut none = ink_device::services::Unsupported;
        let mut b = AutoWarmth { mode: AutoMode::Custom, ..AutoWarmth::default() };
        assert_eq!(b.tick(&mut none, &at(22, 0)), None);
    }

    #[test]
    fn auto_warmth_schedule_round_trips_through_powerd_values() {
        let d = DateTime::from_unix(1_793_404_800, 3600);
        let a = AutoWarmth { mode: AutoMode::Sunset, level: 12, ..AutoWarmth::default() };
        let s = a.schedule(&d);
        assert_eq!((s.enabled, s.mode.as_powerd(), s.level), (true, "Manual", 12));
        assert_eq!((s.start, s.end), AutoWarmth::sun(&d));
        let c = AutoWarmth { mode: AutoMode::Custom, start: DayTime::hm(21, 30), ..AutoWarmth::default() };
        assert_eq!(c.schedule(&d).mode.as_powerd(), "Manual");
        assert_eq!(c.schedule(&d).start, DayTime::hm(21, 30));
        assert!(!AutoWarmth::default().schedule(&d).enabled);
    }

    #[test]
    fn sunset_choice_is_kept_in_a_file_next_to_the_powerd_hours() {
        let root = std::env::temp_dir().join(format!("ink-cc-sunset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (mut f, d) = (Fake::default(), DateTime::from_unix(1_793_404_800, 0));
        let mut a = AutoWarmth::default();
        a.change(&mut f, &d, |a| a.mode = AutoMode::Sunset);
        a.store_sunset(&root);
        assert!(root.join(SUNSET_FILE).exists());
        // powerd reports enabled Manual hours: the file makes them "sunset" again.
        let mut b = AutoWarmth { mode: AutoMode::Custom, ..AutoWarmth::default() };
        assert!(b.load_sunset(&root, &mut f, &d));
        assert_eq!(b.mode, AutoMode::Sunset);
        assert!(!b.load_sunset(&root, &mut f, &d), "read once");
        a.change(&mut f, &d, |a| a.mode = AutoMode::Custom);
        a.store_sunset(&root);
        assert!(!root.join(SUNSET_FILE).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn timer_waits_until_due_and_shortens_the_loop_timeout() {
        let mut f = Fake::default();
        let clock = f.clone();
        let t = tick_auto_warmth(Some(Duration::from_secs(5)), &mut f, &clock);
        assert_eq!(t, Some(Duration::from_secs(5)), "auto warmth off: the loop's own timeout");
        let now = clock.local_now();
        with_auto_warmth(&mut f, |a, l| a.change(l, &now, |a| a.mode = AutoMode::Custom));
        let t = tick_auto_warmth(None, &mut f, &clock).unwrap();
        assert!(t > Duration::ZERO && t <= Duration::from_secs(24 * 3600), "{t:?}");
        assert_eq!(tick_auto_warmth(Some(Duration::from_millis(10)), &mut f, &clock), Some(Duration::from_millis(10)));
        with_auto_warmth(&mut f, |a, l| a.change(l, &now, |a| a.mode = AutoMode::Off));
    }

    #[test]
    fn control_center_tiles_and_sliders_act_at_once() {
        let script = format!(
            "{OPEN}shot open\ntap 1192 264\ntap 192 464\ntap 789 852\nshot tiles\ntap 177 852\nshot night\ntap 600 1500\nwait 50\nshot closed\n"
        );
        let c = run_cc("tiles", &script);
        let f = c.fake.borrow();
        assert_eq!((f.brightness, f.warmth), (13, 3));
        assert!(!f.wifi_on);
        assert!(saved(&c).device.night_mode);
        // Opening is one GL16 update over the panel (coalesced to the whole screen, still
        // partial: the page below does not change), no DU slide-in frames before it;
        // night mode flashes all.
        let u = c.probe.updates();
        let open = u.iter().position(|u| u.class == RefreshClass::Gl16 && u.rect.contains(Point::new(0, 1215))).expect("panel update");
        assert!(u[..open].iter().all(|u| u.class != RefreshClass::Du), "{u:?}");
        assert!(u.iter().any(|u| u.class == RefreshClass::Gc16 && u.rect == Rect::new(0, 0, 1272, 1696)));
    }

    /// Plugin tiles in `ui.control_buttons` (saved as `plugin:<plugin>:<id>`) are drawn
    /// with the plugin's caption and state, ids no plugin answers are skipped (the others
    /// move up), and a tap calls the plugin and shows the tile's new state.
    #[test]
    fn plugin_tiles_show_run_and_skip_unknown_ids() {
        let calls: crate::plugin_entries::test_plugin::Calls = Default::default();
        let mut s = Settings::default();
        s.ui.control_buttons = ["wifi", "plugin:gone:x", "plugin:probe:go", "plugin:probe:nope"].map(String::from).to_vec();
        let lay = layout(Size::new(1272, 1696), 2);
        let centre = |r: Rect| (r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
        let (tx, ty) = centre(lay.tiles[1]);
        let script = format!("{OPEN}wait 100\nshot before\ntap {tx} {ty}\nwait 100\nshot after\nquit\n");
        let (mut device, probe) = headless::open(HeadlessOptions { script: parse(&script).unwrap(), out_dir: None, ..Default::default() });
        let _fake = crate::screens::settings::fakes::install(&mut device);
        let root = std::env::temp_dir().join(format!("ink-cc-plugin-tile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(device, s, Paths { root: root.clone() });
        app.plugins = crate::plugin_entries::test_plugin::Probe::host(&calls);
        app.run(crate::router::Route::Demo).unwrap();
        assert_eq!(*calls.borrow(), ["go"], "one tap, one on_action(go)");
        let state = app.plugins.control_tile("probe", "go").expect("registered").1;
        assert_eq!((state.text.as_str(), state.on), ("taps 1", Some(true)));
        // Wi-Fi and the plugin tile have ink; the skipped ids left no tile at the third place.
        let panel = probe.panel();
        let ink = |r: Rect| (r.y..r.bottom()).flat_map(|y| (r.x..r.right()).map(move |x| (x, y))).filter(|&(x, y)| panel.get(x, y).is_some_and(|v| v < 0x80)).count();
        let third = layout(Size::new(1272, 1696), 3).tiles[2];
        assert!(ink(lay.tiles[0]) > 200 && ink(lay.tiles[1]) > 200, "tiles drawn: {} {}", ink(lay.tiles[0]), ink(lay.tiles[1]));
        assert_eq!(ink(third), 0, "unknown ids leave no empty tile");
        // The tap turned the tile on (filled): the left part of the tile is black.
        assert!(panel.get(tx - 100, ty).is_some_and(|v| v < 0x40), "the tile is on after the tap");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A plugin tile whose state changes while the panel is open is repainted without a
    /// tap: the panel re-reads plugin tiles on a short timer.
    #[test]
    fn plugin_tiles_follow_state_changes_while_open() {
        let mut s = Settings::default();
        s.ui.control_buttons = ["wifi", "plugin:drift:d"].map(String::from).to_vec();
        let tile = layout(Size::new(1272, 1696), 2).tiles[1];
        let (x, y) = (tile.x + 40, tile.y + tile.h as i32 / 2);
        let (mut device, probe) =
            headless::open(HeadlessOptions { script: parse(&format!("{OPEN}sleep 6000\nquit\n")).unwrap(), out_dir: None, ..Default::default() });
        let _fake = crate::screens::settings::fakes::install(&mut device);
        let root = std::env::temp_dir().join(format!("ink-cc-drift-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(device, s, Paths { root: root.clone() });
        app.plugins = crate::plugin_entries::test_plugin::Drift::host();
        app.run(crate::router::Route::Demo).unwrap();
        // `late` is an "on" state: the tile is filled. (The first seconds after opening
        // poll for the device state read; the tile poll follows it.)
        assert!(probe.panel().get(x, y).is_some_and(|v| v < 0x40), "the tile shows the state from after the panel opened");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A plugin tile with a hold action shows the three dots, and a long press calls the
    /// plugin with that action (not the tap's).
    #[test]
    fn holding_a_plugin_tile_runs_its_hold_action() {
        let calls: crate::plugin_entries::test_plugin::Calls = Default::default();
        let mut s = Settings::default();
        s.ui.control_buttons = ["plugin:probe:go"].map(String::from).to_vec();
        let r = layout(Size::new(1272, 1696), 1).tiles[0];
        let (x, y) = (r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
        let script = format!("{OPEN}wait 100\nhold {x} {y} 900\nwait 100\nquit\n");
        let (mut device, probe) = headless::open(HeadlessOptions { script: parse(&script).unwrap(), out_dir: None, ..Default::default() });
        let _fake = crate::screens::settings::fakes::install(&mut device);
        let root = std::env::temp_dir().join(format!("ink-cc-plugin-hold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(device, s, Paths { root: root.clone() });
        app.plugins = crate::plugin_entries::test_plugin::Probe::host(&calls);
        app.run(crate::router::Route::Demo).unwrap();
        assert_eq!(*calls.borrow(), ["settings:go"], "one hold, one on_action(settings:go)");
        // Three dots in the top right corner of the tile.
        let panel = probe.panel();
        let dots = (r.y + 16..r.y + 24).flat_map(|y| (r.right() - 52..r.right() - 16).map(move |x| (x, y))).filter(|&(x, y)| panel.get(x, y).is_some_and(|v| v < 0x80)).count();
        assert!(dots > 30, "dots drawn: {dots} dark pixels");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Holding a plugin tile shows the plugin's settings menu in the same settings popup
    /// as the built-in buttons (not the overlay's menu, not a page); a switch answers the
    /// menu and the plugin's next menu updates the popup in place; a tap outside cancels.
    #[test]
    fn holding_a_plugin_tile_opens_its_menu_in_the_settings_popup() {
        use crate::plugin_entries::test_plugin::{Calls, Toggler};
        let calls: Calls = Default::default();
        let mut s = Settings::default();
        s.ui.control_buttons = ["wifi", "plugin:toggler:t"].map(String::from).to_vec();
        let lay = layout(Size::new(1272, 1696), 2);
        let r = lay.tiles[1];
        let (x, y) = (r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
        // The menu: note "Ready" (96), then rows of 120: Auto start, Server, Done.
        let rows = popup::menu_rows(&[("Ready".into(), String::new()), ("Auto start\toff".into(), "auto".into()), ("Server\tauto".into(), "server".into()), ("Done".into(), "done".into())]);
        let p = SettingsPopup::new("Toggler", rows);
        let f = p.frame(picker_area(&lay), Anchor::Top);
        let switch_y = f.y + popup::TITLE_H as i32 + 96 + 60;
        let below = f.bottom() + 20;
        assert!(below < lay.status.y, "a tap below the popup is still on the panel");
        let script = format!("{OPEN}wait 100\nhold {x} {y} 900\nwait 50\nshot held\ntap {} {switch_y}\nwait 50\nshot switched\ntap 600 {below}\nwait 50\nquit\n", f.x + 200);
        let out_dir = std::env::var_os("INK_SETTINGS_OUT").map(|d| PathBuf::from(d).join("cc-plugin-popup"));
        let (mut device, probe) = headless::open(HeadlessOptions { script: parse(&script).unwrap(), out_dir, ..Default::default() });
        let _fake = crate::screens::settings::fakes::install(&mut device);
        let root = std::env::temp_dir().join(format!("ink-cc-plugin-popup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut app = App::new(device, s, Paths { root: root.clone() });
        app.plugins = Toggler::host(&calls);
        app.run(crate::router::Route::Demo).unwrap();
        assert_eq!(*calls.borrow(), ["settings:opts", "result:auto", "result:"], "held, switched, cancelled by a tap outside");
        assert!(app.plugins.ui.layer_names().is_empty(), "no overlay menu: {:?}", app.plugins.ui.layer_names());
        assert!(app.plugins.claimed_menus().is_none(), "the plugin's menus are given back");
        // After the app's first frames (full screen): the panel and the popup.
        let full = Rect::from_size(Size::new(1272, 1696));
        let u: Vec<_> = probe.updates().into_iter().filter(|d| d.rect != full).collect();
        assert!(u.iter().all(|d| d.class != RefreshClass::Gc16), "a popup never flashes: {u:?}");
        assert!(u.iter().all(|d| d.rect.bottom() <= lay.panel.bottom()), "every update inside the panel: {u:?}");
        // The switch repainted its row, not the whole popup.
        assert!(u.iter().any(|d| d.rect.h <= super::super::rows::ROW_H && d.rect.contains(Point::new(f.x + 200, switch_y))), "{u:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A long press on Full refresh opens its choices in the popup (it used to open the
    /// settings page); a choice is saved and closes the popup.
    #[test]
    fn long_press_on_full_refresh_opens_a_popup_not_a_page() {
        let mut s = Settings::default();
        s.ui.control_buttons = ["wifi", "full_refresh"].map(String::from).to_vec();
        let lay = layout(Size::new(1272, 1696), 2);
        let r = lay.tiles[1];
        let o = advanced::setting_options("full_refresh", &s).unwrap();
        let f = o.popup().frame(picker_area(&lay), Anchor::Top);
        // Row 1 ("Every 4 pages") under the title and the note.
        let row_y = f.y + SettingsPopup::head_h(true) as i32 + 120 + 60;
        let script = format!("{OPEN}hold {} {} 900\nshot popup\ntap {} {row_y}\nwait 50\nshot chosen\n", r.x + 60, r.y + 60, f.x + 200);
        let c = run_cc_with("refresh-popup", &script, s);
        assert_eq!(saved(&c).reader.flash_every, 4);
        let full = Rect::from_size(Size::new(1272, 1696));
        let u: Vec<_> = c.probe.updates().into_iter().filter(|d| d.rect != full).collect();
        assert!(u.len() >= 2 && u.iter().all(|d| d.class != RefreshClass::Gc16 && d.rect.bottom() <= lay.panel.bottom()), "partial, inside the panel: {u:?}");
        assert!(c.probe.uncovered().is_empty(), "{:?}", c.probe.uncovered());
    }

    #[test]
    fn control_center_custom_hours_warm_the_light_now() {
        // Fake clock 09:40. Custom 20:00-08:00, then To + six times: 20:00-11:00.
        let plus = "tap 1076 692\n".repeat(6);
        let c = run_cc("auto", &format!("{OPEN}tap 1105 580\n{plus}shot custom\ntap 834 580\nshot sunset\ntap 1105 580\n"));
        assert_eq!(c.fake.borrow().warmth, 10, "inside the custom hours");
        let c = run_cc("auto-off", &format!("{OPEN}tap 1105 580\n{plus}tap 563 580\nshot off\n"));
        assert_eq!(c.fake.borrow().warmth, 4, "off gives the user's warmth back");
    }

    #[test]
    fn control_center_rotates_and_opens_settings() {
        let c = run_cc("rotate", &format!("{OPEN}tap 1095 852\nwait 50\nshot landscape\n"));
        assert_eq!(saved(&c).device.rotation, 1);
        let mut s = Settings::default();
        s.ui.control_buttons = ["status_bars", "page_animation", "settings"].map(String::from).to_vec();
        let host = crate::plugin_host::PluginHost::new(Vec::new());
        assert_eq!(buttons(&s, &host), ["status_bars", "settings"], "page_animation is gone");
        let c = run_cc_with("settings", &format!("{OPEN}tap 177 852\ntap 483 852\nwait 50\nshot settings\nkey back\n"), s);
        let s = saved(&c);
        assert!(!s.status_bars.top.enabled && !s.status_bars.bottom.enabled);
        assert!(c.probe.updates().len() > 3);
    }
    /// Centre of picker row `i` of `n` rows (with or without a note) on the portrait
    /// panel with the default six buttons.
    fn picker_row(n: usize, note: bool, i: usize) -> (i32, i32) {
        let f = picker_frame(n, note);
        let y = f.y + SettingsPopup::head_h(note) as i32 + (i as u32 * super::super::rows::ROW_H) as i32;
        (f.x + f.w as i32 / 2, y + super::super::rows::ROW_H as i32 / 2)
    }

    /// The popup's box for `n` one-line rows on that panel.
    fn picker_frame(n: usize, note: bool) -> Rect {
        let area = picker_area(&layout(Size::new(1272, 1696), 6));
        let rows = vec![super::super::rows::Row::item("r", "a"); n];
        let p = SettingsPopup::new("T", rows).with_note(if note { "n" } else { "" });
        p.frame(area, Anchor::Top)
    }

    #[test]
    fn long_press_on_rotate_opens_the_angle_picker_and_the_angle_persists() {
        // Rotate is at (1095, 852); the picker has 4 angles and a note; row 3 is 270°.
        let (x, y) = picker_row(4, true, 3);
        let base = run_cc("rotate-open", OPEN).probe.updates().len();
        let c = run_cc("rotate-picker", &format!("{OPEN}hold 1095 852 900\nshot picker\n"));
        let u = c.probe.updates();
        let frame = picker_frame(4, true);
        let opened = &u[base..];
        assert!(opened.iter().any(|u| u.class == RefreshClass::Gl16 && u.rect.contains(Point::new(frame.x, frame.y))), "{opened:?}");
        assert!(opened.iter().all(|u| u.class != RefreshClass::Gc16 && u.rect.bottom() <= 1232), "partial, inside the panel: {opened:?}");
        assert_eq!(saved_or_default(&c).device.rotation, 0, "a long press does not rotate");
        assert!(c.probe.uncovered().is_empty(), "{:?}", c.probe.uncovered());
        let c = run_cc("rotate-270", &format!("{OPEN}hold 1095 852 900\ntap {x} {y}\nwait 50\nshot rotated\n"));
        let s = saved(&c);
        assert_eq!((s.ui.rotate_angle, s.device.rotation), (270, 3));
        // The tap goes to the stored angle (and from there back upright).
        let mut start = Settings::default();
        start.ui.rotate_angle = 180;
        let c = run_cc_with("rotate-tap-180", &format!("{OPEN}tap 1095 852\nwait 50\n"), start);
        assert_eq!(saved(&c).device.rotation, 2);
    }

    #[test]
    fn rotate_sends_one_full_gc16_update_and_the_panel_matches_a_fresh_render() {
        // 90 degrees turns the canvas, 180 keeps its size; both must show the new image.
        for (angle, index) in [(90u16, 1u8), (180, 2), (270, 3)] {
            let mut start = Settings::default();
            start.ui.rotate_angle = angle;
            let open = run_cc_with(&format!("rot-open-{angle}"), OPEN, start.clone());
            let before = open.probe.updates().len();
            let c = run_cc_with(&format!("rot-{angle}"), &format!("{OPEN}tap 1095 852\nwait 50\nshot a\n"), start.clone());
            assert_eq!(saved(&c).device.rotation, index);
            let u = c.probe.updates();
            let after: Vec<_> = u[before..].iter().collect();
            let full = Rect::from_size(Size::new(1272, 1696));
            assert_eq!(after.len(), 1, "angle {angle}: one update for the rotation: {after:?}");
            assert!(after[0].rect == full && after[0].class == RefreshClass::Gc16, "angle {angle}: full GC16: {after:?}");
            // A fresh start in the new orientation, the control center opened with the same swipe
            // from the logical top edge (as panel points), shows the same panel.
            let rot = Rotation::ALL[index as usize];
            let (from, to) = (rot.to_panel(Point::new(600, 20), Size::new(1272, 1696)), rot.to_panel(Point::new(600, 700), Size::new(1272, 1696)));
            start.device.rotation = index;
            let open_script = format!("swipe {} {} {} {}\nwait 50\n", from.x, from.y, to.x, to.y);
            let fresh = run_cc_with(&format!("rot-fresh-{angle}"), &open_script, start);
            assert!(c.probe.panel().pixels != open.probe.panel().pixels, "angle {angle}: the panel still shows the old orientation");
            let differ = c.probe.panel().pixels.iter().zip(&fresh.probe.panel().pixels).filter(|(a, b)| a != b).count();
            // Close to a fresh render (measured: 1270 px at 90/270, about 25000 at 180, from
            // state the fresh run does not share); far from the old orientation.
            assert!(differ < 1272 * 1696 / 50, "angle {angle}: {differ} px differ from a fresh render");
            assert!(c.probe.uncovered().is_empty(), "{:?}", c.probe.uncovered());
        }
    }

    #[test]
    fn long_press_on_wifi_lists_networks_and_joins_one() {
        // Fake: Home (5, saved), Cafe (3, open), Office (2, password); Wi-Fi on, not
        // connected. Rows: Home, Cafe, Office, Scan again, All networks (no note).
        // "Scan again" (row 3) lists again in place first; the list stays the same.
        let (x, y) = picker_row(5, false, 1);
        let (sx, sy) = picker_row(5, false, 3);
        let script = format!("{OPEN}hold 789 852 900\nshot wifi\ntap {sx} {sy}\nwait 50\nshot scanned\ntap {x} {y}\nwait 50\nshot joined\n");
        let c = run_cc("wifi-pick", &script);
        let f = c.fake.borrow();
        assert_eq!(f.connects, [("Cafe".to_string(), None)]);
        assert_eq!(f.connected.as_deref(), Some("Cafe"));
        drop(f);
        assert!(c.probe.uncovered().is_empty(), "closing repaints all it clears: {:?}", c.probe.uncovered());
        // A tap outside the picker closes only the picker; the Wi-Fi tile works again.
        let below = picker_frame(5, false).bottom() + 20;
        let held = run_cc("wifi-held", &format!("{OPEN}hold 789 852 900\n")).probe.updates().len();
        let c = run_cc("wifi-close", &format!("{OPEN}hold 789 852 900\ntap 600 {below}\ntap 789 852\n"));
        // Closing repaints the picker's rect (and the widgets under it), not the panel.
        let closed = &c.probe.updates()[held..];
        assert!(closed.iter().all(|u| u.class == RefreshClass::Gl16 && u.rect.y >= 120 && u.rect.bottom() <= 1232), "{closed:?}");
        let f = c.fake.borrow();
        assert!(f.connects.is_empty() && !f.wifi_on, "picker closed, then the tap turned Wi-Fi off");
        assert!(c.probe.uncovered().is_empty(), "{:?}", c.probe.uncovered());
    }

    /// A held tile opens its picker while the finger is still down (real-time script: the
    /// run ends with the finger down), and the lift neither picks a row nor closes it.
    #[test]
    fn long_press_on_a_tile_opens_the_picker_before_the_lift() {
        for (name, x, y) in [("rotate", 1095, 852), ("wifi", 789, 852)] {
            let none = run_cc(&format!("hold-{name}-none"), &format!("{OPEN}sleep 300\n")).probe.panel();
            let held = run_cc(&format!("hold-{name}-held"), &format!("{OPEN}down {x} {y}\nsleep 900\n")).probe.panel();
            let lifted = run_cc(&format!("hold-{name}-lifted"), &format!("{OPEN}down {x} {y}\nsleep 900\nup {x} {y}\nsleep 400\n")).probe.panel();
            let differing = none.pixels.iter().zip(&held.pixels).filter(|(a, b)| a != b).count();
            assert!(differing > 20_000, "{name}: the picker is not on the panel before the lift ({differing} pixels differ)");
            assert!(held.pixels == lifted.pixels, "{name}: the lift changed the panel");
        }
    }

    /// The visible Close button closes the panel with one GL16 update of the panel's rect
    /// (the page below comes back there), like a tap below it.
    #[test]
    fn close_button_closes_with_one_partial_update() {
        let l = layout(Size::new(1272, 1696), 6);
        let (x, y) = (l.close.x + l.close.w as i32 / 2, l.close.y + l.close.h as i32 / 2);
        let opened = run_cc("close-open", OPEN).probe.updates().len();
        let c = run_cc("close-button", &format!("{OPEN}tap {x} {y}\nwait 50\n"));
        let below = run_cc("close-below", &format!("{OPEN}tap 600 1500\nwait 50\n"));
        // The same updates as a tap below the panel (the closing the app did before), and
        // the panel's rect is covered by what follows.
        let (a, b) = (c.probe.updates(), below.probe.updates());
        assert!(a.len() > opened, "the panel closed: {a:?}");
        assert_eq!(a[opened..], b[opened..], "{a:?} vs {b:?}");
        assert!(c.probe.uncovered().is_empty(), "{:?}", c.probe.uncovered());
    }
}
