//! StarDict dictionaries for the reader's "Look up" action.
//!
//! - [`Dictionary`]: one StarDict dictionary (`.ifo`, `.idx` or `.idx.gz`, `.dict` or
//!   `.dict.dz` with dictzip random access, optional `.syn`).
//! - [`Library`]: every dictionary in the sub-folders of one directory
//!   (`/mnt/us/rinky/dict/<name>/` on the Kindle).
//! - [`normalize::candidates`]: the forms tried for a selected word (punctuation stripped,
//!   lowercase, possessive, simple English inflections).
//! - [`text::html_to_text`]: plain text from the `h`/`x`/`g` definition types.
//! - [`entry::parse`]: senses, examples and sections of known dictionary markups.
//! - [`writer`]: writes small dictionaries (tests, examples, the simulator).

#![warn(missing_docs)]

mod dictzip;
pub mod entry;
mod library;
pub mod normalize;
mod stardict;
pub mod text;
pub mod writer;

pub use entry::Entry;
pub use library::{Hit, Library, LookupResult};
pub use stardict::{ascii_casecmp, stardict_cmp, Dictionary, Field, IfoInfo};

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
pub enum DictError {
    /// File system error, with the path.
    #[error("{path}: {err}")]
    Io {
        /// File.
        path: String,
        /// Cause.
        err: std::io::Error,
    },
    /// The files are not a usable StarDict dictionary.
    #[error("{0}")]
    Format(String),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, DictError>;

pub(crate) fn io_err(path: &std::path::Path, err: std::io::Error) -> DictError {
    DictError::Io { path: path.display().to_string(), err }
}
