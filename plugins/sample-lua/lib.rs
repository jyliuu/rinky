//! The sample Lua plugin (word count and reading timer) as data, and its tests.
//!
//! The plugin itself is `plugin.toml` + `main.lua` in this folder; on the Kindle it lives
//! in `/mnt/us/rinky/plugins/sample-lua/`. This crate exists so the folder can be a
//! workspace member (`plugins/*`) and so `cargo test` runs the plugin through the real
//! Lua host. See `docs/plugins.md`.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};

/// Folder and plugin name.
pub const NAME: &str = "sample-lua";
/// `plugin.toml`.
pub const PLUGIN_TOML: &str = include_str!("plugin.toml");
/// `main.lua`.
pub const MAIN_LUA: &str = include_str!("main.lua");

/// Writes the plugin into `plugins_dir/sample-lua/` and returns that folder.
pub fn install(plugins_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = plugins_dir.join(NAME);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("plugin.toml"), PLUGIN_TOML)?;
    std::fs::write(dir.join("main.lua"), MAIN_LUA)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_doc::{Metadata, Position, Selection, TocEntry};
    use ink_plugin::lua::LuaPlugin;
    use ink_plugin::serde_json::json;
    use ink_plugin::{DocumentAccess, HostCtx, Lifecycle, MemoryStorage, PluginError, PluginManager, Result, UiQueue, UiRequest};

    const PAGES: [&str; 3] = ["Two households, both alike in dignity,", "In fair Verona, where we lay our scene,", "From ancient grudge break to new mutiny,"];

    struct Book {
        page: usize,
        exact: bool,
        selection: Option<Selection>,
    }

    fn pos(i: usize) -> Position {
        format!("/body/DocFragment[3]/body/p[{}]/text().0", i + 1).parse().unwrap()
    }

    impl DocumentAccess for Book {
        fn metadata(&self) -> Metadata {
            Metadata { title: "Romeo and Juliet".into(), ..Metadata::default() }
        }
        fn book_id(&self) -> Option<String> {
            None
        }
        fn toc(&self) -> Vec<TocEntry> {
            Vec::new()
        }
        fn search(&mut self, _query: &str, _limit: usize) -> Result<Vec<ink_doc::SearchHit>> {
            Ok(Vec::new())
        }
        fn read_around(&mut self, _pos: &Position, chars: usize) -> Result<String> {
            Ok(PAGES.join(" ").chars().take(chars).collect())
        }
        fn links(&mut self, _pos: &Position) -> Result<Vec<ink_doc::Link>> {
            Ok(Vec::new())
        }
        fn current_position(&self) -> Result<Position> {
            Ok(pos(self.page))
        }
        fn selection(&self) -> Option<Selection> {
            self.selection.clone()
        }
        fn current_page(&self) -> Option<usize> {
            Some(self.page)
        }
        fn page_count(&self) -> Option<usize> {
            Some(PAGES.len())
        }
        fn page_text(&mut self, page: usize) -> Result<String> {
            if self.exact {
                Ok(PAGES[page].to_string())
            } else {
                Err(PluginError::Unsupported("page text".into()))
            }
        }
    }

    struct Env {
        ui: UiQueue,
        store: MemoryStorage,
        book: Book,
        mgr: PluginManager,
    }

    impl Env {
        fn new(test: &str, exact: bool) -> Self {
            let root = std::env::temp_dir().join(format!("ink-sample-lua-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let dir = install(&root).unwrap();
            let plugin = LuaPlugin::load(&dir).unwrap();
            let mut mgr = PluginManager::new();
            mgr.load(vec![Box::new(plugin)]);
            assert!(mgr.errors.is_empty(), "{:?}", mgr.errors);
            Env { ui: UiQueue::default(), store: MemoryStorage { dir: dir.join("data"), ..Default::default() }, book: Book { page: 0, exact, selection: None }, mgr }
        }

        fn cx<R>(&mut self, f: impl FnOnce(&mut PluginManager, &mut HostCtx) -> R) -> R {
            let perms = self.mgr.manifest(NAME).unwrap().permissions.clone();
            let mut cx = HostCtx::new(&mut self.ui, &mut self.store, &perms).with_doc(Some(&mut self.book));
            f(&mut self.mgr, &mut cx)
        }

        fn broadcast(&mut self, ev: Lifecycle) {
            let Env { ui, store, book, mgr } = self;
            mgr.broadcast(&ev, |m, f| {
                let mut cx = HostCtx::new(ui, store, &m.permissions).with_doc(Some(&mut *book));
                f(&mut cx).unwrap();
            });
            assert!(self.mgr.errors.is_empty(), "{:?}", self.mgr.errors);
        }

        fn action(&mut self, id: &str) {
            self.cx(|m, cx| m.action(NAME, id, cx)).unwrap();
        }

        fn answer(&mut self, id: u32, result: &str) {
            self.cx(|m, cx| m.ui_result(NAME, id, result, cx)).unwrap();
        }

        fn take(&mut self) -> Vec<UiRequest> {
            std::mem::take(&mut self.ui.0)
        }

        fn last_toast(&self) -> String {
            self.ui.0.iter().rev().find_map(|r| if let UiRequest::Toast { text } = r { Some(text.clone()) } else { None }).unwrap_or_default()
        }

        fn status(&self) -> Option<String> {
            self.ui.0.iter().rev().find_map(|r| if let UiRequest::Status { text, .. } = r { Some(text.clone()) } else { None })
        }
    }

    #[test]
    fn manifest_and_registrations() {
        let m = ink_plugin::PluginManifest::parse(PLUGIN_TOML).unwrap();
        assert_eq!(m.name, NAME);
        let env = Env::new("registrations", true);
        let r = &env.mgr.registry;
        assert_eq!(r.menu_items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["stats", "note"]);
        assert_eq!(r.selection_actions[0].label, "Count words");
        assert_eq!(r.gestures[0].gesture, "two_finger_tap");
        assert_eq!(r.settings_pages[0].title, "Reading timer");
        assert_eq!(r.library_actions[0].id, "reading_time");
        assert_eq!(r.status_widgets[0].id, "timer");
    }

    #[test]
    fn reading_session_end_to_end() {
        let mut env = Env::new("session", true);
        env.store.book.insert("seconds".into(), json!(600));
        env.store.book.insert("pages".into(), json!(40));
        env.broadcast(Lifecycle::BookOpen);
        assert_eq!(env.status().as_deref(), Some("Read 10 min"));
        for page in 0..3 {
            env.book.page = page;
            env.broadcast(Lifecycle::PageChange(page));
        }
        env.broadcast(Lifecycle::Suspend);
        env.broadcast(Lifecycle::Resume);

        // Stats viewer.
        env.take();
        env.action("stats");
        let reqs = env.take();
        let UiRequest::TextViewer { markdown, .. } = &reqs[0] else { panic!("{reqs:?}") };
        assert!(markdown.starts_with("# Romeo and Juliet"), "{markdown}");
        assert!(markdown.contains("- Page 3 of 3"), "{markdown}");
        assert!(markdown.contains("Words on this page: **7**"), "{markdown}");
        assert!(markdown.contains("| Pages turned | 2 | 42 |"), "{markdown}");
        assert!(markdown.contains("pages per hour"), "{markdown}");

        // Selection action and gesture.
        env.book.selection = Some(Selection { start: pos(0), end: pos(0), text: "both alike in dignity".into(), page: 0, rects: vec![] });
        let sel = env.book.selection.clone().unwrap();
        env.broadcast(Lifecycle::Selection(sel));
        env.action("count");
        assert_eq!(env.last_toast(), "4 words, 21 characters");
        env.action("timer_toast");
        assert_eq!(env.last_toast(), "Reading for 0 min (book: 10 min)");

        // Note through the keyboard.
        env.take();
        env.action("note");
        let reqs = env.take();
        let UiRequest::TextInput { id, prompt, .. } = &reqs[0] else { panic!("{reqs:?}") };
        assert_eq!(prompt, "Note for page 3");
        env.answer(*id, "a pair of star-cross'd lovers");
        assert_eq!(env.last_toast(), "Note saved");
        let notes = std::fs::read_to_string(env.store.dir.join("notes.md")).unwrap();
        assert!(notes.ends_with("Romeo and Juliet p. 3: a pair of star-cross'd lovers\n"), "{notes}");

        // Settings page: toggle the timer, then reset through the dialog.
        env.take();
        env.action("settings:timer");
        let reqs = env.take();
        let UiRequest::Menu { id, items, .. } = &reqs[0] else { panic!("{reqs:?}") };
        assert_eq!(items[0].0, "Show the timer in the status bar: on");
        env.answer(*id, "toggle");
        assert_eq!(env.store.settings.get("show_timer"), Some(&json!(false)));
        assert_eq!(env.status().as_deref(), Some(""));
        env.take();
        env.action("settings:timer");
        let UiRequest::Menu { id, items, .. } = env.take().remove(0) else { panic!() };
        assert_eq!(items[0].0, "Show the timer in the status bar: off");
        env.answer(id, "reset");
        let UiRequest::Dialog { id, buttons, .. } = env.take().remove(0) else { panic!() };
        assert_eq!(buttons[1].1, "reset");
        env.answer(id, "reset");
        assert_eq!(env.last_toast(), "Reading time reset");
        assert!(env.store.book.get("seconds").is_none());

        // Close: totals saved, one session line.
        env.book.page = 0;
        env.broadcast(Lifecycle::PageChange(0));
        env.broadcast(Lifecycle::Close);
        assert_eq!(env.last_toast(), "Session: 0 min, 1 pages");
        assert_eq!(env.store.book.get("pages"), Some(&json!(1)));
        let csv = std::fs::read_to_string(env.store.dir.join("sessions.csv")).unwrap();
        assert!(csv.trim_end().ends_with(",\"Romeo and Juliet\",0,1"), "{csv}");
        assert_eq!(csv.lines().count(), 1);

        // Library action reads the per-book data of the chosen book.
        env.cx(|m, cx| m.library_action(NAME, "reading_time", Path::new("/mnt/us/Books/romeo.epub"), cx)).unwrap();
        assert_eq!(env.last_toast(), "romeo.epub: 0 min, 1 pages");
        assert!(env.mgr.errors.is_empty(), "{:?}", env.mgr.errors);
    }

    #[test]
    fn estimate_without_page_text() {
        let mut env = Env::new("estimate", false);
        env.broadcast(Lifecycle::BookOpen);
        env.broadcast(Lifecycle::PageChange(0));
        env.take();
        env.action("stats");
        let UiRequest::TextViewer { markdown, .. } = env.take().remove(0) else { panic!() };
        assert!(markdown.contains("Words near this position: **21**"), "{markdown}");
        env.action("count");
        assert_eq!(env.last_toast(), "Nothing is selected");
    }
}
