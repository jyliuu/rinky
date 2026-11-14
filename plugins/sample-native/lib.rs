//! A sample native plugin: a shared library rinky loads from
//! `<RINKY_HOME>/plugins/sample-native/` (see `plugin.toml` next to this file and
//! `docs/plugins.md`, "Native plugins").
//!
//! It adds a reader menu item that counts the words on the page and shows the count in
//! a card, remembers how often it was used, and offers a control-center tile. Build it
//! for the Kindle with
//! `cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p sample-native`.

use ink_plugin_native::{export_plugin, json, Host, Plugin, Registrar, Result, TileState};

/// The plugin state: how many counts this session.
#[derive(Default)]
pub struct WordCount {
    counted: u32,
}

impl Plugin for WordCount {
    const NAME: &'static str = "sample-native";

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.menu_item("count", "Word count", 50)?;
        reg.control_tile("count", "Word count", "info")?;
        reg.log("sample-native loaded");
        Ok(())
    }

    fn on_action(&mut self, host: &mut Host, id: &str) -> Result<()> {
        match id {
            "count" => {
                let text = host.page_text(None)?;
                let words = text.split_whitespace().count();
                self.counted += 1;
                let total = host.setting("total")?.and_then(|v| v.as_u64()).unwrap_or(0) + 1;
                host.set_setting("total", &json!(total))?;
                let times = if total == 1 { "once".to_string() } else { format!("{total} times") };
                host.view("Word count", &format!("**{words}** words on this page.\n\nYou have counted words {times} so far."))?;
                Ok(())
            }
            other => Err(format!("unknown action {other}").into()),
        }
    }

    fn tile_state(&self, id: &str) -> Option<TileState> {
        (id == "count").then(|| TileState { text: format!("{} this session", self.counted), on: None })
    }
}

export_plugin!(WordCount);
