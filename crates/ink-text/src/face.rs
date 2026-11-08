//! Font faces for shaping: file bytes plus the facts read from the `name` and `OS/2`
//! tables (family, weight, slant).
//!
//! Bytes live for the whole program (`&'static [u8]`): embedded faces are static already,
//! files are memory-mapped (unix) or read once, and never released. A device has a
//! bounded set of font files, and mapping keeps untouched pages of large CJK fonts out
//! of memory. The `rustybuzz` face is parsed on first use.

use crate::TextError;
use rustybuzz::ttf_parser;
use std::sync::OnceLock;
use std::path::{Path, PathBuf};

/// What a face says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaceInfo {
    /// Typographic family (`name` ID 16, else ID 1), e.g. `Noto Serif`.
    pub family: String,
    /// Subfamily (`name` ID 17, else ID 2), e.g. `Bold Italic`.
    pub subfamily: String,
    /// Full name (`name` ID 4).
    pub full_name: String,
    /// `OS/2` weight class (400 regular, 700 bold).
    pub weight: u16,
    /// Bold (weight >= 600 or the bold flag).
    pub bold: bool,
    /// Italic or oblique.
    pub italic: bool,
    /// Fixed pitch.
    pub monospace: bool,
    /// Can set Latin body text (has `a`, `A`, `e` and `0`).
    pub latin: bool,
    /// Units per em.
    pub units_per_em: u16,
    /// Number of glyphs.
    pub glyphs: u16,
    /// File the face came from (none for embedded faces).
    pub path: Option<PathBuf>,
    /// Face index inside a `.ttc` collection.
    pub index: u32,
}

fn name(face: &ttf_parser::Face, ids: &[u16]) -> String {
    for id in ids {
        for n in face.names() {
            if n.name_id == *id {
                if let Some(s) = n.to_string() {
                    if !s.is_empty() {
                        return s;
                    }
                }
            }
        }
    }
    String::new()
}

impl FaceInfo {
    /// Reads the facts from a parsed face.
    pub fn read(face: &ttf_parser::Face, path: Option<&Path>, index: u32) -> FaceInfo {
        use ttf_parser::name_id;
        let weight = face.weight().to_number();
        FaceInfo {
            family: name(face, &[name_id::TYPOGRAPHIC_FAMILY, name_id::FAMILY]),
            subfamily: name(face, &[name_id::TYPOGRAPHIC_SUBFAMILY, name_id::SUBFAMILY]),
            full_name: name(face, &[name_id::FULL_NAME]),
            weight,
            bold: face.is_bold() || weight >= 600,
            italic: face.is_italic() || face.is_oblique(),
            monospace: face.is_monospaced(),
            latin: ['a', 'A', 'e', '0'].iter().all(|c| face.glyph_index(*c).is_some()),
            units_per_em: face.units_per_em(),
            glyphs: face.number_of_glyphs(),
            path: path.map(Path::to_path_buf),
            index,
        }
    }

    /// `Family Subfamily` for logs.
    pub fn label(&self) -> String {
        if self.subfamily.is_empty() {
            self.family.clone()
        } else {
            format!("{} {}", self.family, self.subfamily)
        }
    }
}

/// One face ready for shaping and rasterizing.
pub struct FaceData {
    bytes: &'static [u8],
    /// Facts from the font tables.
    pub info: FaceInfo,
    rb: OnceLock<Option<rustybuzz::Face<'static>>>,
    ttf: OnceLock<Option<ttf_parser::Face<'static>>>,
}

impl std::fmt::Debug for FaceData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaceData").field("info", &self.info).field("bytes", &self.bytes.len()).finish()
    }
}

impl FaceData {
    /// A face from static bytes (embedded fonts).
    pub fn from_static(bytes: &'static [u8], index: u32) -> Result<Self, TextError> {
        Self::build(bytes, index, None)
    }

    /// A face from owned bytes. The bytes are kept for the rest of the program.
    pub fn from_vec(bytes: Vec<u8>, index: u32) -> Result<Self, TextError> {
        Self::build(Box::leak(bytes.into_boxed_slice()), index, None)
    }

    /// A face from a font file (memory-mapped on unix).
    pub fn open(path: &Path, index: u32) -> Result<Self, TextError> {
        Self::build(map_file(path)?, index, Some(path))
    }

    /// All faces of a font file (one, or every face of a `.ttc` collection) that parse.
    /// Faces that do not parse are logged and skipped; an error only when none parses.
    pub fn open_all(path: &Path) -> Result<Vec<Self>, TextError> {
        let bytes = map_file(path)?;
        let n = collection_faces(bytes);
        if n == 0 {
            return Err(TextError::Font(format!("{}: corrupt font collection header", path.display())));
        }
        let mut faces = Vec::new();
        let mut first_err = None;
        for i in 0..n {
            match Self::build(bytes, i, Some(path)) {
                Ok(f) => faces.push(f),
                Err(e) => {
                    log::warn!("font {} face {i}: {e}", path.display());
                    first_err.get_or_insert(e);
                }
            }
        }
        match (faces.is_empty(), first_err) {
            (true, Some(e)) => Err(e),
            _ => Ok(faces),
        }
    }

    fn build(bytes: &'static [u8], index: u32, path: Option<&Path>) -> Result<Self, TextError> {
        let face = ttf_parser::Face::parse(bytes, index).map_err(|e| {
            TextError::Font(format!("{}: {e}", path.map(|p| p.display().to_string()).unwrap_or_else(|| "font bytes".into())))
        })?;
        let info = FaceInfo::read(&face, path, index);
        Ok(FaceData { bytes, info, rb: OnceLock::new(), ttf: OnceLock::new() })
    }

    /// The raw file bytes.
    pub fn bytes(&self) -> &'static [u8] {
        self.bytes
    }

    /// The `ttf-parser` face (outlines, cmap), parsed once.
    pub fn ttf(&self) -> Option<&ttf_parser::Face<'static>> {
        self.ttf.get_or_init(|| ttf_parser::Face::parse(self.bytes, self.info.index).ok()).as_ref()
    }

    /// The shaping face, parsed once.
    pub fn shaper(&self) -> Option<&rustybuzz::Face<'static>> {
        self.rb.get_or_init(|| rustybuzz::Face::from_slice(self.bytes, self.info.index)).as_ref()
    }

    /// True when the face maps `c` to a glyph.
    pub fn has_char(&self, c: char) -> bool {
        self.ttf().is_some_and(|f| f.glyph_index(c).is_some())
    }
}

/// Most faces read from one `.ttc` collection. Real collections hold a few to a few
/// dozen faces (the Kindle's largest has 4).
pub const MAX_COLLECTION_FACES: u32 = 256;

/// How many faces to try in the font file `bytes`: 1 for a single font. For a `.ttc`
/// collection, the count in its header, at most [`MAX_COLLECTION_FACES`]; 0 when the
/// header is corrupt, i.e. it claims no faces or more face offsets (4 bytes each after
/// the 12-byte header) than the file holds. `ttf-parser` reads the whole offset table
/// for every face, so no face of such a file parses; a corrupt header claiming
/// 4294967295 faces made the scan try each one (it never finished), and debug builds of
/// `ttf-parser` assert on that table length.
pub fn collection_faces(bytes: &[u8]) -> u32 {
    match ttf_parser::fonts_in_collection(bytes) {
        None => 1,
        Some(claimed) => {
            let fit = u32::try_from(bytes.len().saturating_sub(12) / 4).unwrap_or(u32::MAX);
            if claimed > fit {
                0
            } else {
                claimed.min(MAX_COLLECTION_FACES)
            }
        }
    }
}

/// Maps a file read-only for the rest of the program. A file is mapped once; later
/// calls with the same path return the same bytes.
pub fn map_file(path: &Path) -> Result<&'static [u8], TextError> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static MAPPED: Mutex<Option<HashMap<PathBuf, &'static [u8]>>> = Mutex::new(None);
    let mut guard = MAPPED.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(b) = map.get(path) {
        return Ok(b);
    }
    let bytes = map_new(path)?;
    map.insert(path.to_path_buf(), bytes);
    Ok(bytes)
}

#[cfg(unix)]
fn map_new(path: &Path) -> Result<&'static [u8], TextError> {
    use std::os::fd::AsRawFd;
    let err = |e: std::io::Error| TextError::Font(format!("{}: {e}", path.display()));
    let file = std::fs::File::open(path).map_err(err)?;
    let len = file.metadata().map_err(err)?.len() as usize;
    if len == 0 {
        return Err(TextError::Font(format!("{}: empty file", path.display())));
    }
    // SAFETY: a private read-only mapping of a regular file we just opened; the mapping
    // stays valid after the descriptor is closed and is never unmapped, so the slice is
    // 'static. Font files are not modified while the reader runs.
    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
    if ptr == libc::MAP_FAILED {
        let bytes = std::fs::read(path).map_err(err)?;
        return Ok(Box::leak(bytes.into_boxed_slice()));
    }
    // SAFETY: `ptr` points to `len` readable bytes (see above).
    Ok(unsafe { std::slice::from_raw_parts(ptr as *const u8, len) })
}

#[cfg(not(unix))]
fn map_new(path: &Path) -> Result<&'static [u8], TextError> {
    let bytes = std::fs::read(path).map_err(|e| TextError::Font(format!("{}: {e}", path.display())))?;
    Ok(Box::leak(bytes.into_boxed_slice()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{embedded_font_bytes, Style};

    #[test]
    fn embedded_faces_describe_themselves() {
        let r = FaceData::from_static(embedded_font_bytes(Style::Regular), 0).unwrap();
        assert_eq!(r.info.family, "Noto Serif");
        assert!(!r.info.bold && !r.info.italic);
        assert!(r.has_char('A') && !r.has_char('日'));
        let b = FaceData::from_static(embedded_font_bytes(Style::Bold), 0).unwrap();
        assert!(b.info.bold && b.info.weight >= 700, "{:?}", b.info);
        let i = FaceData::from_static(embedded_font_bytes(Style::Italic), 0).unwrap();
        assert!(i.info.italic);
        assert_eq!(i.info.label(), "Noto Serif Italic");
        assert!(FaceData::from_vec(b"nope".to_vec(), 0).is_err());
    }

    #[test]
    fn opens_files() {
        let p = crate::testdirs::data().join("InkTestFallback.ttf");
        let faces = FaceData::open_all(&p).unwrap();
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].info.family, "InkTest Fallback");
        assert!(faces[0].has_char('日') && !faces[0].has_char('A'));
        assert_eq!(faces[0].info.path.as_deref(), Some(p.as_path()));
        assert!(FaceData::open(Path::new("/nonexistent.ttf"), 0).is_err());
    }

    #[test]
    fn collection_face_count_is_bounded_by_the_file() {
        let ttc = |claimed: u32, offsets: usize| {
            let mut b = b"ttcf\x00\x01\x00\x00".to_vec();
            b.extend(claimed.to_be_bytes());
            b.extend(vec![0u8; 4 * offsets]);
            b
        };
        // A single font: one face.
        assert_eq!(collection_faces(embedded_font_bytes(Style::Regular)), 1);
        // Headers claiming 4294967295 or 1000 faces with no offset table, or 0 faces:
        // corrupt, nothing to try.
        assert_eq!(collection_faces(&ttc(u32::MAX, 0)), 0);
        assert_eq!(collection_faces(&ttc(1000, 0)), 0);
        assert_eq!(collection_faces(&ttc(4, 3)), 0);
        assert_eq!(collection_faces(&ttc(0, 3)), 0);
        // A table that fits: its count, at most the cap.
        assert_eq!(collection_faces(&ttc(3, 3)), 3);
        assert_eq!(collection_faces(&ttc(2000, 2000)), MAX_COLLECTION_FACES);
        // open_all fails cleanly on such files (no panic in debug builds either).
        let dir = std::env::temp_dir().join(format!("ink-text-ttc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, bytes) in [("huge.ttc", ttc(u32::MAX, 0)), ("zeros.ttc", ttc(3, 3))] {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            assert!(FaceData::open_all(&p).is_err(), "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
