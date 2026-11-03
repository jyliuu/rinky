//! INDX records (NCX, KF8 skeleton, fragment and guide tables) with their TAGX tag
//! table and CNCX string records. Follows KindleUnpack's `mobi_index.py`.

use super::palmdb::PalmDb;
use crate::util::{be16, be32};
use std::collections::HashMap;

/// One index entry: its name (key) and tag values.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// Entry name as bytes (a number in ASCII for KF8 tables).
    pub name: Vec<u8>,
    /// Tag number -> values.
    pub tags: HashMap<u8, Vec<u32>>,
}

impl Entry {
    /// First value of `tag`.
    pub fn tag(&self, tag: u8) -> Option<u32> {
        self.tags.get(&tag)?.first().copied()
    }

    /// Value `i` of `tag`.
    pub fn tag_at(&self, tag: u8, i: usize) -> Option<u32> {
        self.tags.get(&tag)?.get(i).copied()
    }

    /// The name as text.
    pub fn name_str(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }
}

/// A whole index: entries in order and the CNCX strings by offset.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Index {
    /// Entries of all index data records.
    pub entries: Vec<Entry>,
    /// CNCX strings: offset (with record number << 16) -> bytes.
    pub cncx: HashMap<u32, Vec<u8>>,
}

/// Forward variable-width integer: 7 bits per byte, the last byte has the high bit set.
pub fn varint(data: &[u8], pos: usize) -> (usize, u32) {
    let mut value: u32 = 0;
    let mut consumed = 0;
    while let Some(&b) = data.get(pos.saturating_add(consumed)) {
        consumed += 1;
        value = (value << 7) | (b & 0x7F) as u32;
        if b & 0x80 != 0 || consumed >= 5 {
            break;
        }
    }
    (consumed.max(1), value)
}

struct Header {
    len: usize,
    start: usize,
    count: usize,
    ncncx: usize,
    ordt2: Option<Vec<u16>>,
}

fn header(data: &[u8]) -> Option<Header> {
    if data.get(..4) != Some(b"INDX") {
        return None;
    }
    let w = |i: usize| be32(data, 4 + 4 * i).unwrap_or(0) as usize;
    let mut h = Header { len: w(0), start: w(4), count: w(5), ncncx: w(12), ordt2: None };
    let code = w(6);
    let ocnt = be32(data, 0xA4).unwrap_or(0);
    let oentries = be32(data, 0xA8).unwrap_or(0) as usize;
    let op2 = be32(data, 0xB0).unwrap_or(0) as usize;
    if (code == 0xFDEA || ocnt != 0 || oentries > 0) && data.get(op2..op2.saturating_add(4)) == Some(b"ORDT") && oentries < 0x10000 {
        h.ordt2 = Some((0..oentries).map(|i| be16(data, op2.saturating_add(4 + 2 * i)).unwrap_or(0)).collect());
    }
    Some(h)
}

/// TAGX table: (tag, values per entry, mask, end flag), and the control byte count.
fn tagx(data: &[u8], start: usize) -> (usize, Vec<(u8, u8, u8, u8)>) {
    if data.get(start..start.saturating_add(4)) != Some(b"TAGX") {
        return (0, Vec::new());
    }
    let first_entry = be32(data, start + 4).unwrap_or(12) as usize;
    let cbc = be32(data, start + 8).unwrap_or(0) as usize;
    let mut tags = Vec::new();
    let mut i = 12;
    // start + 4 <= data.len() here, so start + i + 4 cannot overflow.
    while i + 4 <= first_entry && start + i + 4 <= data.len() {
        let p = start + i;
        tags.push((data[p], data[p + 1], data[p + 2], data[p + 3]));
        i += 4;
    }
    (cbc, tags)
}

fn tag_map(cbc: usize, table: &[(u8, u8, u8, u8)], data: &[u8], start: usize, end: usize) -> HashMap<u8, Vec<u32>> {
    let mut out = HashMap::new();
    let mut cb_index = 0;
    let mut pos = start.saturating_add(cbc);
    // (tag, value count, byte count, values per entry)
    let mut plan: Vec<(u8, Option<u32>, Option<u32>, u8)> = Vec::new();
    for &(tag, per_entry, mask, end_flag) in table {
        if end_flag == 1 {
            cb_index += 1;
            continue;
        }
        let Some(&cbyte) = data.get(start.saturating_add(cb_index)) else { break };
        let value = cbyte & mask;
        if value == 0 {
            continue;
        }
        if value == mask {
            if mask.count_ones() > 1 {
                let (n, v) = varint(data, pos);
                pos = pos.saturating_add(n);
                plan.push((tag, None, Some(v), per_entry));
            } else {
                plan.push((tag, Some(1), None, per_entry));
            }
        } else {
            let mut m = mask;
            let mut v = value;
            while m & 1 == 0 {
                m >>= 1;
                v >>= 1;
            }
            plan.push((tag, Some(v as u32), None, per_entry));
        }
    }
    for (tag, count, bytes, per_entry) in plan {
        let mut values = Vec::new();
        match (count, bytes) {
            (Some(c), _) => {
                for _ in 0..c * per_entry as u32 {
                    if pos >= end {
                        break;
                    }
                    let (n, v) = varint(data, pos);
                    pos = pos.saturating_add(n);
                    values.push(v);
                }
            }
            (None, Some(b)) => {
                let mut consumed = 0;
                while consumed < b as usize && pos < end {
                    let (n, v) = varint(data, pos);
                    pos = pos.saturating_add(n);
                    consumed += n;
                    values.push(v);
                }
            }
            _ => {}
        }
        out.insert(tag, values);
    }
    out
}

fn cncx_strings(data: &[u8], base: u32, out: &mut HashMap<u32, Vec<u8>>) {
    let mut off = 0;
    while off < data.len() {
        if data[off] == 0 {
            break;
        }
        let (n, len) = varint(data, off);
        let end = (off + n).saturating_add(len as usize).min(data.len());
        let s = data.get(off + n..end).unwrap_or(&[]).to_vec();
        out.insert(base.wrapping_add(off as u32), s);
        off = end.max(off + n);
    }
}

/// Reads the index whose primary INDX record is `rec`.
pub fn read(db: &PalmDb, rec: usize) -> Index {
    let mut idx = Index::default();
    let Some(data) = db.record(rec) else { return idx };
    let Some(h) = header(data) else { return idx };
    let (cbc, table) = tagx(data, h.len);
    // Counts come from the file: bounded by the records and bytes that exist.
    let count = h.count.min(db.len().saturating_sub(rec + 1));
    let ncncx = h.ncncx.min(db.len().saturating_sub(rec + 1 + count));
    for j in 0..ncncx {
        if let Some(c) = db.record(rec + count + 1 + j) {
            cncx_strings(c, (j as u32) << 16, &mut idx.cncx);
        }
    }
    for i in rec + 1..rec + 1 + count {
        let Some(data) = db.record(i) else { break };
        let Some(hi) = header(data) else { continue };
        let ordt2 = hi.ordt2.as_ref().or(h.ordt2.as_ref());
        // IDXT: "IDXT" then one 2-byte offset per entry, inside this record.
        let n = hi.count.min(data.len().saturating_sub(hi.start.saturating_add(4)) / 2);
        let mut positions: Vec<usize> = (0..n).map(|j| be16(data, hi.start + 4 + 2 * j).unwrap_or(0) as usize).collect();
        positions.push(hi.start);
        for j in 0..n {
            let (start, end) = (positions[j], positions[j + 1]);
            let Some(&len) = data.get(start) else { continue };
            let raw = data.get(start + 1..start + 1 + len as usize).unwrap_or(&[]);
            let name = match ordt2 {
                Some(o) => raw.iter().map(|b| o.get(*b as usize).copied().unwrap_or(*b as u16) as u8).collect(),
                None => raw.to_vec(),
            };
            let tags = tag_map(cbc, &table, data, start + 1 + len as usize, end.min(data.len()));
            idx.entries.push(Entry { name, tags });
        }
    }
    idx
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes a forward varint.
    pub fn enc(mut v: u32) -> Vec<u8> {
        let mut out = vec![(v & 0x7F) as u8 | 0x80];
        v >>= 7;
        while v > 0 {
            out.insert(0, (v & 0x7F) as u8);
            v >>= 7;
        }
        out
    }

    fn indx(len_hdr: usize, start: usize, count: usize, ncncx: usize, extra: &[u8]) -> Vec<u8> {
        let mut d = b"INDX".to_vec();
        let mut words = vec![0u32; 45];
        words[0] = len_hdr as u32;
        words[4] = start as u32;
        words[5] = count as u32;
        words[12] = ncncx as u32;
        for w in words {
            d.extend_from_slice(&w.to_be_bytes());
        }
        d.resize(len_hdr, 0);
        d.extend_from_slice(extra);
        d
    }

    /// Builds an index (primary record, one data record, one CNCX record) with entries
    /// of tag 1 (one value) and tag 3 (one value), like an NCX.
    pub fn build(entries: &[(&str, u32, u32)], cncx: &[&str]) -> Vec<Vec<u8>> {
        let mut tag = b"TAGX".to_vec();
        tag.extend_from_slice(&20u32.to_be_bytes());
        tag.extend_from_slice(&1u32.to_be_bytes());
        tag.extend_from_slice(&[1, 1, 0x01, 0, 3, 1, 0x02, 0]);
        tag.extend_from_slice(&[0, 0, 0, 1]);
        let primary = indx(0xC0, 0, 1, 1, &tag);
        let mut body = Vec::new();
        let mut pos = Vec::new();
        for (name, a, b) in entries {
            pos.push(0xC0 + body.len());
            body.push(name.len() as u8);
            body.extend_from_slice(name.as_bytes());
            body.push(0x03);
            body.extend(enc(*a));
            body.extend(enc(*b));
        }
        let idxt_at = 0xC0 + body.len();
        let mut idxt = b"IDXT".to_vec();
        for p in pos {
            idxt.extend_from_slice(&(p as u16).to_be_bytes());
        }
        let mut data = indx(0xC0, idxt_at, entries.len(), 0, &body);
        data.extend_from_slice(&idxt);
        let mut c = Vec::new();
        for s in cncx {
            c.extend(enc(s.len() as u32));
            c.extend_from_slice(s.as_bytes());
        }
        vec![primary, data, c]
    }

    #[test]
    fn varints() {
        assert_eq!(varint(&[0x81], 0), (1, 1));
        assert_eq!(varint(&[0x01, 0x80], 0), (2, 128));
        assert_eq!(enc(300), vec![0x02, 0xAC]);
        assert_eq!(varint(&enc(300), 0), (2, 300));
    }

    #[test]
    fn reads_entries_tags_and_cncx() {
        let recs = build(&[("000", 10, 0), ("001", 99, 6)], &["Intro", "Next"]);
        let db = super::super::palmdb::PalmDb::parse(super::super::palmdb::tests::build("x", b"BOOKMOBI", &recs)).unwrap();
        let idx = read(&db, 0);
        assert_eq!(idx.entries.len(), 2);
        assert_eq!(idx.entries[1].name_str(), "001");
        assert_eq!(idx.entries[1].tag(1), Some(99));
        assert_eq!(idx.entries[1].tag(3), Some(6));
        assert_eq!(idx.cncx.get(&0).map(|v| v.as_slice()), Some(&b"Intro"[..]));
        assert_eq!(idx.cncx.get(&6).map(|v| v.as_slice()), Some(&b"Next"[..]));
    }
}
