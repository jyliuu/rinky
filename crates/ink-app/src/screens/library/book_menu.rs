//! The menu a held book opens (Home hero and shelves, Library list and grid: every view
//! that records a `Hit::Book`). Its rows reuse the existing actions: open, the details
//! page, the reading status, the collections menu, plugin actions and delete with
//! confirmation. It is an `Overlay::Menu`, so a tap outside closes it; showing and
//! closing are one GL16 repaint each (no flash, no animation).

use super::{Hit, LibraryScreen, MenuKind, View};
use crate::router::Transition;
use ink_device::Size;
use ink_store::ReadingStatus;
use std::path::PathBuf;

impl LibraryScreen {
    /// The rows of the menu for book `p`: (label, action).
    pub(super) fn book_menu_items(&self, p: &std::path::Path) -> Vec<(String, String)> {
        let status = self.lib.as_ref().and_then(|l| l.index.get(p)).map(|b| b.status);
        let mut items = vec![("Open".to_string(), "open".to_string()), ("Book details".to_string(), "details".to_string())];
        if status == Some(ReadingStatus::Complete) {
            items.push(("Mark as unread".into(), "unread".into()));
        } else {
            items.push(("Mark as finished".into(), "finished".into()));
        }
        items.push(("Add to collection…".into(), "collections".into()));
        items.push(("Export highlights".into(), "export".into()));
        if !self.plugin_actions.is_empty() {
            items.push(("More actions…".into(), "plugins".into()));
        }
        items.push(("Delete…".into(), "delete".into()));
        items
    }

    /// Opens the menu of book `p` next to its cover or row.
    pub(super) fn open_book_menu(&mut self, p: PathBuf, size: Size) {
        log::info!("library: book menu for {} (view {:?})", p.display(), self.view);
        let items = self.book_menu_items(&p);
        self.open_menu(items, MenuKind::Book(p.clone()), Hit::Book(p), size);
    }

    /// Runs the row `a` picked in the menu of book `p`.
    pub(super) fn book_menu_action(&mut self, p: PathBuf, a: &str, size: Size) -> Transition {
        log::info!("library: book menu {a} on {}", p.display());
        match a {
            "open" => return self.open_book(p),
            "details" => self.show(View::Details(p)),
            "finished" | "unread" => {
                let st = if a == "finished" { ReadingStatus::Complete } else { ReadingStatus::New };
                if let Some(lib) = self.lib.as_mut() {
                    lib.set_status(&p, st);
                }
                self.redraw();
            }
            "export" => {
                // The toast is shown by `handle` (it needs the app context).
                let entry = self.lib.as_ref().and_then(|l| l.index.get(&p)).cloned();
                let paths = self.paths.clone();
                self.toast = Some(match (entry, paths) {
                    (Some(e), Some(paths)) => crate::export::export_one(&paths, &e, &ink_store::export::Locator::none()),
                    _ => "Export failed: the library is not loaded".to_string(),
                });
                self.redraw();
            }
            "collections" => return self.act(Hit::CollectionMenu(p), false, size),
            "plugins" => return self.act(Hit::PluginMenu(p), false, size),
            "delete" => return self.act(Hit::Delete(p), false, size),
            _ => self.redraw(),
        }
        Transition::None
    }
}
