//! Character encoding detection for text-like books (TXT, HTML without a declared charset).
//!
//! Order: byte order mark, UTF-16 without BOM (zero-byte pattern), valid UTF-8, then
//! `chardetng` (legacy single- and multi-byte encodings such as cp1251, cp1252, KOI8-R,
//! Shift_JIS, GBK, Big5, EUC-KR, ISO-8859-x).

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};

/// Text decoded from bytes, with the encoding that was used.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    /// The text (invalid sequences replaced by U+FFFD).
    pub text: String,
    /// WHATWG name of the encoding (`UTF-8`, `windows-1251`, ...).
    pub encoding: &'static str,
    /// How the encoding was found.
    pub source: EncodingSource,
}

/// How an encoding was determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodingSource {
    /// Byte order mark.
    Bom,
    /// UTF-16 recognised by its zero bytes.
    ZeroPattern,
    /// The bytes are valid UTF-8.
    ValidUtf8,
    /// Declared by the document (XML declaration, HTML meta).
    Declared,
    /// Guessed by `chardetng`.
    Guessed,
}

/// Bytes sampled for the UTF-16 test and for `chardetng`.
const SAMPLE: usize = 256 * 1024;

/// UTF-16 without BOM: in mostly-ASCII or mostly-Cyrillic/Latin text, one byte of every
/// pair is zero (or a small block number) far more often than the other.
fn utf16_without_bom(bytes: &[u8]) -> Option<&'static Encoding> {
    let n = bytes.len().min(8192) & !1;
    if n < 8 {
        return None;
    }
    let (mut even0, mut odd0) = (0usize, 0usize);
    for (i, b) in bytes[..n].iter().enumerate() {
        if *b == 0 {
            if i % 2 == 0 {
                even0 += 1;
            } else {
                odd0 += 1;
            }
        }
    }
    let pairs = n / 2;
    if odd0 * 10 >= pairs * 3 && even0 * 20 < pairs {
        Some(UTF_16LE)
    } else if even0 * 10 >= pairs * 3 && odd0 * 20 < pairs {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// Guesses the legacy encoding of `bytes` with `chardetng`.
pub fn guess_legacy(bytes: &[u8]) -> &'static Encoding {
    let mut det = chardetng::EncodingDetector::new();
    let sample = &bytes[..bytes.len().min(SAMPLE)];
    det.feed(sample, sample.len() == bytes.len());
    det.guess(None, false)
}

/// Decodes `bytes` with detection (BOM, UTF-16 pattern, UTF-8, chardetng).
pub fn decode_text(bytes: &[u8]) -> Decoded {
    decode_text_owned(bytes.to_vec())
}

/// [`decode_text`] taking the bytes: valid UTF-8 (most books) becomes the text without
/// a copy, so a big file is not held twice while it is decoded.
pub fn decode_text_owned(bytes: Vec<u8>) -> Decoded {
    if let Some((enc, bom)) = Encoding::for_bom(&bytes) {
        let text = enc.decode_without_bom_handling(&bytes[bom..]).0.into_owned();
        return Decoded { text, encoding: enc.name(), source: EncodingSource::Bom };
    }
    if let Some(enc) = utf16_without_bom(&bytes) {
        let text = enc.decode_without_bom_handling(&bytes).0.into_owned();
        return Decoded { text, encoding: enc.name(), source: EncodingSource::ZeroPattern };
    }
    match String::from_utf8(bytes) {
        Ok(text) => Decoded { text, encoding: UTF_8.name(), source: EncodingSource::ValidUtf8 },
        Err(e) => decode_invalid_utf8(e.as_bytes()),
    }
}

/// Bytes that are not valid UTF-8: UTF-8 with a sequence cut at the end (a truncated
/// download), else the legacy encoding `chardetng` guesses.
fn decode_invalid_utf8(bytes: &[u8]) -> Decoded {
    if let Err(e) = std::str::from_utf8(bytes) {
        if e.error_len().is_none() && e.valid_up_to() + 4 > bytes.len() && e.valid_up_to() > 0 {
            let text = String::from_utf8_lossy(bytes).into_owned();
            return Decoded { text, encoding: UTF_8.name(), source: EncodingSource::ValidUtf8 };
        }
    }
    let enc = guess_legacy(bytes);
    let text = enc.decode_without_bom_handling(bytes).0.into_owned();
    Decoded { text, encoding: enc.name(), source: EncodingSource::Guessed }
}

/// Decodes with a declared label when it is known, else with detection ([`decode_text_owned`]).
pub fn decode_with_label(bytes: Vec<u8>, label: Option<&str>) -> Decoded {
    if Encoding::for_bom(&bytes).is_none() {
        if let Some(enc) = label.and_then(|l| Encoding::for_label(l.trim().as_bytes())) {
            // A UTF-16 label on ASCII-compatible bytes is a lie (HTML spec): use UTF-8.
            let enc = if enc == UTF_16LE || enc == UTF_16BE { UTF_8 } else { enc };
            let text = enc.decode_without_bom_handling(&bytes).0.into_owned();
            return Decoded { text, encoding: enc.name(), source: EncodingSource::Declared };
        }
    }
    decode_text_owned(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RU: &str = "Мой дядя самых честных правил, когда не в шутку занемог, он уважать себя заставил \
                      и лучше выдумать не мог. Его пример другим наука; но, боже мой, какая скука \
                      с больным сидеть и день и ночь, не отходя ни шагу прочь! ";
    const DA: &str = "Det var engang en fattig prins; han havde et kongerige, der var ganske lille, \
                      men det var da altid stort nok til at gifte sig på, og gifte sig, det ville han. \
                      Æbler, øl og små søde kager på bordet; café og naïve gæster. ";

    #[test]
    fn boms_and_utf16_patterns() {
        let mut le = vec![0xFF, 0xFE];
        le.extend("Hello Æ".encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(decode_text(&le).text, "Hello Æ");
        let be: Vec<u8> = "Hello there, plain ASCII text in UTF-16BE.".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        let d = decode_text(&be);
        assert_eq!((d.encoding, d.source), ("UTF-16BE", EncodingSource::ZeroPattern));
        assert!(d.text.starts_with("Hello there"));
        let mut u8bom = vec![0xEF, 0xBB, 0xBF];
        u8bom.extend_from_slice("ø".as_bytes());
        assert_eq!(decode_text(&u8bom).text, "ø");
    }

    #[test]
    fn utf8_cp1251_cp1252_koi8() {
        assert_eq!(decode_text(RU.as_bytes()).source, EncodingSource::ValidUtf8);
        let text = RU.repeat(8);
        let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode(&text);
        let d = decode_text(&cp1251);
        assert_eq!(d.encoding, "windows-1251");
        assert_eq!(d.text, text);
        let (koi, _, _) = encoding_rs::KOI8_R.encode(&text);
        let d = decode_text(&koi);
        assert!(d.encoding.starts_with("KOI8"), "{}", d.encoding);
        assert_eq!(d.text, text);
        let da = DA.repeat(6);
        let (cp1252, _, _) = encoding_rs::WINDOWS_1252.encode(&da);
        let d = decode_text(&cp1252);
        assert_eq!(d.encoding, "windows-1252");
        assert_eq!(d.text, da);
        let d = decode_with_label(cp1252.into_owned(), Some("ISO-8859-1"));
        assert_eq!((d.encoding, d.source), ("windows-1252", EncodingSource::Declared));
    }

    #[test]
    fn truncated_utf8_stays_utf8() {
        let mut b = "Søren Kierkegaard".as_bytes().to_vec();
        b.extend_from_slice(&"ø".as_bytes()[..1]);
        assert_eq!(decode_text(&b).encoding, "UTF-8");
    }

    #[test]
    fn owned_and_borrowed_decode_alike() {
        let ru = RU.repeat(8);
        let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode(&ru);
        let mut cut = "Søren".as_bytes().to_vec();
        cut.push(0xC3);
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("Æ text".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        for b in [RU.as_bytes().to_vec(), cp1251.into_owned(), cut, utf16, Vec::new()] {
            assert_eq!(decode_text_owned(b.clone()), decode_text(&b));
        }
    }
}
