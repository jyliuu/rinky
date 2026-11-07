//! Small helpers the compiled-in plugins share: typed reads of plugin settings and
//! menus built from owned labels.
//!
//! They are inherent methods of [`HostCtx`] and go through its checked accessors, so a
//! plugin without the `storage` permission reads every setting as unset.

use crate::host::{HostCtx, UiRequest};
use crate::Result;
use serde_json::Value;

impl HostCtx<'_> {
    /// The plugin setting `key`; `None` when it is unset or the plugin has no `storage`
    /// permission.
    pub fn setting(&mut self, key: &str) -> Option<Value> {
        self.storage().ok()?.get(key)
    }

    /// The setting `key` as a boolean, `default` when it is unset or not a boolean.
    pub fn setting_bool(&mut self, key: &str, default: bool) -> bool {
        self.setting(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    /// The setting `key` as a string (as stored, not trimmed); `None` when it is unset or
    /// not a string.
    pub fn setting_str(&mut self, key: &str) -> Option<String> {
        match self.setting(key)? {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The setting `key` as an unsigned integer; `None` when it is unset or not one.
    pub fn setting_u64(&mut self, key: &str) -> Option<u64> {
        self.setting(key)?.as_u64()
    }

    /// Opens a menu of `(label, action)` items (owned or borrowed strings), see
    /// [`UiRequest::Menu`]. Needs the `ui` permission.
    pub fn menu<L: Into<String>, A: Into<String>>(&mut self, id: u32, title: &str, items: impl IntoIterator<Item = (L, A)>) -> Result<()> {
        let items = items.into_iter().map(|(l, a)| (l.into(), a.into())).collect();
        self.ui()?.request(UiRequest::Menu { id, title: title.into(), items });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{HostCtx, MemoryStorage, Permission, PluginError, UiQueue, UiRequest};
    use serde_json::json;

    #[test]
    fn typed_settings_and_menus() {
        let mut ui = UiQueue::default();
        let mut st = MemoryStorage::default();
        for (k, v) in [("on", json!(true)), ("name", json!("x ")), ("n", json!(7)), ("neg", json!(-1))] {
            st.settings.insert(k.into(), v);
        }
        let perms = [Permission::Ui, Permission::Storage];
        let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
        assert!(cx.setting_bool("on", false) && cx.setting_bool("missing", true) && !cx.setting_bool("name", false));
        assert_eq!(cx.setting_str("name").as_deref(), Some("x "));
        assert_eq!((cx.setting_str("n"), cx.setting_u64("n"), cx.setting_u64("neg"), cx.setting_u64("name")), (None, Some(7), None, None));
        let label = String::from("Owned");
        cx.menu(3, "T", [(label.as_str(), "a"), ("Borrowed", "b")]).unwrap();
        cx.menu(4, "U", vec![(label, String::from("c"))]).unwrap();
        assert_eq!(ui.0[0], UiRequest::Menu { id: 3, title: "T".into(), items: vec![("Owned".into(), "a".into()), ("Borrowed".into(), "b".into())] });
        assert_eq!(ui.0[1].id(), Some(4));

        // Without the permissions: settings read as unset, menus are refused.
        let mut cx = HostCtx::new(&mut ui, &mut st, &[]);
        assert_eq!((cx.setting("on"), cx.setting_bool("on", false)), (None, false));
        assert!(matches!(cx.menu(5, "V", [("a", "a")]), Err(PluginError::Permission(_))));
    }
}
