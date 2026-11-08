//! Long-press options of control center buttons ([`ink_store::prefs::CONTROL_BUTTONS`]).
//!
//! A button listed in [`SPECS`] has more options behind a long press. The tile shows
//! three dots in its top right corner; a long press on it reports [`LONG`]`:<name>`
//! and the control center asks [`options`] what to show. A long press always opens a
//! popup, never a page: the settings popup ([`super::popup::SettingsPopup`], the same
//! component for every button and for plugin tiles), with settings rows built from the
//! button's [`Options`] ([`Options::popup`]):
//!
//! - A choice ([`setting_options`]): a setting with a few values (Rotate: the angle a tap turns to;
//!   Full refresh: how often pages flash; Status bars: the top and bottom bar switches).
//!   The Settings page "Control center buttons" shows the same values as choice rows
//!   ([`settings_rows`], [`settings_action`]), or a link to the button's settings page
//!   when it has one ([`Spec::page`]).
//! - A picker: a list read from the device (Wi-Fi: the networks, with their
//!   signal bars and locks, as on the Wi-Fi page). Settings links to the full page.
//!
//! A row may open the button's full settings page ("All networks…", "All status bar
//! settings…"); that is the user's choice in the popup, not what the long press does.
//!
//! To give another button a long press: add a [`Spec`] line and a case in
//! [`setting_options`] (a choice) or [`options`] (a picker), and its choices in
//! [`choose`].
//!
//! The popup opens with one partial GL16 update (its box between the panel header and
//! the status line), closes with one (the panel under it repaints); a tap outside it,
//! its Close button, Back or a swipe up closes it. A switch in it repaints its row only.
//!
//! Rotate: a tap turns the screen from upright to the chosen angle
//! (`settings.ui.rotate_angle`, default 90) and from any other angle back upright
//! ([`rotate_tap`]). Choosing an angle in the control center stores it and turns the
//! screen there at once; choosing it in Settings only stores it (Settings, Orientation
//! turns the screen).
//!
//! Wi-Fi: the popup lists the last scan (`scan_list`, no new scan on opening) with the
//! connected network first, then saved networks out of range; a tap joins an open or
//! saved network (`connect`, the status line says "Connecting…" first), a secured new
//! one opens Settings on its password page (`join:<essid>`, the settings keyboard).
//! "Scan again" scans (up to 20 s on the Kindle) and lists again; "All networks…" opens
//! the settings Wi-Fi page.

use super::popup::SettingsPopup;
use super::rows::{Lead, Row, Trail};
use super::{current_settings, edit_settings, refresh_name, rotation_name, REFRESH_CHOICES};
use crate::router::{AppCtx, Route, Transition};
use ink_device::{Rotation, WifiNetwork, WifiState};
use ink_store::prefs::{label, CONTROL_BUTTONS, ROTATE_ANGLES};
use ink_store::Settings;
use ink_ui::Icon;

/// Action of a long press on a tile: `cc-long:<button name>`.
pub const LONG: &str = "cc-long";
/// Actions of the picker: `cc-pick:<item index>`, or [`PICK_CLOSE`].
pub const PICK: &str = "cc-pick";
/// A tap outside the picker.
pub const PICK_CLOSE: &str = "cc-pick:close";
/// Settings row actions: `ccadv:<button name>:<item index>`.
pub const SETTINGS_PREFIX: &str = "ccadv";

/// A button with long-press options.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    /// Button name ([`CONTROL_BUTTONS`]).
    pub name: &'static str,
    /// What the long press does (row subtitle in Settings).
    pub what: &'static str,
    /// Settings page with these and more options ([`super::open_at`] name): Settings
    /// links to it instead of listing the choices.
    pub page: Option<&'static str>,
    /// Row icon in Settings.
    pub icon: Icon,
}

/// The buttons with long-press options, in Settings order.
pub const SPECS: &[Spec] = &[
    Spec { name: "wifi", what: "Lists the networks; tap one to join it", page: Some("wifi"), icon: Icon::Wifi },
    Spec { name: "rotate", what: "Chooses the angle a tap turns to", page: None, icon: Icon::Rotate },
    Spec { name: "status_bars", what: "Turns the top and bottom bar on and off", page: Some("status_bars"), icon: Icon::Info },
    Spec { name: "full_refresh", what: "Chooses how often pages fully refresh", page: Some("refresh"), icon: Icon::Star },
];

/// The [`SPECS`] entry of button `name`.
pub fn spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.name == name)
}

/// True when button `name` has long-press options.
pub fn has_options(name: &str) -> bool {
    spec(name).is_some()
}

/// What choosing an item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Rotate: the angle in degrees.
    Angle(u16),
    /// Wi-Fi: join this network.
    Network {
        /// Name.
        essid: String,
        /// Needs a password.
        secured: bool,
        /// A profile is saved.
        known: bool,
    },
    /// Wi-Fi: scan and list again.
    Scan,
    /// Wi-Fi: turn the radio on and list again.
    WifiOn,
    /// Full refresh: every this many pages (`reader.flash_every`, 0 = never).
    Refresh(u32),
    /// Status bars: switch the top (`true`) or bottom bar.
    Bar(bool),
    /// Open this settings page ([`super::open_at`] name).
    Page(String),
}

/// One row of a long-press popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// Text on the left.
    pub label: String,
    /// Value on the right (`Connected · 3/5`, `Saved`...).
    pub detail: String,
    /// `Some`: one of several choices (a check mark when true); `None`: an action row.
    pub selected: Option<bool>,
    /// `Some`: a switch in this state.
    pub switch: Option<bool>,
    /// Left side (Wi-Fi signal bars).
    pub lead: Lead,
    /// A lock (a secured network).
    pub lock: bool,
    /// What a tap does.
    pub choice: Choice,
}

impl Item {
    fn action(label: impl Into<String>, choice: Choice) -> Self {
        Item { label: label.into(), detail: String::new(), selected: None, switch: None, lead: Lead::None, lock: false, choice }
    }

    fn choice(label: impl Into<String>, selected: bool, choice: Choice) -> Self {
        Item { selected: Some(selected), ..Item::action(label, choice) }
    }

    fn switch(label: impl Into<String>, on: bool, choice: Choice) -> Self {
        Item { switch: Some(on), ..Item::action(label, choice) }
    }

    /// The settings row of item `i`: action [`PICK`]`:<i>` (a switch adds `:on`/`:off`);
    /// a row that opens a settings page has a chevron.
    pub fn row(&self, i: usize) -> Row {
        let action = format!("{PICK}:{i}");
        let mut row = match (self.switch, self.selected) {
            (Some(on), _) => Row::toggle(self.label.clone(), on, action),
            (None, Some(c)) => Row::choice(self.label.clone(), c, action),
            (None, None) => Row::item(self.label.clone(), action),
        };
        if matches!(self.choice, Choice::Page(_)) {
            row = row.trail(Trail::Chevron);
        }
        if !self.detail.is_empty() {
            row = row.value(self.detail.clone());
        }
        row.lead(self.lead).lock(self.lock)
    }
}

/// The options of one button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Picker title.
    pub title: String,
    /// One line under the title (empty: none).
    pub note: String,
    /// Rows.
    pub items: Vec<Item>,
}

impl Options {
    /// The rows of these options ([`Item::row`]).
    pub fn rows(&self) -> Vec<Row> {
        self.items.iter().enumerate().map(|(i, item)| item.row(i)).collect()
    }

    /// The settings popup for these options; its Close button reports [`PICK_CLOSE`].
    pub fn popup(&self) -> SettingsPopup {
        SettingsPopup::new(self.title.clone(), self.rows()).with_note(self.note.clone()).with_close(PICK_CLOSE)
    }
}

/// Most rows of a popup that fits in a box `h` pixels tall (with a note line).
pub fn max_items(h: u32) -> usize {
    SettingsPopup::rows_fit(h, true)
}

// -- Rotate -------------------------------------------------------------------------------------

/// Index into [`Rotation::ALL`] (`settings.device.rotation`) of `deg` degrees clockwise.
pub fn angle_index(deg: u16) -> u8 {
    ((deg / 90) % 4) as u8
}

/// `90° · Landscape`.
pub fn angle_label(deg: u16) -> String {
    format!("{deg}° · {}", rotation_name(Rotation::ALL[angle_index(deg) as usize]))
}

/// The rotation (`settings.device.rotation`) a tap on Rotate turns to from `current`:
/// upright to the angle `deg`, any other rotation back upright.
pub fn rotate_tap(current: u8, deg: u16) -> u8 {
    if !current.is_multiple_of(4) {
        0
    } else {
        angle_index(deg)
    }
}

/// The options of a button with a choice, from the settings alone.
pub fn setting_options(name: &str, s: &Settings) -> Option<Options> {
    match name {
        "rotate" => {
            let now = s.ui.rotate_degrees();
            let items = ROTATE_ANGLES
                .iter()
                .map(|&d| Item::choice(angle_label(d), d == now, Choice::Angle(d)))
                .collect();
            Some(Options { title: "Rotate to".into(), note: "A tap turns upright and back to this angle".into(), items })
        }
        "full_refresh" => {
            let now = s.reader.flash_every;
            let items = REFRESH_CHOICES.iter().map(|&n| Item::choice(refresh_name(n), n == now, Choice::Refresh(n))).collect();
            Some(Options { title: "Full refresh".into(), note: "Clears ghosting with a flash of the whole page".into(), items })
        }
        "status_bars" => {
            let b = &s.status_bars;
            let items = vec![
                Item::switch("Top bar", b.top.enabled, Choice::Bar(true)),
                Item::switch("Bottom bar", b.bottom.enabled, Choice::Bar(false)),
                Item::action("All status bar settings…", Choice::Page("status_bars".into())),
            ];
            Some(Options { title: "Status bars".into(), note: "Shown while you read".into(), items })
        }
        _ => None,
    }
}

// -- Wi-Fi --------------------------------------------------------------------------------------

/// The Wi-Fi picker: `state` (`None`: no Wi-Fi), the last scan (`Err`: why there is
/// none), the saved profiles as `(name, secured)`, at most `max` rows.
pub fn wifi_options(state: Option<&WifiState>, airplane: bool, scanned: Result<Vec<WifiNetwork>, String>, saved: &[(String, bool)], max: usize) -> Options {
    let title = "Wi-Fi networks".to_string();
    let all = Item::action("All networks…", Choice::Page("wifi".into()));
    let Some(state) = state else {
        return Options { title, note: "Wi-Fi is not available".into(), items: vec![all] };
    };
    if !state.enabled || airplane {
        let note = if airplane { "Airplane mode is on" } else { "Wi-Fi is off" };
        let mut items = vec![all];
        if !airplane {
            items.insert(0, Item::action("Turn on Wi-Fi", Choice::WifiOn));
        }
        return Options { title, note: note.into(), items };
    }
    let (mut nets, mut note) = match scanned {
        Ok(n) => (n, String::new()),
        Err(e) => (Vec::new(), format!("No list: {e}")),
    };
    let connected = state.essid.clone();
    // Connected first, then by signal (stable: the scan order breaks ties).
    nets.sort_by_key(|n| (connected.as_deref() != Some(n.essid.as_str()), std::cmp::Reverse(n.signal)));
    let mut items: Vec<Item> = Vec::new();
    for n in &nets {
        if items.iter().any(|i| i.label == n.essid) {
            continue;
        }
        let is_on = connected.as_deref() == Some(n.essid.as_str());
        let known = n.known || saved.iter().any(|(e, _)| *e == n.essid);
        let kind = if is_on {
            "Connected"
        } else if known {
            "Saved"
        } else if n.secured {
            "Password"
        } else {
            "Open"
        };
        items.push(Item {
            detail: kind.into(),
            lead: Lead::Signal(n.signal),
            lock: n.secured,
            ..Item::choice(n.essid.clone(), is_on, Choice::Network { essid: n.essid.clone(), secured: n.secured, known })
        });
    }
    for (e, secured) in saved {
        if !items.iter().any(|i| &i.label == e) {
            // Out of range (or not scanned yet): a join may still find it.
            items.push(Item {
                detail: "Saved".into(),
                lead: Lead::Icon(Icon::Wifi),
                lock: *secured,
                ..Item::choice(e.clone(), connected.as_deref() == Some(e.as_str()), Choice::Network { essid: e.clone(), secured: *secured, known: true })
            });
        }
    }
    let room = max.saturating_sub(2).max(1);
    if items.len() > room {
        items.truncate(room);
        if note.is_empty() {
            note = "More under All networks".into();
        }
    }
    if items.is_empty() && note.is_empty() {
        note = "No networks listed yet: tap Scan again".into();
    }
    items.push(Item::action("Scan again", Choice::Scan));
    items.push(all);
    Options { title, note, items }
}

/// The Wi-Fi picker from the device services.
fn wifi_options_from(cx: &mut AppCtx, max: usize) -> Options {
    let wifi = &cx.services.wifi;
    let state = wifi.state().ok();
    let airplane = wifi.airplane_mode().unwrap_or(false);
    let scanned = wifi.scan_list().map_err(|e| e.to_string());
    let saved: Vec<(String, bool)> = wifi.profiles().map(|p| p.into_iter().map(|p| (p.essid, p.secured)).collect()).unwrap_or_default();
    wifi_options(state.as_ref(), airplane, scanned, &saved, max)
}

// -- Opening and choosing -----------------------------------------------------------------------

/// The popup options of a long press on button `name` (`None`: it has none); a device
/// list has at most `max` rows.
pub fn options(name: &str, max: usize, cx: &mut AppCtx) -> Option<Options> {
    spec(name)?;
    match name {
        "wifi" => Some(wifi_options_from(cx, max)),
        _ => setting_options(name, current_settings(cx)),
    }
}

/// Opens settings on page `page` (closing the control center).
pub fn open_settings(page: &str) -> Transition {
    log::info!("control center: open settings page {page}");
    super::open_at(page);
    Transition::Replace(Route::Settings)
}

/// The status line to show before `choice` runs a call that blocks.
pub fn busy_text(choice: &Choice) -> Option<String> {
    match choice {
        Choice::Network { essid, secured, known } if !*secured || *known => Some(format!("Connecting to {essid}…")),
        Choice::Scan => Some("Scanning…".into()),
        Choice::WifiOn => Some("Turning on Wi-Fi…".into()),
        _ => None,
    }
}

/// What the control center does after a choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Close the popup and show this status.
    Done(String),
    /// Show the options again (their new state) and this status.
    Again(String),
    /// Open this settings page.
    Settings(String),
}

/// Runs `choice` from the control center popup.
pub fn choose(choice: &Choice, cx: &mut AppCtx) -> Outcome {
    log::info!("control center: choose {choice:?}");
    match choice {
        Choice::Angle(d) => {
            let d = *d;
            edit_settings(cx, |s| {
                s.ui.rotate_angle = d;
                s.device.rotation = angle_index(d);
            });
            log::info!("control center: rotate angle {d}, rotation {}", angle_index(d));
            // The app turns the screen after this event; on_resize builds the panel again.
            Outcome::Done(format!("Rotate: {}", angle_label(d)))
        }
        Choice::Network { essid, secured, known } => {
            if *secured && !*known {
                return Outcome::Settings(format!("join:{essid}"));
            }
            match cx.services.wifi.connect(essid, None) {
                Ok(()) => Outcome::Done(format!("Connected to {essid}")),
                // A saved password may be wrong: ask for it again.
                Err(e) if *secured => {
                    log::info!("control center: cannot connect to {essid}: {e}");
                    Outcome::Settings(format!("join:{essid}"))
                }
                Err(e) => Outcome::Again(format!("Cannot connect to {essid}: {e}")),
            }
        }
        Choice::Scan => match cx.services.wifi.scan() {
            Ok(n) => Outcome::Again(format!("{} network{} found", n.len(), if n.len() == 1 { "" } else { "s" })),
            Err(e) => Outcome::Again(format!("Scan failed: {e}")),
        },
        Choice::WifiOn => match cx.services.wifi.set_enabled(true) {
            Ok(()) => Outcome::Again("Wi-Fi on".into()),
            Err(e) => Outcome::Again(format!("Wi-Fi: {e}")),
        },
        Choice::Refresh(n) => {
            let n = *n;
            // App::take_settings_update gives the scheduler the new value at once.
            edit_settings(cx, |s| s.reader.flash_every = n);
            Outcome::Done(format!("Full refresh: {}", refresh_name(n)))
        }
        Choice::Bar(top) => {
            let top = *top;
            let mut on = false;
            edit_settings(cx, |s| {
                let b = if top { &mut s.status_bars.top } else { &mut s.status_bars.bottom };
                b.enabled = !b.enabled;
                on = b.enabled;
            });
            Outcome::Again(format!("{} bar {}", if top { "Top" } else { "Bottom" }, if on { "on" } else { "off" }))
        }
        Choice::Page(p) => Outcome::Settings(p.clone()),
    }
}

// -- Settings page ------------------------------------------------------------------------------

/// The "Long-press options" rows of Settings, Control center buttons.
pub fn settings_rows(s: &Settings) -> Vec<Row> {
    let mut rows = vec![Row::header("Long-press options"), Row::note("Hold a control center button to open its options.")];
    for spec in SPECS {
        let name = label(CONTROL_BUTTONS, spec.name);
        match (setting_options(spec.name, s), spec.page) {
            (_, Some(page)) => rows.push(Row::nav(spec.icon, name, format!("page:{page}")).subtitle(spec.what)),
            (Some(o), None) => {
                rows.push(Row::header(format!("{name}: {}", o.title.to_lowercase())));
                for (i, item) in o.items.iter().enumerate() {
                    let row = Row::choice(item.label.clone(), item.selected == Some(true), format!("{SETTINGS_PREFIX}:{}:{i}", spec.name));
                    rows.push(row.icon(spec.icon));
                }
                if !o.note.is_empty() {
                    rows.push(Row::note(format!("{}.", o.note)));
                }
            }
            _ => {}
        }
    }
    rows
}

/// Runs a [`settings_rows`] action (`None`: not one). Choosing an angle only stores it.
pub fn settings_action(action: &str, cx: &mut AppCtx) -> Option<String> {
    let rest = action.strip_prefix(SETTINGS_PREFIX)?.strip_prefix(':')?;
    let (name, i) = rest.rsplit_once(':')?;
    let o = setting_options(name, current_settings(cx))?;
    let item = o.items.get(i.parse::<usize>().ok()?)?;
    match item.choice {
        Choice::Angle(d) => {
            edit_settings(cx, |s| s.ui.rotate_angle = d);
            log::info!("settings: rotate button angle {d}");
            Some(format!("Rotate turns to {}", angle_label(d)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(essid: &str, signal: u8, secured: bool, known: bool) -> WifiNetwork {
        WifiNetwork { essid: essid.into(), signal, secured, known }
    }

    #[test]
    fn rotate_tap_toggles_between_upright_and_the_chosen_angle() {
        assert_eq!(rotate_tap(0, 90), 1);
        assert_eq!(rotate_tap(1, 90), 0);
        assert_eq!(rotate_tap(0, 270), 3);
        assert_eq!(rotate_tap(2, 270), 0, "another angle goes upright first");
        assert_eq!(rotate_tap(0, 0), 0, "angle 0: a tap keeps it upright");
        assert_eq!(angle_label(180), "180° · Portrait, upside down");
    }

    #[test]
    fn every_spec_names_a_control_button_and_opens_something() {
        let s = Settings::default();
        for spec in SPECS {
            assert!(CONTROL_BUTTONS.iter().any(|(n, _)| *n == spec.name), "{}", spec.name);
            // Wi-Fi is the one picker (a device list, with a settings page); every other
            // button is a choice the settings answer.
            match spec.name {
                "wifi" => assert!(spec.page.is_some(), "{}", spec.name),
                _ => assert!(setting_options(spec.name, &s).is_some(), "{}", spec.name),
            }
            if let Some(p) = spec.page {
                assert!(super::super::Page::from_name(p).is_some(), "settings page {p}");
            }
        }
        assert!(has_options("rotate") && has_options("wifi") && !has_options("sleep"));
        assert_eq!(super::super::Page::from_name("join:Office"), Some(super::super::Page::Join("Office".into())));
        assert_eq!(super::super::Page::from_name("join:"), None);
        // Settings shows the rotate angles as choices, the others as links.
        let rows = settings_rows(&s);
        assert!(rows.iter().filter(|r| r.action.starts_with("ccadv:rotate:")).count() == 4);
        assert!(rows.iter().any(|r| r.action == "page:wifi"));
        assert!(rows.iter().any(|r| r.action == "page:refresh") && rows.iter().any(|r| r.action == "page:status_bars"));
        // Full refresh and the status bars have popup options (a long press is never a page).
        let o = setting_options("full_refresh", &s).unwrap();
        assert_eq!(o.items.iter().filter(|i| i.selected == Some(true)).count(), 1);
        let o = setting_options("status_bars", &s).unwrap();
        let rows = o.rows();
        assert_eq!(rows[0].trail, Trail::Toggle(s.status_bars.top.enabled));
        assert_eq!((rows[1].action.as_str(), rows[2].trail), ("cc-pick:1", Trail::Chevron));
    }

    #[test]
    fn wifi_picker_lists_connected_first_saved_out_of_range_and_fits() {
        let on = WifiState { enabled: true, state: "CONNECTED".into(), essid: Some("Cafe".into()), signal: Some(3) };
        let scan = vec![net("Home", 5, true, true), net("Cafe", 3, false, false), net("Office", 2, true, false)];
        let saved = vec![("Home".to_string(), true), ("Cabin".to_string(), false)];
        let o = wifi_options(Some(&on), false, Ok(scan.clone()), &saved, 9);
        let labels: Vec<&str> = o.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Cafe", "Home", "Office", "Cabin", "Scan again", "All networks…"]);
        assert_eq!(o.items[0].selected, Some(true));
        assert_eq!(o.items[3].choice, Choice::Network { essid: "Cabin".into(), secured: false, known: true });
        assert_eq!((o.items[2].detail.as_str(), o.items[2].lead, o.items[2].lock), ("Password", Lead::Signal(2), true));
        assert_eq!(o.items[2].choice, Choice::Network { essid: "Office".into(), secured: true, known: false });
        // Too many for the box: cut, with a note.
        let o = wifi_options(Some(&on), false, Ok(scan), &saved, 4);
        assert_eq!(o.items.len(), 4);
        assert_eq!(o.note, "More under All networks");
        // Off: turn it on.
        let off = WifiState { enabled: false, ..WifiState::default() };
        let o = wifi_options(Some(&off), false, Err("Wi-Fi is off".into()), &saved, 9);
        assert_eq!(o.items[0].choice, Choice::WifiOn);
        assert_eq!(o.note, "Wi-Fi is off");
        assert_eq!(busy_text(&Choice::Network { essid: "Office".into(), secured: true, known: false }), None, "asks for a password");
    }

    #[test]
    fn options_become_settings_rows_in_the_popup() {
        let on = WifiState { enabled: true, state: "CONNECTED".into(), essid: Some("Cafe".into()), signal: Some(3) };
        let o = wifi_options(Some(&on), false, Ok(vec![net("Cafe", 3, false, false), net("Office", 2, true, false)]), &[], 9);
        let rows = o.rows();
        assert_eq!((rows[0].lead, rows[0].trail, rows[0].lock), (Lead::Signal(3), Trail::Check(true), false));
        assert_eq!((rows[1].lead, rows[1].lock, rows[1].value.as_deref()), (Lead::Signal(2), true, Some("Password")));
        assert_eq!(rows[1].action, format!("{PICK}:1"));
        assert_eq!(rows.last().map(|r| r.trail), Some(Trail::Chevron), "All networks opens a page");
        let p = o.popup();
        assert_eq!((p.title.as_str(), p.close_action.as_str()), ("Wi-Fi networks", PICK_CLOSE));
        assert!(max_items(1000) >= 6);
    }
}
