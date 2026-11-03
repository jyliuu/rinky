//! rinky: screens, router, plugin host and the command line.
//!
//! - [`app`]: the event loop ([`app::App`]).
//! - [`router`]: routes, the screen stack, [`router::AppScreen`], [`router::ScreenRegistry`].
//! - [`screens`]: one module per screen; each registers itself with one call.
//! - [`plugin_host`] and [`plugins`]: the compiled-in plugins and their glue.
//! - [`cli`]: subcommands (`render`, `run`, `sim`, `demo`, `selftest`, `refresh`, ...).
//! - [`logging`]: log file, crash logs, exit reasons and the panic hook.
//! - [`fonts`]: installed font families (scan, cache), the UI font, font previews.
//! - [`perf`]: the `open:` timing line (tap to first page).
//! - [`selftest`]: device, lipc, store, network and Lua checks.
//! - [`sleep_cover`]: the book cover shown while asleep.
//! - [`spike`]: suspend/resume test tool (`rinky spike-suspend`).

#![warn(missing_docs)]

pub mod app;
pub mod cli;
pub mod export;
pub mod logging;
pub mod fonts;
pub mod perf;
pub mod plugin_entries;
pub mod plugin_host;
pub mod plugins;
pub mod router;
pub mod screens;
pub mod selftest;
pub mod shown;
pub mod sleep_cover;
pub mod spike;
pub mod update;
