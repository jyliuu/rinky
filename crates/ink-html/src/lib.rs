//! Reflowable document engine (phase 2).
//!
//! [`HtmlDocument`] takes the XHTML of each spine item (from `ink-formats`: EPUB, and
//! FB2, MOBI, TXT, HTML converted to XHTML) and implements [`ink_doc::Document`]:
//!
//! - [`parse`] / [`dom`]: quick-xml for well-formed XHTML, html5ever for broken markup,
//!   building the same DOM as KOReader's crengine (so xpointers match).
//! - [`css`] / [`style`]: a CSS subset, the cascade and user overrides.
//! - [`layout`]: blocks, inline text with hyphenation ([`hyph`]), lists, tables,
//!   images ([`images`]), fonts ([`fonts`], including `@font-face`).
//! - [`paginate`]: pages, the page map, its on-disk cache and the background thread.
//! - [`xpointer`]: crengine xpointers for positions; [`nav`]: EPUB TOC and page list.
//! - [`v1`]: the phase 0 parser, kept to read old `flow:` positions.

#![warn(missing_docs)]

pub mod annot;
pub mod book;
pub mod css;
mod doc;
pub mod dom;
pub mod fonts;
pub mod hyph;
pub mod images;
pub mod layout;
pub mod nav;
pub mod paginate;
pub mod parse;
pub mod render;
pub mod style;
pub mod v1;
pub mod xpointer;

pub use doc::{HtmlChapter, HtmlDocument};
pub use nav::{NavPoint, Navigation};
