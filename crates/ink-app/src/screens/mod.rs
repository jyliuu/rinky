//! Screens. Each module owns one route and registers it with one call; later agents
//! work inside their own module directory only:
//!
//! | Module | Route | Owner |
//! | --- | --- | --- |
//! | `reader/` | `Route::Reader` | `reader` (wave B) |
//! | `library/` | `Route::Library` | `library` (wave B) |
//! | `settings/` | `Route::Settings` | `settings` (wave B) |
//! | `demo/` | `Route::Demo` | `ui` (wave A) |
//! | `stats/` | `Route::Statistics`, `Route::Vocabulary` | `stats` (wave C) |

pub(crate) mod common;
pub mod demo;
pub mod library;
pub mod reader;
pub mod settings;
pub mod stats;

use crate::router::ScreenRegistry;

/// Registers every screen.
pub fn register_all(reg: &mut ScreenRegistry) {
    reader::register(reg);
    library::register(reg);
    settings::register(reg);
    demo::register(reg);
    stats::register(reg);
}
