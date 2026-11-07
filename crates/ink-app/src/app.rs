//! The event loop: input -> gestures -> top screen -> transitions -> paint -> refresh.

use crate::plugin_host::PluginHost;
use crate::router::{AppCtx, AppScreen, Route, ScreenRegistry, Transition};
use ink_device::{Canvas, Device, PowerEvent, Rect, RefreshClass, Rotation};
use ink_doc::Provider;
use ink_formats::FormatsProvider;
use ink_mupdf::MupdfProvider;
use ink_store::{Paths, Settings};
use ink_text::Fonts;
use ink_device::trace::{self, Span};
use ink_ui::{GestureRecognizer, RefreshScheduler, ScreenSpace, Theme, UiEvent};
use std::time::Instant;

/// Builds an [`AppCtx`] from the app's fields (disjoint borrows of `self`).
macro_rules! ctx {
    ($app:ident, $providers:expr) => {
        AppCtx {
            // The theme follows the panel's night flag (set by settings, Aa or the
            // control center), so widgets can draw tracks that read when inverted.
            theme: {
                $app.theme.night = $app.device.screen.night_mode();
                &$app.theme
            },
            canvas: &mut $app.canvas,
            screen: $app.device.screen.as_mut(),
            fonts: &mut $app.fonts,
            refresh: &mut $app.refresh,
            plugins: &mut $app.plugins,
            settings: &$app.settings,
            paths: &$app.paths,
            providers: $providers,
            services: crate::router::Services {
                info: &$app.device.info,
                light: $app.device.light.as_mut(),
                battery: $app.device.battery.as_mut(),
                power: $app.device.power.as_mut(),
                wifi: $app.device.wifi.as_mut(),
                usb: $app.device.usb.as_mut(),
                clock: $app.device.clock.as_mut(),
            },
            settings_update: &mut $app.settings_update,
        }
    };
}

/// Document providers in the order they are asked.
pub static PROVIDERS: [&(dyn Provider + Sync); 2] = [&FormatsProvider, &MupdfProvider];

/// The providers as a plain slice.
pub fn providers() -> Vec<&'static dyn Provider> {
    PROVIDERS.iter().map(|p| *p as &dyn Provider).collect()
}

/// The running application.
pub struct App {
    /// The device.
    pub device: Device,
    /// UI fonts.
    pub fonts: Fonts,
    /// Theme.
    pub theme: Theme,
    /// Settings.
    pub settings: Settings,
    /// File locations.
    pub paths: Paths,
    /// Plugins.
    pub plugins: PluginHost,
    /// Page-turn refresh policy.
    pub refresh: RefreshScheduler,
    canvas: Canvas,
    recognizer: GestureRecognizer,
    registry: ScreenRegistry,
    stack: Vec<Box<dyn AppScreen>>,
    settings_update: Option<Settings>,
    /// Timing of the last screen change (or the app start), ended after the next paint.
    pending_span: Option<Span>,
    /// When the events being handled arrived (slow-frame check).
    busy_since: Option<Instant>,
    /// Night mode and [`App::epoch`] when each screen above the bottom one was pushed
    /// (a pop shows only the popped screen's rect when neither changed).
    below: Vec<(bool, u64)>,
    /// Counts the times the canvas outside a panel screen stopped holding the screen
    /// below: a new canvas, a rotation, a new UI font, a full-screen screen entered.
    epoch: u64,
    /// The rotation the canvas was last fitted to (a half turn keeps the canvas size).
    fitted: Rotation,
    /// The next frame shows the whole panel with one GC16 update (after a rotation).
    full_next: bool,
    /// The device just left the screensaver: the next frame writes the whole canvas
    /// again (GL16). The device restores the page it saved at sleep, but a kernel
    /// resume sometimes leaves the panel and the framebuffer blank, and the next sleep
    /// would then save and restore that blank picture until a page turn.
    repaint_after_wake: bool,
    /// Edge swipes recognised while the finger still moves.
    edge: EarlyEdgeSwipe,
    /// Battery samples for the battery page.
    battery_log: crate::battery_log::BatteryLog,
    /// What the panel shows, kept by the screen wrapper ([`crate::shown`]).
    shown: crate::shown::Shown,
    /// A panel screen above (the control center) was popped and only its rect came
    /// back: the next frame's GC16 damage is shown as GL16 (`Change::Pop`), so a screen
    /// that draws all again on reveal as on entry (with GC16) does not flash.
    revealed_part: bool,
}

/// How far (logical pixels) a touch that starts in the top or bottom edge zone must
/// move inward before it counts as an edge swipe.
pub const EARLY_SWIPE_PX: i32 = 80;

/// Edge swipes without waiting for the lift: a touch that starts in the top edge zone
/// (the top twentieth of the screen, as the reader's gesture map) and moves down
/// [`EARLY_SWIPE_PX`] (mostly vertically) becomes a `Swipe` down at once, so the control
/// center opens while the finger moves; the same upward from the bottom zone (the Aa
/// sheet). The screen gets a `Cancel` for that touch first; the rest of the touch and
/// the gesture its lift makes are dropped.
#[derive(Clone, Debug, Default)]
pub struct EarlyEdgeSwipe {
    /// The touch slot already turned into a swipe.
    fired: Option<u8>,
    /// Drop the next gesture (the one the swallowed touch's lift makes).
    drop_gesture: bool,
}

impl EarlyEdgeSwipe {
    /// Filters `events` for a screen `h` logical pixels high.
    pub fn filter(&mut self, events: Vec<UiEvent>, h: i32) -> Vec<UiEvent> {
        use ink_ui::{Direction, Gesture, TouchKind};
        let mut out = Vec::with_capacity(events.len());
        for ev in events {
            // A new touch starts: a lift that made no gesture (a drag after a hold) must
            // not cost the next touch its gesture.
            if matches!(&ev, UiEvent::Touch(t) if t.kind == TouchKind::Down) {
                self.drop_gesture = false;
            }
            match &ev {
                // A new touch-down in that slot: the old contact ended without an Up
                // reaching here (a screen change resets the recogniser).
                UiEvent::Touch(t) if self.fired == Some(t.slot) && t.kind == TouchKind::Down => {
                    self.fired = None;
                }
                UiEvent::Touch(t) if self.fired == Some(t.slot) => {
                    if matches!(t.kind, TouchKind::Up | TouchKind::Cancel) {
                        self.fired = None;
                        self.drop_gesture = t.kind == TouchKind::Up;
                    }
                    continue;
                }
                UiEvent::Touch(t) if t.kind == TouchKind::Move && self.fired.is_none() => {
                    let (dx, dy) = (t.pos.x - t.start.x, t.pos.y - t.start.y);
                    let zone = h / 20;
                    let vertical = dy.abs() > 2 * dx.abs();
                    let dir = if t.start.y < zone && dy >= EARLY_SWIPE_PX && vertical {
                        Some(Direction::Down)
                    } else if t.start.y >= h - zone && -dy >= EARLY_SWIPE_PX && vertical {
                        Some(Direction::Up)
                    } else {
                        None
                    };
                    if let Some(dir) = dir {
                        self.fired = Some(t.slot);
                        let mut cancel = *t;
                        cancel.kind = TouchKind::Cancel;
                        out.push(UiEvent::Touch(cancel));
                        out.push(UiEvent::Gesture(Gesture::Swipe { from: t.start, to: t.pos, dir }));
                        continue;
                    }
                }
                UiEvent::Gesture(_) if std::mem::take(&mut self.drop_gesture) => continue,
                _ => {}
            }
            out.push(ev);
        }
        out
    }
}

/// Short text for an event in the log (transition causes).
fn describe(ev: &UiEvent) -> String {
    let mut s = format!("{ev:?}");
    if s.len() > 160 {
        let mut cut = 160;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("...");
    }
    s
}

/// The exit reason for a [`Transition::Exit`] that `ev` caused on `screen`.
fn exit_reason(ev: Option<&UiEvent>, screen: &str) -> String {
    match ev {
        Some(UiEvent::Key(k)) if k.key == ink_device::Key::Power => format!("power button (screen {screen})"),
        Some(UiEvent::Quit) => format!("quit event: input closed or window closed (screen {screen})"),
        Some(UiEvent::Power(p)) => format!("power event {p:?} (screen {screen})"),
        Some(ev) => format!("exit button/menu on screen {screen} after {}", describe(ev)),
        None => format!("exit from screen {screen}"),
    }
}

/// Shows one painted frame: the screen damage and the plugin layers' damage (already
/// one list) are coalesced ([`ink_ui::refresh::coalesce`]: touching rects merge, at most
/// 4 rects, a large GL16 area becomes one full-screen GL16), the UI flash policy runs
/// once ([`ink_ui::refresh::RefreshScheduler::ui`]), and each rect goes to the panel in
/// issue order (DU, GL16, REAGL, GC16). Logs the frame's updates at debug level, with
/// the ones the screen sent itself during the paint (page turns, full restores).
pub fn present(cx: &mut AppCtx, frame: crate::plugin_host::hooks::Frame) -> anyhow::Result<()> {
    present_over(cx, frame, None)
}

/// [`present`] with `shown`, a copy of what the panel shows ([`crate::shown`]): a frame
/// that would flash is first cut to the pixels that differ from it
/// ([`ink_ui::refresh::trim_to_changes`]), so popups, menus, dialogs and keyboards
/// over a screen that repaints all of its canvas never flash.
pub fn present_over(cx: &mut AppCtx, frame: crate::plugin_host::hooks::Frame, shown: Option<&crate::shown::Shown>) -> anyhow::Result<()> {
    let screen = cx.canvas.bounds();
    let damage = match shown {
        // The borrow ends before the updates go out (the screen wrapper writes the copy).
        Some(s) => ink_ui::refresh::trim_to_changes(frame.damage, screen, s.borrow().as_ref(), cx.canvas),
        None => frame.damage,
    };
    let list = ink_ui::refresh::coalesce(damage, screen);
    let list = cx.refresh.ui(list);
    for d in &list {
        cx.screen.update(cx.canvas, d.rect, d.class)?;
    }
    if log::log_enabled!(log::Level::Debug) && !(frame.shown.is_empty() && list.is_empty()) {
        let wave = frame.wave.map(|w| format!(" wave {w:?}")).unwrap_or_default();
        log::debug!("frame: {} updates: shown {} queued {}{wave}", frame.shown.len() + list.len(), describe_updates(&frame.shown), describe_updates(&list));
    }
    Ok(())
}

/// `[x,y wxh CLASS, ...]` for a frame log line.
pub fn describe_updates(list: &[ink_ui::refresh::Damage]) -> String {
    let parts: Vec<String> = list.iter().map(|d| format!("{},{} {}x{} {:?}", d.rect.x, d.rect.y, d.rect.w, d.rect.h, d.class)).collect();
    format!("[{}]", parts.join(", "))
}

/// The rotation `settings.device.rotation` names (quarter turns clockwise).
pub fn settings_rotation(settings: &Settings) -> Rotation {
    Rotation::ALL[(settings.device.rotation % 4) as usize]
}

impl App {
    /// Builds the app for `device` with every screen registered.
    pub fn new(device: Device, settings: Settings, paths: Paths) -> Self {
        // Every update to the panel is copied (popups never flash: `present_over`).
        let shown: crate::shown::Shown = Default::default();
        let Device { info, screen, input, light, battery, power, wifi, usb, clock } = device;
        let screen = Box::new(crate::shown::ShownScreen::new(screen, shown.clone()));
        let mut device = Device { info, screen, input, light, battery, power, wifi, usb, clock };
        let rotation = settings_rotation(&settings);
        if device.screen.rotation() != rotation {
            if let Err(e) = device.screen.set_rotation(rotation) {
                log::warn!("rotation {rotation:?} from the settings: {e}");
            }
        }
        crate::screens::settings::apply_device_settings(&mut device, &settings, &paths);
        let device_rotation = device.screen.rotation();
        let size = device.screen.size();
        let space = ScreenSpace { panel: device.screen.panel_size(), rotation: device.screen.rotation() };
        let mut registry = ScreenRegistry::new();
        crate::screens::register_all(&mut registry);
        let refresh = RefreshScheduler::new(settings.reader.flash_every);
        let plugins = PluginHost::load(crate::plugins::compiled_in(), &settings, &paths);
        let battery_log = crate::battery_log::BatteryLog::new(paths.battery_log());
        crate::fonts::start(&paths, &settings); // aa: font registry and UI font
        App {
            device,
            fonts: crate::fonts::builtin_ui_fonts(),
            theme: Theme::day(),
            settings,
            paths,
            plugins,
            refresh,
            canvas: Canvas::new(size.w as usize, size.h as usize),
            recognizer: GestureRecognizer::new(space),
            registry,
            stack: Vec::new(),
            settings_update: None,
            pending_span: None,
            busy_since: None,
            below: Vec::new(),
            epoch: 0,
            fitted: device_rotation,
            full_next: false,
            repaint_after_wake: false,
            edge: EarlyEdgeSwipe::default(),
            battery_log,
            shown,
            revealed_part: false,
        }
    }

    /// Writes a battery sample (`awake` false: the device goes to sleep now).
    fn note_battery(&mut self, awake: bool) {
        let Ok(state) = self.device.battery.state() else { return };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        self.battery_log.note(now, state, awake);
    }

    /// Runs until a screen exits or the input ends.
    pub fn run(&mut self, start: Route) -> anyhow::Result<()> {
        ink_device::trace::set_stop_listener(true);
        let r = self.run_loop(start);
        self.plugins.app_exit();
        ink_device::trace::set_stop_listener(false);
        r
    }

    fn run_loop(&mut self, start: Route) -> anyhow::Result<()> {
        let providers = providers();
        log::info!("screen: (start) -> {} ({start:?})", start.kind());
        self.pending_span = Some(Span::since("app start: process start -> first frame", trace::process_start()));
        let screen = self.registry.build(&start)?;
        self.stack.push(screen);
        self.enter_top(&providers)?;
        loop {
            self.fit_canvas(&providers)?;
            self.swap_ui_fonts(&providers)?; // aa: UI font loaded on a thread
            let full_next = std::mem::take(&mut self.full_next);
            let repaint_after_wake = std::mem::take(&mut self.repaint_after_wake);
            let Some(top) = self.stack.last_mut() else { return Ok(()) };
            self.recognizer.set_double_tap(top.double_tap_mode(&self.settings));
            crate::plugin_host::hooks::keep_awake(&mut self.plugins, self.device.power.as_mut());
            self.refresh.set_night(self.device.screen.night_mode());
            {
                let mut cx = ctx!(self, &providers);
                let mut frame = crate::plugin_host::hooks::app_frame(top.as_mut(), &mut cx)?;
                let all = cx.canvas.bounds();
                // A screen that already showed the whole canvas with GC16 while painting (the
                // reader) sent the one full update itself.
                let shown_full = frame.shown.iter().any(|d| d.rect == all && d.class == RefreshClass::Gc16);
                if full_next && !shown_full {
                    frame.damage =vec![ink_ui::refresh::Damage { rect: cx.canvas.bounds(), class: RefreshClass::Gc16 }];
                } else if std::mem::take(&mut self.revealed_part) {
                    // Back from a panel screen: the screen below may draw all again as on
                    // entry, but only the panel's rect changed. GL16, cut to the pixels
                    // that differ from the panel's copy (present_over): no flash.
                    for d in frame.damage.iter_mut().filter(|d| d.class == RefreshClass::Gc16) {
                        d.class = RefreshClass::Gl16;
                    }
                }
                present_over(&mut cx, frame, Some(&self.shown))?;
                if repaint_after_wake && !shown_full && !full_next {
                    log::info!("wake: whole canvas written again (GL16)");
                    cx.screen.update(cx.canvas, all, RefreshClass::Gl16)?;
                }
            }
            // Test screens check that the frame showed every pixel it drew.
            self.device.screen.frame_done(&self.canvas);
            self.after_paint();
            // The wait is taken after the paint: the paint can start timers (the reader
            // notes the page shown after a turn and writes it to the sidecar once the
            // page rested; taken before, the loop slept until the next input).
            let Some(top) = self.stack.last() else { return Ok(()) };
            let timeout = crate::plugin_host::hooks::app_timeout(top.timeout(), &self.plugins);
            let timeout = crate::fonts::ui_font_pending().then_some(std::time::Duration::from_millis(200)).into_iter().chain(timeout).min();
            // Waits for input, the screen's timeout or a gesture timer (holds); keeps the
            // recogniser's rotation in step with the screen.
            let events = ink_ui::pump::next_events(
                &mut self.recognizer,
                self.device.input.as_mut(),
                self.device.screen.as_ref(),
                timeout,
            )?;
            self.busy_since = Some(Instant::now());
            // SIGTERM/SIGINT/SIGHUP (rinky-ctl.sh stop/restart, the launcher, the
            // watchdog): every screen closes as on an exit (the reader writes its
            // position) before the process exits with 128 + the signal.
            if let Some(sig) = ink_device::trace::stop_requested() {
                let from = self.stack.last().map_or("(none)", |s| s.name());
                log::info!("screen: {from} -> (exit) (signal {})", ink_device::trace::signal_name(sig));
                crate::logging::set_exit_reason(format!("signal {}", ink_device::trace::signal_name(sig)), true);
                while !self.stack.is_empty() {
                    self.exit_top(&providers);
                }
                return Ok(());
            }
            let events = self.edge.filter(events, self.canvas.height as i32);
            if self.battery_log.due() && self.device.power.sleep_state() == ink_device::sleep::SleepState::Awake {
                self.note_battery(true);
            }
            for ev in events {
                crate::perf::input_seen();
                if let UiEvent::Gesture(g) = &ev {
                    log::info!("gesture: {g:?}");
                }
                match ev {
                    UiEvent::Power(PowerEvent::GoingToScreenSaver) => self.note_battery(false),
                    UiEvent::Power(PowerEvent::OutOfScreenSaver) => self.note_battery(true),
                    _ => {}
                }
                if matches!(ev, UiEvent::Power(PowerEvent::OutOfScreenSaver)) {
                    self.repaint_after_wake = true;
                    // Carried somewhere else while asleep: look for networks there.
                    self.device.wifi.after_wake();
                }
                if matches!(ev, UiEvent::Power(PowerEvent::GoingToScreenSaver | PowerEvent::OutOfScreenSaver | PowerEvent::Resuming)) {
                    // The screensaver (or a resume) changed the panel behind the app's back.
                    *self.shown.borrow_mut() = None;
                }
                // A swipe down from the top edge opens the quick light panel over any
                // screen (screens::settings::quick_light) but the reader, which maps it with
                // its gesture map (screens::reader::actions).
                if crate::screens::settings::quick_light::is_trigger(&ev)
                    && self.stack.last().is_some_and(|t| !matches!(t.name(), "quicklight" | "reader"))
                {
                    self.apply(Transition::Push(Route::QuickLight), &providers, Some(&ev))?;
                    continue;
                }
                let transition = {
                    let Some(top) = self.stack.last_mut() else { return Ok(()) };
                    let mut cx = ctx!(self, &providers);
                    if crate::plugin_host::hooks::app_handle(top.as_mut(), &ev, &mut cx) {
                        Transition::None
                    } else {
                        top.handle(&ev, &mut cx)?
                    }
                };
                self.take_settings_update();
                if !self.apply(transition, &providers, Some(&ev))? {
                    return Ok(());
                }
            }
        }
    }

    /// Ends the pending screen-change span and warns about a slow frame (events plus
    /// paint over [`trace::SLOW_FRAME`]).
    fn after_paint(&mut self) {
        if let Some(span) = self.pending_span.take() {
            span.finish();
        }
        if let Some(t) = self.busy_since.take() {
            let d = t.elapsed();
            let name = self.stack.last().map_or("(none)", |s| s.name());
            if d > trace::SLOW_FRAME {
                log::warn!("slow frame: {} ms on screen {name} (event handling and paint)", d.as_millis());
            } else {
                log::debug!("frame time: {} ms on screen {name}", d.as_millis());
            }
        }
    }

    /// Starts the span for a change to `route`: from the event that caused it to the
    /// first paint of the new screen (for a book: tap -> first page on screen).
    fn start_span(&mut self, route: &Route) {
        let name = match route {
            Route::Reader(p) => format!("open book {:?}: tap -> first page", p.file_name().unwrap_or_default()),
            r => format!("screen {}: tap -> first frame", r.kind()),
        };
        self.pending_span = Some(Span::since(name, self.busy_since.unwrap_or_else(Instant::now)));
    }

    /// Keeps a settings change made by a screen: stores it, saves it and applies the
    /// rotation.
    fn take_settings_update(&mut self) {
        let Some(s) = self.settings_update.take() else { return };
        if let Err(e) = s.save(&self.paths.settings()) {
            log::warn!("cannot save settings: {e}");
        }
        let rotation = settings_rotation(&s);
        if s.ui.font_family != self.settings.ui.font_family {
            crate::fonts::request_ui_font(&s.ui.font_family);
        }
        self.plugins.ui.keyboard_layouts = crate::screens::settings::keyboard_layouts::layouts(&s);
        // "Full refresh: every N pages" (settings, Aa) applies at once, not after a restart.
        if self.refresh.flash_every != s.reader.flash_every {
            log::info!("refresh: flash every {} page turns (was {})", s.reader.flash_every, self.refresh.flash_every);
            self.refresh.flash_every = s.reader.flash_every;
        }
        self.settings = s;
        if self.device.screen.rotation() != rotation {
            log::info!("rotation {rotation:?}");
            // A half turn keeps the size (fit_canvas keeps the canvas), but every
            // screen must draw again.
            self.epoch += 1;
            if let Err(e) = self.device.screen.set_rotation(rotation) {
                log::warn!("rotation {rotation:?}: {e}");
            }
        }
    }

    /// Puts a UI font loaded by [`crate::fonts::request_ui_font`] in place and draws the
    /// top screen again with it.
    fn swap_ui_fonts(&mut self, providers: &[&dyn Provider]) -> anyhow::Result<()> {
        let Some((name, fonts)) = crate::fonts::take_ui_fonts() else { return Ok(()) };
        log::info!("UI font now '{}'", if name.is_empty() { crate::fonts::UI_BUILTIN } else { &name });
        self.fonts = fonts;
        self.epoch += 1;
        self.enter_top(providers)
    }

    /// Gives the canvas the screen's logical size after a rotation and lets the top
    /// screen lay itself out again.
    fn fit_canvas(&mut self, providers: &[&dyn Provider]) -> anyhow::Result<()> {
        let size = self.device.screen.size();
        let rotation = self.device.screen.rotation();
        let turned = rotation != self.fitted;
        if !turned && (self.canvas.width, self.canvas.height) == (size.w as usize, size.h as usize) {
            return Ok(());
        }
        self.fitted = rotation;
        // The panel shows the old orientation: one full GC16 update of the new image, also
        // for a half turn, where the canvas keeps its size and nothing else would redraw.
        self.full_next = true;
        log::info!("canvas {}x{} -> {}x{} (rotation {rotation:?})", self.canvas.width, self.canvas.height, size.w, size.h);
        self.canvas = Canvas::new(size.w as usize, size.h as usize);
        self.epoch += 1;
        let Some(top) = self.stack.last_mut() else { return Ok(()) };
        let mut cx = ctx!(self, providers);
        top.on_resize(&mut cx)
    }

    fn enter_top(&mut self, providers: &[&dyn Provider]) -> anyhow::Result<()> {
        let Some(top) = self.stack.last_mut() else { return Ok(()) };
        log::info!("screen: {}", top.name());
        let mut cx = ctx!(self, providers);
        top.on_enter(&mut cx)
    }

    /// A new top screen that covers the whole screen draws over everything below:
    /// screens below a panel screen above it must then draw all again on a pop.
    fn mark_full_screen_top(&mut self) {
        let screen = self.canvas.bounds();
        if self.stack.last().is_some_and(|s| s.covered_rect(screen) == screen) {
            self.epoch += 1;
        }
    }

    /// The top screen is shown again after a pop; `exposed` is what the popped screen
    /// covered.
    fn reveal_top(&mut self, providers: &[&dyn Provider], exposed: Rect) -> anyhow::Result<()> {
        let Some(top) = self.stack.last_mut() else { return Ok(()) };
        let full_screen = exposed == self.canvas.bounds();
        self.revealed_part = !full_screen;
        let hint = self.refresh.class_for(ink_ui::refresh::Change::Pop { full_screen, over_page: top.name() == "reader" });
        log::info!("screen: {} (revealed {},{} {}x{}, {hint:?})", top.name(), exposed.x, exposed.y, exposed.w, exposed.h);
        let mut cx = ctx!(self, providers);
        top.on_reveal(&mut cx, exposed, hint)
    }

    fn exit_top(&mut self, providers: &[&dyn Provider]) {
        if let Some(mut top) = self.stack.pop() {
            let mut cx = ctx!(self, providers);
            top.on_exit(&mut cx);
        }
    }

    /// Applies a transition caused by `ev`; returns false when the app should stop.
    /// Logs `screen: A -> B (cause)` and records the exit reason.
    fn apply(&mut self, t: Transition, providers: &[&dyn Provider], ev: Option<&UiEvent>) -> anyhow::Result<bool> {
        if t == Transition::None {
            return Ok(true);
        }
        let from = self.stack.last().map_or("(none)", |s| s.name());
        let cause = ev.map_or_else(|| "no event".to_string(), describe);
        match &t {
            Transition::None => {}
            Transition::Push(r) => log::info!("screen: {from} -> {} (push {r:?}; {cause})", r.kind()),
            Transition::Replace(r) => log::info!("screen: {from} -> {} (replace {r:?}; {cause})", r.kind()),
            Transition::Pop => {
                let below = self.stack.len().checked_sub(2).and_then(|i| self.stack.get(i)).map_or("(exit)", |s| s.name());
                log::info!("screen: {from} -> {below} (back; {cause})");
                if below == "(exit)" {
                    crate::logging::set_exit_reason(format!("back from the last screen {from} after {cause}"), false);
                }
            }
            Transition::Exit => {
                log::info!("screen: {from} -> (exit) ({cause})");
                crate::logging::set_exit_reason(exit_reason(ev, from), false);
            }
        }
        match t {
            Transition::None => {}
            Transition::Push(r) => {
                self.start_span(&r);
                let s = self.registry.build(&r)?;
                self.below.push((self.device.screen.night_mode(), self.epoch));
                self.stack.push(s);
                self.recognizer.reset();
                // After on_enter: a panel screen knows its rect only once laid out.
                let entered = self.enter_top(providers);
                if entered.is_ok() {
                    self.mark_full_screen_top();
                }
                if let Err(e) = entered {
                    // A screen that cannot start must not end the app: drop it and go
                    // back to the screen below (which repaints in its on_enter).
                    log::error!("cannot enter {r:?}: {e:#}");
                    self.stack.pop();
                    self.below.pop();
                    if self.stack.is_empty() {
                        return Err(e);
                    }
                    self.enter_top(providers)?;
                }
            }
            Transition::Replace(r) => {
                self.start_span(&r);
                let s = self.registry.build(&r)?;
                self.exit_top(providers);
                self.stack.push(s);
                self.recognizer.reset();
                self.enter_top(providers)?;
                self.mark_full_screen_top();
            }
            Transition::Pop => {
                if let Some(below) = self.stack.len().checked_sub(2).and_then(|i| self.stack.get(i)) {
                    let name = format!("screen {}: back -> first frame", below.name());
                    self.pending_span = Some(Span::since(name, self.busy_since.unwrap_or_else(Instant::now)));
                }
                // What the popped screen covered; all of it when the night mode changed
                // or the canvas below stopped holding the screen below (`epoch`).
                let screen = self.canvas.bounds();
                let state = (self.device.screen.night_mode(), self.epoch);
                let same = self.below.pop().is_none_or(|b| b == state);
                let covered = self.stack.last().map_or(screen, |s| s.covered_rect(screen));
                let exposed = if same { covered.intersect(&screen) } else { screen };
                self.exit_top(providers);
                if self.stack.is_empty() {
                    return Ok(false);
                }
                self.recognizer.reset();
                self.reveal_top(providers, exposed)?;
            }
            Transition::Exit => {
                while !self.stack.is_empty() {
                    self.exit_top(providers);
                }
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    use ink_device::Point;
    use ink_ui::{Direction, Gesture, Touch, TouchKind};
    use std::time::Duration;

    fn t(kind: TouchKind, x: i32, y: i32, sy: i32) -> UiEvent {
        UiEvent::Touch(Touch { slot: 0, kind, pos: Point::new(x, y), start: Point::new(600, sy), held: Duration::ZERO, moved: false })
    }

    #[test]
    fn edge_swipes_fire_while_the_finger_moves_and_swallow_the_rest() {
        let mut e = EarlyEdgeSwipe::default();
        let h = 1696;
        assert_eq!(e.filter(vec![t(TouchKind::Down, 600, 10, 10), t(TouchKind::Move, 600, 60, 10)], h).len(), 2, "not far enough yet");
        let out = e.filter(vec![t(TouchKind::Move, 610, 100, 10)], h);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], UiEvent::Touch(x) if x.kind == TouchKind::Cancel));
        assert!(matches!(out[1], UiEvent::Gesture(Gesture::Swipe { dir: Direction::Down, .. })));
        let lift = UiEvent::Gesture(Gesture::Swipe { from: Point::new(600, 10), to: Point::new(600, 400), dir: Direction::Down });
        assert!(e.filter(vec![t(TouchKind::Move, 610, 300, 10), t(TouchKind::Up, 610, 400, 10), lift], h).is_empty(), "the rest is dropped");
        let tap = UiEvent::Gesture(Gesture::Tap { pos: Point::new(5, 5) });
        assert_eq!(e.filter(vec![tap.clone()], h), vec![tap], "the next touch is normal");
        // The lift never arrives (the recogniser was reset): the next touch-down passes.
        e.filter(vec![t(TouchKind::Down, 600, 10, 10), t(TouchKind::Move, 600, 120, 10)], h);
        let down = t(TouchKind::Down, 300, 264, 264);
        assert_eq!(e.filter(vec![down.clone(), t(TouchKind::Move, 400, 264, 264)], h).len(), 2);
        // Up from the bottom zone; a touch in the middle never fires.
        let out = e.filter(vec![t(TouchKind::Down, 600, 1690, 1690), t(TouchKind::Move, 600, 1600, 1690)], h);
        assert!(matches!(out.last(), Some(UiEvent::Gesture(Gesture::Swipe { dir: Direction::Up, .. }))));
        // Its lift makes no gesture (a drag after a hold): the next tap still passes.
        assert!(e.filter(vec![t(TouchKind::Up, 600, 1500, 1690)], h).is_empty());
        let tap2 = UiEvent::Gesture(Gesture::Tap { pos: Point::new(600, 800) });
        let out = e.filter(vec![t(TouchKind::Down, 600, 800, 800), t(TouchKind::Up, 600, 800, 800), tap2.clone()], h);
        assert_eq!(out.last(), Some(&tap2));
        let mut e = EarlyEdgeSwipe::default();
        assert_eq!(e.filter(vec![t(TouchKind::Down, 600, 800, 800), t(TouchKind::Move, 600, 1000, 800)], h).len(), 2);
    }
}

/// The app loop with an input that waits like the device (`ReaderInput`): it returns to
/// the loop only when its timeout passes (real time) or input arrives. The sim scripts
/// (`wait MS`) only move a virtual clock and never wait.
#[cfg(test)]
mod position_save_tests {
    use super::*;
    use ink_device::headless::{self, HeadlessOptions};
    use ink_device::script::{parse, Played, ScriptPlayer};
    use ink_device::{Input, InputEvent};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::Duration;

    #[derive(Default)]
    struct Seen {
        /// Every timeout the loop passed after the scripted input ran out.
        timeouts: Vec<Option<Duration>>,
        /// Real time slept in those waits.
        slept: Duration,
        /// The sidecar position when the first page was shown (written at the open).
        first: Option<String>,
        /// The sidecar position read after the loop handled each wait (before any exit
        /// flush).
        position: Option<String>,
    }

    struct DeviceLikeInput {
        events: VecDeque<InputEvent>,
        book: PathBuf,
        seen: Rc<RefCell<Seen>>,
    }

    impl Input for DeviceLikeInput {
        fn next_event(&mut self, timeout: Option<Duration>) -> ink_device::Result<Option<InputEvent>> {
            let position = || ink_store::BookState::load(&self.book).ok().and_then(|s| s.position);
            let mut seen = self.seen.borrow_mut();
            if seen.first.is_none() {
                seen.first = position();
            }
            if let Some(ev) = self.events.pop_front() {
                return Ok(Some(ev));
            }
            seen.timeouts.push(timeout);
            // The loop handled the last wake-up (a timer event) before calling again.
            if seen.slept > Duration::ZERO {
                seen.position = position();
            }
            let saved = seen.position.is_some() && seen.position != seen.first;
            match timeout {
                // The device sleeps until the next touch (none comes): the test ends
                // here. Quit runs the exit flush, so the sidecar was read before.
                Some(t) if !saved && t <= Duration::from_secs(3) && seen.slept < Duration::from_secs(6) => {
                    std::thread::sleep(t);
                    seen.slept += t;
                    Ok(None)
                }
                _ => Ok(Some(InputEvent::Quit)),
            }
        }
    }

    /// A page turn is written to the sidecar about 2 s later with no further input. The
    /// loop used to read the reader's timeout before the paint that notes the page, so it
    /// slept until the next input (2026-11-02 emulator: no idle save).
    #[test]
    fn page_turn_position_is_saved_after_the_idle_delay_without_input() {
        let dir = std::env::temp_dir().join(format!("ink-app-idle-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let book = dir.join("juliet.epub");
        std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/juliet.epub"), &book).unwrap();
        let mut player = ScriptPlayer::new(&parse("tap 1000 900\n").unwrap());
        let mut events = VecDeque::new();
        while player.remaining() > 0 {
            if let Played::Event(ev) = player.next_item() {
                events.push_back(ev);
            }
        }
        let seen = Rc::new(RefCell::new(Seen::default()));
        let (mut device, _probe) = headless::open(HeadlessOptions::default());
        device.input = Box::new(DeviceLikeInput { events, book: book.clone(), seen: seen.clone() });
        // No minute tick (no timed bar widgets) and no plugins: nothing else wakes the loop.
        let mut settings = Settings::default();
        settings.status_bars.top.enabled = false;
        settings.status_bars.bottom.enabled = false;
        let mut app = App::new(device, settings, Paths { root: dir.join("home") });
        app.plugins = PluginHost::new(Vec::new());
        app.run(Route::Reader(book.clone())).unwrap();
        let seen = seen.borrow();
        assert!(seen.first.is_some(), "the open writes the first page");
        assert!(
            seen.position.is_some() && seen.position != seen.first,
            "sidecar still at {:?} after the turn with no input; loop timeouts {:?}, slept {:?}",
            seen.position,
            seen.timeouts,
            seen.slept
        );
        assert!(seen.slept <= POSITION_WAIT, "saved after {:?}", seen.slept);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The idle delay plus slack for the tap's own timer and slow test machines.
    const POSITION_WAIT: Duration = Duration::from_millis(4000);
}
