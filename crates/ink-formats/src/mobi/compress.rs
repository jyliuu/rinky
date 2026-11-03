//! MOBI text record decompression: PalmDOC LZ77 (compression 2) and HUFF/CDIC
//! (compression 17480), plus the trailing entries that follow each text record.

use crate::util::be32;
use ink_doc::{DocError, Result};

/// Decompresses one PalmDOC (LZ77) record.
pub fn palmdoc(src: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(src.len() * 2);
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        i += 1;
        match c {
            1..=8 => {
                let end = (i + c as usize).min(src.len());
                out.extend_from_slice(&src[i..end]);
                i = end;
            }
            0 | 9..=0x7F => out.push(c),
            0xC0..=0xFF => {
                out.push(b' ');
                out.push(c ^ 0x80);
            }
            0x80..=0xBF => {
                if i >= src.len() {
                    break;
                }
                let pair = ((c as u16) << 8 | src[i] as u16) & 0x3FFF;
                i += 1;
                let dist = (pair >> 3) as usize;
                let len = (pair & 7) as usize + 3;
                if dist == 0 || dist > out.len() {
                    continue;
                }
                let start = out.len() - dist;
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
            }
        }
    }
    out
}

/// Compresses with PalmDOC literals only (for tests and fixtures).
pub fn palmdoc_literal(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in src.chunks(8) {
        if chunk.iter().all(|b| matches!(b, 0 | 9..=0x7F)) {
            out.extend_from_slice(chunk);
        } else {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
    }
    out
}

/// Size of the trailing entries at the end of a text record (MOBI header extra flags).
pub fn trailing_size(data: &[u8], extra_flags: u16) -> usize {
    let mut num = 0usize;
    let size = data.len();
    let mut flags = extra_flags >> 1;
    while flags != 0 {
        if flags & 1 != 0 && num < size {
            // Backward-encoded variable-width integer at the end of what is left.
            let mut psize = size - num;
            let mut bitpos = 0;
            let mut result = 0usize;
            loop {
                let v = data[psize - 1];
                result |= ((v & 0x7F) as usize) << bitpos;
                bitpos += 7;
                psize -= 1;
                if v & 0x80 != 0 || bitpos >= 28 || psize == 0 {
                    break;
                }
            }
            num += result;
        }
        flags >>= 1;
    }
    if extra_flags & 1 != 0 && num < size {
        num += (data[size - num - 1] & 0x3) as usize + 1;
    }
    num.min(size)
}

#[derive(Clone, Copy, Debug, Default)]
struct Code {
    len: u32,
    term: bool,
    max: u64,
}

/// Most bytes one text record or one dictionary phrase may expand to (text records are
/// 4096 bytes; a phrase expands to a few hundred at most).
pub const MAX_RECORD_OUT: usize = 1 << 20;

/// Most bytes all expanded dictionary phrases of one book may hold together.
pub const MAX_PHRASE_BYTES: usize = 64 << 20;

/// HUFF/CDIC decompressor (dictionary compression used by some Mobipocket books).
#[derive(Clone, Debug)]
pub struct HuffCdic {
    dict1: Vec<Code>,
    mincode: Vec<u64>,
    maxcode: Vec<u64>,
    dict: Vec<Option<(Vec<u8>, bool)>>,
    /// Bytes of phrases expanded so far (bounded by [`MAX_PHRASE_BYTES`]).
    expanded: usize,
}

impl HuffCdic {
    /// Builds the decoder from the HUFF record and the CDIC records that follow it.
    pub fn new(huff: &[u8], cdics: &[&[u8]]) -> Result<Self> {
        let bad = |m: &str| DocError::Format(format!("HUFF/CDIC: {m}"));
        if huff.get(..8) != Some(b"HUFF\x00\x00\x00\x18") {
            return Err(bad("bad HUFF header"));
        }
        let off1 = be32(huff, 8).ok_or_else(|| bad("short HUFF"))? as usize;
        let off2 = be32(huff, 12).ok_or_else(|| bad("short HUFF"))? as usize;
        let mut dict1 = Vec::with_capacity(256);
        for i in 0..256 {
            let v = be32(huff, off1 + 4 * i).ok_or_else(|| bad("short dict1"))?;
            let len = v & 0x1F;
            let term = v & 0x80 != 0;
            if len == 0 {
                return Err(bad("zero code length"));
            }
            let max = (((v >> 8) as u64 + 1) << (32 - len)) - 1;
            dict1.push(Code { len, term, max });
        }
        let mut mincode = vec![0u64; 33];
        let mut maxcode = vec![u64::MAX; 33];
        for len in 1..=32usize {
            let lo = be32(huff, off2 + 8 * (len - 1)).ok_or_else(|| bad("short dict2"))? as u64;
            let hi = be32(huff, off2 + 8 * (len - 1) + 4).ok_or_else(|| bad("short dict2"))? as u64;
            mincode[len] = lo << (32 - len);
            maxcode[len] = ((hi + 1) << (32 - len)).wrapping_sub(1);
        }
        let mut dict = Vec::new();
        for c in cdics {
            if c.get(..8) != Some(b"CDIC\x00\x00\x00\x10") {
                return Err(bad("bad CDIC header"));
            }
            let phrases = be32(c, 8).ok_or_else(|| bad("short CDIC"))? as usize;
            let bits = be32(c, 12).ok_or_else(|| bad("short CDIC"))?.min(31);
            // The phrase offsets (2 bytes each) must be in the record.
            let n = (1usize << bits).min(phrases.saturating_sub(dict.len())).min(c.len().saturating_sub(16) / 2);
            for k in 0..n {
                let off = u16::from_be_bytes([c.get(16 + 2 * k).copied().unwrap_or(0), c.get(17 + 2 * k).copied().unwrap_or(0)]) as usize;
                let at = 16 + off;
                let blen = u16::from_be_bytes([c.get(at).copied().unwrap_or(0), c.get(at + 1).copied().unwrap_or(0)]) as usize;
                let len = blen & 0x7FFF;
                let slice = c.get(at + 2..(at + 2 + len).min(c.len())).unwrap_or(&[]).to_vec();
                dict.push(Some((slice, blen & 0x8000 != 0)));
            }
        }
        Ok(HuffCdic { dict1, mincode, maxcode, dict, expanded: 0 })
    }

    /// Decompresses one text record.
    pub fn unpack(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        self.unpack_depth(data, 0)
    }

    fn unpack_depth(&mut self, data: &[u8], depth: u32) -> Result<Vec<u8>> {
        if depth > 32 {
            return Err(DocError::Format("HUFF/CDIC: dictionary recursion too deep".into()));
        }
        let mut buf = data.to_vec();
        buf.extend_from_slice(&[0; 8]);
        let word = |pos: usize| u64::from_be_bytes(buf[pos..pos + 8].try_into().expect("8 bytes"));
        let mut bitsleft = data.len() as i64 * 8;
        let mut pos = 0usize;
        let mut x = word(pos);
        let mut n: i64 = 32;
        let mut out = Vec::new();
        loop {
            if n <= 0 {
                pos += 4;
                if pos + 8 > buf.len() {
                    break;
                }
                x = word(pos);
                n += 32;
            }
            let code = (x >> n) & 0xFFFF_FFFF;
            let mut c = self.dict1[(code >> 24) as usize];
            if !c.term {
                while (c.len as usize) < 32 && code < self.mincode[c.len as usize] {
                    c.len += 1;
                }
                c.max = self.maxcode[c.len as usize];
            }
            n -= c.len as i64;
            bitsleft -= c.len as i64;
            if bitsleft < 0 {
                break;
            }
            let r = (c.max.wrapping_sub(code) >> (32 - c.len)) as usize;
            let entry = self.dict.get_mut(r).ok_or_else(|| DocError::Format(format!("HUFF/CDIC: phrase {r} out of range")))?.take();
            let (slice, done) = entry.ok_or_else(|| DocError::Format("HUFF/CDIC: recursive phrase".into()))?;
            let slice = if done {
                slice
            } else {
                let s = self.unpack_depth(&slice, depth + 1)?;
                self.expanded = self.expanded.saturating_add(s.len());
                if self.expanded > MAX_PHRASE_BYTES {
                    return Err(DocError::Format("HUFF/CDIC: dictionary expands to more than 64 MiB".into()));
                }
                s
            };
            if out.len() + slice.len() > MAX_RECORD_OUT {
                return Err(DocError::Format("HUFF/CDIC: a record expands to more than 1 MiB".into()));
            }
            out.extend_from_slice(&slice);
            self.dict[r] = Some((slice, true));
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn palmdoc_roundtrip_and_backrefs() {
        // Literal run, space+char pair, and a back reference (distance 6, length 4).
        let mut src = vec![b'a', b'b', b'c', 0xC0 ^ 0x80 ^ b'x' ^ 0x80];
        src[3] = b'x' ^ 0x80; // 0xF8: space then 'x'
        assert_eq!(palmdoc(&src), b"abc x");
        let dist = 5u16;
        let len = 4u16;
        let pair = 0x8000 | (dist << 3) | (len - 3);
        let src = [b'h', b'e', b'l', b'l', b'o', (pair >> 8) as u8, pair as u8];
        assert_eq!(palmdoc(&src), b"hellohell");
        assert_eq!(palmdoc(&[3, 0xE9, 0xF0, 0x80]), vec![0xE9, 0xF0, 0x80]);
        let text = "Ærøskøbing – a test of literal runs with UTF-8 bytes.".as_bytes();
        assert_eq!(palmdoc(&palmdoc_literal(text)), text);
    }

    #[test]
    fn trailing_entries() {
        // Multibyte flag only: last byte & 3 = 1 -> 2 bytes.
        assert_eq!(trailing_size(b"text\x41\x01", 1), 2);
        // One backward varint entry of size 3 (byte 0x83 = 3 with the stop bit) then multibyte 0.
        assert_eq!(trailing_size(b"text\x00\x00\x83", 0b10), 3);
        assert_eq!(trailing_size(b"text\x00\x00\x83", 0b11), 4);
        assert_eq!(trailing_size(b"text", 0), 0);
    }

    /// A HUFF/CDIC encoder for tests: 8-bit codes 0x01..=0xFF for phrases 254..=0 and
    /// 10-bit codes `00000000 xx` for phrases 259..=256 (exercises the dict2 path).
    pub fn encode(phrases: &[(Vec<u8>, bool)], text: &[usize]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut huff = b"HUFF\x00\x00\x00\x18".to_vec();
        huff.extend_from_slice(&24u32.to_be_bytes());
        huff.extend_from_slice(&(24u32 + 1024).to_be_bytes());
        huff.extend_from_slice(&[0; 8]);
        for c in 0..256u32 {
            let v: u32 = if c == 0 { 9 } else { 8 | 0x80 | (255 << 8) };
            huff.extend_from_slice(&v.to_be_bytes());
        }
        for len in 1..=32u32 {
            let (lo, hi): (u32, u32) = match len {
                9 => (0x1FF, 0),
                10 => (0, 259),
                _ => (u32::MAX >> (32 - len), 0),
            };
            huff.extend_from_slice(&lo.to_be_bytes());
            huff.extend_from_slice(&hi.to_be_bytes());
        }
        let mut cdic = b"CDIC\x00\x00\x00\x10".to_vec();
        cdic.extend_from_slice(&(phrases.len() as u32).to_be_bytes());
        cdic.extend_from_slice(&9u32.to_be_bytes());
        let mut offs = Vec::new();
        let mut body = Vec::new();
        let table = phrases.len() * 2;
        for (p, literal) in phrases {
            offs.push((table + body.len()) as u16);
            let blen = p.len() as u16 | if *literal { 0x8000 } else { 0 };
            body.extend_from_slice(&blen.to_be_bytes());
            body.extend_from_slice(p);
        }
        for o in offs {
            cdic.extend_from_slice(&o.to_be_bytes());
        }
        cdic.extend_from_slice(&body);
        // Bit writer.
        let mut bits: Vec<bool> = Vec::new();
        for &r in text {
            if r <= 254 {
                let c = 255 - r as u32;
                for k in (0..8).rev() {
                    bits.push(c >> k & 1 == 1);
                }
            } else {
                let v = 259 - r as u32;
                bits.extend([false; 8]);
                bits.push(v >> 1 & 1 == 1);
                bits.push(v & 1 == 1);
            }
        }
        let mut data = vec![0u8; bits.len().div_ceil(8)];
        for (i, b) in bits.iter().enumerate() {
            if *b {
                data[i / 8] |= 0x80 >> (i % 8);
            }
        }
        // Pad so the remaining bits cannot form a whole code: fill with a 10-bit prefix tail
        // is not needed because bitsleft stops the decoder when fewer than 8 bits remain.
        (huff, cdic, data)
    }

    #[test]
    fn huffcdic_codes_and_recursion() {
        let mut phrases: Vec<(Vec<u8>, bool)> = (0..260).map(|i| (vec![b'a' + (i % 26) as u8], true)).collect();
        phrases[0] = (b"Hello".to_vec(), true);
        phrases[1] = (b", ".to_vec(), true);
        phrases[256] = (b"world".to_vec(), true);
        phrases[257] = (b"!".to_vec(), true);
        // Phrase 2 is itself compressed: phrase 0 then phrase 257.
        let (_, _, inner) = encode(&phrases, &[0, 257]);
        phrases[2] = (inner, false);
        let (huff, cdic, data) = encode(&phrases, &[0, 1, 256, 257, 2, 2]);
        let mut h = HuffCdic::new(&huff, &[&cdic]).unwrap();
        assert_eq!(h.unpack(&data).unwrap(), b"Hello, world!Hello!Hello!");
        assert!(HuffCdic::new(b"nope", &[]).is_err());
    }

    #[test]
    fn huffcdic_bombs_are_refused() {
        // Phrase k (k < 20) is compressed and names phrase k+1 200 times; phrase 20 is
        // 1000 literal bytes. Fully expanded, phrase 0 is 1000 * 200^20 bytes.
        let mut phrases: Vec<(Vec<u8>, bool)> = (0..260).map(|_| (vec![b'x'], true)).collect();
        phrases[20] = (vec![b'z'; 1000], true);
        for k in 0..20 {
            let (_, _, inner) = encode(&phrases, &[k + 1; 200]);
            phrases[k] = (inner, false);
        }
        let (huff, cdic, data) = encode(&phrases, &[0]);
        let mut h = HuffCdic::new(&huff, &[&cdic]).unwrap();
        let t = std::time::Instant::now();
        let e = h.unpack(&data).unwrap_err();
        assert!(e.to_string().contains("more than"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(10));
        // A CDIC record claiming 4 billion phrases of 31 bits: only the offsets present
        // in the record are read.
        let mut big = b"CDIC\x00\x00\x00\x10".to_vec();
        big.extend_from_slice(&u32::MAX.to_be_bytes());
        big.extend_from_slice(&31u32.to_be_bytes());
        big.extend_from_slice(&[0, 4, 0x80, 1, b'q']);
        let h = HuffCdic::new(&huff, &[&big]).unwrap();
        assert_eq!(h.dict.len(), 2);
    }
}
