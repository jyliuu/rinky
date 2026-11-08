//! Mobipocket and Kindle books (DRM-free): MOBI 6, KF8 (AZW3), joint MOBI 6 + KF8 files,
//! AZW and PRC, plain PalmDOC text.
//!
//! - [`palmdb`]: the Palm database container.
//! - [`header`]: PalmDOC + MOBI header and EXTH metadata (title, authors, publisher,
//!   description, ISBN, ASIN, subjects, date, language, cover offset, KF8 boundary).
//! - [`compress`]: PalmDOC LZ77 and HUFF/CDIC, trailing entries.
//! - [`index`]: INDX/TAGX/CNCX tables (NCX TOC, KF8 skeleton/fragment/guide).
//! - [`mobi6`]: old MOBI markup to XHTML: `filepos` links and anchors,
//!   `<mbp:pagebreak>` chapter splits, `recindex` images.
//! - [`kf8`]: KF8 reconstruction from skeleton and fragment tables and FDST flows,
//!   `kindle:pos:fid` links, `kindle:embed` resources and `kindle:flow` stylesheets.
//!
//! Joint files are read through their KF8 part. Books with the encryption field set in
//! record 0 are refused (their EXTH metadata and cover stay readable).

pub mod compress;
pub mod header;
pub mod index;
pub mod kf8;
pub mod mobi6;
pub mod palmdb;

use crate::book::{Book, BookInfo, Cover, Identifier};
use crate::util::{extension_for, sniff_media_type};
use crate::Format;
use header::{exth, MobiHeader};
use ink_doc::{DocError, ResourceLoader, Result};
use palmdb::PalmDb;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// A converted MOBI part: chapters, TOC and in-memory files (KF8 flows).
pub type Converted = (Vec<ink_html::HtmlChapter>, Vec<crate::book::TocNode>, HashMap<String, Vec<u8>>);

/// Kinds of resource records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResKind {
    /// Image bytes as stored.
    Image,
    /// `FONT` record (zlib and XOR obfuscation).
    Font,
    /// `CRES` record (HD image after a 12-byte header).
    Cres,
}

/// One resource: path, record and kind.
#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    /// 1-based resource number (`recindex`, `kindle:embed` base 32).
    pub number: usize,
    /// Path served by the loader (`res/00001.jpg`).
    pub path: String,
    /// Record index.
    pub record: usize,
    /// Kind.
    pub kind: ResKind,
    /// Media type.
    pub media_type: String,
}

/// Resource records from the first resource record on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resources {
    /// By number.
    pub by_number: HashMap<usize, Resource>,
}

impl Resources {
    /// Path of resource `n` (1-based).
    pub fn path(&self, n: usize) -> Option<&str> {
        self.by_number.get(&n).map(|r| r.path.as_str())
    }
}

/// An opened Mobipocket file.
#[derive(Clone, Debug)]
pub struct Mobi {
    /// The database.
    pub db: PalmDb,
    /// Record 0 header.
    pub header: MobiHeader,
    /// KF8 header (record 0 itself for AZW3, the record after `BOUNDARY` in joint files).
    pub kf8: Option<MobiHeader>,
}

/// Base 32 digits used by `kindle:` URIs (0-9, A-V).
pub fn base32(s: &str) -> Option<usize> {
    let mut v = 0usize;
    for c in s.chars() {
        let d = c.to_digit(32)? as usize;
        v = v.checked_mul(32)?.checked_add(d)?;
    }
    Some(v)
}

/// Most bytes of decompressed book text (MOBI 6 markup or KF8 flows). Real books have
/// a few MB; record 0's `text_length` is not trusted for allocations.
pub const MAX_TEXT: usize = 64 << 20;

/// Most bytes a `FONT` record may decompress to.
pub const MAX_FONT: usize = 32 << 20;

/// Decodes a `FONT` record (KindleUnpack `read_font_record`): optional XOR of the first
/// 1040 bytes with the key in the record, then optional zlib.
pub fn decode_font(rec: &[u8]) -> Option<Vec<u8>> {
    decode_font_max(rec, MAX_FONT)
}

/// [`decode_font`] stopping after `max` bytes (`None` when the font is larger, unless
/// `max` is below [`MAX_FONT`], which reads only a prefix for sniffing the type).
fn decode_font_max(rec: &[u8], max: usize) -> Option<Vec<u8>> {
    use crate::util::be32;
    if rec.get(..4) != Some(b"FONT") {
        return None;
    }
    // Declared size: only a hint, bounded by what the record can hold.
    let declared = be32(rec, 4)? as usize;
    let flags = be32(rec, 8)?;
    let dstart = be32(rec, 12)? as usize;
    let xor_len = be32(rec, 16)? as usize;
    let xor_start = be32(rec, 20)? as usize;
    let mut data = rec.get(dstart..)?.to_vec();
    if flags & 2 != 0 && xor_len > 0 {
        let key = rec.get(xor_start..xor_start.checked_add(xor_len)?)?;
        for (i, b) in data.iter_mut().take(1040).enumerate() {
            *b ^= key[i % xor_len];
        }
    }
    if flags & 1 != 0 {
        let mut out = Vec::with_capacity(declared.min(max).min(data.len().saturating_mul(4)));
        let mut z = flate2::read::ZlibDecoder::new(&data[..]).take(max as u64 + 1);
        if max < MAX_FONT {
            // A prefix: a truncated stream is fine.
            let _ = z.read_to_end(&mut out);
            out.truncate(max);
            return (!out.is_empty()).then_some(out);
        }
        z.read_to_end(&mut out).ok()?;
        if out.len() > max {
            return None;
        }
        data = out;
    }
    data.truncate(max);
    Some(data)
}

/// The first bytes of a `FONT` record's font, enough to tell its type.
fn font_head(rec: &[u8]) -> Option<Vec<u8>> {
    decode_font_max(rec, 64)
}

impl Mobi {
    /// Parses a Mobipocket file.
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let db = PalmDb::parse(data)?;
        let tc = db.kind();
        if tc != "BOOKMOBI" && tc != "TEXtREAd" {
            return Err(DocError::Format(format!("not a Mobipocket book (type {tc})")));
        }
        let rec0 = db.record(0).ok_or_else(|| DocError::Format("MOBI: no record 0".into()))?;
        let header = MobiHeader::parse(rec0, 0)?;
        let kf8 = if header.is_kf8() { Some(header.clone()) } else { find_kf8(&db, &header) };
        Ok(Mobi { db, header, kf8 })
    }

    /// Reads and parses `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Mobi::parse(std::fs::read(path)?)
    }

    /// The header the book is read with (KF8 when present).
    pub fn main(&self) -> &MobiHeader {
        self.kf8.as_ref().unwrap_or(&self.header)
    }

    /// DRM scheme when the book is encrypted.
    pub fn drm(&self) -> Option<String> {
        let enc = self.header.encryption.max(self.kf8.as_ref().map(|k| k.encryption).unwrap_or(0));
        match enc {
            0 => None,
            1 => Some("Mobipocket (old)".into()),
            _ => Some("Kindle/Mobipocket".into()),
        }
    }

    /// Format detail.
    pub fn version(&self) -> String {
        if !self.header.has_mobi {
            return "PalmDOC".into();
        }
        match (&self.kf8, self.header.is_kf8()) {
            (Some(_), true) => "KF8 (AZW3)".into(),
            (Some(_), false) => "MOBI 6 + KF8".into(),
            _ => format!("MOBI {}", self.header.version.min(7)),
        }
    }

    /// The decompressed text of the book part described by `h`.
    pub fn text(&self, h: &MobiHeader) -> Result<Vec<u8>> {
        let mut huff = match h.compression {
            17480 => {
                let first = h.abs(h.huff_offset).ok_or_else(|| DocError::Format("HUFF/CDIC: no HUFF record".into()))?;
                let huff = self.db.record(first).ok_or_else(|| DocError::Format("HUFF/CDIC: HUFF record missing".into()))?;
                let count = (h.huff_count as usize).min(self.db.len().saturating_sub(first));
                let cdics: Vec<&[u8]> = (1..count).filter_map(|i| self.db.record(first + i)).collect();
                Some(compress::HuffCdic::new(huff, &cdics)?)
            }
            1 | 2 => None,
            c => return Err(DocError::Unsupported(format!("MOBI compression type {c}"))),
        };
        // text_length comes from the file: a hint, bounded by the cap and the file size.
        let hint = (h.text_length as usize).min(MAX_TEXT).min(self.db.data.len().saturating_mul(4));
        let mut out = Vec::with_capacity(hint);
        for i in 1..=h.text_records as usize {
            let Some(rec) = h.start.checked_add(i).and_then(|r| self.db.record(r)) else { break };
            let rec = &rec[..rec.len() - compress::trailing_size(rec, h.extra_flags)];
            match (h.compression, huff.as_mut()) {
                (2, _) => out.extend_from_slice(&compress::palmdoc(rec)),
                (17480, Some(hc)) => out.extend_from_slice(&hc.unpack(rec)?),
                _ => out.extend_from_slice(rec),
            }
            if out.len() > MAX_TEXT {
                return Err(DocError::Format(format!("MOBI: text larger than {} MiB", MAX_TEXT >> 20)));
            }
        }
        if h.text_length > 0 && out.len() > h.text_length as usize {
            out.truncate(h.text_length as usize);
        }
        Ok(out)
    }

    /// Resource records of the part described by `h`, numbered from 1.
    pub fn resources(&self, h: &MobiHeader) -> Resources {
        let mut res = Resources::default();
        let first = self.first_resource(h);
        let Some(first) = first else { return res };
        let mut n = 0;
        for rec in first..self.db.len() {
            let Some(data) = self.db.record(rec) else { break };
            n += 1;
            let magic = data.get(..4).unwrap_or(&[]);
            if magic == b"BOUN" || magic == b"EOF\r" || magic == b"\xe9\x8e\r\n" {
                break;
            }
            let (kind, media) = if magic == b"FONT" {
                match font_head(data) {
                    Some(f) => (ResKind::Font, sniff_media_type(&f).unwrap_or("font/ttf").to_string()),
                    None => continue,
                }
            } else if magic == b"CRES" {
                match sniff_media_type(data.get(12..).unwrap_or(&[])) {
                    Some(m) => (ResKind::Cres, m.to_string()),
                    None => continue,
                }
            } else {
                match sniff_media_type(data) {
                    Some(m) if m.starts_with("image/") => (ResKind::Image, m.to_string()),
                    _ => continue,
                }
            };
            let path = format!("res/{n:05}.{}", extension_for(&media));
            res.by_number.insert(n, Resource { number: n, path, record: rec, kind, media_type: media });
        }
        res
    }

    fn first_resource(&self, h: &MobiHeader) -> Option<usize> {
        let is_resource = |r: usize| {
            self.db.record(r).is_some_and(|d| {
                sniff_media_type(d).is_some() || matches!(d.get(..4), Some(b"FONT") | Some(b"CRES") | Some(b"RESC") | Some(b"kind"))
            })
        };
        let own = h.abs(h.first_resource).filter(|r| *r < self.db.len());
        let base = self.header.abs(self.header.first_resource).filter(|r| *r < self.db.len());
        match (own, base) {
            (Some(r), _) if is_resource(r) => Some(r),
            (_, Some(r)) if is_resource(r) => Some(r),
            (o, b) => o.or(b),
        }
    }

    /// Cover resource: EXTH 201 (offset from the first resource), else the KF8 cover URI.
    pub fn cover<'r>(&self, res: &'r Resources) -> Option<&'r Resource> {
        let h = self.main();
        let by_offset = |h: &MobiHeader| h.exth.u32(exth::COVER_OFFSET).filter(|v| *v != header::NULL).and_then(|off| res.by_number.get(&(off as usize + 1)));
        by_offset(h)
            .or_else(|| by_offset(&self.header))
            .or_else(|| {
                let uri = h.exth.string(exth::KF8_COVER_URI, 65001)?;
                let n = uri.strip_prefix("kindle:embed:")?.split('?').next()?;
                res.by_number.get(&base32(n)?)
            })
            .filter(|r| r.media_type.starts_with("image/"))
    }

    /// Metadata from the EXTH records and the full name.
    pub fn info(&self) -> BookInfo {
        let h = self.main();
        let cp = h.codepage;
        let mut info = BookInfo::new(Format::Mobi);
        info.version = self.version();
        let get = |t: u32| h.exth.string(t, cp).or_else(|| self.header.exth.string(t, self.header.codepage));
        let all = |t: u32| {
            let v = h.exth.strings(t, cp);
            if v.is_empty() {
                self.header.exth.strings(t, self.header.codepage)
            } else {
                v
            }
        };
        info.metadata.title = h.title();
        if info.metadata.title.is_empty() {
            info.metadata.title = self.db.name.replace('_', " ");
        }
        info.metadata.authors = all(exth::AUTHOR);
        info.metadata.publisher = get(exth::PUBLISHER);
        info.metadata.description = get(exth::DESCRIPTION).map(|d| crate::xml::collapse_ws(&crate::xml::strip_tags(&d)));
        info.metadata.language = get(exth::LANGUAGE).or_else(|| locale_language(h.locale));
        info.subjects = all(exth::SUBJECT);
        info.date = get(exth::DATE);
        for isbn in all(exth::ISBN) {
            info.identifiers.push(Identifier { scheme: Some("ISBN".into()), value: isbn });
        }
        for t in [exth::ASIN, exth::ASIN2] {
            for asin in all(t) {
                if !info.identifiers.iter().any(|i| i.value == asin) {
                    info.identifiers.push(Identifier { scheme: Some("ASIN".into()), value: asin });
                }
            }
        }
        if let Some(src) = get(exth::SOURCE) {
            info.identifiers.push(Identifier { scheme: Some("source".into()), value: src });
        }
        info.metadata.identifier = info.identifiers.first().map(|i| i.value.clone());
        if let Some(t) = get(exth::CDE_TYPE) {
            info.version = format!("{} {t}", info.version);
        }
        if let Some(scheme) = self.drm() {
            info.refusal = Some(crate::epub::drm_message(&scheme));
            info.drm = Some(scheme);
        }
        info
    }
}

/// Language from the MOBI locale field (low byte = Windows primary language id).
fn locale_language(locale: u32) -> Option<String> {
    Some(
        match locale & 0xFF {
            9 => "en",
            7 => "de",
            12 => "fr",
            10 => "es",
            16 => "it",
            6 => "da",
            19 => "nl",
            25 => "ru",
            22 => "pt",
            29 => "sv",
            20 => "no",
            11 => "fi",
            21 => "pl",
            4 => "zh",
            17 => "ja",
            _ => return None,
        }
        .to_string(),
    )
}

fn find_kf8(db: &PalmDb, h: &MobiHeader) -> Option<MobiHeader> {
    let try_at = |i: usize| -> Option<MobiHeader> {
        let rec = db.record(i)?;
        let k = MobiHeader::parse(rec, i).ok()?;
        (k.has_mobi && k.is_kf8()).then_some(k)
    };
    if let Some(b) = h.exth.u32(exth::KF8_BOUNDARY).filter(|b| *b != header::NULL) {
        for i in [b as usize, b as usize + 1] {
            if let Some(k) = try_at(i) {
                return Some(k);
            }
        }
    }
    (1..db.len()).find(|i| db.record(*i) == Some(b"BOUNDARY")).and_then(|i| try_at(i + 1))
}

/// Serves `res/NNNNN.ext` from resource records and converted files (stylesheets) from memory.
pub struct MobiLoader {
    db: PalmDb,
    resources: HashMap<String, Resource>,
    files: HashMap<String, Vec<u8>>,
}

impl MobiLoader {
    /// A loader over `db` with `res` and extra in-memory `files`.
    pub fn new(db: PalmDb, res: &Resources, files: HashMap<String, Vec<u8>>) -> Self {
        let resources = res.by_number.values().map(|r| (r.path.clone(), r.clone())).collect();
        MobiLoader { db, resources, files }
    }
}

impl ResourceLoader for MobiLoader {
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        if let Some(f) = self.files.get(path) {
            return Ok(f.clone());
        }
        let r = self.resources.get(path).ok_or_else(|| DocError::NotFound(path.into()))?;
        let data = self.db.record(r.record).ok_or_else(|| DocError::NotFound(path.into()))?;
        match r.kind {
            ResKind::Image => Ok(data.to_vec()),
            ResKind::Cres => Ok(data.get(12..).unwrap_or(&[]).to_vec()),
            ResKind::Font => decode_font(data).ok_or_else(|| DocError::Format(format!("bad FONT record {path}"))),
        }
    }

    fn exists(&self, path: &str) -> bool {
        self.files.contains_key(path) || self.resources.contains_key(path)
    }
}

fn font_paths(res: &Resources) -> Vec<String> {
    let mut v: Vec<&Resource> = res.by_number.values().filter(|r| r.kind == ResKind::Font).collect();
    v.sort_by_key(|r| r.number);
    v.into_iter().map(|r| r.path.clone()).collect()
}

/// Message for an encrypted book.
fn refuse(m: &Mobi) -> Result<()> {
    match m.drm() {
        Some(s) => Err(DocError::Unsupported(crate::epub::drm_message(&s))),
        None => Ok(()),
    }
}

/// Converts a Mobipocket file (KF8 part preferred).
pub fn load(path: &Path) -> Result<Book> {
    let m = Mobi::open(path)?;
    refuse(&m)?;
    let mut info = m.info();
    if info.metadata.title.is_empty() {
        info.metadata.title = crate::title_from(path);
    }
    if !m.header.has_mobi {
        // Plain PalmDOC: text.
        let text = header::decode(&m.text(&m.header)?, 1252);
        let (chapters, toc) = crate::txt::convert_text(&text, &info.metadata.title);
        return Ok(Book { info, chapters, toc, page_list: Vec::new(), resources: Box::new(ink_doc::MemoryLoader::default()) });
    }
    let (h, kf8) = match &m.kf8 {
        Some(k) => (k.clone(), true),
        None => (m.header.clone(), false),
    };
    let res = m.resources(&h);
    info.metadata.cover = m.cover(&res).map(|r| r.path.clone());
    info.fonts = font_paths(&res);
    let converted = if kf8 { kf8::convert(&m, &h, &res) } else { mobi6::convert(&m, &h, &res) };
    let (chapters, toc, files) = match converted {
        Ok(c) => c,
        // A broken KF8 part in a joint file: fall back to the MOBI 6 part.
        Err(e) if kf8 && !m.header.is_kf8() => {
            log::warn!("KF8 part unreadable ({e}); using the MOBI 6 part");
            let res6 = m.resources(&m.header);
            let c = mobi6::convert(&m, &m.header, &res6)?;
            return Ok(Book {
                info,
                chapters: c.0,
                toc: c.1,
                page_list: Vec::new(),
                resources: Box::new(MobiLoader::new(m.db.clone(), &res6, c.2)),
            });
        }
        Err(e) => return Err(e),
    };
    if chapters.is_empty() {
        return Err(DocError::Format("MOBI: no text".into()));
    }
    Ok(Book { info, chapters, toc, page_list: Vec::new(), resources: Box::new(MobiLoader::new(m.db.clone(), &res, files)) })
}

/// Metadata and cover of a Mobipocket file (also for DRM-protected books).
pub fn read_info(path: &Path) -> Result<BookInfo> {
    let m = Mobi::open(path)?;
    let mut info = m.info();
    if info.metadata.title.is_empty() {
        info.metadata.title = crate::title_from(path);
    }
    let res = m.resources(m.main());
    info.fonts = font_paths(&res);
    if let Some(c) = m.cover(&res) {
        let loader = MobiLoader::new(m.db.clone(), &res, HashMap::new());
        if let Ok(bytes) = loader.read(&c.path) {
            info.metadata.cover = Some(c.path.clone());
            info.cover = Some(Cover { path: c.path.clone(), media_type: c.media_type.clone(), bytes });
        }
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_and_fonts() {
        assert_eq!(base32("0001"), Some(1));
        assert_eq!(base32("000V"), Some(31));
        assert_eq!(base32("0010"), Some(32));
        assert_eq!(base32("00x!"), None);
        // FONT record: zlib + XOR.
        let font = b"\x00\x01\x00\x00 a tiny font that is not really a font".to_vec();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut z, &font).unwrap();
        let mut data = z.finish().unwrap();
        let key = [0x11u8, 0x22, 0x33];
        for (i, b) in data.iter_mut().take(1040).enumerate() {
            *b ^= key[i % 3];
        }
        let mut rec = b"FONT".to_vec();
        for v in [font.len() as u32, 3, 24 + 3, 3, 24] {
            rec.extend_from_slice(&v.to_be_bytes());
        }
        rec.extend_from_slice(&key);
        rec.extend_from_slice(&data);
        assert_eq!(decode_font(&rec).unwrap(), font);
        assert_eq!(decode_font(b"nope"), None);
    }

    #[test]
    fn plain_palmdoc_text() {
        let text = "CHAPTER 1\n\nA plain PalmDOC book with caf\u{e9} text.\n\nCHAPTER 2\n\nThe end.\n";
        let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode(text);
        let mut rec0 = vec![0u8; 16];
        rec0[0..2].copy_from_slice(&2u16.to_be_bytes());
        rec0[4..8].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        rec0[8..10].copy_from_slice(&1u16.to_be_bytes());
        rec0[10..12].copy_from_slice(&4096u16.to_be_bytes());
        let db = palmdb::tests::build("Plain_Text", b"TEXtREAd", &[rec0, compress::palmdoc_literal(&bytes)]);
        let dir = std::env::temp_dir().join(format!("ink-pdb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("plain.pdb");
        std::fs::write(&f, db).unwrap();
        let book = load(&f).unwrap();
        assert_eq!(book.info.version, "PalmDOC");
        assert_eq!(book.info.metadata.title, "Plain Text");
        assert_eq!(book.toc.len(), 2);
        assert!(book.chapters.iter().any(|c| c.html.contains("caf\u{e9} text")));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
