//! Amazon KFX and Topaz: detected, listed with metadata where it is readable, refused.
//!
//! - KFX container (`CONT`): little-endian header (version, header length, container
//!   info offset/length); the container info is an Ion struct with the index table
//!   (`$413`/`$414`: entries of id, type, offset, length) and the DRM scheme (`$411`).
//!   Entities (`ENTY`) hold one Ion value each. The `book_metadata` entity (`$490`) has
//!   `categorised_metadata` (`$491`): a list of {category `$495`, metadata `$258`: list
//!   of {key `$492`, value `$307`}} with keys such as `title`, `author`, `language`,
//!   `publisher`, `ASIN`, `cde_content_type`. Field numbers follow the `YJ_symbols`
//!   shared table as used by calibre's KFX Input plugin. Checked against KFX containers
//!   written by two independent KFX generators (boko, kfxgen: title, author, language,
//!   ASIN, content type all read), not against a file made by Amazon (none is
//!   available DRM-free), so metadata is best effort and the file name is the fallback
//!   title.
//! - `\xeaDRMION\xee` files are DRM-protected Ion: refused without metadata.
//! - KFX-ZIP (calibre output): the first `.kfx` member is read.
//! - Topaz (`TPZ`): refused without metadata.

use crate::book::{BookInfo, Identifier};
use crate::util::{read_head, read_limited, MAX_ENTRY};
use crate::Format;
use ink_doc::{DocError, Result};
use std::io::Read;
use std::path::Path;

/// An Ion value (binary Ion 1.0, the subset KFX metadata uses).
#[derive(Clone, Debug, PartialEq)]
pub enum Ion {
    /// Null of any type.
    Null,
    /// Boolean.
    Bool(bool),
    /// Integer.
    Int(i64),
    /// Float.
    Float(f64),
    /// Symbol id.
    Symbol(u32),
    /// String.
    Str(String),
    /// Blob or clob.
    Bytes(Vec<u8>),
    /// List or s-expression.
    List(Vec<Ion>),
    /// Struct: (field symbol id, value).
    Struct(Vec<(u32, Ion)>),
    /// Decimal, timestamp: not decoded.
    Other,
}

impl Ion {
    /// Field `sid` of a struct.
    pub fn get(&self, sid: u32) -> Option<&Ion> {
        match self {
            Ion::Struct(f) => f.iter().find(|(k, _)| *k == sid).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string of a string value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Ion::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Integer value.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Ion::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Items of a list.
    pub fn items(&self) -> &[Ion] {
        match self {
            Ion::List(v) => v,
            _ => &[],
        }
    }
}

/// Deepest nesting of Ion containers and annotation wrappers read (each level is a
/// recursive call of `Reader::value`). KFX metadata nests fewer than 10 levels.
const MAX_ION_DEPTH: u32 = 64;

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
    depth: u32,
}

impl Reader<'_> {
    fn varuint(&mut self) -> Option<u64> {
        let mut v: u64 = 0;
        for _ in 0..10 {
            let c = *self.b.get(self.pos)?;
            self.pos += 1;
            v = (v << 7) | (c & 0x7F) as u64;
            if c & 0x80 != 0 {
                return Some(v);
            }
        }
        None
    }

    fn uint(&mut self, len: usize) -> Option<u64> {
        let s = self.b.get(self.pos..self.pos + len)?;
        self.pos += len;
        Some(s.iter().fold(0u64, |a, x| (a << 8) | *x as u64))
    }

    /// Reads one value; `None` at the end or on malformed data.
    fn value(&mut self) -> Option<Ion> {
        loop {
            // Skip Ion version markers between values.
            if self.b.get(self.pos..self.pos + 4) == Some(&[0xE0, 0x01, 0x00, 0xEA]) {
                self.pos += 4;
                continue;
            }
            let td = *self.b.get(self.pos)?;
            self.pos += 1;
            let (t, l) = (td >> 4, td & 0x0F);
            if t == 1 {
                return Some(match l {
                    0 => Ion::Bool(false),
                    1 => Ion::Bool(true),
                    _ => Ion::Null,
                });
            }
            if l == 15 {
                return Some(Ion::Null);
            }
            let len = if l == 14 { usize::try_from(self.varuint()?).ok()? } else { l as usize };
            // A sorted struct (L = 1) has its length as a VarUInt.
            let len = if t == 13 && l == 1 { usize::try_from(self.varuint()?).ok()? } else { len };
            let end = self.pos.checked_add(len)?;
            if end > self.b.len() {
                return None;
            }
            let v = match t {
                0 => {
                    // NOP padding.
                    self.pos = end;
                    continue;
                }
                2 => Ion::Int(self.uint(len)? as i64),
                3 => Ion::Int((self.uint(len)? as i64).wrapping_neg()),
                4 => {
                    let v = match len {
                        4 => f32::from_bits(self.uint(4)? as u32) as f64,
                        8 => f64::from_bits(self.uint(8)?),
                        _ => 0.0,
                    };
                    Ion::Float(v)
                }
                7 => Ion::Symbol(self.uint(len)? as u32),
                8 => Ion::Str(String::from_utf8_lossy(&self.b[self.pos..end]).into_owned()),
                9 | 10 => Ion::Bytes(self.b[self.pos..end].to_vec()),
                11..=13 => {
                    if self.depth > MAX_ION_DEPTH {
                        return None;
                    }
                    let mut sub = Reader { b: &self.b[..end], pos: self.pos, depth: self.depth + 1 };
                    if t == 13 {
                        let mut fields = Vec::new();
                        while sub.pos < end {
                            let k = sub.varuint()? as u32;
                            match sub.value() {
                                Some(v) => fields.push((k, v)),
                                None => break,
                            }
                        }
                        Ion::Struct(fields)
                    } else {
                        let mut items = Vec::new();
                        while sub.pos < end {
                            match sub.value() {
                                Some(v) => items.push(v),
                                None => break,
                            }
                        }
                        Ion::List(items)
                    }
                }
                14 => {
                    // Annotation wrapper: skip the annotations, read the value. Wrappers
                    // nest through `value`, so they count towards the depth limit too.
                    if self.depth > MAX_ION_DEPTH {
                        return None;
                    }
                    let mut sub = Reader { b: &self.b[..end], pos: self.pos, depth: self.depth + 1 };
                    let alen = usize::try_from(sub.varuint()?).ok()?;
                    sub.pos = sub.pos.checked_add(alen).filter(|p| *p <= end)?;
                    let v = sub.value()?;
                    self.pos = end;
                    return Some(v);
                }
                _ => Ion::Other,
            };
            self.pos = end;
            return Some(v);
        }
    }
}

/// Annotations of the value at the start of `b` (after an optional version marker).
fn annotations(b: &[u8]) -> Vec<u32> {
    let mut r = Reader { b, pos: 0, depth: 0 };
    if b.starts_with(&[0xE0, 0x01, 0x00, 0xEA]) {
        r.pos = 4;
    }
    let Some(&td) = b.get(r.pos) else { return Vec::new() };
    if td >> 4 != 14 {
        return Vec::new();
    }
    r.pos += 1;
    if td & 0x0F == 14 && r.varuint().is_none() {
        return Vec::new();
    }
    let Some(alen) = r.varuint() else { return Vec::new() };
    let end = r.pos.saturating_add(usize::try_from(alen).unwrap_or(usize::MAX));
    let mut out = Vec::new();
    while r.pos < end {
        match r.varuint() {
            Some(s) => out.push(s as u32),
            None => break,
        }
    }
    out
}

/// Reads the values of an Ion binary stream, skipping symbol tables (`$ion_symbol_table`, SID 3).
pub fn parse_ion(b: &[u8]) -> Vec<Ion> {
    let mut r = Reader { b, pos: 0, depth: 0 };
    let mut out = Vec::new();
    loop {
        let start = r.pos;
        let Some(v) = r.value() else { break };
        let ann = annotations(&b[start..]);
        if ann.first() != Some(&3) {
            out.push(v);
        }
        if r.pos <= start {
            break;
        }
    }
    out
}

/// Symbol ids used for metadata (calibre KFX Input naming).
mod sid {
    pub const DRM_SCHEME: u32 = 411;
    pub const INDEX_OFFSET: u32 = 413;
    pub const INDEX_LENGTH: u32 = 414;
    pub const METADATA: u32 = 258;
    pub const BOOK_METADATA: u32 = 490;
    pub const CATEGORISED: u32 = 491;
    pub const KEY: u32 = 492;
    pub const VALUE: u32 = 307;
}

/// What a KFX container says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KfxInfo {
    /// Container version.
    pub version: u16,
    /// DRM scheme from the container info (0 = none).
    pub drm_scheme: i64,
    /// Entities that are DRMION-encrypted.
    pub encrypted_entities: usize,
    /// Metadata key/value pairs from `book_metadata`.
    pub metadata: Vec<(String, String)>,
}

/// Parses a KFX `CONT` container.
pub fn parse_container(data: &[u8]) -> Result<KfxInfo> {
    let bad = |m: &str| DocError::Format(format!("KFX: {m}"));
    if !data.starts_with(b"CONT") {
        return Err(bad("not a KFX container"));
    }
    let le16 = |o: usize| data.get(o..o.checked_add(2)?).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let le32 = |o: usize| data.get(o..o.checked_add(4)?).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize);
    let le64 = |o: usize| data.get(o..o.checked_add(8)?).map(|s| u64::from_le_bytes(s.try_into().expect("8")) as usize);
    let version = le16(4).ok_or_else(|| bad("short header"))?;
    let header_len = le32(6).ok_or_else(|| bad("short header"))?;
    let ci_off = le32(10).ok_or_else(|| bad("short header"))?;
    let ci_len = le32(14).ok_or_else(|| bad("short header"))?;
    let ci = data.get(ci_off..ci_off.saturating_add(ci_len)).ok_or_else(|| bad("container info out of range"))?;
    let info = parse_ion(ci).into_iter().next().unwrap_or(Ion::Null);
    let mut out = KfxInfo { version, drm_scheme: info.get(sid::DRM_SCHEME).and_then(Ion::as_int).unwrap_or(0), ..Default::default() };
    let idx_off = info.get(sid::INDEX_OFFSET).and_then(Ion::as_int).unwrap_or(-1);
    let idx_len = info.get(sid::INDEX_LENGTH).and_then(Ion::as_int).unwrap_or(0).max(0) as usize;
    if idx_off < 0 {
        return Ok(out);
    }
    for i in (0..idx_len).step_by(24) {
        let Some(e) = usize::try_from(idx_off).ok().and_then(|o| o.checked_add(i)) else { break };
        let (Some(_id), Some(ty), Some(off), Some(len)) = (le32(e), le32(e + 4), le64(e + 8), le64(e + 16)) else { break };
        let start = header_len.saturating_add(off);
        let Some(ent) = data.get(start..start.saturating_add(len)) else { continue };
        if !ent.starts_with(b"ENTY") {
            continue;
        }
        let ehl = ent.get(6..10).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize).unwrap_or(ent.len());
        let body = ent.get(ehl..).unwrap_or(&[]);
        if body.starts_with(b"\xeaDRMION\xee") {
            out.encrypted_entities += 1;
            continue;
        }
        if ty as u32 == sid::BOOK_METADATA {
            let v = parse_ion(body).into_iter().next().unwrap_or(Ion::Null);
            for cat in v.get(sid::CATEGORISED).map(Ion::items).unwrap_or(&[]) {
                for kv in cat.get(sid::METADATA).map(Ion::items).unwrap_or(&[]) {
                    if let (Some(k), Some(v)) = (kv.get(sid::KEY).and_then(Ion::as_str), kv.get(sid::VALUE).and_then(Ion::as_str)) {
                        out.metadata.push((k.to_string(), v.to_string()));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Message for KFX books.
pub const KFX_MESSAGE: &str = "This is a KFX book (Amazon's own format). rinky cannot read KFX; open it with \"Exit to Kindle\".";
/// Message for DRMION files.
pub const DRMION_MESSAGE: &str = "This is a DRM-protected KFX book. rinky cannot read KFX or DRM-protected books; open it with \"Exit to Kindle\".";
/// Message for Topaz books.
pub const TOPAZ_MESSAGE: &str = "This is a Topaz book (an old Amazon format with DRM). rinky cannot read it; open it with \"Exit to Kindle\".";

/// The container bytes: the file (its first [`MAX_ENTRY`] bytes; metadata comes first
/// in KFX containers, and books with large images can be hundreds of MB), or the first
/// `.kfx` member of a KFX-ZIP.
fn container_bytes(path: &Path) -> Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let mut magic = [0u8; 4];
    let is_zip = f.read_exact(&mut magic).is_ok() && &magic == b"PK\x03\x04";
    drop(f);
    if is_zip {
        let f = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(f).map_err(|e| DocError::Format(format!("KFX-ZIP: {e}")))?;
        let names: Vec<String> = zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
        let Some(name) = names.iter().find(|n| n.to_ascii_lowercase().ends_with(".kfx")).or(names.first()) else {
            return Err(DocError::Format("KFX-ZIP: empty".into()));
        };
        let e = zip.by_name(name).map_err(|e| DocError::Format(format!("KFX-ZIP: {e}")))?;
        let size = e.size();
        return Ok(read_limited(e, size, MAX_ENTRY)?);
    }
    Ok(read_head(path, MAX_ENTRY)?)
}

/// Metadata (best effort) and the refusal message of a KFX or Topaz file.
pub fn read_info(path: &Path, format: Format) -> Result<BookInfo> {
    let mut info = BookInfo::new(format);
    info.metadata.title = crate::title_from(path);
    if format == Format::Topaz {
        info.version = "Topaz".into();
        info.drm = Some("Amazon Topaz".into());
        info.refusal = Some(TOPAZ_MESSAGE.into());
        return Ok(info);
    }
    let data = match container_bytes(path) {
        Ok(d) => d,
        // Unreadable KFX-ZIP or file: still listed as KFX and refused.
        Err(e) => {
            log::warn!("{}: KFX container unreadable: {e}", path.display());
            info.refusal = Some(KFX_MESSAGE.into());
            info.version = "KFX".into();
            return Ok(info);
        }
    };
    if data.starts_with(b"\xeaDRMION\xee") {
        info.version = "KFX (DRMION)".into();
        info.drm = Some("Amazon KFX".into());
        info.refusal = Some(DRMION_MESSAGE.into());
        return Ok(info);
    }
    info.refusal = Some(KFX_MESSAGE.into());
    info.version = "KFX".into();
    let Ok(k) = parse_container(&data) else { return Ok(info) };
    info.version = format!("KFX container v{}", k.version);
    if k.drm_scheme != 0 || k.encrypted_entities > 0 {
        info.drm = Some("Amazon KFX".into());
    }
    for (key, v) in &k.metadata {
        let v = v.trim().to_string();
        if v.is_empty() {
            continue;
        }
        match key.as_str() {
            "title" => info.metadata.title = v,
            "author" => info.metadata.authors.push(v),
            "language" => info.metadata.language = Some(v),
            "publisher" => info.metadata.publisher = Some(v),
            "description" => info.metadata.description = Some(v),
            "issue_date" => info.date = Some(v),
            "ASIN" | "asin" => info.identifiers.push(Identifier { scheme: Some("ASIN".into()), value: v }),
            "cde_content_type" => info.version = format!("{} {v}", info.version),
            _ => {}
        }
    }
    info.metadata.identifier = info.identifiers.first().map(|i| i.value.clone());
    Ok(info)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn varuint(v: u64) -> Vec<u8> {
        let mut out = vec![(v & 0x7F) as u8 | 0x80];
        let mut v = v >> 7;
        while v > 0 {
            out.insert(0, (v & 0x7F) as u8);
            v >>= 7;
        }
        out
    }

    fn tl(t: u8, body: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        if body.len() < 14 {
            out.push(t << 4 | body.len() as u8);
        } else {
            out.push(t << 4 | 14);
            out.extend(varuint(body.len() as u64));
        }
        out.extend(body);
        out
    }

    /// Ion writer for tests.
    pub fn string(s: &str) -> Vec<u8> {
        tl(8, s.as_bytes().to_vec())
    }
    pub fn int(v: u64) -> Vec<u8> {
        let b: Vec<u8> = v.to_be_bytes().into_iter().skip_while(|x| *x == 0).collect();
        tl(2, b)
    }
    pub fn list(items: Vec<Vec<u8>>) -> Vec<u8> {
        tl(11, items.concat())
    }
    pub fn strukt(fields: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
        tl(13, fields.into_iter().flat_map(|(k, v)| [varuint(k as u64), v].concat()).collect())
    }

    #[test]
    fn ion_values() {
        let v = [vec![0xE0, 0x01, 0x00, 0xEA], strukt(vec![(10, string("hello")), (11, int(300)), (12, list(vec![int(1), string("a long string value here")]))])].concat();
        let vals = parse_ion(&v);
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0].get(10).and_then(Ion::as_str), Some("hello"));
        assert_eq!(vals[0].get(11).and_then(Ion::as_int), Some(300));
        assert_eq!(vals[0].get(12).unwrap().items()[1].as_str(), Some("a long string value here"));
        // A symbol table (annotation $3) is skipped.
        let st = tl(14, [varuint(1), varuint(3), strukt(vec![])].concat());
        assert_eq!(parse_ion(&[st, int(5)].concat()), vec![Ion::Int(5)]);
        assert_eq!(parse_ion(&[0x2E, 0xFF]), Vec::<Ion>::new());
    }

    /// A KFX container with a book_metadata entity.
    pub fn container(meta: &[(&str, &str)], drm: bool) -> Vec<u8> {
        let entries: Vec<Vec<u8>> = meta.iter().map(|(k, v)| strukt(vec![(sid::KEY, string(k)), (sid::VALUE, string(v))])).collect();
        let value = [vec![0xE0, 0x01, 0x00, 0xEA], strukt(vec![(sid::CATEGORISED, list(vec![strukt(vec![(495, string("kindle_title_metadata")), (sid::METADATA, list(entries))])]))])].concat();
        let mut ent = b"ENTY".to_vec();
        ent.extend_from_slice(&1u16.to_le_bytes());
        let ehdr = strukt(vec![(410, int(0)), (411, int(0))]);
        ent.extend_from_slice(&((10 + ehdr.len()) as u32).to_le_bytes());
        ent.extend_from_slice(&ehdr);
        ent.extend_from_slice(&value);
        let header_len = 18usize;
        let idx: Vec<u8> = [1000u32.to_le_bytes().to_vec(), sid::BOOK_METADATA.to_le_bytes().to_vec(), 0u64.to_le_bytes().to_vec(), (ent.len() as u64).to_le_bytes().to_vec()].concat();
        let idx_off = header_len + ent.len();
        let ci = strukt(vec![(409, string("CR!TEST")), (sid::DRM_SCHEME, int(drm as u64)), (sid::INDEX_OFFSET, int(idx_off as u64)), (sid::INDEX_LENGTH, int(idx.len() as u64))]);
        let ci_off = idx_off + idx.len();
        let mut out = b"CONT".to_vec();
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&(header_len as u32).to_le_bytes());
        out.extend_from_slice(&(ci_off as u32).to_le_bytes());
        out.extend_from_slice(&(ci.len() as u32).to_le_bytes());
        out.extend_from_slice(&ent);
        out.extend_from_slice(&idx);
        out.extend_from_slice(&ci);
        out
    }

    #[test]
    fn container_metadata() {
        let c = container(&[("title", "A KFX Title"), ("author", "Some Author"), ("language", "en"), ("ASIN", "B000TEST00")], true);
        let k = parse_container(&c).unwrap();
        assert_eq!(k.version, 2);
        assert_eq!(k.drm_scheme, 1);
        assert_eq!(k.metadata[0], ("title".into(), "A KFX Title".into()));
        assert_eq!(k.metadata.len(), 4);
        assert!(parse_container(b"nope").is_err());
    }
}
