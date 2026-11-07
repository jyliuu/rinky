//! Test doubles for plugin tests: a [`DocumentAccess`] whose answers are plain fields.

use crate::host::DocumentAccess;
use crate::{PluginError, Result};
use ink_doc::{Link, Metadata, Position, SearchHit, Selection, TocEntry};

/// An open book for plugin tests. Every answer comes from a field: no table of contents,
/// search hits or links; `read_around` returns the first characters of [`FakeBook::text`];
/// `current_position` and `page_text` are [`PluginError::Unsupported`] while
/// [`FakeBook::position`] and [`FakeBook::page_texts`] are empty.
#[derive(Clone, Debug, Default)]
pub struct FakeBook {
    /// Metadata.
    pub metadata: Metadata,
    /// Book id (partial MD5).
    pub id: Option<String>,
    /// Where the reader is.
    pub position: Option<Position>,
    /// The current selection.
    pub selection: Option<Selection>,
    /// The page the reader shows (0-based).
    pub page: Option<usize>,
    /// Pages in the book.
    pub pages: Option<usize>,
    /// The text around any position.
    pub text: String,
    /// Text of each page.
    pub page_texts: Vec<String>,
}

impl FakeBook {
    /// A book with this title and nothing else.
    pub fn titled(title: &str) -> FakeBook {
        FakeBook { metadata: Metadata { title: title.into(), ..Metadata::default() }, ..FakeBook::default() }
    }
}

/// A selection of `text` on the first page, at a fixed crengine xpointer.
pub fn selection(text: &str) -> Selection {
    let p: Position = "/body/DocFragment[3]/body/p[1]/text().4".parse().expect("xpointer");
    Selection { start: p.clone(), end: p, text: text.into(), page: 0, rects: Vec::new() }
}

impl DocumentAccess for FakeBook {
    fn metadata(&self) -> Metadata {
        self.metadata.clone()
    }
    fn book_id(&self) -> Option<String> {
        self.id.clone()
    }
    fn toc(&self) -> Vec<TocEntry> {
        Vec::new()
    }
    fn search(&mut self, _query: &str, _limit: usize) -> Result<Vec<SearchHit>> {
        Ok(Vec::new())
    }
    fn read_around(&mut self, _pos: &Position, chars: usize) -> Result<String> {
        Ok(self.text.chars().take(chars).collect())
    }
    fn links(&mut self, _pos: &Position) -> Result<Vec<Link>> {
        Ok(Vec::new())
    }
    fn current_position(&self) -> Result<Position> {
        self.position.clone().ok_or_else(|| PluginError::Unsupported("position".into()))
    }
    fn selection(&self) -> Option<Selection> {
        self.selection.clone()
    }
    fn current_page(&self) -> Option<usize> {
        self.page
    }
    fn page_count(&self) -> Option<usize> {
        self.pages
    }
    fn page_text(&mut self, page: usize) -> Result<String> {
        match self.page_texts.get(page) {
            Some(t) => Ok(t.clone()),
            None if self.page_texts.is_empty() => Err(PluginError::Unsupported("page text".into())),
            None => Err(PluginError::Failed(format!("no page {}", page + 1))),
        }
    }
}
