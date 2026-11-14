//! Test doubles for plugin tests: a [`DocumentAccess`] whose answers are plain fields,
//! a temporary folder that removes itself, and [`built_library`] for the native plugin
//! libraries cargo builds next to the tests.

use crate::host::DocumentAccess;
use crate::{PluginError, Result};
use ink_doc::{Link, Metadata, Position, SearchHit, Selection, TocEntry};
use std::path::{Path, PathBuf};

/// A fresh, empty folder for one test (`<temp>/rinky-<tag>-<pid>`), removed again when
/// dropped. It dereferences to its [`Path`].
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// Creates the folder; one left over from an earlier run is emptied first.
    pub fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("rinky-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temporary folder");
        TempDir(dir)
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

/// The shared library (`cdylib`) of workspace crate `crate_name` that cargo built for
/// this test run (the crate is a dev-dependency of the test's crate). Where cargo puts
/// it depends on its version and build-directory layout (beside the test binary in
/// `target/<profile>/deps`, or elsewhere under the profile folder), so this looks
/// beside the test binary first and then searches the folders above it, taking the
/// newest file of that name. Panics when there is none.
pub fn built_library(crate_name: &str) -> PathBuf {
    let file = format!("{}{}{}", std::env::consts::DLL_PREFIX, crate_name.replace('-', "_"), std::env::consts::DLL_SUFFIX);
    let exe = std::env::current_exe().expect("test binary path");
    let ancestors: Vec<&Path> = exe.ancestors().skip(1).take(6).collect();
    for a in &ancestors {
        for c in [a.join(&file), a.join("deps").join(&file)] {
            if c.is_file() {
                return c;
            }
        }
    }
    // Newest match in a bounded walk of the folders above (the profile folder, then
    // the target folder).
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    fn walk(dir: &Path, file: &str, depth: u32, best: &mut Option<(std::time::SystemTime, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth > 0 {
                walk(&p, file, depth - 1, best);
            } else if ft.is_file() && e.file_name().to_str() == Some(file) {
                let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
                    *best = Some((t, p));
                }
            }
        }
    }
    for a in ancestors.iter().skip(2) {
        walk(a, &file, 5, &mut best);
        if best.is_some() {
            break;
        }
    }
    match best {
        Some((_, p)) => p,
        None => panic!("{file} not found near {} (built by cargo as a dev-dependency)", exe.display()),
    }
}
