//! Screen stack and router.
//!
//! A [`Route`] names a screen. Each screen module registers a factory for its route kind
//! with one [`ScreenRegistry::register`] call (`screens::<name>::register`). The app keeps
//! a stack of [`AppScreen`]s; a screen answers events with a [`Transition`].

use crate::plugin_host::PluginHost;
use ink_device::{Battery, Canvas, Clock, DeviceInfo, Light, Power, Rect, RefreshClass, Screen, Usb, Wifi};
use ink_doc::Provider;
use ink_store::{Paths, Settings};
use ink_text::Fonts;
use ink_ui::{Damage, RefreshScheduler, Theme, UiEvent};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// Where to go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// Read a book.
    Reader(PathBuf),
    /// The library / home screen.
    Library,
    /// Settings.
    Settings,
    /// Widget gallery.
    Demo,
    /// Quick light panel over the screen below (swipe down from the top edge).
    QuickLight,
    /// Reading statistics (screens::stats).
    Statistics,
    /// Vocabulary and flashcards (screens::stats::vocab).
    Vocabulary,
}

impl Route {
    /// Registry key of this route.
    pub fn kind(&self) -> &'static str {
        match self {
            Route::Reader(_) => "reader",
            Route::Library => "library",
            Route::Settings => "settings",
            Route::Demo => "demo",
            Route::QuickLight => "quicklight",
            Route::Statistics => "statistics",
            Route::Vocabulary => "vocabulary",
        }
    }
}

/// What a screen wants after an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    /// Stay.
    None,
    /// Open a screen on top.
    Push(Route),
    /// Replace this screen.
    Replace(Route),
    /// Close this screen (exit when it was the last).
    Pop,
    /// Leave the app.
    Exit,
}

/// Everything a screen may use while handling an event or painting.
pub struct AppCtx<'a> {
    /// Drawing target, logical screen size.
    pub canvas: &'a mut Canvas,
    /// The display, for screens that must show something before a slow step.
    pub screen: &'a mut dyn Screen,
    /// UI fonts.
    pub fonts: &'a mut Fonts,
    /// Theme.
    pub theme: &'a Theme,
    /// Page-turn refresh policy.
    pub refresh: &'a mut RefreshScheduler,
    /// Plugins.
    pub plugins: &'a mut PluginHost,
    /// Settings.
    pub settings: &'a Settings,
    /// File locations.
    pub paths: &'a Paths,
    /// Document providers, tried in order.
    pub providers: &'a [&'a dyn Provider],
    /// Device services other than screen and input (light, Wi-Fi, ...).
    pub services: Services<'a>,
    /// A screen that changes settings puts the new value here; the app takes it after
    /// the event, keeps it and saves it to `paths.settings()`.
    pub settings_update: &'a mut Option<Settings>,
}

/// The device services a screen may use (borrowed from [`ink_device::Device`]).
pub struct Services<'a> {
    /// Backend and model.
    pub info: &'a DeviceInfo,
    /// Frontlight.
    pub light: &'a mut dyn Light,
    /// Battery.
    pub battery: &'a mut dyn Battery,
    /// Sleep, screensaver, suspend.
    pub power: &'a mut dyn Power,
    /// Wi-Fi.
    pub wifi: &'a mut dyn Wifi,
    /// USB.
    pub usb: &'a mut dyn Usb,
    /// Clock.
    pub clock: &'a mut dyn Clock,
}

impl Services<'_> {
    /// Borrows the services again for a shorter lifetime (an `AppCtx` built inside
    /// another one, as in `plugin_host::hooks::app_paint`).
    pub fn reborrow(&mut self) -> Services<'_> {
        Services {
            info: self.info,
            light: &mut *self.light,
            battery: &mut *self.battery,
            power: &mut *self.power,
            wifi: &mut *self.wifi,
            usb: &mut *self.usb,
            clock: &mut *self.clock,
        }
    }
}

impl AppCtx<'_> {
    /// Shows `rect` of the canvas now, outside the app loop's coalesced frame. For
    /// animation frames and other updates that must reach the panel before the paint
    /// returns; everything else is returned as damage from `AppScreen::paint`.
    pub fn present_now(&mut self, rect: Rect, class: RefreshClass) -> anyhow::Result<()> {
        self.screen.update(self.canvas, rect, class)?;
        Ok(())
    }

    /// Older name of [`AppCtx::present_now`].
    pub fn show(&mut self, rect: Rect, class: RefreshClass) -> anyhow::Result<()> {
        self.present_now(rect, class)
    }

    /// Shows the whole canvas now.
    pub fn show_all(&mut self, class: RefreshClass) -> anyhow::Result<()> {
        let r = self.canvas.bounds();
        self.show(r, class)
    }
}

/// A full-screen view.
pub trait AppScreen {
    /// Name for logs.
    fn name(&self) -> &'static str;

    /// Called when the screen becomes the top of the stack (also after the screen above
    /// it was popped).
    fn on_enter(&mut self, _cx: &mut AppCtx) -> anyhow::Result<()> {
        Ok(())
    }

    /// Called before the screen is removed from the stack.
    fn on_exit(&mut self, _cx: &mut AppCtx) {}

    /// The part of `screen` this screen draws over (the screen below shows elsewhere).
    /// Default: all of it. A panel over the screen below (quick light) returns the
    /// panel, so a pop shows only that rect again.
    fn covered_rect(&self, screen: Rect) -> Rect {
        screen
    }

    /// Called when the screen above was popped and this one is the top again:
    /// `exposed` is what the popped screen covered, `hint` the class for it
    /// (`Change::Pop`). The default does what a new entry does ([`AppScreen::on_enter`]).
    fn on_reveal(&mut self, cx: &mut AppCtx, _exposed: Rect, _hint: RefreshClass) -> anyhow::Result<()> {
        self.on_enter(cx)
    }

    /// Called on the top screen after the logical screen size changed (rotation); the
    /// canvas already has the new size. The default lays the screen out again through
    /// `on_enter`.
    fn on_resize(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        self.on_enter(cx)
    }

    /// Handles one event.
    fn handle(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition>;

    /// Repaints what changed into `cx.canvas` and returns the damage to show.
    fn paint(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>>;

    /// The open document, if any: the app loop passes it to plugins when it delivers
    /// their dialog answers, network results and suspend/resume (`plugin_host::hooks`).
    fn document(&mut self) -> Option<&mut dyn ink_doc::Document> {
        None
    }

    /// Shows `position` (a position string from a plugin, `UiRequest::GoTo`); returns
    /// false when the screen has no book.
    fn plugin_go_to(&mut self, _position: &str) -> bool {
        false
    }

    /// How long the event loop may sleep before calling `handle(UiEvent::Timer(0))`;
    /// `None` sleeps until input.
    fn timeout(&self) -> Option<Duration> {
        None
    }

    /// How the gesture recogniser reports double taps while this screen is on top
    /// (asked before every wait). Default: taps at once, a double tap after the second.
    fn double_tap_mode(&self, _settings: &Settings) -> ink_ui::DoubleTapMode {
        ink_ui::DoubleTapMode::After
    }
}

/// Builds a screen for a route.
pub type ScreenFactory = fn(&Route) -> anyhow::Result<Box<dyn AppScreen>>;

/// Route kind -> factory.
#[derive(Default)]
pub struct ScreenRegistry {
    factories: BTreeMap<&'static str, ScreenFactory>,
}

impl ScreenRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the factory for a route kind (`"reader"`, `"library"`, ...).
    pub fn register(&mut self, kind: &'static str, factory: ScreenFactory) {
        self.factories.insert(kind, factory);
    }

    /// Builds the screen for `route`.
    pub fn build(&self, route: &Route) -> anyhow::Result<Box<dyn AppScreen>> {
        match self.factories.get(route.kind()) {
            Some(f) => f(route),
            None => anyhow::bail!("no screen registered for route {:?}", route.kind()),
        }
    }

    /// Registered kinds.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.factories.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_builds_registered_routes() {
        let mut reg = ScreenRegistry::new();
        crate::screens::register_all(&mut reg);
        assert_eq!(reg.kinds(), vec!["demo", "library", "quicklight", "reader", "settings", "statistics", "vocabulary"]);
        for r in [Route::Library, Route::Settings, Route::Demo, Route::Reader("x.epub".into())] {
            assert!(reg.build(&r).is_ok(), "{r:?}");
        }
        assert!(ScreenRegistry::new().build(&Route::Library).is_err());
    }
}
