//! Screens. Each module owns one route and registers it with one call:
//!
//! | Module | Route |
//! | --- | --- |
//! | `reader/` | `Route::Reader` |
//! | `library/` | `Route::Library` |
//! | `settings/` | `Route::Settings` |
//! | `demo/` | `Route::Demo` |
//! | `stats/` | `Route::Statistics`, `Route::Vocabulary` |

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
