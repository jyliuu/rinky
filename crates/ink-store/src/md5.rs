//! Book identity: KOReader's partial MD5.
//!
//! KOReader (`frontend/util.lua`, `util.partialMD5`) hashes 1024-byte samples at
//! offsets `lshift(1024, 2*i)` for `i = -1 ..= 10`, stopping at the first sample that
//! cannot be read (end of file). LuaJIT's `bit.lshift` masks the shift count to 5 bits,
//! so `i = -1` gives `lshift(1024, -2) = 1024 << 30 = 0` (32-bit overflow): the offsets are
//! 0, 1 KiB, 4 KiB, 16 KiB, 64 KiB, 256 KiB, 1 MiB, 4 MiB, 16 MiB, 64 MiB, 256 MiB, 1 GiB.
//! A sample that is cut short by the end of file is hashed as it is; the next one
//! starts at or past the end and stops the loop.
//!
//! This is the document id KOSync and KOReader statistics use. Checked against:
//! - KOReader's spec values (`spec/unit/util_spec.lua`: `leaves.epub` ->
//!   `59d481d168cca6267322f150c5f6a2a3`, `tall.pdf` -> `41cce710f34e5ec21315e19c99821415`);
//! - [`vectors::VECTORS`]: 36 files sized on and around every sample offset, hashed by
//!   KOReader's own `util.partialMD5` run under LuaJIT and by an independent Python
//!   implementation;
//! - a real KOReader sidecar's `partial_md5_checksum` together with its book file.

use md5::{Digest, Md5};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Sample size and step of the algorithm.
pub const SAMPLE: u64 = 1024;

/// The sample offsets, in order.
pub fn sample_offsets() -> impl Iterator<Item = u64> {
    (-1i32..=10).map(|i| if i < 0 { 0 } else { SAMPLE << (2 * i as u64) })
}

/// Partial MD5 of any seekable reader, as lowercase hex.
pub fn partial_md5_reader<R: Read + Seek>(r: &mut R) -> std::io::Result<String> {
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; SAMPLE as usize];
    for off in sample_offsets() {
        r.seek(SeekFrom::Start(off))?;
        let mut n = 0;
        while n < buf.len() {
            let got = r.read(&mut buf[n..])?;
            if got == 0 {
                break;
            }
            n += got;
        }
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Partial MD5 of a file, as lowercase hex (KOReader's book id).
pub fn partial_md5(path: &Path) -> std::io::Result<String> {
    partial_md5_reader(&mut std::fs::File::open(path)?)
}

/// True if `s` has the form of a partial MD5 as [`partial_md5`] writes it: exactly 32
/// lowercase hex digits. Book ids read from files (our sidecars, `library.json`,
/// KOReader's `partial_md5_checksum`) are checked with this before they are used in a
/// path or compared.
pub fn is_partial_md5(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// `s` as a book id: lowercased if it is 32 hex digits, else `None`.
pub fn normalize_md5(s: &str) -> Option<String> {
    let l = s.trim().to_ascii_lowercase();
    is_partial_md5(&l).then_some(l)
}

/// Test vectors from KOReader's own implementation.
pub mod vectors {
    /// `(size, partial MD5)` of the first `size` bytes of [`stream`]. Hashes printed by
    /// KOReader's `util.partialMD5` (copied verbatim, with KOReader's `ffi/sha2` md5) under
    /// LuaJIT 2.1, and identical from an independent Python implementation that emulates
    /// 32-bit `bit.lshift`. Sizes sit on and one byte around every sample boundary.
    pub const VECTORS: &[(usize, &str)] = &[
        (0, "d41d8cd98f00b204e9800998ecf8427e"),
        (1, "26b17225b626fb9238849fd60eabdf60"),
        (1023, "62f584f400ad65c0d32b46678340f380"),
        (1024, "b9c590eb516e65ca90ac8030685b3f23"),
        (1025, "93451fab99fa5df997228393df034a99"),
        (2047, "e8304efec560d738858146e2aaeead99"),
        (2048, "b0e9cb7395c89ac0bb99ba0b33348719"),
        (2049, "b0e9cb7395c89ac0bb99ba0b33348719"),
        (4095, "b0e9cb7395c89ac0bb99ba0b33348719"),
        (4096, "b0e9cb7395c89ac0bb99ba0b33348719"),
        (4097, "055938ab7ff5579aad29d66444a1f8f4"),
        (5119, "1b0ac8da354b54e86ded69398b05763d"),
        (5120, "34a893d7f1ef2621bfe290d0a4a8aeb6"),
        (5121, "34a893d7f1ef2621bfe290d0a4a8aeb6"),
        (16383, "34a893d7f1ef2621bfe290d0a4a8aeb6"),
        (16384, "34a893d7f1ef2621bfe290d0a4a8aeb6"),
        (16385, "7ed605ab78033744f2a38f2b3938fad3"),
        (17407, "f2b1ae45db6bb36e0086218c701a1ba5"),
        (17408, "4cd3d37655eca07a66dff12a2093746d"),
        (17409, "4cd3d37655eca07a66dff12a2093746d"),
        (65535, "4cd3d37655eca07a66dff12a2093746d"),
        (65536, "4cd3d37655eca07a66dff12a2093746d"),
        (65537, "6cce6f4ba6e27aedebcd998ae530c46d"),
        (262143, "e423824d3131433d082057414fcb78d0"),
        (262144, "e423824d3131433d082057414fcb78d0"),
        (262145, "52dcf97a3f00dc29da65d8f7a3b13a18"),
        (1048575, "7f734f34b91a62ea275ac2af6be033d2"),
        (1048576, "7f734f34b91a62ea275ac2af6be033d2"),
        (1048577, "26a424986b6fb4f82ebc64b40d4d8122"),
        (1049599, "b6a541bd3247fe42515d2585d1c09494"),
        (1049600, "07768c784eaf3f6045b63a6336355fd6"),
        (1049601, "07768c784eaf3f6045b63a6336355fd6"),
        (4194303, "07768c784eaf3f6045b63a6336355fd6"),
        (4194304, "07768c784eaf3f6045b63a6336355fd6"),
        (4194305, "f44d50a456df37dfc5d9d573da0845c2"),
        (5000000, "7cf87dbe220ecea7dc580f24bed32835"),
    ];

    /// KOReader's spec values (`spec/unit/util_spec.lua`) for files in its `test/` data.
    pub const SPEC_FILES: &[(&str, &str)] =
        &[("leaves.epub", "59d481d168cca6267322f150c5f6a2a3"), ("tall.pdf", "41cce710f34e5ec21315e19c99821415")];

    /// The byte stream the vectors hash: xorshift32 from 2463534242, top byte of each
    /// state (`x ^= x << 13; x ^= x >> 17; x ^= x << 5`).
    pub fn stream(len: usize) -> Vec<u8> {
        let mut x: u32 = 2463534242;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 24) as u8
            })
            .collect()
    }

    /// Checks every vector in memory and, if `dir` is given, through files written there
    /// (so the platform's file reads and seeks are exercised). Returns the count checked.
    pub fn check(dir: Option<&std::path::Path>) -> Result<usize, String> {
        let max = VECTORS.iter().map(|v| v.0).max().unwrap_or(0);
        let data = stream(max);
        for &(len, want) in VECTORS {
            let got = super::partial_md5_reader(&mut std::io::Cursor::new(&data[..len])).map_err(|e| e.to_string())?;
            if got != want {
                return Err(format!("size {len}: got {got}, KOReader {want}"));
            }
            if let Some(dir) = dir {
                let f = dir.join(format!("v{len:07}.bin"));
                std::fs::write(&f, &data[..len]).map_err(|e| format!("{}: {e}", f.display()))?;
                let got = super::partial_md5(&f).map_err(|e| e.to_string());
                let _ = std::fs::remove_file(&f);
                if got.as_deref() != Ok(want) {
                    return Err(format!("file of size {len}: got {got:?}, KOReader {want}"));
                }
            }
        }
        Ok(VECTORS.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;

    #[test]
    fn offsets() {
        let v: Vec<u64> = sample_offsets().collect();
        assert_eq!(v, [0, 1024, 4096, 16384, 65536, 262144, 1 << 20, 4 << 20, 16 << 20, 64 << 20, 256 << 20, 1 << 30]);
    }

    #[test]
    fn md5_form() {
        assert!(is_partial_md5("cf7ce9b8d4e135c8ac340f18d1e58c85"));
        for bad in ["", "a", "CF7CE9B8D4E135C8AC340F18D1E58C85", "cf7ce9b8d4e135c8ac340f18d1e58c8", "../../../../../../tmp/rinky-v", "cf7ce9b8d4e135c8ac340f18d1e58c8g", "é".repeat(16).as_str()] {
            assert!(!is_partial_md5(bad), "{bad:?}");
        }
        assert_eq!(normalize_md5(" CF7CE9B8D4E135C8AC340F18D1E58C85 ").as_deref(), Some("cf7ce9b8d4e135c8ac340f18d1e58c85"));
        assert_eq!(normalize_md5("/../../var/local/x"), None);
    }

    #[test]
    fn koreader_luajit_vectors() {
        let tmp = crate::testutil::TempDir::new("md5");
        assert_eq!(vectors::check(Some(tmp.path())).unwrap(), 36);
    }

    #[test]
    fn short_reads_are_completed() {
        /// A reader that returns at most 7 bytes per call.
        struct Trickle(Cursor<Vec<u8>>);
        impl Read for Trickle {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = buf.len().min(7);
                self.0.read(&mut buf[..n])
            }
        }
        impl Seek for Trickle {
            fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
                self.0.seek(pos)
            }
        }
        let data = vectors::stream(70_000);
        let want = vectors::VECTORS.iter().find(|v| v.0 == 65537).unwrap().1;
        assert_eq!(partial_md5_reader(&mut Trickle(Cursor::new(data[..65537].to_vec()))).unwrap(), want);
    }

    /// `INK_TESTDATA_DIR` (set on the emulator), else the repository's `testdata/`.
    #[test]
    fn juliet_id() {
        let dir = std::env::var("INK_TESTDATA_DIR").map(PathBuf::from).unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata"));
        let p = dir.join("juliet.epub");
        if !p.exists() {
            eprintln!("skipped: {} not found", p.display());
            return;
        }
        // Same value from KOReader's util.partialMD5 under LuaJIT and from Python.
        assert_eq!(partial_md5(&p).unwrap(), "cf7ce9b8d4e135c8ac340f18d1e58c85");
    }

    /// Runs against KOReader's own test files: `KOREADER_TEST_DIR`, else the local
    /// KOReader checkout; skipped when neither exists.
    #[test]
    fn koreader_spec_values() {
        let dir = std::env::var("KOREADER_TEST_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../koreader/test")));
        if !dir.join("leaves.epub").exists() {
            eprintln!("skipped: no KOReader test files in {}", dir.display());
            return;
        }
        for (name, want) in vectors::SPEC_FILES {
            assert_eq!(partial_md5(&dir.join(name)).unwrap(), *want, "{name}");
        }
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(partial_md5(Path::new("/no/such/book.epub")).is_err());
    }
}
