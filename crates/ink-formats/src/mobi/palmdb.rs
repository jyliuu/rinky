//! Palm database container: 78-byte header, then 8-byte record entries (offset, attributes,
//! unique id). Record `i` runs from its offset to the next record's offset.

use crate::util::{be16, be32};
use ink_doc::{DocError, Result};
use std::sync::Arc;

/// A parsed Palm database (MOBI, AZW, AZW3, PRC, PalmDOC).
#[derive(Clone, Debug)]
pub struct PalmDb {
    /// The whole file.
    pub data: Arc<Vec<u8>>,
    /// Database name (32 bytes, NUL-terminated).
    pub name: String,
    /// Type and creator, e.g. `BOOKMOBI` or `TEXtREAd`.
    pub type_creator: [u8; 8],
    /// Start offset of each record.
    pub offsets: Vec<usize>,
}

impl PalmDb {
    /// Parses the header and record list.
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let bad = |m: &str| DocError::Format(format!("PalmDB: {m}"));
        if data.len() < 78 {
            return Err(bad("file too short"));
        }
        let name_end = data[..32].iter().position(|b| *b == 0).unwrap_or(32);
        let name = String::from_utf8_lossy(&data[..name_end]).into_owned();
        let mut type_creator = [0u8; 8];
        type_creator.copy_from_slice(&data[60..68]);
        let n = be16(&data, 76).unwrap_or(0) as usize;
        if 78 + n * 8 > data.len() {
            return Err(bad("record list past the end of the file"));
        }
        let mut offsets = Vec::with_capacity(n);
        for i in 0..n {
            let off = be32(&data, 78 + i * 8).unwrap_or(0) as usize;
            offsets.push(off);
        }
        // Offsets must be inside the file and not decrease.
        let mut last = 78 + n * 8;
        for o in &mut offsets {
            if *o < last || *o > data.len() {
                *o = last.min(data.len());
            }
            last = *o;
        }
        Ok(PalmDb { data: Arc::new(data), name, type_creator, offsets })
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// True when there are no records.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Bytes of record `i`.
    pub fn record(&self, i: usize) -> Option<&[u8]> {
        let start = *self.offsets.get(i)?;
        let end = self.offsets.get(i + 1).copied().unwrap_or(self.data.len());
        self.data.get(start..end.max(start))
    }

    /// The type/creator as text.
    pub fn kind(&self) -> String {
        String::from_utf8_lossy(&self.type_creator).into_owned()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a Palm database from records.
    pub fn build(name: &str, tc: &[u8; 8], records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0u8; 78];
        out[..name.len().min(31)].copy_from_slice(&name.as_bytes()[..name.len().min(31)]);
        out[60..68].copy_from_slice(tc);
        out[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
        let mut off = 78 + records.len() * 8 + 2;
        for (i, r) in records.iter().enumerate() {
            out.extend_from_slice(&(off as u32).to_be_bytes());
            out.extend_from_slice(&((2 * i) as u32).to_be_bytes());
            off += r.len();
        }
        out.extend_from_slice(&[0, 0]);
        for r in records {
            out.extend_from_slice(r);
        }
        out
    }

    #[test]
    fn records() {
        let db = PalmDb::parse(build("Book", b"BOOKMOBI", &[b"zero".to_vec(), b"one!!".to_vec()])).unwrap();
        assert_eq!(db.name, "Book");
        assert_eq!(db.kind(), "BOOKMOBI");
        assert_eq!(db.len(), 2);
        assert_eq!(db.record(0), Some(&b"zero"[..]));
        assert_eq!(db.record(1), Some(&b"one!!"[..]));
        assert_eq!(db.record(2), None);
        assert!(PalmDb::parse(vec![0; 10]).is_err());
    }
}
