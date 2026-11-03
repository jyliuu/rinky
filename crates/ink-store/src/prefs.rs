//! Customization settings shared by several screens: the action names, the reader
//! gesture map, the top and bottom status bars, the UI font, keyboard layouts, the
//! bottom navigation bar and the control-center buttons.
//!
//! Everything is stored as plain strings in `settings.toml`, so a newer file still loads
//! in an older binary and the other way round: an unknown action, widget or item name is
//! kept in the file and treated as `none` / skipped by the screens. The lists of known
//! names ([`ACTIONS`], [`GESTURES`], [`STATUS_WIDGETS`], [`NAV_ITEMS`],
//! [`CONTROL_BUTTONS`], [`KEYBOARD_LAYOUTS`]) carry the English label the settings
//! screens show.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Actions a gesture can run, as `(name, label)`. `plugin:<plugin>:<item>` (a plugin
/// action or menu item, listed by the settings page from the plugin registry) is also
/// accepted and is not listed here.
pub const ACTIONS: &[(&str, &str)] = &[
    ("none", "Nothing"),
    ("next_page", "Next page"),
    ("prev_page", "Previous page"),
    ("menu", "Reader menu"),
    ("reading_settings", "Reading settings (Aa)"),
    ("toc", "Table of contents"),
    ("goto", "Go to page"),
    ("bookmark", "Toggle bookmark"),
    ("bookmarks", "Bookmarks and highlights"),
    ("search", "Search in book"),
    ("back", "Back (after a link or footnote)"),
    ("control_center", "Control center"),
    ("light_up", "Brighter"),
    ("light_down", "Dimmer"),
    ("warmth_up", "Warmer"),
    ("warmth_down", "Cooler"),
    ("night_mode", "Toggle night mode"),
    ("font_bigger", "Larger text"),
    ("font_smaller", "Smaller text"),
    ("zoom_in", "Zoom in (PDF)"),
    ("zoom_out", "Zoom out (PDF)"),
    ("status_bars", "Show or hide status bars"),
    ("full_refresh", "Full screen refresh"),
    ("rotate", "Rotate screen"),
    ("library", "Library"),
    ("home", "Home"),
    ("sleep", "Sleep"),
    ("exit", "Exit to Kindle (asks first)"),
];

/// Reader gestures (`trigger`, label). Corners are squares of 1/8 of the screen width;
/// edge swipes start within 1/20 of the screen height (or width) of that edge.
pub const GESTURES: &[(&str, &str)] = &[
    ("tap_left", "Tap left side"),
    ("tap_right", "Tap right side"),
    ("tap_center", "Tap middle"),
    ("tap_top_left", "Tap top-left corner"),
    ("tap_top_right", "Tap top-right corner"),
    ("tap_bottom_left", "Tap bottom-left corner"),
    ("tap_bottom_right", "Tap bottom-right corner"),
    ("double_tap", "Double tap"),
    ("long_press", "Long press on a word"),
    ("swipe_left", "Swipe left"),
    ("swipe_right", "Swipe right"),
    ("swipe_up", "Swipe up"),
    ("swipe_down", "Swipe down"),
    ("swipe_up_from_bottom", "Swipe up from the bottom edge"),
    ("swipe_down_from_top", "Swipe down from the top edge"),
    ("swipe_right_from_left", "Swipe in from the left edge"),
    ("swipe_left_from_right", "Swipe in from the right edge"),
    ("two_finger_swipe_up", "Two-finger swipe up"),
    ("two_finger_swipe_down", "Two-finger swipe down"),
    ("two_finger_swipe_left", "Two-finger swipe left"),
    ("two_finger_swipe_right", "Two-finger swipe right"),
    ("two_finger_tap", "Two-finger tap"),
    ("pinch_in", "Pinch in"),
    ("pinch_out", "Spread (pinch out)"),
];

/// The default action of each gesture in [`GESTURES`].
pub const DEFAULT_GESTURES: &[(&str, &str)] = &[
    ("tap_left", "prev_page"),
    ("tap_right", "next_page"),
    ("tap_center", "menu"),
    ("tap_top_left", "bookmark"),
    ("tap_top_right", "none"),
    ("tap_bottom_left", "none"),
    ("tap_bottom_right", "none"),
    ("double_tap", "none"),
    ("long_press", "dictionary"),
    ("swipe_left", "next_page"),
    ("swipe_right", "prev_page"),
    ("swipe_up", "none"),
    ("swipe_down", "none"),
    ("swipe_up_from_bottom", "reading_settings"),
    ("swipe_down_from_top", "control_center"),
    ("swipe_right_from_left", "back"),
    ("swipe_left_from_right", "none"),
    ("two_finger_swipe_up", "light_up"),
    ("two_finger_swipe_down", "light_down"),
    ("two_finger_swipe_left", "warmth_down"),
    ("two_finger_swipe_right", "warmth_up"),
    ("two_finger_tap", "status_bars"),
    ("pinch_in", "font_smaller"),
    ("pinch_out", "font_bigger"),
];

/// `long_press` also accepts `dictionary` (select the word and look it up) and
/// `select` (select the word only).
pub const LONG_PRESS_ACTIONS: &[(&str, &str)] = &[("dictionary", "Select word and look it up"), ("select", "Select word")];

/// Status-bar widgets, as `(name, label)`.
pub const STATUS_WIDGETS: &[(&str, &str)] = &[
    ("clock", "Clock"),
    ("battery", "Battery %"),
    ("battery_icon", "Battery icon"),
    ("battery_icon_percent", "Battery icon and %"),
    ("wifi", "Wi-Fi"),
    ("frontlight", "Light level"),
    ("page", "Page number"),
    ("page_of_total", "Page of total"),
    ("percent", "Percent read"),
    ("chapter_title", "Chapter title"),
    ("book_title", "Book title"),
    ("author", "Author"),
    ("pages_left_chapter", "Pages left in chapter"),
    ("time_left_chapter", "Time left in chapter"),
    ("time_left_book", "Time left in book"),
    ("bookmark", "Bookmark mark"),
];

/// Progress-bar styles of a status bar, as `(name, label)`.
pub const PROGRESS_BARS: &[(&str, &str)] = &[("none", "None"), ("thin", "Thin"), ("thick", "Thick"), ("chapters", "Thin with chapter ticks")];

/// Built-in bottom navigation bar items, as `(name, label)`. Plugins add library entries,
/// saved as `plugin:<plugin>:<id>`.
pub const NAV_ITEMS: &[(&str, &str)] = &[
    ("home", "Home"),
    ("library", "Library"),
    ("collections", "Collections"),
    ("files", "Files"),
    ("search", "Search"),
    ("stats", "Statistics"),
    ("opds", "Catalogs (OPDS)"),
    ("settings", "Settings"),
];

/// Built-in control-center buttons, as `(name, label)`. Plugins add their own tiles, saved
/// as `plugin:<plugin>:<id>` (the old `sync` name is the KOSync plugin's
/// `plugin:kosync:sync_now`).
pub const CONTROL_BUTTONS: &[(&str, &str)] = &[
    ("night_mode", "Night mode"),
    ("auto_warmth", "Auto warmth"),
    ("wifi", "Wi-Fi"),
    ("rotate", "Rotate"),
    ("full_refresh", "Refresh screen"),
    ("status_bars", "Status bars"),
    ("screenshot", "Screenshot"),
    ("sleep", "Sleep"),
    ("settings", "Settings"),
];

/// On-screen keyboard layouts, as `(name, label)`.
pub const KEYBOARD_LAYOUTS: &[(&str, &str)] = &[
    ("en", "English (QWERTY)"),
    ("da", "Danish"),
    ("de", "German (QWERTZ)"),
    ("fr", "French (AZERTY)"),
    ("es", "Spanish"),
    ("sv", "Swedish"),
    ("nb", "Norwegian"),
];

/// The label of `name` in one of the lists above, or `name` itself.
pub fn label<'a>(list: &[(&'a str, &'a str)], name: &'a str) -> &'a str {
    list.iter().find(|(n, _)| *n == name).map(|(_, l)| *l).unwrap_or(name)
}

/// Reader gesture map: trigger name -> action name. Triggers missing from the file use
/// [`DEFAULT_GESTURES`]; [`GestureMap::action`] is what the reader asks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GestureMap(pub BTreeMap<String, String>);

impl Default for GestureMap {
    fn default() -> Self {
        GestureMap(DEFAULT_GESTURES.iter().map(|(g, a)| (g.to_string(), a.to_string())).collect())
    }
}

impl GestureMap {
    /// The action for `trigger`: the stored one, else the default, else `none`.
    pub fn action(&self, trigger: &str) -> &str {
        if let Some(a) = self.0.get(trigger) {
            return a;
        }
        DEFAULT_GESTURES.iter().find(|(g, _)| *g == trigger).map(|(_, a)| *a).unwrap_or("none")
    }

    /// Sets `trigger` to `action`.
    pub fn set(&mut self, trigger: &str, action: &str) {
        self.0.insert(trigger.to_string(), action.to_string());
    }
}

/// One status bar (top or bottom of the reading page).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusBar {
    /// Drawn at all.
    pub enabled: bool,
    /// Widgets at the left, in order (names from [`STATUS_WIDGETS`]).
    pub left: Vec<String>,
    /// Widgets in the centre, in order.
    pub center: Vec<String>,
    /// Widgets at the right, in order.
    pub right: Vec<String>,
    /// Progress bar style (names from [`PROGRESS_BARS`]).
    pub progress_bar: String,
    /// Text size in pixels.
    pub font_px: f32,
    /// Text between two widgets in one slot.
    pub separator: String,
}

impl Default for StatusBar {
    fn default() -> Self {
        StatusBar {
            enabled: true,
            left: Vec::new(),
            center: Vec::new(),
            right: Vec::new(),
            progress_bar: "none".into(),
            font_px: 26.0,
            separator: " · ".into(),
        }
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Distance of each status bar from its screen edge, in pixels.
pub const BAR_EDGE_PX: i32 = 16;
/// Height of a thin progress bar's box: an 8 px track and 8 px ticks above and below.
pub const BAR_THIN_BOX_H: u32 = 24;
/// Height of a thick progress bar's box (a 12 px track).
pub const BAR_THICK_BOX_H: u32 = 36;
/// Gap between the progress bar and the text row of one bar.
pub const BAR_ROW_GAP: u32 = 4;
/// Smallest bar text size.
pub const BAR_MIN_FONT_PX: f32 = 16.0;
/// Largest bar text size.
pub const BAR_MAX_FONT_PX: f32 = 44.0;

impl StatusBar {
    /// Text size: `font_px` within [`BAR_MIN_FONT_PX`]..=[`BAR_MAX_FONT_PX`] (26 when
    /// not a number).
    pub fn text_px(&self) -> f32 {
        if self.font_px.is_finite() {
            self.font_px.clamp(BAR_MIN_FONT_PX, BAR_MAX_FONT_PX)
        } else {
            26.0
        }
    }

    /// True when the progress bar style is not `none`.
    pub fn has_progress(&self) -> bool {
        matches!(self.progress_bar.as_str(), "thin" | "thick" | "chapters")
    }

    /// Height of the bar's band: its text row (`text`), its progress box (`progress`),
    /// or both with [`BAR_ROW_GAP`] between them; 0 with neither.
    pub fn band_h(&self, text: bool, progress: bool) -> i32 {
        let text_h = (self.text_px() * 1.4).ceil() as i32;
        let box_h = if self.progress_bar == "thick" { BAR_THICK_BOX_H } else { BAR_THIN_BOX_H } as i32;
        match (text, progress) {
            (true, true) => text_h + BAR_ROW_GAP as i32 + box_h,
            (true, false) => text_h,
            (false, true) => box_h,
            (false, false) => 0,
        }
    }

    /// Pixels the bar takes from its screen edge to its inner edge ([`BAR_EDGE_PX`] plus
    /// the band), 0 when it is off or has no widgets and no progress bar. The reader's
    /// top and bottom margins are measured from there.
    pub fn room(&self) -> f32 {
        let text = self.enabled && [&self.left, &self.center, &self.right].iter().any(|s| !s.is_empty());
        match self.band_h(text, self.enabled && self.has_progress()) {
            0 => 0.0,
            h => (BAR_EDGE_PX + h) as f32,
        }
    }
}

/// Current [`StatusBars::version`].
pub const STATUS_BARS_VERSION: u32 = 2;

/// The reader's top and bottom status bars.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusBars {
    /// Layout version of this table: 2 since the old-style mapping turns the top bar on
    /// (a file without the key was written by the first mapping, which turned it off;
    /// see `Settings` loading).
    pub version: u32,
    /// Above the text.
    pub top: StatusBar,
    /// Below the text.
    pub bottom: StatusBar,
}

impl Default for StatusBars {
    /// Top: clock left, chapter title centre, battery right. Bottom: the chapter-tick
    /// progress bar, time left in chapter left, percent right (the old `footer = "full"`).
    fn default() -> Self {
        StatusBars {
            version: STATUS_BARS_VERSION,
            top: StatusBar { left: strings(&["clock"]), center: strings(&["chapter_title"]), right: strings(&["battery"]), ..StatusBar::default() },
            bottom: StatusBar {
                left: strings(&["time_left_chapter"]),
                right: strings(&["percent"]),
                progress_bar: "chapters".into(),
                ..StatusBar::default()
            },
        }
    }
}

/// UI customization.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// UI font family name (empty: the built-in font).
    pub font_family: String,
    /// Enabled keyboard layouts; the first is the default, the globe key cycles them.
    pub keyboard_layouts: Vec<String>,
    /// Bottom navigation bar items, left to right (2 to 5).
    pub nav_items: Vec<String>,
    /// Control-center buttons, in order.
    pub control_buttons: Vec<String>,
    /// Angle (degrees clockwise: 0, 90, 180 or 270) the control center's Rotate button
    /// turns to from upright; a tap on it at any other angle goes back upright. Chosen
    /// by a long press on the button or in Settings, Control center buttons. Other
    /// values read as [`DEFAULT_ROTATE_ANGLE`] ([`UiSettings::rotate_degrees`]).
    pub rotate_angle: u16,
    /// Ask before "Exit to Kindle".
    pub confirm_exit: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            font_family: String::new(),
            keyboard_layouts: strings(&["en"]),
            nav_items: strings(&["home", "library", "collections", "settings"]),
            control_buttons: strings(&["night_mode", "auto_warmth", "wifi", "rotate", "full_refresh", "plugin:kosync:sync_now"]),
            rotate_angle: DEFAULT_ROTATE_ANGLE,
            confirm_exit: true,
        }
    }
}

/// Angles the Rotate button may turn to, in degrees clockwise.
pub const ROTATE_ANGLES: [u16; 4] = [0, 90, 180, 270];

/// The Rotate button's angle when none (or an unknown one) is stored: landscape.
pub const DEFAULT_ROTATE_ANGLE: u16 = 90;

impl UiSettings {
    /// The Rotate button's angle: [`UiSettings::rotate_angle`] when it is one of
    /// [`ROTATE_ANGLES`], else [`DEFAULT_ROTATE_ANGLE`].
    pub fn rotate_degrees(&self) -> u16 {
        if ROTATE_ANGLES.contains(&self.rotate_angle) {
            self.rotate_angle
        } else {
            DEFAULT_ROTATE_ANGLE
        }
    }
}

/// Page-turn animation value: `on` or `off`. Older files used `slide`, `curl` and
/// `fade`, which read as `on`; anything else but `off` reads as `on`.
pub fn page_animation_on(value: &str) -> bool {
    value != "off"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_every_gesture_and_name_known_actions() {
        for (g, _) in GESTURES {
            assert!(DEFAULT_GESTURES.iter().any(|(d, _)| d == g), "{g} has no default");
        }
        let map = GestureMap::default();
        for (g, a) in DEFAULT_GESTURES {
            assert_eq!(map.action(g), *a);
            assert!(ACTIONS.iter().chain(LONG_PRESS_ACTIONS).any(|(n, _)| n == a), "{a} unknown");
        }
        assert_eq!(GestureMap(BTreeMap::new()).action("swipe_up_from_bottom"), "reading_settings");
        assert_eq!(map.action("no_such_gesture"), "none");
        assert!(page_animation_on("slide") && page_animation_on("on") && !page_animation_on("off"));
        assert_eq!(label(NAV_ITEMS, "stats"), "Statistics");
        assert_eq!(label(NAV_ITEMS, "unknown"), "unknown");
    }
}
