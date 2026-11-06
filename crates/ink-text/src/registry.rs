//! Font discovery: lists font files in directories such as `/usr/java/lib/fonts`
//! (system fonts on the Kindle) and `/mnt/us/fonts` (user fonts), reads each face's
//! family and style, and orders the faces that make good fallbacks.

use crate::face::{collection_faces, map_file, FaceInfo};
use rustybuzz::ttf_parser;
use std::path::{Path, PathBuf};

/// The e-reader's font folders, in order (earlier folders win a family name): its
/// system fonts (Bookerly, Amazon Ember, ... on a Kindle; Nickel's on a Kobo; read on
/// the device, never copied or packaged), the user's fonts and rinky's own folder.
/// Missing folders are skipped by the scan.
pub fn device_font_dirs() -> Vec<PathBuf> {
    ink_platform::current().font_dirs()
}

/// A font file found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFile {
    /// Full path.
    pub path: PathBuf,
    /// File name without extension, e.g. `NotoSerif-Bold`.
    pub stem: String,
}

/// Lists `.ttf`, `.otf` and `.ttc` files in `dirs` (not recursive), sorted by path.
/// Missing directories are skipped.
pub fn scan_font_dirs<P: AsRef<Path>>(dirs: &[P]) -> Vec<FontFile> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir.as_ref()) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            let ext = path.extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
                out.push(FontFile { path, stem });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// One face of a font file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontEntry {
    /// File.
    pub path: PathBuf,
    /// File name without extension.
    pub stem: String,
    /// Position of the file's directory in the scanned list (0 = first, e.g. user fonts).
    pub dir_rank: usize,
    /// What the face says about itself (face index included).
    pub info: FaceInfo,
}

/// Reads every face of every font file in `dirs` (files are mapped, only the header
/// tables are touched). Unreadable files are logged and skipped.
pub fn scan_fonts<P: AsRef<Path>>(dirs: &[P]) -> Vec<FontEntry> {
    let mut out = Vec::new();
    for (rank, dir) in dirs.iter().enumerate() {
        for f in scan_font_dirs(&[dir.as_ref()]) {
            let bytes = match map_file(&f.path) {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("font {}: {e}", f.path.display());
                    continue;
                }
            };
            // Bounded: a corrupt .ttc header may claim 4294967295 faces.
            let n = collection_faces(bytes);
            if n == 0 {
                log::warn!("font {}: corrupt collection header, skipped", f.path.display());
            }
            for i in 0..n {
                match ttf_parser::Face::parse(bytes, i) {
                    Ok(face) => out.push(FontEntry {
                        path: f.path.clone(),
                        stem: f.stem.clone(),
                        dir_rank: rank,
                        info: FaceInfo::read(&face, Some(&f.path), i),
                    }),
                    Err(e) => log::warn!("font {} face {i}: {e}", f.path.display()),
                }
            }
        }
    }
    out
}

/// Faces known to be good fallbacks on the Kindle, by file stem, best first: symbols,
/// Chinese, Japanese, Korean, Arabic, Hebrew, Indic, then a pan-Unicode font.
pub const PREFERRED_FALLBACKS: &[&str] = &[
    "Kindle_Symbol",
    "STSongMedium",
    "STSongTC",
    "TBMinchoMedium_213",
    "NotoSerifKR-Medium",
    "NotoSansKR-Regular",
    "NotoNaskh-Regular",
    "NotoSansHebrew-Regular",
    "NotoSansDevanagari-Regular",
    "NotoSansBengali-Regular",
    "NotoSansGujarati-Regular",
    "NotoSansKannada-Regular",
    "NotoSansMalayalam-Regular",
    "NotoSansTamil-Regular",
    "NotoSansTelugu-Regular",
    "code2000",
];

/// Orders faces for use as fallbacks: upright faces of normal or medium weight only;
/// earlier directories first (user fonts before system fonts); inside a directory the
/// [`PREFERRED_FALLBACKS`] in their order, then the others by file name.
pub fn fallback_order(entries: &[FontEntry]) -> Vec<&FontEntry> {
    let mut v: Vec<&FontEntry> = entries.iter().filter(|e| !e.info.italic && e.info.weight <= 500).collect();
    let pref = |e: &FontEntry| PREFERRED_FALLBACKS.iter().position(|p| *p == e.stem).unwrap_or(usize::MAX);
    v.sort_by(|a, b| (a.dir_rank, pref(a), &a.path, a.info.index).cmp(&(b.dir_rank, pref(b), &b.path, b.info.index)));
    v
}

/// One line per directory for logs: file and face counts and the families found.
pub fn summary<P: AsRef<Path>>(dirs: &[P], entries: &[FontEntry]) -> Vec<String> {
    dirs.iter()
        .enumerate()
        .map(|(rank, d)| {
            let in_dir: Vec<&FontEntry> = entries.iter().filter(|e| e.dir_rank == rank).collect();
            let mut files: Vec<&PathBuf> = in_dir.iter().map(|e| &e.path).collect();
            files.dedup();
            let mut families: Vec<&str> = in_dir.iter().map(|e| e.info.family.as_str()).collect();
            families.sort_unstable();
            families.dedup();
            format!(
                "{}: {} files, {} faces, {} families: {}",
                d.as_ref().display(),
                files.len(),
                in_dir.len(),
                families.len(),
                families.join(", ")
            )
        })
        .collect()
}

/// One face of an installed family: file, face index and weight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceRef {
    /// Font file.
    pub path: PathBuf,
    /// Face index in the file (always 0: collections are not offered as families).
    pub index: u32,
    /// `OS/2` weight class.
    pub weight: u16,
}

/// A text font family with up to four faces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontFamily {
    /// Family name (`name` ID 16, else ID 1), e.g. `Bookerly`.
    pub name: String,
    /// Faces by slot: 0 regular, 1 bold, 2 italic, 3 bold italic.
    pub faces: [Option<FaceRef>; 4],
}

impl FontFamily {
    /// The regular face (every listed family has one).
    pub fn regular(&self) -> Option<&FaceRef> {
        self.faces[0].as_ref()
    }

    /// The face for `bold` / `italic`, falling back to bold, then regular.
    pub fn face(&self, bold: bool, italic: bool) -> Option<&FaceRef> {
        let slot = bold as usize | (italic as usize) << 1;
        let chain: &[usize] = match slot {
            3 => &[3, 1, 0],
            2 => &[2, 0],
            1 => &[1, 0],
            _ => &[0],
        };
        chain.iter().find_map(|&i| self.faces[i].as_ref())
    }

    /// `regular, bold, italic` style letters present, for logs (e.g. `RBI-`).
    pub fn styles(&self) -> String {
        ["R", "B", "I", "BI"].iter().zip(&self.faces).map(|(l, f)| if f.is_some() { *l } else { "-" }).collect()
    }
}

/// True when face `index` of `path` can set Latin body text (has `a`, `A` and `0`).
fn is_text_face(path: &Path, index: u32) -> bool {
    let Ok(bytes) = map_file(path) else { return false };
    let Ok(face) = ttf_parser::Face::parse(bytes, index) else { return false };
    ['a', 'A', 'e', '0'].iter().all(|c| face.glyph_index(*c).is_some())
}

/// Groups scanned faces into text families by family name: per slot (regular, bold,
/// italic, bold italic) the face whose weight is nearest 400 (regular slots, weight
/// below 600) or 700 (bold slots). Left out: faces of `.ttc` collections (index > 0 or
/// a collection file), the [`PREFERRED_FALLBACKS`] (symbols, CJK and other scripts),
/// faces without Latin letters, and families without a regular-weight face. When two
/// directories have the same family, the earlier directory wins. Sorted by name.
pub fn group_families(entries: &[FontEntry]) -> Vec<FontFamily> {
    use std::collections::BTreeMap;
    let mut by_name: BTreeMap<String, (usize, FontFamily)> = BTreeMap::new();
    for e in entries {
        let is_ttc = e.path.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("ttc"));
        if is_ttc || e.info.index != 0 || e.info.family.trim().is_empty() || PREFERRED_FALLBACKS.contains(&e.stem.as_str()) {
            continue;
        }
        if !is_text_face(&e.path, e.info.index) {
            continue;
        }
        let key = e.info.family.trim().to_ascii_lowercase();
        let entry = by_name.entry(key).or_insert_with(|| (e.dir_rank, FontFamily { name: e.info.family.trim().to_string(), ..FontFamily::default() }));
        if e.dir_rank > entry.0 {
            continue;
        }
        let bold = e.info.weight >= 600 || (e.info.bold && e.info.weight == 0);
        let slot = bold as usize | (e.info.italic as usize) << 1;
        let target: i32 = if bold { 700 } else { 400 };
        let dist = |w: u16| (w as i32 - target).abs();
        let better = match &entry.1.faces[slot] {
            None => true,
            Some(cur) => dist(e.info.weight) < dist(cur.weight) || (dist(e.info.weight) == dist(cur.weight) && e.path < cur.path),
        };
        if better {
            entry.1.faces[slot] = Some(FaceRef { path: e.path.clone(), index: e.info.index, weight: e.info.weight });
        }
    }
    by_name.into_values().map(|(_, f)| f).filter(|f| f.regular().is_some()).collect()
}

/// The installed families, set once the font scan is done ([`set_installed`]).
static INSTALLED: std::sync::RwLock<Option<std::sync::Arc<[FontFamily]>>> = std::sync::RwLock::new(None);

/// Publishes the installed families for every thread (the app's font scan calls this).
pub fn set_installed(families: Vec<FontFamily>) {
    *INSTALLED.write().unwrap_or_else(|e| e.into_inner()) = Some(families.into());
}

/// The installed families; empty before the scan (and on hosts without font folders).
pub fn installed() -> std::sync::Arc<[FontFamily]> {
    INSTALLED.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| Vec::new().into())
}

/// True once [`set_installed`] was called.
pub fn installed_known() -> bool {
    INSTALLED.read().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// The installed family `name` (case-insensitive).
pub fn installed_family(name: &str) -> Option<FontFamily> {
    let name = name.trim();
    installed().iter().find(|f| f.name.eq_ignore_ascii_case(name)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_fonts() -> PathBuf {
        crate::testdirs::repo_fonts()
    }

    fn test_fonts() -> PathBuf {
        crate::testdirs::data()
    }

    #[test]
    fn finds_repo_fonts() {
        let found = scan_font_dirs(&[repo_fonts().as_path(), Path::new("/nonexistent")]);
        let stems: Vec<_> = found.iter().map(|f| f.stem.as_str()).collect();
        assert_eq!(stems, vec!["NotoSerif-Bold", "NotoSerif-Italic", "NotoSerif-Regular"]);
    }

    #[test]
    fn reads_styles_and_orders_fallbacks() {
        let dirs = [test_fonts(), repo_fonts()];
        let entries = scan_fonts(&dirs);
        assert_eq!(entries.len(), 4);
        let bold = entries.iter().find(|e| e.stem == "NotoSerif-Bold").unwrap();
        assert!(bold.info.bold && !bold.info.italic && bold.dir_rank == 1);
        let order: Vec<&str> = fallback_order(&entries).iter().map(|e| e.stem.as_str()).collect();
        // Bold and italic faces are not fallbacks; the first directory comes first.
        assert_eq!(order, vec!["InkTestFallback", "NotoSerif-Regular"]);
        let lines = summary(&dirs, &entries);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("1 files, 1 faces, 1 families: InkTest Fallback"), "{}", lines[0]);
        assert!(lines[1].contains("3 files, 3 faces, 1 families: Noto Serif"), "{}", lines[1]);
    }

    #[test]
    fn preferred_fallbacks_come_first() {
        let mk = |stem: &str, rank| FontEntry {
            path: PathBuf::from(format!("/f/{stem}.ttf")),
            stem: stem.into(),
            dir_rank: rank,
            info: FaceInfo { weight: 400, ..FaceInfo::default() },
        };
        let entries = vec![mk("Bookerly-Regular", 1), mk("code2000", 1), mk("STSongMedium", 1), mk("Mine", 0)];
        let order: Vec<&str> = fallback_order(&entries).iter().map(|e| e.stem.as_str()).collect();
        assert_eq!(order, vec!["Mine", "STSongMedium", "code2000", "Bookerly-Regular"]);
    }

    #[test]
    fn groups_families_by_name_and_weight() {
        let mk = |stem: &str, family: &str, weight: u16, italic: bool, rank| FontEntry {
            path: PathBuf::from(format!("/f/{stem}.ttf")),
            stem: stem.into(),
            dir_rank: rank,
            info: FaceInfo { family: family.into(), weight, italic, bold: weight >= 600, ..FaceInfo::default() },
        };
        // Files that do not exist are not text faces: grouping only sees real fonts.
        let fake = vec![mk("Ember-Thin", "Ember", 100, false, 0), mk("Ember-Rg", "Ember", 400, false, 0)];
        assert!(group_families(&fake).is_empty());
        // Real files: the repository's Noto Serif faces form one family.
        let entries = scan_fonts(&[repo_fonts()]);
        let fams = group_families(&entries);
        assert_eq!(fams.len(), 1, "{fams:?}");
        assert_eq!(fams[0].name, "Noto Serif");
        assert_eq!(fams[0].styles(), "RBI-");
        assert_eq!(fams[0].face(true, true).unwrap().path, fams[0].faces[1].as_ref().unwrap().path);
    }
}
