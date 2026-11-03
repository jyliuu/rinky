//! Plugin entries in the lists the user customizes: the control-center buttons
//! (`settings.ui.control_buttons`), the navigation bar (`settings.ui.nav_items`) and the
//! reader's gesture map (`settings.gestures`, [`gesture_choices`]).
//!
//! A built-in item is saved by its plain name (`wifi`, `stats`). A plugin item is saved as
//! `plugin:<plugin>:<id>` ([`ink_plugin::registry::saved_id`]). The settings pages list
//! the built-in names followed by the entries of the enabled plugins
//! ([`control_choices`], [`nav_choices`]); the screens that show the lists skip saved ids
//! that no registered entry answers (plugin removed or turned off) with one log line
//! ([`control_chosen`], [`nav_chosen`]).

use crate::plugin_host::PluginHost;
use ink_plugin::registry::{parse_saved_id, saved_id};
use ink_store::prefs::{CONTROL_BUTTONS, NAV_ITEMS};
use ink_store::Settings;

/// The saved id older versions used for the KOSync tile (`sync`).
pub const LEGACY_SYNC: &str = "sync";

/// The canonical saved id of `name`: the old built-in `sync` button is the KOSync plugin's
/// `sync_now` tile now.
pub fn canonical(name: &str) -> String {
    if name == LEGACY_SYNC {
        saved_id(kosync::NAME, "sync_now")
    } else {
        name.to_string()
    }
}

/// `(saved id, label)` of the control tiles of the enabled plugins.
pub fn control_choices(host: &PluginHost) -> Vec<(String, String)> {
    host.control_tiles().iter().map(|t| (saved_id(&t.plugin, &t.id), t.label.clone())).collect()
}

/// `(saved id, label)` of the library entries of the enabled plugins.
pub fn nav_choices(host: &PluginHost) -> Vec<(String, String)> {
    host.library_entries().iter().map(|e| (saved_id(&e.plugin, &e.id), e.label.clone())).collect()
}

/// `(saved id, label)` of what the enabled plugins offer for the reader's gesture map:
/// their registered actions, then their reader menu items (an item that is also an action
/// is listed once).
pub fn gesture_choices(host: &PluginHost) -> Vec<(String, String)> {
    let actions = host.actions().iter().map(|a| (&a.plugin, &a.id, &a.label));
    let menu = host.menu_items().iter().map(|m| (&m.plugin, &m.id, &m.label));
    let mut out: Vec<(String, String)> = Vec::new();
    for (plugin, id, label) in actions.chain(menu) {
        let saved = saved_id(plugin, id);
        if !out.iter().any(|(s, _)| *s == saved) {
            out.push((saved, label.clone()));
        }
    }
    out
}

/// `base` followed by `extra`, as the `(name, label)` list the ordered-choice rows take.
pub fn merged<'a>(base: &[(&'a str, &'a str)], extra: &'a [(String, String)]) -> Vec<(&'a str, &'a str)> {
    base.iter().copied().chain(extra.iter().map(|(n, l)| (n.as_str(), l.as_str()))).collect()
}

/// True when `name` is a built-in control button or a registered plugin tile.
pub fn control_known(host: &PluginHost, name: &str) -> bool {
    match parse_saved_id(name) {
        Some((plugin, id)) => host.control_tiles().iter().any(|t| t.plugin == plugin && t.id == id),
        None => CONTROL_BUTTONS.iter().any(|(n, _)| *n == name),
    }
}

/// True when `name` is a built-in navigation item or a registered library entry.
pub fn nav_known(host: &PluginHost, name: &str) -> bool {
    match parse_saved_id(name) {
        Some((plugin, id)) => host.library_entry(plugin, id).is_some(),
        None => NAV_ITEMS.iter().any(|(n, _)| *n == name),
    }
}

/// The control-center buttons to show: `settings.ui.control_buttons` with the legacy
/// name mapped, without duplicates, and without names `known` refuses (logged), at most
/// `max`.
pub fn control_chosen(s: &Settings, max: usize, known: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in &s.ui.control_buttons {
        let b = canonical(b);
        if !known(&b) {
            log::info!("control center: button {b} is not available (plugin removed or off); skipped");
        } else if !out.contains(&b) && out.len() < max {
            out.push(b);
        }
    }
    out
}

/// The navigation items to show: `settings.ui.nav_items` without unknown names (logged).
pub fn nav_chosen(s: &Settings, host: &PluginHost) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in &s.ui.nav_items {
        if !nav_known(host, n) {
            log::info!("navigation bar: item {n} is not available (plugin removed or off); skipped");
        } else if !out.contains(n) {
            out.push(n.clone());
        }
    }
    out
}

/// The label of navigation item `name` for the settings summary.
pub fn nav_label(host: &PluginHost, name: &str) -> String {
    match parse_saved_id(name) {
        Some((plugin, id)) => host.library_entry(plugin, id).map_or_else(|| name.to_string(), |e| e.label.clone()),
        None => ink_store::prefs::label(NAV_ITEMS, name).to_string(),
    }
}

/// A plugin for the screen tests: a tile "Probe go", a library entry "Probe lib", and a
/// list of the actions it was called with.
#[cfg(test)]
pub(crate) mod test_plugin {
    use ink_plugin::{HostCtx, Plugin, PluginManifest, Registrar, Result, TileState};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Action ids the plugin was called with, shared with the test.
    pub(crate) type Calls = Rc<RefCell<Vec<String>>>;

    pub(crate) struct Probe {
        manifest: PluginManifest,
        pub(crate) calls: Calls,
    }

    impl Probe {
        pub(crate) fn new(calls: &Calls) -> Self {
            Probe { manifest: PluginManifest::new("probe", "1", &[]), calls: calls.clone() }
        }

        /// A host with only this plugin.
        pub(crate) fn host(calls: &Calls) -> crate::plugin_host::PluginHost {
            crate::plugin_host::PluginHost::new(vec![Box::new(Probe::new(calls))])
        }
    }

    impl Plugin for Probe {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
            reg.control_tile("go", "Probe go", "chart").on_hold("settings:go");
            reg.library_entry("lib", "Probe lib", "download");
            reg.action("act", "Probe: Act");
            reg.menu_item("act", "Probe act (menu)", 1);
            reg.menu_item("menu", "Probe menu", 2);
            Ok(())
        }
        fn on_action(&mut self, _cx: &mut HostCtx, id: &str) -> Result<()> {
            self.calls.borrow_mut().push(id.to_string());
            Ok(())
        }
        fn tile_state(&self, id: &str) -> Option<TileState> {
            let taps = self.calls.borrow().iter().filter(|c| *c == "go").count();
            (id == "go").then(|| TileState { text: format!("taps {taps}"), on: Some(taps > 0) })
        }
    }

    /// A plugin with settings: the page "Toggler options" and a tile "Toggler" whose
    /// long press opens the same settings (`settings:opts`). The settings are a menu
    /// (id [`Toggler::MENU`]): a status note, an "Auto start" switch, a "Server" value and
    /// a "Done" action that closes it; a tap on the switch flips it and sends the menu
    /// again. `calls` records `on_action` ids and `result:<answer>` for menu answers.
    pub(crate) struct Toggler {
        manifest: PluginManifest,
        pub(crate) calls: Calls,
        auto: bool,
    }

    impl Toggler {
        /// Request id of the settings menu.
        pub(crate) const MENU: u32 = 1;

        /// A host with only this plugin.
        pub(crate) fn host(calls: &Calls) -> crate::plugin_host::PluginHost {
            let p = Toggler { manifest: PluginManifest::new("toggler", "1", &[ink_plugin::Permission::Ui]), calls: calls.clone(), auto: false };
            crate::plugin_host::PluginHost::new(vec![Box::new(p)])
        }

        fn menu(&self, cx: &mut HostCtx) -> Result<()> {
            let auto = format!("Auto start\t{}", if self.auto { "on" } else { "off" });
            cx.ui()?.menu(Self::MENU, "Toggler", &[("Ready", ""), (auto.as_str(), "auto"), ("Server\tauto", "server"), ("Done", "done")]);
            Ok(())
        }
    }

    impl Plugin for Toggler {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
            reg.settings_page("opts", "Toggler options");
            reg.control_tile("t", "Toggler", "chart").on_hold("settings:opts");
            Ok(())
        }
        fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
            self.calls.borrow_mut().push(id.to_string());
            if id == "settings:opts" {
                self.menu(cx)?;
            }
            Ok(())
        }
        fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("result:{result}"));
            if id == Self::MENU && result == "auto" {
                self.auto = !self.auto;
                self.menu(cx)?;
            }
            Ok(())
        }
    }

    /// A tile whose state changes on its own: `early`, then `late` from one second
    /// after the plugin loads (like the SSH server's address when a network goes away).
    pub(crate) struct Drift {
        manifest: PluginManifest,
        loaded: std::time::Instant,
    }

    impl Drift {
        /// A host with only this plugin.
        pub(crate) fn host() -> crate::plugin_host::PluginHost {
            let p = Drift { manifest: PluginManifest::new("drift", "1", &[]), loaded: std::time::Instant::now() };
            crate::plugin_host::PluginHost::new(vec![Box::new(p)])
        }
    }

    impl Plugin for Drift {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
            reg.control_tile("d", "Drift", "chart");
            Ok(())
        }
        fn tile_state(&self, id: &str) -> Option<TileState> {
            let late = self.loaded.elapsed() >= std::time::Duration::from_secs(1);
            (id == "d").then(|| TileState { text: if late { "late" } else { "early" }.into(), on: Some(late) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_plugin::Probe;
    use super::*;

    #[test]
    fn choices_chosen_and_known_follow_the_registered_entries() {
        let calls = Default::default();
        let host = Probe::host(&calls);
        assert_eq!(control_choices(&host), [("plugin:probe:go".to_string(), "Probe go".to_string())]);
        assert_eq!(nav_choices(&host), [("plugin:probe:lib".to_string(), "Probe lib".to_string())]);
        let g = gesture_choices(&host);
        assert_eq!(g, [("plugin:probe:act".to_string(), "Probe: Act".to_string()), ("plugin:probe:menu".to_string(), "Probe menu".to_string())]);
        assert!(control_known(&host, "plugin:probe:go") && control_known(&host, "wifi"));
        assert!(!control_known(&host, "plugin:probe:nope") && !control_known(&host, "plugin:gone:go") && !control_known(&host, "bogus"));
        assert!(nav_known(&host, "plugin:probe:lib") && nav_known(&host, "stats") && !nav_known(&host, "plugin:probe:go"));
        let mut s = Settings::default();
        s.ui.nav_items = ["home", "plugin:gone:x", "plugin:probe:lib", "home", "nope"].map(String::from).to_vec();
        assert_eq!(nav_chosen(&s, &host), ["home", "plugin:probe:lib"]);
        assert_eq!(nav_label(&host, "plugin:probe:lib"), "Probe lib");
        assert_eq!(nav_label(&host, "stats"), "Statistics");
        assert_eq!(nav_label(&host, "plugin:gone:x"), "plugin:gone:x");
        s.ui.control_buttons = ["sync", "plugin:probe:go"].map(String::from).to_vec();
        assert_eq!(control_chosen(&s, 8, |n| control_known(&host, n)), ["plugin:probe:go"], "no kosync here: the legacy sync button is skipped");
    }

    #[test]
    fn legacy_sync_becomes_the_kosync_tile() {
        assert_eq!(canonical("sync"), "plugin:kosync:sync_now");
        assert_eq!(canonical("wifi"), "wifi");
    }

    #[test]
    fn chosen_skips_unknown_and_repeated_names() {
        let mut s = Settings::default();
        s.ui.control_buttons = ["wifi", "plugin:gone:x", "sync", "wifi", "bogus", "plugin:kosync:sync_now"].map(String::from).to_vec();
        let known = |n: &str| n == "wifi" || n == "plugin:kosync:sync_now";
        assert_eq!(control_chosen(&s, 8, known), ["wifi", "plugin:kosync:sync_now"]);
        assert_eq!(control_chosen(&s, 1, known), ["wifi"]);
    }

    #[test]
    fn merged_lists_built_ins_first() {
        let extra = vec![("plugin:a:b".to_string(), "B".to_string())];
        let m = merged(NAV_ITEMS, &extra);
        assert_eq!(m.len(), NAV_ITEMS.len() + 1);
        assert_eq!(m.last(), Some(&("plugin:a:b", "B")));
        assert_eq!(ink_store::prefs::label(&m, "plugin:a:b"), "B");
    }
}
