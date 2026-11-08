//! Settings page "Diagnostics": the log level (`info` or `debug`, stored
//! as `[plugins.diagnostics] log_level`, applied at once unless `RINKY_LOG` or
//! `RUST_LOG` is set), "View log" (the last [`VIEW_LINES`] lines of the log file, newest
//! first, in the scrolling list), the crash logs (`crash-*.log`: open, delete) and
//! "Write debug report" (a folder with the logs, settings without secrets, VERSION and
//! the crash logs). The file work is in [`crate::logging`].

use super::rows::Row;
use super::{current_settings, edit_settings, PageModule, Reply};
use crate::logging;
use crate::router::AppCtx;
use ink_store::Settings;
use ink_ui::Icon;
use std::path::PathBuf;

/// Log lines shown by "View log".
pub const VIEW_LINES: usize = 200;
/// Characters per row in the viewer (caption text across the panel width).
const WRAP: usize = 84;

/// What the page shows.
#[derive(Clone, Debug, Default, PartialEq)]
enum View {
    /// Level, log, crash logs, report.
    #[default]
    Main,
    /// The end of the log file.
    Log,
    /// One crash log.
    Crash(PathBuf),
}

/// The page.
#[derive(Default)]
pub struct Page {
    view: View,
}

fn level_name(l: &str) -> &'static str {
    match l {
        "debug" => "Debug",
        _ => "Info",
    }
}

/// Viewer rows for `lines` (oldest first in `lines`; shown newest first).
fn text_rows(lines: &[String]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(lines.len() * 2);
    for l in lines.iter().rev() {
        for piece in logging::wrap_line(&logging::display_line(l), WRAP) {
            rows.push(Row::note(piece));
        }
    }
    rows
}

impl Page {
    fn main_rows(&self, cx: &mut AppCtx) -> Vec<Row> {
        let level = logging::settings_log_level(current_settings(cx));
        let mut rows = vec![Row::header("Log level")];
        for l in logging::LEVELS {
            let text = match l {
                "debug" => "Debug (more detail, bigger log)",
                _ => "Info (normal)",
            };
            rows.push(Row::choice(text, level == l, format!("diag:level:{l}")));
        }
        if let Some(env) = logging::level_from_env() {
            rows.push(Row::note(format!("RINKY_LOG/RUST_LOG sets {env}; it wins over this choice")));
        }
        let log = logging::log_path();
        let size = log.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| ink_device::trace::human_bytes(m.len()));
        rows.push(Row::header("Log"));
        let where_ = match &log {
            Some(p) => p.display().to_string(),
            None => "No log file: logging to stderr".into(),
        };
        rows.push(Row::nav(Icon::Book, "View log", "diag:view").value(size.unwrap_or_else(|| "in memory".into())).subtitle(where_));
        rows.push(Row::header("Crash logs"));
        let crashes = logging::crash_logs(&cx.paths.root);
        if crashes.is_empty() {
            rows.push(Row::note("No crash logs"));
        }
        for c in crashes {
            let name = c.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let size = std::fs::metadata(&c).map(|m| ink_device::trace::human_bytes(m.len())).unwrap_or_default();
            rows.push(Row::nav(Icon::Info, name.clone(), format!("diag:crash:{name}")).value(size));
        }
        rows.push(Row::header("Bug report"));
        rows.push(
            Row::item("Write debug report", "diag:report")
                .icon(Icon::Info)
                .subtitle("Logs, settings (no secrets), VERSION, crash logs -> debug-report-<time>/"),
        );
        rows
    }

    fn log_rows(&self) -> Vec<Row> {
        logging::flush();
        let (lines, source) = match logging::log_path() {
            Some(p) => match logging::tail_file(&p, VIEW_LINES) {
                Ok(l) => (l, format!("{} (newest first)", p.display())),
                Err(e) => (logging::recent_lines(), format!("cannot read {}: {e}; lines kept in memory", p.display())),
            },
            None => (logging::recent_lines(), "lines kept in memory (newest first)".into()),
        };
        log::info!("diagnostics: view log, {} lines from {source}", lines.len());
        let mut rows = vec![Row::header(format!("Last {} lines", lines.len())), Row::note(source)];
        rows.extend(text_rows(&lines));
        rows
    }

    fn crash_rows(&self, path: &PathBuf) -> Vec<Row> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut rows = vec![Row::item("Delete this crash log", format!("diag:delete:{name}")).icon(Icon::Trash)];
        match std::fs::read_to_string(path) {
            Ok(text) => {
                log::info!("diagnostics: open crash log {}", path.display());
                rows.push(Row::header(name));
                // Crash logs read top down: message and backtrace first.
                for l in text.lines() {
                    for piece in logging::wrap_line(l, WRAP) {
                        rows.push(Row::note(piece));
                    }
                }
            }
            Err(e) => {
                log::warn!("diagnostics: cannot read {}: {e}", path.display());
                rows.push(Row::note(format!("Cannot read {name}: {e}")));
            }
        }
        rows
    }
}

impl PageModule for Page {
    fn title(&self) -> Option<String> {
        match &self.view {
            View::Main => None,
            View::Log => Some("Log".into()),
            View::Crash(_) => Some("Crash log".into()),
        }
    }

    fn summary(&self, s: &Settings) -> String {
        format!("Log level {}", level_name(logging::settings_log_level(s)))
    }

    fn rows(&mut self, cx: &mut AppCtx) -> Vec<Row> {
        match self.view.clone() {
            View::Main => self.main_rows(cx),
            View::Log => self.log_rows(),
            View::Crash(p) => self.crash_rows(&p),
        }
    }

    fn action(&mut self, action: &str, cx: &mut AppCtx) -> Reply {
        let Some(a) = action.strip_prefix("diag:") else { return Reply::default() };
        if let Some(l) = a.strip_prefix("level:") {
            let Some(filter) = logging::parse_level(l).filter(|_| logging::LEVELS.contains(&l)) else {
                return Reply::default();
            };
            edit_settings(cx, |s| logging::set_settings_log_level(s, l));
            let now = logging::set_level(filter);
            log::info!("diagnostics: log level {l} chosen; in force {now}");
            return Reply::rebuild(format!("Log level: {}", level_name(l)));
        }
        if a == "view" {
            log::info!("diagnostics: open the log viewer");
            self.view = View::Log;
            return Reply::rebuild("Newest lines first");
        }
        if let Some(name) = a.strip_prefix("crash:") {
            self.view = View::Crash(cx.paths.root.join(name));
            return Reply::rebuild(name.to_string());
        }
        if let Some(name) = a.strip_prefix("delete:") {
            // Only a crash log in the install folder (no path parts in the name).
            if !(name.starts_with("crash-") && name.ends_with(".log") && !name.contains('/')) {
                return Reply::default();
            }
            let path = cx.paths.root.join(name);
            self.view = View::Main;
            return match std::fs::remove_file(&path) {
                Ok(()) => {
                    log::info!("diagnostics: deleted crash log {}", path.display());
                    Reply::rebuild(format!("Deleted {name}"))
                }
                Err(e) => {
                    log::warn!("diagnostics: cannot delete {}: {e}", path.display());
                    Reply::rebuild(format!("Cannot delete {name}: {e}"))
                }
            };
        }
        if a == "report" {
            log::info!("diagnostics: write debug report");
            return match logging::write_debug_report(cx.paths) {
                Ok((dir, n)) => {
                    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    Reply::rebuild(format!("Wrote {n} files to {name}"))
                }
                Err(e) => {
                    log::warn!("diagnostics: debug report failed: {e}");
                    Reply::rebuild(format!("Debug report failed: {e}"))
                }
            };
        }
        Reply::default()
    }

    fn back(&mut self) -> bool {
        if self.view == View::Main {
            return false;
        }
        log::info!("diagnostics: back to the main page");
        self.view = View::Main;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_rows_are_newest_first_and_wrapped() {
        let lines = vec!["[INFO 10:00:00.000 +1 a] old".to_string(), format!("[WARN 10:00:01.000 +2 a] {}", "x".repeat(120))];
        let rows = text_rows(&lines);
        assert!(rows[0].title.starts_with("10:00:01 WARN"), "{:?}", rows[0]);
        assert_eq!(rows.last().map(|r| r.title.as_str()), Some("10:00:00 INFO old"));
        assert!(rows.iter().all(|r| r.title.chars().count() <= WRAP));
    }
}
