//! Small helpers: SHA-1 (EPUB font obfuscation keys), tolerant base64 (FB2 binaries),
//! big-endian readers and media-type sniffing.

use std::io::Read;

/// Largest zip entry read into memory (EPUB chapters, images and fonts, the FB2 file of
/// an `.fb2.zip`, FB3 parts, the KFX file of a KFX-ZIP). A larger entry is an error, so
/// a small zip that inflates to gigabytes cannot exhaust the reader's memory.
pub const MAX_ENTRY: u64 = 64 << 20;

/// Largest cover image read for the library (a larger cover is skipped).
pub const MAX_COVER: u64 = 16 << 20;

/// Reads `r` to the end. `declared` is the size the container claims (used for the
/// first allocation when it is within `limit`); more than `limit` bytes is an error.
pub fn read_limited(r: impl Read, declared: u64, limit: u64) -> std::io::Result<Vec<u8>> {
    if declared > limit {
        return Err(too_large(limit));
    }
    let mut out = Vec::with_capacity(declared as usize);
    r.take(limit.saturating_add(1)).read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        return Err(too_large(limit));
    }
    Ok(out)
}

/// The first `limit` bytes of a file (all of it when it is smaller).
pub fn read_head(path: &std::path::Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let f = std::fs::File::open(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut out = Vec::with_capacity(len.min(limit) as usize);
    f.take(limit).read_to_end(&mut out)?;
    Ok(out)
}

fn too_large(limit: u64) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("entry larger than {} MiB", limit >> 20))
}

/// SHA-1 digest of `data` (FIPS 180-4).
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// Decodes base64, skipping whitespace and other bytes outside the alphabet (FB2
/// binaries are wrapped at 76 columns). Stops at the first `=`.
pub fn base64_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

/// Big-endian u16 at `off`, if in range.
pub fn be16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(off..off.checked_add(2)?)?.try_into().ok()?))
}

/// Big-endian u32 at `off`, if in range.
pub fn be32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(off..off.checked_add(4)?)?.try_into().ok()?))
}

/// First position at or after `from` where `needle` occurs in `hay` (`None` for an
/// empty needle).
pub fn find_bytes(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    find_with(hay, needle, from, false)
}

/// [`find_bytes`] with ASCII letters compared case-insensitively.
pub fn find_bytes_ci(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    find_with(hay, needle, from, true)
}

/// Scans for the needle's first byte (a tight loop the compiler vectorises), then
/// compares the rest there.
fn find_with(hay: &[u8], needle: &[u8], from: usize, ci: bool) -> Option<usize> {
    let (&first, rest) = needle.split_first()?;
    let last = hay.len().checked_sub(needle.len())?;
    let (a, b) = if ci { (first.to_ascii_lowercase(), first.to_ascii_uppercase()) } else { (first, first) };
    let mut i = from;
    while i <= last {
        let p = i + hay[i..=last].iter().position(|&c| c == a || c == b)?;
        let tail = &hay[p + 1..p + needle.len()];
        if if ci { tail.eq_ignore_ascii_case(rest) } else { tail == rest } {
            return Some(p);
        }
        i = p + 1;
    }
    None
}

/// Media type of an image or font from its first bytes.
pub fn sniff_media_type(b: &[u8]) -> Option<&'static str> {
    Some(if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        "image/gif"
    } else if b.starts_with(b"BM") && b.len() > 14 {
        "image/bmp"
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else if b.starts_with(b"<svg") || (b.starts_with(b"<?xml") && b[..b.len().min(512)].windows(4).any(|w| w == b"<svg")) {
        "image/svg+xml"
    } else if b.starts_with(&[0, 1, 0, 0]) || b.starts_with(b"true") {
        "font/ttf"
    } else if b.starts_with(b"OTTO") {
        "font/otf"
    } else if b.starts_with(b"wOFF") {
        "font/woff"
    } else if b.starts_with(b"wOF2") {
        "font/woff2"
    } else {
        return None;
    })
}

/// File extension for a media type.
pub fn extension_for(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "font/ttf" => "ttf",
        "font/otf" => "otf",
        "font/woff" => "woff",
        "font/woff2" => "woff2",
        "text/css" => "css",
        _ => "bin",
    }
}

/// Languages by English name (Gutenberg `Language:` headers, lowercase) and by Windows
/// primary language id (the low byte of the MOBI locale field), with their codes.
pub const LANGUAGES: [(&str, u32, &str); 15] = [
    ("english", 9, "en"),
    ("german", 7, "de"),
    ("french", 12, "fr"),
    ("spanish", 10, "es"),
    ("italian", 16, "it"),
    ("danish", 6, "da"),
    ("dutch", 19, "nl"),
    ("russian", 25, "ru"),
    ("portuguese", 22, "pt"),
    ("swedish", 29, "sv"),
    ("norwegian", 20, "no"),
    ("finnish", 11, "fi"),
    ("polish", 21, "pl"),
    ("chinese", 4, "zh"),
    ("japanese", 17, "ja"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn sha1_vectors() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(&sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(hex(&sha1(&vec![b'a'; 1_000_000])), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn base64() {
        assert_eq!(base64_decode(b"aGVs\nbG8g\r\n d29y bGQ="), b"hello world");
        assert_eq!(base64_decode(b"TWFu"), b"Man");
        assert_eq!(base64_decode(b"TWE="), b"Ma");
        assert_eq!(base64_decode(b""), b"");
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff_media_type(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_media_type(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff_media_type(b"OTTO...."), Some("font/otf"));
        assert_eq!(sniff_media_type(b"????"), None);
        assert_eq!(extension_for("image/png"), "png");
        assert_eq!(be32(&[0, 0, 1, 2], 0), Some(258));
        assert_eq!(be16(&[1], 0), None);
    }

    #[test]
    fn byte_search() {
        let hay = b"<a FilePos=1><b filepos=22>";
        assert_eq!(find_bytes(hay, b"filepos=", 0), Some(16));
        assert_eq!(find_bytes_ci(hay, b"filepos=", 0), Some(3));
        assert_eq!(find_bytes_ci(hay, b"FILEPOS=", 4), Some(16));
        assert_eq!(find_bytes(hay, b"<", 1), Some(13));
        assert_eq!(find_bytes(hay, b">", hay.len() - 1), Some(hay.len() - 1));
        assert_eq!(find_bytes(hay, b">", hay.len()), None);
        assert_eq!(find_bytes(hay, b">", usize::MAX), None);
        assert_eq!(find_bytes(hay, b"", 0), None);
        assert_eq!(find_bytes(b"ab", b"abc", 0), None);
        assert_eq!(find_bytes(b"aab", b"ab", 0), Some(1));
    }
}
