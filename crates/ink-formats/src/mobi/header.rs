//! Record 0: PalmDOC header, MOBI header and EXTH metadata.
//!
//! Offsets follow KindleUnpack's `mobi_header.py`. Record indices in a KF8 header are
//! relative to that header's record (`start`); [`MobiHeader::abs`] adds it.

use crate::util::{be16, be32};
use ink_doc::{DocError, Result};

/// EXTH record types used here.
pub mod exth {
    /// Author (repeatable).
    pub const AUTHOR: u32 = 100;
    /// Publisher.
    pub const PUBLISHER: u32 = 101;
    /// Description.
    pub const DESCRIPTION: u32 = 103;
    /// ISBN.
    pub const ISBN: u32 = 104;
    /// Subject (repeatable).
    pub const SUBJECT: u32 = 105;
    /// Publishing date.
    pub const DATE: u32 = 106;
    /// Contributor.
    pub const CONTRIBUTOR: u32 = 108;
    /// Source.
    pub const SOURCE: u32 = 112;
    /// ASIN.
    pub const ASIN: u32 = 113;
    /// Record index of the KF8 header in a joint MOBI 6 + KF8 file.
    pub const KF8_BOUNDARY: u32 = 121;
    /// KF8 cover URI (`kindle:embed:XXXX`).
    pub const KF8_COVER_URI: u32 = 129;
    /// Cover image: offset from the first resource record.
    pub const COVER_OFFSET: u32 = 201;
    /// Thumbnail: offset from the first resource record.
    pub const THUMB_OFFSET: u32 = 202;
    /// Document type (`EBOK`, `PDOC`, `EBSP` for samples).
    pub const CDE_TYPE: u32 = 501;
    /// Updated title.
    pub const UPDATED_TITLE: u32 = 503;
    /// ASIN (alternative).
    pub const ASIN2: u32 = 504;
    /// Language.
    pub const LANGUAGE: u32 = 524;
}

/// EXTH records in file order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exth {
    /// (type, data).
    pub records: Vec<(u32, Vec<u8>)>,
}

impl Exth {
    /// Parses an EXTH block (starting with `EXTH`).
    pub fn parse(b: &[u8]) -> Exth {
        let mut records = Vec::new();
        if b.get(..4) != Some(b"EXTH") {
            return Exth { records };
        }
        let count = be32(b, 8).unwrap_or(0) as usize;
        let mut pos = 12;
        for _ in 0..count {
            let (Some(t), Some(len)) = (be32(b, pos), be32(b, pos + 4)) else { break };
            let len = len as usize;
            if len < 8 || len > b.len() - pos {
                break;
            }
            records.push((t, b[pos + 8..pos + len].to_vec()));
            pos += len;
        }
        Exth { records }
    }

    /// First record of type `t`.
    pub fn get(&self, t: u32) -> Option<&[u8]> {
        self.records.iter().find(|(k, _)| *k == t).map(|(_, v)| v.as_slice())
    }

    /// All records of type `t`, decoded with `codepage`, trimmed, without empty ones.
    pub fn strings(&self, t: u32, codepage: u32) -> Vec<String> {
        self.records
            .iter()
            .filter(|(k, _)| *k == t)
            .map(|(_, v)| decode(v, codepage).trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// First string of type `t`.
    pub fn string(&self, t: u32, codepage: u32) -> Option<String> {
        self.strings(t, codepage).into_iter().next()
    }

    /// Record `t` as a big-endian u32.
    pub fn u32(&self, t: u32) -> Option<u32> {
        let v = self.get(t)?;
        match v.len() {
            4.. => be32(v, 0),
            2 => be16(v, 0).map(u32::from),
            1 => Some(v[0] as u32),
            _ => None,
        }
    }
}

/// Decodes MOBI text in `codepage` (65001 = UTF-8, 1252 = Windows Latin 1, ...).
pub fn decode(b: &[u8], codepage: u32) -> String {
    match codepage {
        65001 => String::from_utf8_lossy(b).into_owned(),
        cp => {
            let enc = encoding_rs::Encoding::for_label(format!("windows-{cp}").as_bytes()).unwrap_or(encoding_rs::WINDOWS_1252);
            enc.decode_without_bom_handling(b).0.into_owned()
        }
    }
}

/// Value meaning "no record".
pub const NULL: u32 = 0xFFFF_FFFF;

/// The headers of record 0 (or of the KF8 header record in a joint file).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MobiHeader {
    /// Record index of this header (0, or the KF8 header in a joint file).
    pub start: usize,
    /// 1 = none, 2 = PalmDOC, 17480 = HUFF/CDIC.
    pub compression: u16,
    /// Uncompressed text length.
    pub text_length: u32,
    /// Number of text records.
    pub text_records: u16,
    /// Text record size (4096).
    pub record_size: u16,
    /// 0 = none, 1 = old Mobipocket, 2 = Mobipocket/Kindle DRM.
    pub encryption: u16,
    /// True when the `MOBI` header is present (false for plain PalmDOC).
    pub has_mobi: bool,
    /// MOBI header length.
    pub header_length: u32,
    /// MOBI type (2 = book, 3 = PalmDOC, 257 = news, ...).
    pub mobi_type: u32,
    /// Text encoding (1252 or 65001).
    pub codepage: u32,
    /// File version (6 for MOBI 6, 8 for KF8).
    pub version: u32,
    /// First record after the text (relative).
    pub first_non_book: u32,
    /// Full name (title).
    pub full_name: String,
    /// Locale code.
    pub locale: u32,
    /// First resource (image) record (relative).
    pub first_resource: u32,
    /// HUFF record (relative).
    pub huff_offset: u32,
    /// HUFF + CDIC record count.
    pub huff_count: u32,
    /// EXTH flags (0x40 = EXTH present).
    pub exth_flags: u32,
    /// Trailing entry flags of text records.
    pub extra_flags: u16,
    /// NCX index record (relative).
    pub ncx_index: u32,
    /// KF8: FDST record (relative).
    pub fdst_index: u32,
    /// KF8: fragment index record (relative).
    pub frag_index: u32,
    /// KF8: skeleton index record (relative).
    pub skel_index: u32,
    /// KF8: guide index record (relative).
    pub guide_index: u32,
    /// EXTH records.
    pub exth: Exth,
}

impl MobiHeader {
    /// Parses record `rec` found at record index `start`.
    pub fn parse(rec: &[u8], start: usize) -> Result<Self> {
        if rec.len() < 16 {
            return Err(DocError::Format("MOBI: record 0 too short".into()));
        }
        let u32_at = |o: usize| be32(rec, o).unwrap_or(NULL);
        let mut h = MobiHeader {
            start,
            compression: be16(rec, 0).unwrap_or(1),
            text_length: be32(rec, 4).unwrap_or(0),
            text_records: be16(rec, 8).unwrap_or(0),
            record_size: be16(rec, 10).unwrap_or(4096),
            encryption: be16(rec, 12).unwrap_or(0),
            codepage: 1252,
            first_non_book: NULL,
            first_resource: NULL,
            huff_offset: NULL,
            ncx_index: NULL,
            fdst_index: NULL,
            frag_index: NULL,
            skel_index: NULL,
            guide_index: NULL,
            ..Default::default()
        };
        if rec.get(16..20) != Some(b"MOBI") {
            return Ok(h);
        }
        h.has_mobi = true;
        h.header_length = u32_at(0x14);
        h.mobi_type = u32_at(0x18);
        h.codepage = u32_at(0x1C);
        h.version = u32_at(0x24);
        h.first_non_book = u32_at(0x50);
        let name_off = u32_at(0x54) as usize;
        let name_len = u32_at(0x58) as usize;
        h.locale = u32_at(0x5C);
        h.first_resource = u32_at(0x6C);
        h.huff_offset = u32_at(0x70);
        h.huff_count = u32_at(0x74);
        h.exth_flags = u32_at(0x80);
        let hl = h.header_length as usize;
        // Fields exist only when the header is long enough.
        let field = |o: usize| if o + 4 <= hl.saturating_add(0x10) { u32_at(o) } else { NULL };
        if hl >= 0xE4 {
            h.extra_flags = be16(rec, 0xF2).unwrap_or(0);
        }
        h.ncx_index = field(0xF4);
        if h.version >= 8 {
            h.fdst_index = field(0xC0);
            h.frag_index = field(0xF8);
            h.skel_index = field(0xFC);
            h.guide_index = field(0x104);
        }
        if h.exth_flags & 0x40 != 0 {
            if let Some(b) = rec.get(hl.saturating_add(0x10)..) {
                h.exth = Exth::parse(b);
            }
        }
        if let Some(n) = rec.get(name_off..name_off.saturating_add(name_len)) {
            h.full_name = decode(n, h.codepage).trim().to_string();
        }
        Ok(h)
    }

    /// Absolute record index of a relative one (`None` for the null value).
    pub fn abs(&self, rel: u32) -> Option<usize> {
        (rel != NULL).then(|| self.start.checked_add(rel as usize)).flatten()
    }

    /// True for a KF8 (AZW3) header.
    pub fn is_kf8(&self) -> bool {
        self.version >= 8
    }

    /// The title: EXTH 503 (updated title), else the full name.
    pub fn title(&self) -> String {
        self.exth.string(exth::UPDATED_TITLE, self.codepage).unwrap_or_else(|| self.full_name.clone())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds an EXTH block.
    pub fn exth_block(records: &[(u32, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        for (t, d) in records {
            body.extend_from_slice(&t.to_be_bytes());
            body.extend_from_slice(&((d.len() + 8) as u32).to_be_bytes());
            body.extend_from_slice(d);
        }
        let mut out = b"EXTH".to_vec();
        out.extend_from_slice(&((body.len() + 12) as u32).to_be_bytes());
        out.extend_from_slice(&(records.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out
    }

    /// Record 0 with a MOBI header of length 0x108 and `fields` (offset, value) set.
    pub fn record0(compression: u16, text_len: u32, text_records: u16, encryption: u16, name: &str, exth: &[(u32, &[u8])], fields: &[(usize, u32)]) -> Vec<u8> {
        let mut r = vec![0u8; 0x10 + 0x108];
        r[0..2].copy_from_slice(&compression.to_be_bytes());
        r[4..8].copy_from_slice(&text_len.to_be_bytes());
        r[8..10].copy_from_slice(&text_records.to_be_bytes());
        r[10..12].copy_from_slice(&4096u16.to_be_bytes());
        r[12..14].copy_from_slice(&encryption.to_be_bytes());
        r[16..20].copy_from_slice(b"MOBI");
        let set = |r: &mut Vec<u8>, o: usize, v: u32| r[o..o + 4].copy_from_slice(&v.to_be_bytes());
        set(&mut r, 0x14, 0x108);
        set(&mut r, 0x18, 2);
        set(&mut r, 0x1C, 65001);
        set(&mut r, 0x24, 6);
        for o in [0x28, 0x2C, 0x50, 0x6C, 0x70, 0xC0, 0xF4, 0xF8, 0xFC, 0x104] {
            set(&mut r, o, NULL);
        }
        set(&mut r, 0x74, 0);
        set(&mut r, 0x80, 0x40);
        for (o, v) in fields {
            set(&mut r, *o, *v);
        }
        let ex = exth_block(exth);
        let name_off = r.len() + ex.len();
        set(&mut r, 0x54, name_off as u32);
        set(&mut r, 0x58, name.len() as u32);
        r.extend_from_slice(&ex);
        r.extend_from_slice(name.as_bytes());
        r.extend_from_slice(&[0, 0]);
        r
    }

    #[test]
    fn header_and_exth() {
        let rec = record0(2, 1000, 1, 0, "Full Name", &[(100, b"Ann"), (100, b"Bob"), (201, &3u32.to_be_bytes()), (503, b"Better Title")], &[(0x6C, 5), (0xF0, 3)]);
        let h = MobiHeader::parse(&rec, 0).unwrap();
        assert!(h.has_mobi);
        assert_eq!(h.compression, 2);
        assert_eq!(h.text_length, 1000);
        assert_eq!(h.full_name, "Full Name");
        assert_eq!(h.title(), "Better Title");
        assert_eq!(h.exth.strings(exth::AUTHOR, 65001), vec!["Ann", "Bob"]);
        assert_eq!(h.exth.u32(exth::COVER_OFFSET), Some(3));
        assert_eq!(h.abs(h.first_resource), Some(5));
        assert_eq!(h.abs(h.ncx_index), None);
        assert_eq!(h.extra_flags, 3);
        assert!(!h.is_kf8());
        assert_eq!(decode(b"\xe6\xf8\xe5", 1252), "æøå");
    }
}
