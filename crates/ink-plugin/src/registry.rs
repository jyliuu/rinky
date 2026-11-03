//! Extension points plugins register into during `on_load`.
//!
//! Every entry carries the plugin name and an action id. When the user triggers an
//! entry, the app calls [`crate::Plugin::on_action`] of that plugin with the id.

/// Item in the reader's menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    /// Owning plugin.
    pub plugin: String,
    /// Action id passed to `on_action`.
    pub id: String,
    /// Label shown.
    pub label: String,
    /// Sort key (lower first).
    pub order: i32,
}

/// Action offered on a text selection (Insightful's "Explain", dictionary lookup, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionAction {
    /// Owning plugin.
    pub plugin: String,
    /// Action id.
    pub id: String,
    /// Label shown in the selection popup.
    pub label: String,
}

/// A gesture bound to a plugin action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GestureBinding {
    /// Owning plugin.
    pub plugin: String,
    /// Action id.
    pub id: String,
    /// Gesture name: `two_finger_tap`, `long_press`, `swipe_up_from_bottom`,
    /// `corner_hold:top_left`, ...; see [`GESTURES`].
    pub gesture: String,
    /// Description for the gesture settings page.
    pub label: String,
}

/// A function the plugin offers for the reader's gesture map (Settings, Gestures): the
/// user can put it on any reader gesture. Triggering it calls
/// [`crate::Plugin::on_action`] with `id`, with the book open but no selection. The saved
/// action in `settings.toml` is `plugin:<plugin>:<id>`. Reader menu items are offered
/// there too, so register an action only for what the menu does not reach in one tap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderAction {
    /// Owning plugin.
    pub plugin: String,
    /// Action id.
    pub id: String,
    /// Name in the gesture picker; it should say which plugin it belongs to
    /// ("Insightful: Ask about the book").
    pub label: String,
}

/// A page in Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsPage {
    /// Owning plugin.
    pub plugin: String,
    /// Page id (`on_action` receives `settings:<id>` when opened).
    pub id: String,
    /// Title in the settings list.
    pub title: String,
}

/// Action on a book in the library (long-press menu).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryAction {
    /// Owning plugin.
    pub plugin: String,
    /// Action id.
    pub id: String,
    /// Label.
    pub label: String,
}

/// A tile the user can put in the control center (Settings, Control center buttons).
/// Tapping it calls [`crate::Plugin::on_action`] with `id`. The saved id in
/// `settings.toml` is `plugin:<plugin>:<id>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlTile {
    /// Owning plugin.
    pub plugin: String,
    /// Action id (also the tile id).
    pub id: String,
    /// Caption (bold line of the tile; also the name in the settings list).
    pub label: String,
    /// Name of an icon of the app's icon set (`sync`, `download`, `wifi`, ...; any case).
    /// An unknown name draws the info icon.
    pub icon: String,
    /// Text under the caption when the plugin gives no [`crate::Plugin::tile_state`]
    /// (Lua plugins set it with `ink.ui.tile_state`).
    pub state: String,
    /// `Some(text)`: the tile needs Wi-Fi; with Wi-Fi off a tap shows `text` as the
    /// control center's status line and does not call the plugin.
    pub needs_wifi: Option<String>,
    /// Status line shown after a tap (empty: the caption).
    pub working: String,
    /// `Some(action)`: a long press calls `on_action(action)` (three dots in the corner),
    /// for example `settings:<page>` to open the plugin's settings page.
    pub hold: Option<String>,
}

impl ControlTile {
    /// Needs Wi-Fi on; `hint` is shown when it is off.
    pub fn needs_wifi(&mut self, hint: &str) -> &mut Self {
        self.needs_wifi = Some(hint.to_string());
        self
    }

    /// Status line after a tap.
    pub fn working(&mut self, text: &str) -> &mut Self {
        self.working = text.to_string();
        self
    }

    /// Initial state text.
    pub fn state(&mut self, text: &str) -> &mut Self {
        self.state = text.to_string();
        self
    }

    /// A long press calls `on_action(action)`.
    pub fn on_hold(&mut self, action: &str) -> &mut Self {
        self.hold = Some(action.to_string());
        self
    }
}

/// A library-level action (no book): an entry in the library's overflow menu and an item
/// the user can put in the navigation bar (Settings, Navigation bar). Triggering it calls
/// [`crate::Plugin::on_action`] with `id`. The saved id is `plugin:<plugin>:<id>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    /// Owning plugin.
    pub plugin: String,
    /// Action id.
    pub id: String,
    /// Label (menu text, navigation bar caption).
    pub label: String,
    /// Icon name, as for [`ControlTile::icon`].
    pub icon: String,
}

/// State of a control-center tile: the line under the caption and, for toggles, whether
/// it is on (filled).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileState {
    /// Line under the caption.
    pub text: String,
    /// `Some(on)` draws a toggle, `None` an action tile.
    pub on: Option<bool>,
}

/// Prefix of saved ids of plugin entries: `plugin:<plugin>:<id>`.
pub const SAVED_PREFIX: &str = "plugin:";

/// The id under which `settings.toml` saves entry `id` of `plugin`.
pub fn saved_id(plugin: &str, id: &str) -> String {
    format!("{SAVED_PREFIX}{plugin}:{id}")
}

/// Splits a saved id into `(plugin, id)`; `None` for built-in names.
pub fn parse_saved_id(saved: &str) -> Option<(&str, &str)> {
    let (plugin, id) = saved.strip_prefix(SAVED_PREFIX)?.split_once(':')?;
    (!plugin.is_empty() && !id.is_empty()).then_some((plugin, id))
}

/// Where a status-bar widget goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusSlot {
    /// Left of the reader's status bar.
    Left,
    /// Centre.
    Center,
    /// Right.
    Right,
}

impl StatusSlot {
    /// `left`, `center` (or `centre`) or `right`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "left" => Some(StatusSlot::Left),
            "center" | "centre" | "middle" => Some(StatusSlot::Center),
            "right" => Some(StatusSlot::Right),
            _ => None,
        }
    }

    /// The name [`StatusSlot::parse`] accepts.
    pub fn name(self) -> &'static str {
        match self {
            StatusSlot::Left => "left",
            StatusSlot::Center => "center",
            StatusSlot::Right => "right",
        }
    }
}

/// Gesture names plugins can bind (the app maps recognised gestures to these).
pub const GESTURES: &[&str] = &[
    "two_finger_tap",
    "double_tap",
    "long_press",
    "swipe_up_from_bottom",
    "swipe_down_from_top",
    "corner_hold:top_left",
    "corner_hold:top_right",
    "corner_hold:bottom_left",
    "corner_hold:bottom_right",
    "pinch_in",
    "pinch_out",
];

/// A short text the plugin keeps up to date in the status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusWidget {
    /// Owning plugin.
    pub plugin: String,
    /// Widget id.
    pub id: String,
    /// Slot.
    pub slot: StatusSlot,
    /// Current text.
    pub text: String,
}

/// All registrations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Registry {
    /// Reader menu items.
    pub menu_items: Vec<MenuItem>,
    /// Selection actions.
    pub selection_actions: Vec<SelectionAction>,
    /// Gesture bindings.
    pub gestures: Vec<GestureBinding>,
    /// Functions offered for the reader's gesture map.
    pub actions: Vec<ReaderAction>,
    /// Settings pages.
    pub settings_pages: Vec<SettingsPage>,
    /// Library actions.
    pub library_actions: Vec<LibraryAction>,
    /// Status-bar widgets.
    pub status_widgets: Vec<StatusWidget>,
    /// Control-center tiles.
    pub control_tiles: Vec<ControlTile>,
    /// Library-level actions (overflow menu, navigation bar).
    pub library_entries: Vec<LibraryEntry>,
}

/// Registration handle for one plugin (fills in the plugin name).
pub struct Registrar<'a> {
    /// Plugin name.
    pub plugin: &'a str,
    /// Target registry.
    pub registry: &'a mut Registry,
}

impl Registrar<'_> {
    fn s(v: &str) -> String {
        v.to_string()
    }

    /// Adds a reader menu item.
    pub fn menu_item(&mut self, id: &str, label: &str, order: i32) {
        let plugin = Self::s(self.plugin);
        self.registry.menu_items.push(MenuItem { plugin, id: Self::s(id), label: Self::s(label), order });
        self.registry.menu_items.sort_by_key(|m| m.order);
    }

    /// Adds a selection action.
    pub fn selection_action(&mut self, id: &str, label: &str) {
        let plugin = Self::s(self.plugin);
        self.registry.selection_actions.push(SelectionAction { plugin, id: Self::s(id), label: Self::s(label) });
    }

    /// Binds a gesture.
    pub fn gesture(&mut self, id: &str, gesture: &str, label: &str) {
        let plugin = Self::s(self.plugin);
        self.registry.gestures.push(GestureBinding { plugin, id: Self::s(id), gesture: Self::s(gesture), label: Self::s(label) });
    }

    /// Offers a function for the reader's gesture map.
    pub fn action(&mut self, id: &str, label: &str) {
        let plugin = Self::s(self.plugin);
        self.registry.actions.push(ReaderAction { plugin, id: Self::s(id), label: Self::s(label) });
    }

    /// Adds a settings page.
    pub fn settings_page(&mut self, id: &str, title: &str) {
        let plugin = Self::s(self.plugin);
        self.registry.settings_pages.push(SettingsPage { plugin, id: Self::s(id), title: Self::s(title) });
    }

    /// Adds a library action.
    pub fn library_action(&mut self, id: &str, label: &str) {
        let plugin = Self::s(self.plugin);
        self.registry.library_actions.push(LibraryAction { plugin, id: Self::s(id), label: Self::s(label) });
    }

    /// Adds a control-center tile; the returned tile takes optional settings
    /// ([`ControlTile::needs_wifi`], [`ControlTile::working`], [`ControlTile::state`]).
    pub fn control_tile(&mut self, id: &str, label: &str, icon: &str) -> &mut ControlTile {
        let plugin = Self::s(self.plugin);
        let tile = ControlTile { plugin, id: Self::s(id), label: Self::s(label), icon: Self::s(icon), state: String::new(), needs_wifi: None, working: String::new(), hold: None };
        self.registry.control_tiles.push(tile);
        self.registry.control_tiles.last_mut().expect("just pushed")
    }

    /// Adds a library-level action (no book) for the overflow menu and the navigation bar.
    pub fn library_entry(&mut self, id: &str, label: &str, icon: &str) {
        let plugin = Self::s(self.plugin);
        self.registry.library_entries.push(LibraryEntry { plugin, id: Self::s(id), label: Self::s(label), icon: Self::s(icon) });
    }

    /// Adds a status-bar widget.
    pub fn status_widget(&mut self, id: &str, slot: StatusSlot, text: &str) {
        let plugin = Self::s(self.plugin);
        self.registry.status_widgets.push(StatusWidget { plugin, id: Self::s(id), slot, text: Self::s(text) });
    }
}

impl Registry {
    /// Texts of the status widgets in `slot`, joined with two spaces (empty texts skipped).
    pub fn status_line(&self, slot: StatusSlot) -> String {
        let texts: Vec<&str> = self.status_widgets.iter().filter(|w| w.slot == slot && !w.text.is_empty()).map(|w| w.text.as_str()).collect();
        texts.join("  ")
    }

    /// Bindings for gesture `name`.
    pub fn gesture_bindings(&self, name: &str) -> Vec<&GestureBinding> {
        self.gestures.iter().filter(|g| g.gesture == name).collect()
    }

    /// Removes every entry of `plugin` (when it fails or is disabled).
    pub fn remove_plugin(&mut self, plugin: &str) {
        self.menu_items.retain(|e| e.plugin != plugin);
        self.selection_actions.retain(|e| e.plugin != plugin);
        self.gestures.retain(|e| e.plugin != plugin);
        self.actions.retain(|e| e.plugin != plugin);
        self.settings_pages.retain(|e| e.plugin != plugin);
        self.library_actions.retain(|e| e.plugin != plugin);
        self.status_widgets.retain(|e| e.plugin != plugin);
        self.control_tiles.retain(|e| e.plugin != plugin);
        self.library_entries.retain(|e| e.plugin != plugin);
    }

    /// Updates the registered state text of a control tile.
    pub fn set_tile_state(&mut self, plugin: &str, id: &str, text: &str) -> bool {
        match self.control_tiles.iter_mut().find(|t| t.plugin == plugin && t.id == id) {
            Some(t) => {
                t.state = text.to_string();
                true
            }
            None => false,
        }
    }

    /// Updates the text of a status widget.
    pub fn set_status_text(&mut self, plugin: &str, id: &str, text: &str) -> bool {
        match self.status_widgets.iter_mut().find(|w| w.plugin == plugin && w.id == id) {
            Some(w) => {
                w.text = text.to_string();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_sort_and_remove() {
        let mut reg = Registry::default();
        let mut a = Registrar { plugin: "a", registry: &mut reg };
        a.menu_item("late", "Late", 10);
        a.menu_item("early", "Early", -1);
        a.selection_action("explain", "Explain");
        a.status_widget("clock", StatusSlot::Right, "12:00");
        let mut b = Registrar { plugin: "b", registry: &mut reg };
        b.gesture("x", "two_finger_tap", "X");
        b.settings_page("main", "B settings");
        b.library_action("send", "Send");
        b.control_tile("t", "Tile", "sync").needs_wifi("wifi first").working("busy");
        b.library_entry("e", "Entry", "download");
        b.action("act", "B: Act");
        assert_eq!(reg.menu_items[0].id, "early");
        assert_eq!(reg.status_line(StatusSlot::Right), "12:00");
        assert_eq!(reg.status_line(StatusSlot::Left), "");
        assert_eq!(reg.gesture_bindings("two_finger_tap").len(), 1);
        assert_eq!(StatusSlot::parse("Centre"), Some(StatusSlot::Center));
        assert_eq!(StatusSlot::parse("top"), None);
        assert_eq!(StatusSlot::Right.name(), "right");
        assert!(reg.set_status_text("a", "clock", "12:01"));
        assert!(!reg.set_status_text("a", "nope", ""));
        assert_eq!((reg.control_tiles.len(), reg.library_entries.len()), (1, 1));
        assert_eq!(reg.control_tiles[0].needs_wifi.as_deref(), Some("wifi first"));
        assert!(reg.set_tile_state("b", "t", "On"));
        assert!(!reg.set_tile_state("b", "x", "On"));
        assert_eq!(reg.control_tiles[0].state, "On");
        assert_eq!(saved_id("b", "t"), "plugin:b:t");
        assert_eq!(parse_saved_id("plugin:b:t:x"), Some(("b", "t:x")));
        assert_eq!(parse_saved_id("wifi"), None);
        assert_eq!(parse_saved_id("plugin:b"), None);
        let mut copy = reg.clone();
        assert_eq!(copy.actions.len(), 1);
        copy.remove_plugin("b");
        assert!(copy.control_tiles.is_empty() && copy.library_entries.is_empty() && copy.actions.is_empty());
        reg.remove_plugin("a");
        assert!(reg.menu_items.is_empty() && reg.status_widgets.is_empty());
        assert_eq!(reg.gestures.len(), 1);
    }
}
