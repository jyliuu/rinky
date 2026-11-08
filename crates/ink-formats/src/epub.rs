//! EPUB 2 and 3.
//!
//! `META-INF/container.xml` -> OPF (metadata, manifest, spine, guide) -> XHTML chapters.
//! Spine items that are missing from the archive are passed on as empty chapters so
//! `DocFragment[N]` numbering stays aligned with the spine (KOReader counts every
//! itemref). Chapter bytes are handed to the engine unchanged.
//!
//! - Metadata: titles (EPUB 3 `title-type main` first), creators (role `aut` or none),
//!   language, identifiers with schemes (EPUB 2 `opf:scheme`, EPUB 3 `identifier-type`
//!   and `urn:` prefixes), publisher, description, date, subjects, series (calibre
//!   `calibre:series` or EPUB 3 `belongs-to-collection`).
//! - TOC: EPUB 3 nav (`epub:type="toc"`, nested `ol`) or NCX `navMap` (nested
//!   `navPoint`s); page list from nav `page-list` or NCX `pageList`.
//! - Cover: manifest `cover-image`, `<meta name="cover">`, the guide's cover page, or a
//!   manifest image named like a cover.
//! - Fonts: IDPF (`http://www.idpf.org/2008/embedding`, SHA-1 key, 1040 bytes) and Adobe
//!   (`http://ns.adobe.com/pdf/enc#RC`, UUID key, 1024 bytes) obfuscation is undone when
//!   the engine reads the font.
//! - DRM: any other encryption (Adobe ADEPT, Apple FairPlay, Readium LCP, ...) is refused
//!   with a message; metadata stays readable.

use crate::book::{Book, BookDocument, BookInfo, Cover, Identifier, PageTarget, Target, TocNode};
use crate::util::{read_limited, sha1, sniff_media_type, MAX_COVER, MAX_ENTRY};
use crate::xml::{self, collapse_ws, Element};
use crate::Format;
use ink_doc::{percent_decode, resolve_href, DocError, Metadata, ResourceLoader, Result};
use ink_html::{HtmlChapter, HtmlDocument};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::{Arc, Mutex};
use zip::ZipArchive;

/// Font obfuscation key for one resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Obfuscation {
    /// IDPF: XOR the first 1040 bytes with the SHA-1 of the unique identifier.
    Idpf([u8; 20]),
    /// Adobe: XOR the first 1024 bytes with the 16 bytes of the book's UUID.
    Adobe([u8; 16]),
}

impl Obfuscation {
    /// Undoes (or applies; XOR is symmetric) the obfuscation in place.
    pub fn apply(&self, data: &mut [u8]) {
        let (key, len): (&[u8], usize) = match self {
            Obfuscation::Idpf(key) => (key, 1040),
            Obfuscation::Adobe(key) => (key, 1024),
        };
        for (b, k) in data.iter_mut().take(len).zip(key.iter().cycle()) {
            *b ^= k;
        }
    }
}

/// Resources read from the EPUB zip on demand; obfuscated fonts are deobfuscated.
pub struct ZipLoader<R: Read + Seek> {
    zip: RefCell<ZipArchive<R>>,
    obfuscated: HashMap<String, Obfuscation>,
    lower: RefCell<Option<HashMap<String, String>>>,
}

impl<R: Read + Seek> ZipLoader<R> {
    /// Wraps an open archive.
    pub fn new(zip: ZipArchive<R>) -> Self {
        ZipLoader { zip: RefCell::new(zip), obfuscated: HashMap::new(), lower: RefCell::new(None) }
    }

    /// Entry names in the archive.
    pub fn names(&self) -> Vec<String> {
        self.zip.borrow().file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect()
    }

    /// Marks `path` as obfuscated with `key`.
    pub fn set_obfuscation(&mut self, path: String, key: Obfuscation) {
        self.obfuscated.insert(path, key);
    }

    /// Paths with font obfuscation.
    pub fn obfuscated(&self) -> &HashMap<String, Obfuscation> {
        &self.obfuscated
    }

    /// The archive entry name for `path`: exact, else a case-insensitive match (books
    /// made on case-insensitive file systems).
    fn entry_name(&self, path: &str) -> Option<String> {
        let zip = self.zip.borrow();
        if zip.index_for_name(path).is_some() {
            return Some(path.to_string());
        }
        let mut lower = self.lower.borrow_mut();
        let map = lower.get_or_insert_with(|| zip.file_names().filter_map(|n| n.ok()).map(|n| (n.to_lowercase(), n.into_owned())).collect());
        map.get(&path.to_lowercase()).cloned()
    }

    fn read_raw(&self, path: &str) -> Result<Vec<u8>> {
        self.read_raw_max(path, MAX_ENTRY)
    }

    /// Entry bytes as stored, at most `limit` (a larger entry is an error).
    fn read_raw_max(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let name = self.entry_name(path).ok_or_else(|| DocError::NotFound(path.into()))?;
        let mut zip = self.zip.borrow_mut();
        let entry = zip.by_name(&name).map_err(|_| DocError::NotFound(path.into()))?;
        let size = entry.size();
        read_limited(entry, size, limit).map_err(|e| DocError::Format(format!("{path}: {e}")))
    }

    /// The size `path` declares in the archive's central directory (uncompressed).
    fn stored_size(&self, path: &str) -> Option<usize> {
        let name = self.entry_name(path)?;
        let zip = self.zip.borrow();
        let size = zip.by_index_data(zip.index_for_name(&name)?).ok()?.size();
        usize::try_from(size).ok()
    }

    /// The bytes of entry `path` up to the end of the first `needle` (ASCII, matched
    /// ignoring case), inflating no more than that; `None` when the first `max` bytes
    /// do not have it, or the entry is neither stored nor deflated. `inflate` (raw
    /// deflate) is reset and reused: reading many heads allocates no inflater each.
    fn read_until_ci(&self, path: &str, needle: &[u8], max: usize, inflate: &mut flate2::Decompress) -> Option<Vec<u8>> {
        let name = self.entry_name(path)?;
        let mut zip = self.zip.borrow_mut();
        let index = zip.index_for_name(&name)?;
        let mut raw = zip.by_index_raw(index).ok()?;
        let deflated = match raw.compression() {
            _ if raw.encrypted() => return None,
            zip::CompressionMethod::Stored => false,
            zip::CompressionMethod::Deflated => true,
            _ => return None,
        };
        inflate.reset(false);
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            // At the end (n == 0) the inflater gives what it still holds.
            let n = raw.read(&mut chunk).ok()?;
            let from = buf.len().saturating_sub(needle.len() - 1);
            if deflated {
                inflate_into(inflate, &chunk[..n], &mut buf)?;
            } else {
                buf.extend_from_slice(&chunk[..n]);
            }
            if let Some(p) = buf[from..].windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle)) {
                buf.truncate(from + p + needle.len());
                return Some(buf);
            }
            if n == 0 || buf.len() > max {
                return None;
            }
        }
    }

    /// Entry bytes (deobfuscated), at most `limit`.
    pub fn read_max(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let mut bytes = self.read_raw_max(path, limit)?;
        if let Some(key) = self.obfuscated.get(path) {
            key.apply(&mut bytes);
        }
        Ok(bytes)
    }
}

/// Inflates all of `input` onto `out`, and whatever output the inflater still holds.
fn inflate_into(inflate: &mut flate2::Decompress, input: &[u8], out: &mut Vec<u8>) -> Option<()> {
    let mut used = 0;
    loop {
        out.reserve(4096);
        let (was_in, was_out) = (inflate.total_in(), inflate.total_out());
        let status = inflate.decompress_vec(&input[used..], out, flate2::FlushDecompress::None).ok()?;
        used += (inflate.total_in() - was_in) as usize;
        let stuck = inflate.total_in() == was_in && inflate.total_out() == was_out;
        let drained = used == input.len() && out.len() < out.capacity();
        if status == flate2::Status::StreamEnd || stuck || drained {
            return Some(());
        }
    }
}

impl<R: Read + Seek> ResourceLoader for ZipLoader<R> {
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.read_max(path, MAX_ENTRY)
    }

    fn exists(&self, path: &str) -> bool {
        self.entry_name(path).is_some()
    }
}

/// One manifest item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ManifestItem {
    /// `id`.
    pub id: String,
    /// `href` as written (relative to the OPF).
    pub href: String,
    /// Full zip path.
    pub path: String,
    /// `media-type`.
    pub media_type: String,
    /// EPUB 3 `properties` (`nav`, `cover-image`, `svg`, ...).
    pub properties: Vec<String>,
}

impl ManifestItem {
    fn has(&self, prop: &str) -> bool {
        self.properties.iter().any(|p| p == prop)
    }

    /// True for font files.
    pub fn is_font(&self) -> bool {
        let m = self.media_type.to_ascii_lowercase();
        let p = self.path.to_ascii_lowercase();
        m.contains("font") || m.contains("opentype") || [".ttf", ".otf", ".woff", ".woff2"].iter().any(|e| p.ends_with(e))
    }

    /// True for images.
    pub fn is_image(&self) -> bool {
        self.media_type.starts_with("image/")
    }
}

/// What the OPF says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Package {
    /// Path of the OPF inside the zip.
    pub opf_path: String,
    /// `version` attribute of `<package>` (`2.0`, `3.0`).
    pub version: String,
    /// Metadata (title, creators, language, identifier, publisher, description, cover, series).
    pub metadata: Metadata,
    /// Every `dc:identifier` with its scheme.
    pub identifiers: Vec<Identifier>,
    /// The value of the identifier named by `unique-identifier`.
    pub unique_identifier: Option<String>,
    /// `dc:subject` values.
    pub subjects: Vec<String>,
    /// `dc:date`.
    pub date: Option<String>,
    /// Manifest: id -> href (relative to the OPF).
    pub manifest: HashMap<String, String>,
    /// Manifest items in document order.
    pub items: Vec<ManifestItem>,
    /// Spine idrefs in reading order.
    pub spine: Vec<String>,
    /// `linear="no"` per spine entry.
    pub nonlinear: Vec<bool>,
    /// Manifest id of the NCX (`<spine toc="...">`).
    pub ncx_id: Option<String>,
    /// Guide references: (type, href relative to the OPF).
    pub guide: Vec<(String, String)>,
    /// `<meta name="cover" content="...">` as written.
    pub cover_meta: Option<String>,
}

impl Package {
    fn dir(&self) -> &str {
        dir_of(&self.opf_path)
    }

    /// Full zip path of manifest item `id`.
    pub fn item_path(&self, id: &str) -> Option<String> {
        self.manifest.get(id).map(|href| resolve_href(self.dir(), href))
    }

    /// The manifest item with `id`.
    pub fn item(&self, id: &str) -> Option<&ManifestItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Zip path of the cover image, by EPUB 3 property, `<meta name="cover">` or a
    /// manifest image named like a cover. The guide's cover page needs the archive
    /// ([`find_cover`]).
    pub fn cover_image(&self) -> Option<String> {
        if let Some(it) = self.items.iter().find(|i| i.has("cover-image")) {
            return Some(it.path.clone());
        }
        if let Some(c) = &self.cover_meta {
            if let Some(it) = self.item(c) {
                if it.is_image() || it.media_type.is_empty() {
                    return Some(it.path.clone());
                }
            }
            // Some books put the href in `content`.
            if let Some(it) = self.items.iter().find(|i| i.href == *c && i.is_image()) {
                return Some(it.path.clone());
            }
        }
        self.items
            .iter()
            .find(|i| i.is_image() && (i.id.to_ascii_lowercase().contains("cover") || i.href.to_ascii_lowercase().contains("cover")))
            .map(|i| i.path.clone())
    }
}

fn text_of(bytes: Vec<u8>) -> String {
    // Valid UTF-8 (nearly every book) keeps its buffer: no copy of a whole chapter.
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Chapter bytes as text: UTF-8 as before (lossy); UTF-16 or a declared legacy
/// encoding are decoded properly.
fn chapter_text(bytes: Vec<u8>) -> String {
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    let legacy = xml::declared_encoding(&bytes)
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .is_some_and(|e| e != encoding_rs::UTF_8 && e != encoding_rs::UTF_16LE && e != encoding_rs::UTF_16BE);
    let text = if utf16 || legacy { xml::decode_xml_bytes(&bytes) } else { text_of(bytes) };
    // A chapter nested deeper than the XML limit goes through the HTML repair, which
    // flattens it (the engine and the id-path walk recurse over the tree).
    if xml::nesting_exceeds(&text, xml::MAX_DEPTH) {
        log::warn!("chapter nested deeper than {} elements: flattened", xml::MAX_DEPTH);
        return crate::xhtml::to_xhtml(&text, &mut crate::xhtml::NoRewrite);
    }
    text
}

/// Plain text of a description written as escaped HTML (`&lt;p&gt;...`, which the XML
/// parser has unescaped once): tags dropped, the common entities decoded.
fn description_text(s: &str) -> String {
    let decoded = xml::strip_tags(s).replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&");
    collapse_ws(&decoded)
}

fn scheme_of(value: &str, declared: Option<String>) -> Option<String> {
    if declared.is_some() {
        return declared;
    }
    let l = value.to_ascii_lowercase();
    for (prefix, scheme) in [("urn:uuid:", "UUID"), ("urn:isbn:", "ISBN"), ("isbn:", "ISBN"), ("uuid:", "UUID"), ("urn:doi:", "DOI"), ("doi:", "DOI")] {
        if l.starts_with(prefix) {
            return Some(scheme.into());
        }
    }
    let digits: String = value.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if (digits.len() == 13 && (digits.starts_with("978") || digits.starts_with("979")) && digits.chars().all(|c| c.is_ascii_digit()))
        || (digits.len() == 10 && digits[..9].chars().all(|c| c.is_ascii_digit()))
    {
        return Some("ISBN".into());
    }
    None
}

/// Parses an OPF document found at `opf_path` in the archive.
pub fn parse_opf(xml_text: &str, opf_path: &str) -> Result<Package> {
    let root = xml::parse(xml_text).map_err(|e| DocError::Format(format!("OPF: {e}")))?;
    let version = root.attr("version").unwrap_or("").to_string();
    let mut p = Package { opf_path: opf_path.into(), version, ..Default::default() };
    let unique_id = root.attr("unique-identifier").map(str::to_string);
    let empty = Element::default();
    let meta = root.child("metadata").unwrap_or(&empty);
    // EPUB 3 refinements: refined id -> [(property, value, scheme)].
    let mut refines: HashMap<String, Vec<(String, String, Option<String>)>> = HashMap::new();
    let mut all_meta = Vec::new();
    meta.find_all("meta", &mut all_meta);
    for m in &all_meta {
        if let (Some(r), Some(prop)) = (m.attr("refines"), m.attr("property")) {
            refines.entry(r.trim_start_matches('#').to_string()).or_default().push((
                prop.to_string(),
                m.text_collapsed(),
                m.attr("scheme").map(str::to_string),
            ));
        }
    }
    let refined = |el: &Element, prop: &str| -> Option<String> {
        let id = el.attr("id")?;
        refines.get(id)?.iter().find(|(p, _, _)| p == prop).map(|(_, v, _)| v.clone())
    };
    let mut dc = Vec::new();
    for name in ["title", "creator", "language", "identifier", "publisher", "description", "date", "subject"] {
        let mut v = Vec::new();
        meta.find_all(name, &mut v);
        dc.push((name, v));
    }
    let get = |name: &str| dc.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone()).unwrap_or_default();
    // Title: the "main" one if marked, else the first.
    let titles = get("title");
    let title = titles.iter().find(|t| refined(t, "title-type").as_deref() == Some("main")).or(titles.first());
    if let Some(t) = title {
        p.metadata.title = t.text().trim().to_string();
    }
    let creators = get("creator");
    let role = |c: &Element| c.attr("role").map(str::to_string).or_else(|| refined(c, "role"));
    let mut authors: Vec<String> =
        creators.iter().filter(|c| matches!(role(c).as_deref(), None | Some("aut"))).map(|c| c.text_collapsed()).collect();
    if authors.is_empty() {
        authors = creators.iter().map(|c| c.text_collapsed()).collect();
    }
    p.metadata.authors = authors.into_iter().filter(|a| !a.is_empty()).collect();
    p.metadata.language = get("language").first().map(|l| l.text_collapsed()).filter(|l| !l.is_empty());
    p.metadata.publisher = get("publisher").first().map(|l| l.text_collapsed()).filter(|l| !l.is_empty());
    p.metadata.description = get("description").first().map(|d| description_text(&d.text())).filter(|d| !d.is_empty());
    p.date = get("date").first().map(|d| d.text_collapsed()).filter(|d| !d.is_empty());
    p.subjects = get("subject").iter().map(|s| s.text_collapsed()).filter(|s| !s.is_empty()).collect();
    for id in get("identifier") {
        let value = id.text_collapsed();
        if value.is_empty() {
            continue;
        }
        let declared = id.attr("scheme").map(str::to_string).or_else(|| refined(id, "identifier-type"));
        if unique_id.is_some() && id.attr("id") == unique_id.as_deref() {
            p.unique_identifier = Some(value.clone());
        }
        p.identifiers.push(Identifier { scheme: scheme_of(&value, declared), value });
    }
    p.metadata.identifier = p.unique_identifier.clone().or_else(|| p.identifiers.first().map(|i| i.value.clone()));
    // Series: calibre meta, else EPUB 3 collection.
    let named = |n: &str| all_meta.iter().find(|m| m.attr("name") == Some(n)).and_then(|m| m.attr("content")).map(str::to_string);
    if let Some(s) = named("calibre:series").filter(|s| !s.trim().is_empty()) {
        let idx = named("calibre:series_index").and_then(|i| i.trim().parse::<f32>().ok()).unwrap_or(1.0);
        p.metadata.series = Some((s.trim().to_string(), idx));
    } else if let Some(c) = all_meta.iter().find(|m| {
        m.attr("property") == Some("belongs-to-collection")
            && refined(m, "collection-type").as_deref().is_none_or(|t| t == "series")
    }) {
        let idx = refined(c, "group-position").and_then(|i| i.trim().parse::<f32>().ok()).unwrap_or(1.0);
        p.metadata.series = Some((c.text_collapsed(), idx));
    }
    p.cover_meta = named("cover").map(|c| c.trim().to_string());
    if let Some(manifest) = root.child("manifest") {
        for it in manifest.children_named("item") {
            if let (Some(id), Some(href)) = (it.attr("id"), it.attr("href")) {
                p.manifest.insert(id.to_string(), href.to_string());
                p.items.push(ManifestItem {
                    id: id.to_string(),
                    href: href.to_string(),
                    path: resolve_href(dir_of(opf_path), href),
                    media_type: it.attr("media-type").unwrap_or("").to_string(),
                    properties: it.attr("properties").unwrap_or("").split_whitespace().map(str::to_string).collect(),
                });
            }
        }
    }
    if let Some(spine) = root.child("spine") {
        p.ncx_id = spine.attr("toc").map(str::to_string);
        for r in spine.children_named("itemref") {
            if let Some(id) = r.attr("idref") {
                p.spine.push(id.to_string());
                p.nonlinear.push(r.attr("linear") == Some("no"));
            }
        }
    }
    if let Some(guide) = root.child("guide") {
        for r in guide.children_named("reference") {
            if let (Some(t), Some(h)) = (r.attr("type"), r.attr("href")) {
                p.guide.push((t.to_ascii_lowercase(), h.to_string()));
            }
        }
    }
    p.metadata.cover = p.cover_image();
    Ok(p)
}

/// Reads the container and OPF of an open archive.
pub fn read_package<R: Read + Seek>(loader: &ZipLoader<R>) -> Result<Package> {
    let container = text_of(loader.read_raw("META-INF/container.xml")?);
    let croot = xml::parse(&container).map_err(|e| DocError::Format(format!("container.xml: {e}")))?;
    let mut rootfiles = Vec::new();
    croot.find_all("rootfile", &mut rootfiles);
    let opf_path = rootfiles
        .iter()
        .find(|r| r.attr("media-type") == Some("application/oebps-package+xml"))
        .or(rootfiles.first())
        .and_then(|r| r.attr("full-path"))
        .map(|p| percent_decode(p).trim_start_matches('/').to_string())
        .ok_or_else(|| DocError::Format("container.xml has no rootfile".into()))?;
    parse_opf(&xml::decode_xml_bytes(&loader.read_raw(&opf_path)?), &opf_path)
}

/// One `EncryptedData` entry of `META-INF/encryption.xml`.
#[derive(Clone, Debug, PartialEq)]
pub struct Encrypted {
    /// Algorithm URI.
    pub algorithm: String,
    /// Zip path of the encrypted resource.
    pub path: String,
}

const IDPF_FONT: &str = "http://www.idpf.org/2008/embedding";
const ADOBE_FONT: &str = "http://ns.adobe.com/pdf/enc#RC";

/// Parses `META-INF/encryption.xml`.
pub fn parse_encryption(xml_text: &str) -> Vec<Encrypted> {
    let Ok(root) = xml::parse(xml_text) else { return Vec::new() };
    let mut data = Vec::new();
    root.find_all("EncryptedData", &mut data);
    data.iter()
        .filter_map(|d| {
            let algorithm = d.find("EncryptionMethod")?.attr("Algorithm")?.trim().to_string();
            let uri = d.find("CipherReference")?.attr("URI")?;
            Some(Encrypted { algorithm, path: resolve_href("", uri) })
        })
        .collect()
}

/// The DRM scheme of an archive, if any (font obfuscation is not DRM).
pub fn drm_scheme<R: Read + Seek>(loader: &ZipLoader<R>, encrypted: &[Encrypted]) -> Option<String> {
    let names: Vec<String> = loader.names().iter().map(|n| n.to_ascii_lowercase()).collect();
    let has = |n: &str| names.iter().any(|x| x == n);
    if has("meta-inf/license.lcpl") {
        return Some("Readium LCP".into());
    }
    if has("meta-inf/sinf.xml") {
        return Some("Apple FairPlay".into());
    }
    let other = encrypted.iter().any(|e| e.algorithm != IDPF_FONT && e.algorithm != ADOBE_FONT);
    if !other {
        return None;
    }
    let rights = loader.read_raw("META-INF/rights.xml").map(text_of).unwrap_or_default().to_ascii_lowercase();
    Some(if rights.contains("adept") || rights.contains("adobe") {
        "Adobe ADEPT".into()
    } else if rights.contains("barnes") || rights.contains("bn.com") {
        "Barnes & Noble".into()
    } else if names.iter().any(|n| n.contains("kobo")) {
        "Kobo".into()
    } else {
        "encrypted EPUB (unknown DRM)".into()
    })
}

/// Message shown when a DRM-protected book is opened.
pub fn drm_message(scheme: &str) -> String {
    format!("This book is protected by DRM ({scheme}). rinky can only open DRM-free books.")
}

fn uuid_bytes(s: &str) -> Option<[u8; 16]> {
    let l = s.trim().to_ascii_lowercase();
    let l = l.strip_prefix("urn:uuid:").or_else(|| l.strip_prefix("uuid:")).unwrap_or(&l);
    let hex: String = l.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// The IDPF obfuscation key: SHA-1 of the unique identifier without whitespace.
pub fn idpf_key(unique_identifier: &str) -> [u8; 20] {
    let id: String = unique_identifier.chars().filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n')).collect();
    sha1(id.as_bytes())
}

/// The Adobe obfuscation key: the UUID among the identifiers (unique identifier first).
pub fn adobe_key(pkg: &Package) -> Option<[u8; 16]> {
    pkg.unique_identifier
        .iter()
        .chain(pkg.identifiers.iter().map(|i| &i.value))
        .find_map(|v| uuid_bytes(v))
}

fn set_obfuscation<R: Read + Seek>(loader: &mut ZipLoader<R>, pkg: &Package, encrypted: &[Encrypted]) {
    let uid = pkg.unique_identifier.clone().or_else(|| pkg.identifiers.first().map(|i| i.value.clone()));
    for e in encrypted {
        let key = match e.algorithm.as_str() {
            IDPF_FONT => uid.as_deref().map(|u| Obfuscation::Idpf(idpf_key(u))),
            ADOBE_FONT => adobe_key(pkg).map(Obfuscation::Adobe),
            _ => None,
        };
        if let Some(k) = key {
            loader.set_obfuscation(e.path.clone(), k);
        }
    }
}

/// Zip path -> spine index (first occurrence).
fn spine_index(pkg: &Package) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for (i, id) in pkg.spine.iter().enumerate() {
        if let Some(p) = pkg.item_path(id) {
            m.entry(p).or_insert(i);
        }
    }
    m
}

fn target_of(base_dir: &str, href: &str, spine: &HashMap<String, usize>) -> Option<Target> {
    let path = resolve_href(base_dir, href);
    let chapter = *spine.get(&path)?;
    let frag = href.split_once('#').map(|(_, f)| percent_decode(f)).filter(|f| !f.is_empty());
    Some(Target { chapter, id: frag })
}

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i + 1],
        None => "",
    }
}

fn ncx_points(el: &Element, dir: &str, spine: &HashMap<String, usize>) -> Vec<TocNode> {
    el.children_named("navPoint")
        .map(|np| {
            let title = np.child("navLabel").map(|l| l.text_collapsed()).unwrap_or_default();
            let target = np.child("content").and_then(|c| c.attr("src")).and_then(|s| target_of(dir, s, spine));
            TocNode { title, target, children: ncx_points(np, dir, spine) }
        })
        .collect()
}

/// TOC and page list from an NCX document at `ncx_path`.
pub fn parse_ncx(xml_text: &str, ncx_path: &str, spine: &HashMap<String, usize>) -> (Vec<TocNode>, Vec<PageTarget>) {
    let Ok(root) = xml::parse(xml_text) else { return (Vec::new(), Vec::new()) };
    let dir = dir_of(ncx_path);
    let toc = root.child("navMap").map(|m| ncx_points(m, dir, spine)).unwrap_or_default();
    let mut pages = Vec::new();
    if let Some(pl) = root.child("pageList") {
        for pt in pl.children_named("pageTarget") {
            let label = pt.child("navLabel").map(|l| l.text_collapsed()).filter(|l| !l.is_empty()).or_else(|| pt.attr("value").map(str::to_string));
            let target = pt.child("content").and_then(|c| c.attr("src")).and_then(|s| target_of(dir, s, spine));
            if let (Some(label), Some(target)) = (label, target) {
                pages.push(PageTarget { label, target });
            }
        }
    }
    (prune(toc), pages)
}

/// Drops entries without a target and without children.
fn prune(nodes: Vec<TocNode>) -> Vec<TocNode> {
    nodes
        .into_iter()
        .filter_map(|mut n| {
            n.children = prune(n.children);
            (n.target.is_some() || !n.children.is_empty()).then_some(n)
        })
        .collect()
}

fn nav_list(ol: &Element, dir: &str, spine: &HashMap<String, usize>) -> Vec<TocNode> {
    ol.children_named("li")
        .map(|li| {
            let a = li.child("a").or_else(|| li.child("span"));
            let title = a.map(|a| a.text_collapsed()).unwrap_or_default();
            let target = a.and_then(|a| a.attr("href")).and_then(|h| target_of(dir, h, spine));
            let children = li.child("ol").or_else(|| li.child("ul")).map(|o| nav_list(o, dir, spine)).unwrap_or_default();
            TocNode { title, target, children }
        })
        .collect()
}

fn nav_type(nav: &Element) -> String {
    nav.qattr("epub:type").or_else(|| nav.attr("type")).unwrap_or("").to_string()
}

/// TOC and page list from an EPUB 3 navigation document at `nav_path`.
pub fn parse_nav(xhtml: &str, nav_path: &str, spine: &HashMap<String, usize>) -> (Vec<TocNode>, Vec<PageTarget>) {
    let Ok(root) = xml::parse(xhtml) else { return (Vec::new(), Vec::new()) };
    let dir = dir_of(nav_path);
    let mut navs = Vec::new();
    root.find_all("nav", &mut navs);
    let list = |nav: &Element| nav.child("ol").or_else(|| nav.child("ul")).or_else(|| nav.find("ol")).map(|o| nav_list(o, dir, spine));
    let toc = navs.iter().find(|n| nav_type(n).split_whitespace().any(|t| t == "toc")).and_then(|n| list(n)).unwrap_or_default();
    let mut pages = Vec::new();
    if let Some(pl) = navs.iter().find(|n| nav_type(n).split_whitespace().any(|t| t == "page-list")) {
        fn walk(nodes: Vec<TocNode>, out: &mut Vec<PageTarget>) {
            for n in nodes {
                if let Some(t) = n.target {
                    out.push(PageTarget { label: n.title, target: t });
                }
                walk(n.children, out);
            }
        }
        walk(list(pl).unwrap_or_default(), &mut pages);
    }
    (prune(toc), pages)
}

/// The cover image path: [`Package::cover_image`], else the first image of the guide's
/// cover page (or of the first spine item when it is named like a cover).
pub fn find_cover<R: Read + Seek>(loader: &ZipLoader<R>, pkg: &Package) -> Option<String> {
    // The manifest's own answer when it names an image (property or `<meta name="cover">`).
    let names_image = |c: &String| pkg.item(c).is_some_and(|i| i.is_image()) || pkg.items.iter().any(|i| i.href == *c && i.is_image());
    if pkg.items.iter().any(|i| i.has("cover-image")) || pkg.cover_meta.as_ref().is_some_and(names_image) {
        return pkg.cover_image();
    }
    let page = pkg
        .guide
        .iter()
        .find(|(t, _)| t == "cover" || t == "title-page")
        .map(|(_, h)| resolve_href(pkg.dir(), h))
        .or_else(|| {
            let first = pkg.item_path(pkg.spine.first()?)?;
            first.to_ascii_lowercase().contains("cover").then_some(first)
        });
    let first_image = |page: String| {
        let root = xml::parse(&chapter_text(loader.read_raw(&page).ok()?)).ok()?;
        let src = root.find("img").and_then(|i| i.attr("src")).or_else(|| root.find("image").and_then(|i| i.attr("href")))?;
        Some(resolve_href(dir_of(&page), src))
    };
    page.and_then(first_image).or_else(|| pkg.cover_image())
}

fn open_archive(path: &Path) -> Result<ZipLoader<File>> {
    let file = File::open(path)?;
    let zip = ZipArchive::new(file).map_err(|e| DocError::Format(format!("not a zip: {e}")))?;
    Ok(ZipLoader::new(zip))
}

fn info_of<R: Read + Seek>(loader: &ZipLoader<R>, pkg: &Package, drm: Option<String>) -> BookInfo {
    let mut info = BookInfo::new(Format::Epub);
    info.version = format!("EPUB {}", if pkg.version.is_empty() { "2.0" } else { &pkg.version });
    info.metadata = pkg.metadata.clone();
    info.metadata.cover = find_cover(loader, pkg);
    info.identifiers = pkg.identifiers.clone();
    info.subjects = pkg.subjects.clone();
    info.date = pkg.date.clone();
    info.fonts = pkg.items.iter().filter(|i| i.is_font()).map(|i| i.path.clone()).collect();
    if let Some(scheme) = drm {
        info.refusal = Some(drm_message(&scheme));
        info.drm = Some(scheme);
    }
    info
}

fn cover_of<R: Read + Seek>(loader: &ZipLoader<R>, info: &BookInfo, pkg: &Package) -> Option<Cover> {
    let path = info.metadata.cover.clone()?;
    let bytes = loader.read_max(&path, MAX_COVER).ok()?;
    let media_type = sniff_media_type(&bytes)
        .map(str::to_string)
        .or_else(|| pkg.items.iter().find(|i| i.path == path).map(|i| i.media_type.clone()))?;
    Some(Cover { path, media_type, bytes })
}

/// Package, DRM status and font keys of `path` (`zip`: its archive, when already open).
fn prepare(path: &Path, zip: Option<ZipArchive<File>>) -> Result<(ZipLoader<File>, Package, Option<String>)> {
    let mut loader = zip.map(ZipLoader::new).map_or_else(|| open_archive(path), Ok)?;
    let encrypted = loader.read_raw("META-INF/encryption.xml").map(|b| parse_encryption(&xml::decode_xml_bytes(&b))).unwrap_or_default();
    let pkg = read_package(&loader)?;
    let drm = drm_scheme(&loader, &encrypted);
    set_obfuscation(&mut loader, &pkg, &encrypted);
    Ok((loader, pkg, drm))
}

/// Metadata, identifiers, cover and DRM status, without reading chapters.
pub fn read_info(path: &Path, zip: Option<ZipArchive<File>>) -> Result<BookInfo> {
    let (loader, pkg, drm) = prepare(path, zip)?;
    let mut info = info_of(&loader, &pkg, drm);
    info.cover = cover_of(&loader, &info, &pkg);
    Ok(info)
}

/// The book's TOC (nav for EPUB 3 when it has entries, else NCX) and page list.
pub fn read_toc<R: Read + Seek>(loader: &ZipLoader<R>, pkg: &Package) -> (Vec<TocNode>, Vec<PageTarget>) {
    let spine = spine_index(pkg);
    let mut toc = Vec::new();
    let mut pages = Vec::new();
    if let Some(nav) = pkg.items.iter().find(|i| i.has("nav")) {
        if let Ok(b) = loader.read_raw(&nav.path) {
            (toc, pages) = parse_nav(&chapter_text(b), &nav.path, &spine);
        }
    }
    let ncx = pkg
        .ncx_id
        .as_deref()
        .and_then(|id| pkg.item(id))
        .or_else(|| pkg.items.iter().find(|i| i.media_type == "application/x-dtbncx+xml"));
    if let Some(ncx) = ncx {
        if toc.is_empty() || pages.is_empty() {
            if let Ok(b) = loader.read_raw(&ncx.path) {
                let (t, p) = parse_ncx(&xml::decode_xml_bytes(&b), &ncx.path, &spine);
                if toc.is_empty() {
                    toc = t;
                }
                if pages.is_empty() {
                    pages = p;
                }
            }
        }
    }
    (toc, pages)
}

/// Opens an EPUB file with the book's own TOC and page list, as [`load`] and
/// [`Book::into_document`] do, except that the spine items are read when the engine
/// first needs them: the first page reads its own item (and the heads of the others,
/// for their stylesheets), the background paginator the rest. DRM is refused.
pub fn open_book(path: &Path, zip: Option<ZipArchive<File>>) -> Result<BookDocument> {
    use ink_doc::timing::step;
    let t = std::time::Instant::now();
    let (loader, pkg, drm) = prepare(path, zip)?;
    let t = step("zip+opf", t);
    if let Some(scheme) = drm {
        return Err(DocError::Unsupported(drm_message(&scheme)));
    }
    let mut info = info_of(&loader, &pkg, None);
    info.cover = None;
    let (toc, page_list) = read_toc(&loader, &pkg);
    let t = step("toc+nav read", t);
    let paths = spine_paths(&pkg);
    let spine = Arc::new(LazySpine::new(path, loader, &paths));
    let files = Box::new(SpineFiles(spine.clone()));
    // Image sizes through the same handles on the paginator threads: nothing has to
    // scan every spine item for images first.
    let images: Arc<dyn ResourceLoader + Send + Sync> = Arc::new(SpineFiles(spine.clone()));
    let doc = HtmlDocument::new_lazy(info.metadata.clone(), paths, spine, files, Some(images))?;
    step("styles+fonts", t);
    Ok(BookDocument::new(doc, info, toc, page_list))
}

/// The path of every spine item (empty for an idref the manifest does not have).
fn spine_paths(pkg: &Package) -> Vec<String> {
    pkg.spine.iter().map(|idref| pkg.item_path(idref).unwrap_or_default()).collect()
}

/// The longest head [`LazySpine::read_head`] reads looking for `<body`.
const MAX_HEAD: usize = 256 * 1024;

/// Spine items of an EPUB read on demand, from any thread. Each reader takes an open
/// handle on the file from a pool (opening another when all are in use), so the
/// background paginator and the first page do not wait for each other.
struct LazySpine {
    file: std::path::PathBuf,
    obfuscated: HashMap<String, Obfuscation>,
    /// Path of each spine item (empty: not in the manifest).
    paths: Vec<String>,
    /// Stored (uncompressed) size of each spine item: 0 when it cannot be read.
    sizes: Vec<usize>,
    /// Open handles nobody is using; there is always at least one handle.
    handles: Mutex<Vec<ZipLoader<File>>>,
    /// Signalled when a handle comes back to `handles`.
    returned: std::sync::Condvar,
    /// The inflater [`ZipLoader::read_until_ci`] reuses for every head.
    inflate: Mutex<flate2::Decompress>,
}

impl LazySpine {
    /// The spine items at `paths` of `file`, whose archive `loader` has open.
    fn new(file: &Path, loader: ZipLoader<File>, paths: &[String]) -> Self {
        let sizes = paths.iter().map(|p| if p.is_empty() { 0 } else { loader.stored_size(p).unwrap_or(0) }).collect();
        let obfuscated = loader.obfuscated.clone();
        let (handles, inflate) = (Mutex::new(vec![loader]), Mutex::new(flate2::Decompress::new(false)));
        let returned = std::sync::Condvar::new();
        LazySpine { file: file.to_path_buf(), obfuscated, paths: paths.to_vec(), sizes, handles, returned, inflate }
    }

    /// Runs `f` with a handle on the file: an idle one, else a newly opened one, else
    /// (the file cannot be opened again, e.g. no file descriptors left) the next one
    /// another thread gives back. A spine item is never read as empty for want of a
    /// handle: it would stay empty.
    fn with_handle<T>(&self, f: impl FnOnce(&ZipLoader<File>) -> T) -> T {
        /// Gives the handle back when dropped, also when `f` panics.
        struct Lent<'a>(&'a LazySpine, Option<ZipLoader<File>>);
        impl Drop for Lent<'_> {
            fn drop(&mut self) {
                if let Some(l) = self.1.take() {
                    self.0.handles.lock().unwrap_or_else(|e| e.into_inner()).push(l);
                    self.0.returned.notify_one();
                }
            }
        }
        let lent = Lent(self, Some(self.take_handle()));
        f(lent.1.as_ref().expect("lent handle"))
    }

    fn take_handle(&self) -> ZipLoader<File> {
        if let Some(l) = self.handles.lock().unwrap_or_else(|e| e.into_inner()).pop() {
            return l;
        }
        match open_archive(&self.file) {
            Ok(mut l) => {
                l.obfuscated = self.obfuscated.clone();
                return l;
            }
            Err(e) => log::warn!("epub: another handle on {}: {e}", self.file.display()),
        }
        let mut idle = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(l) = idle.pop() {
                return l;
            }
            idle = self.returned.wait(idle).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl ink_html::book::SpineSource for LazySpine {
    fn read(&self, i: usize) -> String {
        let path = &self.paths[i];
        if path.is_empty() {
            return String::new();
        }
        self.with_handle(|l| l.read(path).map(chapter_text).unwrap_or_default())
    }

    /// The bytes up to the first `<body` (any case), decoded as [`chapter_text`] decodes
    /// the whole item: only for UTF-8, where a prefix that ends at an ASCII byte decodes
    /// to a prefix of the whole text. (An item nested deeper than the XML limit is
    /// rewritten whole by [`chapter_text`]; its own head has the same stylesheets.)
    fn read_head(&self, i: usize) -> Option<String> {
        let path = &self.paths[i];
        if path.is_empty() || self.obfuscated.contains_key(path) || self.sizes[i] as u64 > MAX_ENTRY {
            return None;
        }
        let mut inflate = self.inflate.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = self.with_handle(|l| l.read_until_ci(path, b"<body", MAX_HEAD, &mut inflate))?;
        let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
        let declared = xml::declared_encoding(&bytes).and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()));
        if utf16 || declared.is_some_and(|e| e != encoding_rs::UTF_8) {
            return None;
        }
        Some(text_of(bytes))
    }

    fn len_hint(&self, i: usize) -> Option<usize> {
        Some(self.sizes[i])
    }
}

/// The book's resources (images, stylesheets, fonts) through the handles of a
/// [`LazySpine`].
struct SpineFiles(Arc<LazySpine>);

impl ResourceLoader for SpineFiles {
    fn read(&self, path: &str) -> Result<Vec<u8>> {
        self.0.with_handle(|l| l.read(path))
    }

    fn exists(&self, path: &str) -> bool {
        self.0.with_handle(|l| l.exists(path))
    }
}

/// [`load`], plus the engine's navigation (TOC and page list hrefs) for [`open`] when
/// `with_nav` is set.
fn load_parts(path: &Path, zip: Option<ZipArchive<File>>, with_nav: bool) -> Result<(Book, Option<ink_html::Navigation>)> {
    use ink_doc::timing::step;
    let t = std::time::Instant::now();
    let (loader, pkg, drm) = prepare(path, zip)?;
    let t = step("zip+opf", t);
    if let Some(scheme) = drm {
        return Err(DocError::Unsupported(drm_message(&scheme)));
    }
    let mut info = info_of(&loader, &pkg, None);
    info.cover = None;
    let chapters = read_spine(path, &loader, spine_paths(&pkg));
    if chapters.iter().all(|c| c.html.is_empty()) {
        return Err(DocError::Format("no readable chapters in spine".into()));
    }
    let t = step("spine read", t);
    let (toc, page_list) = read_toc(&loader, &pkg);
    // TOC and page list for the engine itself (NCX / EPUB 3 nav parsed by ink-html,
    // api-request html.md): `open` hands it to HtmlDocument::set_navigation. The book's
    // own TOC above is what `load` callers use, so they skip this second parse.
    let nav = with_nav
        .then(|| loader.read(&pkg.opf_path).map(|b| ink_html::nav::from_opf(&text_of(b), &pkg.opf_path, &loader)).unwrap_or_default());
    step("toc+nav read", t);
    Ok((Book { info, chapters, toc, page_list, resources: Box::new(loader) }, nav))
}

/// Threads that read (inflate and decode) spine items: up to 4, one per core.
fn spine_threads(items: usize) -> usize {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    cores.min(4).min(items / 4).max(1)
}

/// Reads the spine items (`paths`, empty for a missing item) as text. Big spines are
/// inflated on several threads, each with its own handle on the file (the archive's
/// central directory is read again per thread, which costs far less than inflating).
fn read_spine(file: &Path, loader: &ZipLoader<File>, paths: Vec<String>) -> Vec<HtmlChapter> {
    let read_one = |l: &ZipLoader<File>, path: &str| if path.is_empty() { String::new() } else { l.read(path).map(chapter_text).unwrap_or_default() };
    let threads = spine_threads(paths.len());
    if threads <= 1 {
        return paths.into_iter().map(|p| HtmlChapter { html: read_one(loader, &p), path: p }).collect();
    }
    let mut html: Vec<String> = vec![String::new(); paths.len()];
    let per = paths.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let mut slots = html.chunks_mut(per);
        // The first chunk is read here with the loader that is already open.
        let first = slots.next();
        let mut handles = Vec::new();
        for (k, slot) in slots.enumerate() {
            let start = (k + 1) * per;
            let paths = &paths[start..start + slot.len()];
            let obfuscated = loader.obfuscated.clone();
            handles.push(scope.spawn(move || {
                let own = match open_archive(file) {
                    Ok(mut l) => {
                        l.obfuscated = obfuscated;
                        Some(l)
                    }
                    Err(e) => {
                        log::warn!("epub: second handle on {}: {e}", file.display());
                        None
                    }
                };
                if let Some(l) = own {
                    for (out, p) in slot.iter_mut().zip(paths) {
                        *out = read_one(&l, p);
                    }
                }
            }));
        }
        if let Some(slot) = first {
            for (out, p) in slot.iter_mut().zip(&paths) {
                *out = read_one(loader, p);
            }
        }
        for h in handles {
            let _ = h.join();
        }
    });
    // Slots of a thread that could not open the file (or panicked) are still empty:
    // read them here (an empty item is read twice, which costs nothing).
    for (i, p) in paths.iter().enumerate() {
        if html[i].is_empty() && !p.is_empty() {
            html[i] = read_one(loader, p);
        }
    }
    paths.into_iter().zip(html).map(|(path, html)| HtmlChapter { path, html }).collect()
}

/// Converts an EPUB file (`zip`: its archive, when already open). DRM is refused.
pub fn load(path: &Path, zip: Option<ZipArchive<File>>) -> Result<Book> {
    Ok(load_parts(path, zip, false)?.0)
}

/// Opens an EPUB file as the plain engine document, with the engine's own TOC and page
/// list (crengine xpointers resolved by ink-html).
pub fn open(path: &Path, zip: Option<ZipArchive<File>>) -> Result<HtmlDocument> {
    let (book, nav) = load_parts(path, zip, true)?;
    let mut doc = book.into_html_document()?;
    doc.set_navigation(nav.unwrap_or_default());
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opf_metadata_manifest_spine() {
        let opf = r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title> Romeo and Juliet </dc:title><dc:creator>William Shakespeare</dc:creator><dc:language>en</dc:language>
            <meta name="cover" content="img"/></metadata>
            <manifest><item id="c1" href="Text/c%201.xhtml"/><item id="img" href="../cover.jpg" media-type="image/jpeg"/></manifest>
            <spine><itemref idref="c1"/><itemref idref="missing"/></spine></package>"#;
        let p = parse_opf(opf, "OEBPS/content.opf").unwrap();
        assert_eq!(p.metadata.title, "Romeo and Juliet");
        assert_eq!(p.metadata.authors, vec!["William Shakespeare".to_string()]);
        assert_eq!(p.metadata.language.as_deref(), Some("en"));
        assert_eq!(p.spine, vec!["c1", "missing"]);
        assert_eq!(p.item_path("c1").as_deref(), Some("OEBPS/Text/c 1.xhtml"));
        assert_eq!(p.item_path("missing"), None);
        assert_eq!(p.cover_image().as_deref(), Some("cover.jpg"));
    }

    #[test]
    fn epub3_refines_series_identifiers() {
        let opf = r##"<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
            <dc:title id="t2">The Subtitle</dc:title><dc:title id="t1">Main Title</dc:title>
            <meta refines="#t1" property="title-type">main</meta>
            <dc:creator id="c1">Ann Author</dc:creator><meta refines="#c1" property="role" scheme="marc:relators">aut</meta>
            <dc:creator id="c2">Ed Itor</dc:creator><meta refines="#c2" property="role" scheme="marc:relators">edt</meta>
            <dc:creator opf:role="aut">Second Author</dc:creator>
            <dc:identifier id="uid">urn:uuid:12345678-1234-1234-1234-123456789abc</dc:identifier>
            <dc:identifier id="isbn">9780000000002</dc:identifier>
            <dc:identifier opf:scheme="calibre">abc</dc:identifier>
            <dc:description>&lt;p&gt;A &lt;b&gt;bold&lt;/b&gt; book.&lt;/p&gt;</dc:description>
            <dc:subject>Fiction</dc:subject><dc:date>2020-01-02</dc:date>
            <meta property="belongs-to-collection" id="s1">The Saga</meta>
            <meta refines="#s1" property="collection-type">series</meta><meta refines="#s1" property="group-position">3</meta>
            </metadata><manifest><item id="cv" href="img/c.png" media-type="image/png" properties="cover-image"/>
            <item id="f" href="fonts/a.otf" media-type="application/vnd.ms-opentype"/></manifest><spine/></package>"##;
        let p = parse_opf(opf, "OPS/package.opf").unwrap();
        assert_eq!(p.metadata.title, "Main Title");
        assert_eq!(p.metadata.authors, vec!["Ann Author", "Second Author"]);
        assert_eq!(p.metadata.identifier.as_deref(), Some("urn:uuid:12345678-1234-1234-1234-123456789abc"));
        let schemes: Vec<Option<&str>> = p.identifiers.iter().map(|i| i.scheme.as_deref()).collect();
        assert_eq!(schemes, vec![Some("UUID"), Some("ISBN"), Some("calibre")]);
        assert_eq!(p.metadata.description.as_deref(), Some("A bold book."));
        assert_eq!(p.metadata.series, Some(("The Saga".into(), 3.0)));
        assert_eq!(p.subjects, vec!["Fiction"]);
        assert_eq!(p.date.as_deref(), Some("2020-01-02"));
        assert_eq!(p.cover_image().as_deref(), Some("OPS/img/c.png"));
        assert!(p.items[1].is_font());
        assert_eq!(adobe_key(&p).unwrap()[..2], [0x12, 0x34]);
    }

    #[test]
    fn calibre_series_and_ncx_nav() {
        let opf = r#"<package version="2.0"><metadata><meta name="calibre:series" content="Foo"/><meta name="calibre:series_index" content="2.5"/></metadata></package>"#;
        assert_eq!(parse_opf(opf, "content.opf").unwrap().metadata.series, Some(("Foo".into(), 2.5)));
        let spine: HashMap<String, usize> = [("OEBPS/a.xhtml".to_string(), 0), ("OEBPS/b c.xhtml".to_string(), 1)].into();
        let ncx = r#"<ncx><navMap><navPoint><navLabel><text>One</text></navLabel><content src="a.xhtml"/>
            <navPoint><navLabel><text>One.1</text></navLabel><content src="b%20c.xhtml#s1"/></navPoint></navPoint>
            <navPoint><navLabel><text>Gone</text></navLabel><content src="nope.xhtml"/></navPoint></navMap>
            <pageList><pageTarget value="7"><navLabel><text>vii</text></navLabel><content src="b%20c.xhtml#p7"/></pageTarget></pageList></ncx>"#;
        let (toc, pages) = parse_ncx(ncx, "OEBPS/toc.ncx", &spine);
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].children[0].target, Some(Target::id(1, "s1")));
        assert_eq!(pages, vec![PageTarget { label: "vii".into(), target: Target::id(1, "p7") }]);
        let nav = r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol>
            <li><a href="../a.xhtml">One</a><ol><li><a href="../b%20c.xhtml#x">Sub</a></li></ol></li>
            <li><span>Group</span><ol><li><a href="../b%20c.xhtml">Two</a></li></ol></li></ol></nav>
            <nav epub:type="page-list" hidden=""><ol><li><a href="../a.xhtml#p1">1</a></li><li><a href="../b%20c.xhtml#p2">2</a></li></ol></nav></body></html>"#;
        let (toc, pages) = parse_nav(nav, "OEBPS/nav/nav.xhtml", &spine);
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].children[0].target, Some(Target::id(1, "x")));
        assert_eq!(toc[1].target, None);
        assert_eq!(toc[1].children[0].title, "Two");
        assert_eq!(pages.len(), 2);
    }

    #[test]
    fn obfuscation_keys_and_drm() {
        // SHA-1("urn:uuid:x") applied twice is the identity; key derivation strips whitespace.
        assert_eq!(idpf_key(" urn:uuid:x \n"), idpf_key("urn:uuid:x"));
        let mut data: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let orig = data.clone();
        let k = Obfuscation::Idpf(idpf_key("id"));
        k.apply(&mut data);
        assert_ne!(data[..1040], orig[..1040]);
        assert_eq!(data[1040..], orig[1040..]);
        k.apply(&mut data);
        assert_eq!(data, orig);
        let a = Obfuscation::Adobe(uuid_bytes("urn:uuid:00112233-4455-6677-8899-aabbccddeeff").unwrap());
        a.apply(&mut data);
        assert_eq!(data[0], 0x00);
        assert_eq!(data[1], 1 ^ 0x11);
        assert_eq!(data[1024..], orig[1024..]);
        assert!(uuid_bytes("isbn:123").is_none());
        let enc = r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
            <enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
            <enc:CipherData><enc:CipherReference URI="OEBPS/fonts/a%20b.otf"/></enc:CipherData></enc:EncryptedData>
            <enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
            <enc:CipherData><enc:CipherReference URI="OEBPS/ch1.xhtml"/></enc:CipherData></enc:EncryptedData></encryption>"#;
        let e = parse_encryption(enc);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].path, "OEBPS/fonts/a b.otf");
        assert!(drm_message("Adobe ADEPT").contains("DRM"));
    }

    /// Writes an EPUB with `chapters` (path under `OEBPS/`, XHTML) and `files` (path
    /// under `OEBPS/`, bytes) to `dir/name`. Entries named `stored*` are not compressed.
    fn write_epub(dir: &Path, name: &str, chapters: &[(&str, String)], files: &[(&str, &[u8])]) -> std::path::PathBuf {
        use std::io::Write;
        std::fs::create_dir_all(dir).unwrap();
        let f = dir.join(name);
        let mut z = zip::ZipWriter::new(File::create(&f).unwrap());
        let mut add = |path: &str, bytes: &[u8]| {
            let stored = path.rsplit('/').next().is_some_and(|n| n.starts_with("stored"));
            let method = if stored { zip::CompressionMethod::Stored } else { zip::CompressionMethod::Deflated };
            z.start_file(path, zip::write::SimpleFileOptions::default().compression_method(method)).unwrap();
            z.write_all(bytes).unwrap();
        };
        add("mimetype", b"application/epub+zip");
        let container = r#"<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#;
        add("META-INF/container.xml", container.as_bytes());
        let item = |(i, (p, _)): (usize, &(&str, String))| format!(r#"<item id="c{i}" href="{p}" media-type="application/xhtml+xml"/>"#);
        let items: String = chapters.iter().enumerate().map(item).collect();
        let spine: String = (0..chapters.len()).map(|i| format!(r#"<itemref idref="c{i}"/>"#)).collect();
        let opf = format!(
            r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>T</dc:title></metadata><manifest>{items}</manifest><spine>{spine}</spine></package>"#
        );
        add("OEBPS/content.opf", opf.as_bytes());
        for (p, html) in chapters {
            add(&format!("OEBPS/{p}"), html.as_bytes());
        }
        for (p, bytes) in files {
            add(&format!("OEBPS/{p}"), bytes);
        }
        z.finish().unwrap();
        f
    }

    /// Page positions, Kindle locations, TOC and page labels of `doc` once paginated.
    fn summary(doc: &mut BookDocument) -> Vec<String> {
        use ink_doc::Document;
        doc.set_layout(&ink_doc::LayoutSettings::for_screen(600, 800)).unwrap();
        assert!(doc.wait_paginated(std::time::Duration::from_secs(120)));
        let mut out = Vec::new();
        for i in 0..doc.page_count().pages {
            let p = doc.position_of_page(i).unwrap();
            out.push(format!("page {i} {p} {:?}", doc.location(&p)));
        }
        out.extend(doc.toc().iter().map(|e| format!("toc {} {} {} {:?}", e.level, e.title, e.position, e.page)));
        out.extend(doc.page_labels().iter().map(|(l, p)| format!("label {l} {p}")));
        let book = doc.html().book();
        let mut sheets: Vec<&String> = book.sheets.keys().collect();
        sheets.sort();
        out.push(format!("sheets {sheets:?} faces {} fonts {}", book.font_faces.len(), book.fonts.len()));
        out
    }

    #[test]
    fn lazy_open_matches_the_eager_open() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        for name in ["juliet.epub", "synthetic.epub", "formats/epub2-ncx.epub", "formats/epub3-nav.epub"] {
            let path = root.join(name);
            let mut lazy = open_book(&path, None).unwrap();
            // Opening reads the first spine item with content and only the heads of the others
            // (a missing item is "read" as empty).
            let chapters = &lazy.html().book().chapters;
            let first = chapters.iter().position(|c| !c.html().trim().is_empty()).unwrap();
            assert!(chapters.iter().skip(first + 1).all(|c| !c.is_read() || c.html().is_empty()), "{name}: read before needed");
            let mut eager = load(&path, None).unwrap().into_document().unwrap();
            assert_eq!(summary(&mut lazy), summary(&mut eager), "{name}");
        }
    }

    #[test]
    fn spine_heads_are_prefixes_of_the_items() {
        use ink_html::book::SpineSource;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        for name in ["juliet.epub", "synthetic.epub", "formats/epub2-ncx.epub", "formats/epub3-nav.epub"] {
            let path = root.join(name);
            let (loader, pkg, _) = prepare(&path, None).unwrap();
            let paths = spine_paths(&pkg);
            let spine = LazySpine::new(&path, loader, &paths);
            for i in 0..paths.len() {
                let whole = spine.read(i);
                assert_eq!(spine.len_hint(i), Some(whole.len()), "{name} {i}");
                if whole.is_empty() {
                    continue; // not in the archive
                }
                let head = spine.read_head(i).unwrap_or_else(|| panic!("{name} {i}: no head"));
                assert!(whole.starts_with(&head) && head.to_ascii_lowercase().ends_with("<body"), "{name} {i}: {head}");
            }
        }
    }

    #[test]
    fn lazy_heads_give_the_same_stylesheets() {
        let dir = std::env::temp_dir().join(format!("ink-epub-heads-{}", std::process::id()));
        let head = r#"<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><link rel="stylesheet" href="a.css"/><style>@import "b.css"; @font-face { font-family: F; src: url(f.ttf) }</style></head>"#;
        // Nested deeper than the XML limit: the whole item is rewritten, its head read as is.
        let deep = format!("{}<body>{}x{}</body></html>", head.replace("a.css", "e.css"), "<div>".repeat(300), "</div>".repeat(300));
        // A <style> that no </style> closes: the prefix is not enough.
        let open_style = format!("{head}<style>@import \"c.css\";<BODY><p>x</p></BODY></html>");
        let latin1 = r#"<?xml version="1.0" encoding="iso-8859-1"?><html><head><link rel="stylesheet" href="d.css"/></head><body><p>caf"#;
        // Heads longer than one read: deflated and stored.
        let metas: String = (0..400).map(|i| format!(r#"<meta name="m{i}" content="{}"/>"#, i * 7919 % 1000)).collect();
        let long = format!("{}<body><p>Long</p></body></html>", head.replace("<head>", &format!("<head>{metas}")).replace("a.css", "g.css"));
        let chapters = [
            ("one.xhtml", format!("{head}<body><p>One</p></body></html>")),
            ("deep.xhtml", deep),
            ("open.xhtml", open_style),
            ("latin1.xhtml", format!("{latin1}\u{e9}</p></body></html>")),
            ("long.xhtml", long.clone()),
            ("stored.xhtml", long.replace("g.css", "h.css")),
        ];
        let css: &[u8] = b"p { margin: 0 }";
        let files = [("a.css", css), ("b.css", css), ("c.css", css), ("d.css", css), ("e.css", css), ("g.css", css), ("h.css", css), ("f.ttf", b"FONT")];
        let path = write_epub(&dir, "heads.epub", &chapters, &files);
        {
            use ink_html::book::SpineSource;
            let (loader, pkg, _) = prepare(&path, None).unwrap();
            let spine = LazySpine::new(&path, loader, &spine_paths(&pkg));
            // The deep item is rewritten whole, so its raw head is not a prefix of it; the
            // latin1 one is read whole.
            let heads = [Some(true), Some(false), Some(true), None, Some(true), Some(true)];
            for (i, want) in heads.into_iter().enumerate() {
                let head = spine.read_head(i);
                assert_eq!(head.as_ref().map(|h| spine.read(i).starts_with(h.as_str())), want, "item {i}");
            }
        }
        let mut lazy = open_book(&path, None).unwrap();
        let mut eager = load(&path, None).unwrap().into_document().unwrap();
        let (l, e) = (summary(&mut lazy), summary(&mut eager));
        assert_eq!(l, e);
        assert!(l.last().unwrap().contains(r#"["OEBPS/a.css", "OEBPS/b.css", "OEBPS/c.css", "OEBPS/d.css", "OEBPS/e.css", "OEBPS/g.css", "OEBPS/h.css"]"#), "{l:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_reader_without_a_handle_waits_for_one() {
        use ink_html::book::SpineSource;
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/juliet.epub");
        let (loader, pkg, _) = prepare(&path, None).unwrap();
        let paths = spine_paths(&pkg);
        // A path that cannot be opened: the pool has only the handle it was given.
        let spine = LazySpine::new(Path::new("/nonexistent/juliet.epub"), loader, &paths);
        let want = spine.read(1);
        assert!(!want.is_empty());
        std::thread::scope(|s| {
            let reader = spine.with_handle(|_| {
                let h = s.spawn(|| spine.read(1));
                std::thread::sleep(std::time::Duration::from_millis(50));
                assert!(!h.is_finished(), "read without a handle");
                h
            });
            assert_eq!(reader.join().unwrap(), want);
        });
    }

    #[test]
    fn not_a_zip_is_a_format_error() {
        let dir = std::env::temp_dir().join(format!("ink-epub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("bad.epub");
        std::fs::write(&f, b"definitely not a zip").unwrap();
        assert!(matches!(open(&f, None), Err(DocError::Format(_))));
        assert!(matches!(open(&dir.join("missing.epub"), None), Err(DocError::Io(_))));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
