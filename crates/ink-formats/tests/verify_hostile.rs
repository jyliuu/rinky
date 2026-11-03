//! Adversarial robustness tests for `ink-formats` (verifier, cycle 1).
//!
//! Each attack writes a crafted (small, synthetic) file into a temp dir and drives it
//! through `ink_formats::read_info` / `ink_formats::load` — the two entry points the
//! library scan and the reader use. The worry is not a wrong result but a crash on the
//! real device: a panic (release builds this crate with `panic = "abort"`), a stack
//! overflow, or an allocation far past the Kindle's ~600 MB of RAM.
//!
//! How it runs. Every payload runs in a **child process** (this same test binary
//! re-executed with `INK_VERIFY_PAYLOAD=<name>`), inside a thread with an 8 MiB stack
//! (the reader opens books on a normal-sized stack). This binary installs a counting
//! global allocator that refuses any single request over 384 MiB or a live total over
//! 1 GiB and aborts, so a 32-bit `with_capacity(value-from-file)` that would OOM the
//! device reproduces here, and a runaway can never exhaust the shared build machine. The
//! parent asserts the child finished within a deadline and exited 0; a payload that
//! aborts, overflows the stack, panics or hangs fails its parent test. In verifier cycle 1
//! the confirmed-crash attacks carried `#[ignore = "verifier: ..."]`; all of them are
//! fixed and run in the normal suite now (fix cycle 1 added more attacks of the same
//! classes: INDX data count, CDIC count, deep KF8 part, deep EPUB chapter, HTML
//! formatting reconstruction).
//!
//! Run one attack directly:
//!   cargo test -p ink-formats --test verify_hostile -- --exact <name> --nocapture
//!
//! Environment: `INK_VERIFY_DEADLINE_SCALE=N` multiplies every deadline (slow targets),
//! `INK_FUZZ_FULL=1` runs the engine on every fuzz case, `INK_FORMATS_TESTDATA` points
//! at the testdata directory, `TMPDIR` holds the crafted files (txt_50mb writes 50 MB).

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- allocator cap

const MIB: usize = 1 << 20;
const SINGLE_CAP: usize = 384 * MIB;
const LIVE_CAP: usize = 1024 * MIB;

struct Capped;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() > SINGLE_CAP {
            return std::ptr::null_mut();
        }
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        if live > LIVE_CAP {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            return std::ptr::null_mut();
        }
        PEAK.fetch_max(live, Ordering::Relaxed);
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > SINGLE_CAP {
            return std::ptr::null_mut();
        }
        if new_size > layout.size() {
            let live = LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed) + (new_size - layout.size());
            if live > LIVE_CAP {
                LIVE.fetch_sub(new_size - layout.size(), Ordering::Relaxed);
                return std::ptr::null_mut();
            }
            PEAK.fetch_max(live, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static ALLOC: Capped = Capped;

// ---------------------------------------------------------------- temp files

fn tmp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-verify-{}-{}", std::process::id(), line!()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_tmp(name: &str, bytes: &[u8]) -> (PathBuf, PathBuf) {
    let dir = tmp_dir();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    (dir, p)
}

/// Drives a file through both library (`read_info`) and reader (`load`) paths. Layout is
/// run only for small inputs: paginating a large book in a debug build is slow and is not
/// what these attacks probe (release lays a 50 MB TXT out in ~0.04 s per the implementer).
fn open_both(path: &Path) {
    let _ = ink_formats::read_info(path);
    if let Ok(book) = ink_formats::load(path) {
        let _ = book.positions();
        let small = std::fs::metadata(path).map(|m| m.len() < 4 << 20).unwrap_or(false);
        if small {
            if let Ok(mut d) = book.into_document() {
                use ink_doc::{Document, LayoutSettings};
                let _ = d.set_layout(&LayoutSettings::for_screen(600, 800));
            }
        }
    }
}

/// The ink-formats part of opening a book (library scan and conversion, no engine).
fn open_formats(path: &Path) {
    let _ = ink_formats::read_info(path);
    if let Ok(book) = ink_formats::load(path) {
        let _ = book.positions();
    }
}

// ---------------------------------------------------------------- minimal zip builder

fn crc32(data: &[u8], crc: &mut u32) {
    for &b in data {
        *crc ^= b as u32;
        for _ in 0..8 {
            *crc = (*crc >> 1) ^ (0xEDB8_8320 & (!(*crc & 1)).wrapping_add(1));
        }
    }
}

struct ZipFile {
    name: String,
    stored: Vec<u8>, // deflate stream
    method: u16,     // 0 store, 8 deflate
    crc: u32,
    uncompressed_declared: u32, // value written into the headers (may be a lie)
}

/// Builds a minimal zip the `zip` crate reads back.
fn build_zip(files: &[ZipFile]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut offsets = Vec::new();
    for f in files {
        offsets.push(out.len() as u32);
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&f.method.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&f.crc.to_le_bytes());
        out.extend_from_slice(&(f.stored.len() as u32).to_le_bytes());
        out.extend_from_slice(&f.uncompressed_declared.to_le_bytes());
        out.extend_from_slice(&(f.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(f.name.as_bytes());
        out.extend_from_slice(&f.stored);
    }
    for (f, off) in files.iter().zip(&offsets) {
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&f.method.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&f.crc.to_le_bytes());
        central.extend_from_slice(&(f.stored.len() as u32).to_le_bytes());
        central.extend_from_slice(&f.uncompressed_declared.to_le_bytes());
        central.extend_from_slice(&(f.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(f.name.as_bytes());
    }
    let cd_off = out.len() as u32;
    let cd_len = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment
    out
}

fn store(name: &str, data: &[u8]) -> ZipFile {
    let mut crc = 0xFFFF_FFFFu32;
    crc32(data, &mut crc);
    ZipFile { name: name.into(), stored: data.to_vec(), method: 0, crc: !crc, uncompressed_declared: data.len() as u32 }
}

/// A deflate stream of `n` zero bytes, streamed so the builder never holds `n` bytes.
/// Returns (deflate stream, crc of the zeros).
fn deflate_zeros(n: usize) -> (Vec<u8>, u32) {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut crc = 0xFFFF_FFFFu32;
    let chunk = vec![0u8; 64 * 1024];
    let mut left = n;
    while left > 0 {
        let k = left.min(chunk.len());
        enc.write_all(&chunk[..k]).unwrap();
        crc32(&chunk[..k], &mut crc);
        left -= k;
    }
    (enc.finish().unwrap(), !crc)
}

// ---------------------------------------------------------------- MOBI builder

/// A one-record-0 MOBI/PalmDB with a MOBI header; `fields` sets (offset, value) words.
fn mobi_record0(compression: u16, text_len: u32, text_records: u16, fields: &[(usize, u32)]) -> Vec<u8> {
    let mut r = vec![0u8; 0x10 + 0x108];
    r[0..2].copy_from_slice(&compression.to_be_bytes());
    r[4..8].copy_from_slice(&text_len.to_be_bytes());
    r[8..10].copy_from_slice(&text_records.to_be_bytes());
    r[10..12].copy_from_slice(&4096u16.to_be_bytes());
    r[16..20].copy_from_slice(b"MOBI");
    let set = |r: &mut Vec<u8>, o: usize, v: u32| r[o..o + 4].copy_from_slice(&v.to_be_bytes());
    set(&mut r, 0x14, 0x108); // header length
    set(&mut r, 0x18, 2); // type book
    set(&mut r, 0x1C, 65001); // utf-8
    set(&mut r, 0x24, 6); // version
    for o in [0x28, 0x2C, 0x50, 0x54, 0x58, 0x6C, 0x70, 0xC0, 0xF4, 0xF8, 0xFC, 0x104] {
        set(&mut r, o, 0xFFFF_FFFF);
    }
    set(&mut r, 0x74, 0); // huff count
    set(&mut r, 0x80, 0); // exth flags: none
    for (o, v) in fields {
        set(&mut r, *o, *v);
    }
    r
}

fn palmdb(name: &str, tc: &[u8; 8], records: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0u8; 78];
    let nb = name.as_bytes();
    out[..nb.len().min(31)].copy_from_slice(&nb[..nb.len().min(31)]);
    out[60..68].copy_from_slice(tc);
    out[76..78].copy_from_slice(&(records.len() as u16).to_be_bytes());
    let mut off = 78 + records.len() * 8;
    for (i, r) in records.iter().enumerate() {
        out.extend_from_slice(&(off as u32).to_be_bytes());
        out.extend_from_slice(&((2 * i) as u32).to_be_bytes());
        off += r.len();
    }
    for r in records {
        out.extend_from_slice(r);
    }
    out
}

// ---------------------------------------------------------------- Ion / KFX builder

fn ion_varuint(v: u64) -> Vec<u8> {
    let mut out = vec![(v & 0x7F) as u8 | 0x80];
    let mut v = v >> 7;
    while v > 0 {
        out.insert(0, (v & 0x7F) as u8);
        v >>= 7;
    }
    out
}

/// A chain of `depth` nested Ion annotation wrappers (type 14) around a null, after an
/// Ion version marker.
///
/// Each wrapper is `0xEE, VarUInt(body length), 0x81 (annotation length 1), 0x81 (sid 1)`
/// followed by the next wrapper (or the null `0x0F`). The headers are computed from the
/// inside out and written outermost first, so building is linear in `depth`. (Implementer,
/// fix cycle 2: the first version re-copied the whole inner value at every level, about
/// 86 GB of copying for 120 000 levels; 2 s on the host, many minutes under the ARM
/// emulator. The bytes are the same; `ion_builder_is_linear_and_unchanged` compares the
/// two.)
fn ion_nested_annotations(depth: usize) -> Vec<u8> {
    let mut headers: Vec<Vec<u8>> = Vec::with_capacity(depth);
    let mut inner_len = 1usize; // the null
    for _ in 0..depth {
        let mut h = vec![0xEEu8];
        h.extend(ion_varuint(2 + inner_len as u64));
        h.extend(ion_varuint(1)); // annotation length = 1 byte
        h.push(0x81); // one annotation sid (VarUInt 1)
        inner_len += h.len();
        headers.push(h);
    }
    let mut out = Vec::with_capacity(4 + inner_len);
    out.extend_from_slice(&[0xE0, 0x01, 0x00, 0xEA]);
    for h in headers.iter().rev() {
        out.extend_from_slice(h);
    }
    out.push(0x0F);
    out
}

/// The verifier's original builder (quadratic: copies the inner value at every level).
/// Kept only to check that [`ion_nested_annotations`] writes the same bytes.
fn ion_nested_annotations_copying(depth: usize) -> Vec<u8> {
    // innermost: null (0x0F)
    let mut v = vec![0x0Fu8];
    for _ in 0..depth {
        // annotation wrapper: td=0xEE (type 14, len VarUInt), len, annot_len=1, annot sid=1, value
        let mut body = Vec::new();
        body.extend(ion_varuint(1)); // annot length = 1 byte
        body.push(0x81); // one annotation sid (VarUInt 1)
        body.extend_from_slice(&v);
        let mut w = vec![0xEEu8];
        w.extend(ion_varuint(body.len() as u64));
        w.extend(body);
        v = w;
    }
    [vec![0xE0, 0x01, 0x00, 0xEA], v].concat()
}

/// A CONT KFX container whose container-info Ion is `ci`.
fn kfx_container(ci: &[u8]) -> Vec<u8> {
    let header_len = 18usize;
    let ci_off = header_len; // no entities
    let mut out = b"CONT".to_vec();
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&(header_len as u32).to_le_bytes());
    out.extend_from_slice(&(ci_off as u32).to_le_bytes());
    out.extend_from_slice(&(ci.len() as u32).to_le_bytes());
    out.extend_from_slice(ci);
    out
}

// ================================================================ payloads

fn payload(name: &str) {
    match name {
        // --- MOBI ---------------------------------------------------------
        "mobi_font_huge_usize" => {
            // A FONT resource record claiming ~4 GiB uncompressed; decode_font reserves it.
            let mut font = b"FONT".to_vec();
            for v in [0xFFFF_FFF0u32, 1 /*zlib flag*/, 24, 0, 24] {
                font.extend_from_slice(&v.to_be_bytes());
            }
            let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(b"not really a font").unwrap();
            font.extend_from_slice(&enc.finish().unwrap());
            let rec0 = mobi_record0(1, 4, 1, &[(0x50, 1) /*first_non_book*/, (0x6C, 2) /*first_resource rel*/]);
            let db = palmdb("Book", b"BOOKMOBI", &[rec0, b"<html><body><p>hi</p></body></html>".to_vec(), font]);
            let (_d, p) = write_tmp("font.mobi", &db);
            open_both(&p);
        }
        "mobi_text_length_huge" => {
            // record 0 text_length = 0xFFFFFFFF; Mobi::text does Vec::with_capacity(that).
            let rec0 = mobi_record0(1, 0xFFFF_FFFF, 1, &[(0x50, 2)]);
            let db = palmdb("Book", b"BOOKMOBI", &[rec0, b"<html><body>hi</body></html>".to_vec()]);
            let (_d, p) = write_tmp("tl.mobi", &db);
            open_both(&p);
        }
        "mobi_fdst_count_huge" => {
            // KF8 header with an FDST record claiming a huge flow count. (Implementer,
            // fix cycle 1: the version is at 0x24 of record 0, where mobi_record0 writes
            // it; the first version of this payload wrote 8 at 0x34, so the book stayed
            // MOBI 6 and the FDST was never read.)
            let rec0 = mobi_record0(1, 10, 1, &[(0x24, 8) /*KF8*/, (0x50, 2) /*first_non_book*/, (0xC0, 2) /*fdst rel*/]);
            let mut fdst = b"FDST".to_vec();
            fdst.extend_from_slice(&12u32.to_be_bytes()); // header len
            fdst.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // num sections
            let db = palmdb("Book", b"BOOKMOBI", &[rec0, b"<html><body>hi</body></html>".to_vec(), fdst]);
            let (_d, p) = write_tmp("fdst.azw3", &db);
            open_both(&p);
        }
        "mobi_indx_count_huge" => {
            // An NCX INDX primary record whose per-record count field is ~4G.
            let mut indx = b"INDX".to_vec();
            let mut words = vec![0u32; 45];
            words[0] = 0xC0; // header length
            words[4] = 0; // idxt start
            words[5] = 0xFFFF_FFFF; // count
            words[12] = 0; // ncncx
            for w in words {
                indx.extend_from_slice(&w.to_be_bytes());
            }
            indx.resize(0xC0, 0);
            let rec0 = mobi_record0(1, 10, 1, &[(0x50, 2) /*first_non_book*/, (0xF4, 2) /*ncx rel*/]);
            let db = palmdb("Book", b"BOOKMOBI", &[rec0, b"<html><body>hi</body></html>".to_vec(), indx]);
            let (_d, p) = write_tmp("indx.mobi", &db);
            open_both(&p);
        }
        "mobi_bad_offsets" => {
            // Record offsets that point past the file / out of order (PalmDB should clamp).
            let mut out = vec![0u8; 78];
            out[60..68].copy_from_slice(b"BOOKMOBI");
            out[76..78].copy_from_slice(&3u16.to_be_bytes());
            for off in [0xFFFF_FFFFu32, 10, 0x7FFF_FFFF] {
                out.extend_from_slice(&off.to_be_bytes());
                out.extend_from_slice(&0u32.to_be_bytes());
            }
            out.extend_from_slice(&[0u8; 64]);
            let (_d, p) = write_tmp("off.mobi", &out);
            open_both(&p);
        }

        "mobi_indx_data_count_huge" => {
            // NCX index: the primary record names one data record, whose own entry
            // count is ~4G (the old reader collected that many IDXT offsets).
            let indx = |count: u32, idxt: u32| {
                let mut r = b"INDX".to_vec();
                let mut words = vec![0u32; 45];
                words[0] = 0xC0; // header length
                words[4] = idxt; // IDXT offset
                words[5] = count;
                for w in words {
                    r.extend_from_slice(&w.to_be_bytes());
                }
                r.resize(0xC0, 0);
                r
            };
            let primary = indx(1, 0);
            let mut data = indx(0xFFFF_FFFF, 0xC0);
            data.extend_from_slice(b"IDXT\x00\xC8\x00\x00");
            data.extend_from_slice(&[3, b'a', b'b', b'c']);
            let rec0 = mobi_record0(1, 10, 1, &[(0x50, 2), (0xF4, 2) /*ncx rel*/]);
            let db = palmdb("Book", b"BOOKMOBI", &[rec0, b"<html><body>hi</body></html>".to_vec(), primary, data]);
            let (_d, p) = write_tmp("indxdata.mobi", &db);
            open_both(&p);
        }
        "mobi_cdic_count_huge" => {
            // HUFF/CDIC book whose CDIC record claims 4G phrases of 31 bits (the old
            // reader pushed up to 2^31 dictionary entries).
            let mut huff = b"HUFF\x00\x00\x00\x18".to_vec();
            huff.extend_from_slice(&24u32.to_be_bytes());
            huff.extend_from_slice(&(24u32 + 1024).to_be_bytes());
            huff.extend_from_slice(&[0; 8]);
            for _ in 0..256u32 {
                huff.extend_from_slice(&(8u32 | 0x80 | (255 << 8)).to_be_bytes());
            }
            huff.extend_from_slice(&[0; 32 * 8]);
            let mut cdic = b"CDIC\x00\x00\x00\x10".to_vec();
            cdic.extend_from_slice(&u32::MAX.to_be_bytes());
            cdic.extend_from_slice(&31u32.to_be_bytes());
            cdic.extend_from_slice(&[0, 4, 0x80, 2, b'h', b'i']);
            let mut rec0 = mobi_record0(17480, 2, 1, &[(0x50, 2), (0x70, 2) /*huff rel*/, (0x74, 2) /*huff count*/]);
            rec0[0..2].copy_from_slice(&17480u16.to_be_bytes());
            let db = palmdb("Book", b"BOOKMOBI", &[rec0, vec![0xFF; 8], huff, cdic]);
            let (_d, p) = write_tmp("cdic.mobi", &db);
            open_both(&p);
        }
        "kf8_deep_part" => {
            // A KF8 book whose single (well-formed) part nests 100 000 divs: it must go
            // through the HTML repair, not straight to the engine.
            let n = 100_000;
            let text = format!("<html><body>{}x{}</body></html>", "<div>".repeat(n), "</div>".repeat(n)).into_bytes();
            let rec0 = mobi_record0(1, text.len() as u32, 1, &[(0x24, 8), (0x50, 2)]);
            let db = palmdb("Book", b"BOOKMOBI", &[rec0, text]);
            let (_d, p) = write_tmp("deep.azw3", &db);
            open_both(&p);
        }

        // --- KFX ----------------------------------------------------------
        "kfx_deep_annotations" => {
            // Nested Ion annotation wrappers: Reader::value recurses on type 14 with no
            // depth guard (the guard covers only list/struct types 11-13).
            let t = Instant::now();
            let ci = ion_nested_annotations(120_000);
            let kfx = kfx_container(&ci);
            let (_d, p) = write_tmp("deep.kfx", &kfx);
            let built = t.elapsed();
            let t = Instant::now();
            open_both(&p);
            eprintln!("TIME build {built:?}, read_info+load {:?}", t.elapsed());
        }
        "kfx_empty" => {
            let (_d, p) = write_tmp("empty.kfx", b"");
            open_both(&p);
        }

        // --- EPUB ---------------------------------------------------------
        "epub_zip_bomb_chapter" => {
            // A spine chapter declared huge but tiny when stored. The `zip` crate caps the
            // per-entry read, so this stays bounded (a few hundred MiB) and opens; the test
            // documents that no unbounded read happens through the chapter path.
            let (bomb, crc) = deflate_zeros(150 * MIB);
            let container = br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
            let opf = br#"<package version="2.0" xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Bomb</dc:title></metadata><manifest><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#;
            let files = vec![
                store("mimetype", b"application/epub+zip"),
                store("META-INF/container.xml", container),
                store("content.opf", opf),
                ZipFile { name: "c.xhtml".into(), stored: bomb, method: 8, crc, uncompressed_declared: (150 * MIB) as u32 },
            ];
            let (_d, p) = write_tmp("bomb.epub", &build_zip(&files));
            open_both(&p);
        }
        "epub_zip_bomb_cover" => {
            // A cover image declared huge but tiny when stored; read_info reads the cover
            // during a library scan. Bounded by the `zip` crate, as above.
            let (bomb, crc) = deflate_zeros(150 * MIB);
            let container = br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
            let opf = br#"<package version="3.0" xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Bomb</dc:title></metadata><manifest><item id="cv" href="cover.jpg" media-type="image/jpeg" properties="cover-image"/><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#;
            let files = vec![
                store("mimetype", b"application/epub+zip"),
                store("META-INF/container.xml", container),
                store("content.opf", opf),
                store("c.xhtml", b"<html><body><p>x</p></body></html>"),
                ZipFile { name: "cover.jpg".into(), stored: bomb, method: 8, crc, uncompressed_declared: (150 * MIB) as u32 },
            ];
            let (_d, p) = write_tmp("bombcover.epub", &build_zip(&files));
            open_both(&p);
        }
        "epub_deep_chapter" => {
            // A spine chapter of 100 000 nested divs (EPUB chapters otherwise reach the
            // engine unchanged).
            let n = 100_000;
            let chapter = format!("<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>{}deep{}</body></html>", "<div>".repeat(n), "</div>".repeat(n));
            let container = br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
            let opf = br#"<package version="2.0" xmlns="http://www.idpf.org/2007/opf"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Deep</dc:title></metadata><manifest><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#;
            let files = vec![store("mimetype", b"application/epub+zip"), store("META-INF/container.xml", container), store("content.opf", opf), store("c.xhtml", chapter.as_bytes())];
            let (_d, p) = write_tmp("deep.epub", &build_zip(&files));
            open_both(&p);
        }
        "epub_no_container" => {
            let files = vec![store("random.txt", b"hello"), store("content.opf", b"<package/>")];
            let (_d, p) = write_tmp("nocont.epub", &build_zip(&files));
            open_both(&p);
        }
        "epub_broken_opf" => {
            let container = br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
            let files = vec![
                store("META-INF/container.xml", container),
                store("content.opf", b"<package><metadata><dc:title>unclosed"),
            ];
            let (_d, p) = write_tmp("broken.epub", &build_zip(&files));
            open_both(&p);
        }
        "epub_spine_missing_items" => {
            let container = br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
            let opf = br#"<package version="2.0" xmlns="http://www.idpf.org/2007/opf"><metadata/><manifest/><spine><itemref idref="a"/><itemref idref="b"/></spine></package>"#;
            let files = vec![store("META-INF/container.xml", container), store("content.opf", opf)];
            let (_d, p) = write_tmp("nospine.epub", &build_zip(&files));
            open_both(&p);
        }
        "epub_path_traversal" => {
            let container = br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
            let opf = br#"<package version="2.0" xmlns="http://www.idpf.org/2007/opf"><metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">t</dc:title></metadata><manifest><item id="c" href="../../../../../../etc/passwd" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#;
            let files = vec![store("META-INF/container.xml", container), store("content.opf", opf)];
            let (_d, p) = write_tmp("trav.epub", &build_zip(&files));
            open_both(&p);
        }
        "epub_broken_encryption_xml" => {
            let container = br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
            let opf = br#"<package version="2.0" xmlns="http://www.idpf.org/2007/opf"><metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">t</dc:title></metadata><manifest><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#;
            let files = vec![
                store("META-INF/container.xml", container),
                store("META-INF/encryption.xml", b"<encryption><garbage"),
                store("content.opf", opf),
                store("c.xhtml", b"<html><body><p>x</p></body></html>"),
            ];
            let (_d, p) = write_tmp("badenc.epub", &build_zip(&files));
            open_both(&p);
        }

        // --- XML / FB2 / HTML deep nesting --------------------------------
        "fb2_deep_sections" => {
            let depth = 60_000;
            let mut s = String::from("<?xml version=\"1.0\"?><FictionBook><description><title-info><book-title>t</book-title></title-info></description><body>");
            for _ in 0..depth {
                s.push_str("<section>");
            }
            s.push_str("<p>deep</p>");
            for _ in 0..depth {
                s.push_str("</section>");
            }
            s.push_str("</body></FictionBook>");
            let (_d, p) = write_tmp("deep.fb2", s.as_bytes());
            open_both(&p);
        }
        "html_deep_divs" => {
            let depth = 100_000;
            let mut s = String::from("<html><body>");
            for _ in 0..depth {
                s.push_str("<div>");
            }
            s.push('x');
            for _ in 0..depth {
                s.push_str("</div>");
            }
            s.push_str("</body></html>");
            let (_d, p) = write_tmp("deep.html", s.as_bytes());
            open_both(&p);
        }
        "html_formatting_reconstruct" => {
            // Each <b id=N> is closed by </div> but stays an active formatting element,
            // and the next <b> re-opens all earlier ones (HTML5 reconstruction):
            // quadratic and N deep without a limit.
            let mut s = String::from("<html><body>");
            for i in 0..30_000 {
                s.push_str(&format!("<div><b id=b{i}>x</div>"));
            }
            s.push_str("</body></html>");
            let (_d, p) = write_tmp("afe.html", s.as_bytes());
            open_both(&p);
        }
        "xml_unclosed_quadratic" => {
            // N opens then N non-matching closes: parse closes each with rposition.
            let n = 200_000;
            let mut s = String::from("<FictionBook><description><title-info><book-title>t</book-title></title-info></description><body>");
            for _ in 0..n {
                s.push_str("<a>");
            }
            for _ in 0..n {
                s.push_str("</z>");
            }
            s.push_str("</body></FictionBook>");
            let (_d, p) = write_tmp("quad.fb2", s.as_bytes());
            open_both(&p);
        }

        // --- TXT ----------------------------------------------------------
        "txt_50mb" => {
            let dir = tmp_dir();
            let p = dir.join("big.txt");
            let mut f = std::fs::File::create(&p).unwrap();
            let para = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \n\n";
            let mut written = 0usize;
            while written < 50 * MIB {
                f.write_all(para).unwrap();
                written += para.len();
            }
            drop(f);
            open_both(&p);
        }
        "txt_cp1252" => {
            let (bytes, _, _) = encoding_rs::WINDOWS_1252.encode("Blåbærgrød café naïve — résumé.\n\nSecond paragraph here.\n");
            let (_d, p) = write_tmp("da.txt", &bytes);
            open_both(&p);
        }
        "txt_utf16_nobom" => {
            let s: Vec<u8> = "Plain ASCII text in UTF-16 big endian with no byte order mark at all.".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
            let (_d, p) = write_tmp("u16.txt", &s);
            open_both(&p);
        }
        "txt_shiftjis" => {
            let (bytes, _, _) = encoding_rs::SHIFT_JIS.encode("これは日本語のテストです。\n\n二番目の段落。\n");
            let (_d, p) = write_tmp("ja.txt", &bytes);
            open_both(&p);
        }

        // --- empty files --------------------------------------------------
        "empty_files" => {
            for ext in ["epub", "fb2", "fb2.zip", "mobi", "azw3", "kfx", "html", "txt", "fb3", "tpz"] {
                let (_d, p) = write_tmp(&format!("empty.{ext}"), b"");
                open_both(&p);
            }
        }

        // --- mutation fuzz ------------------------------------------------
        "fuzz_mutations" => run_fuzz_mutations(),

        other => panic!("unknown payload: {other}"),
    }
}

/// Deterministic xorshift PRNG.
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// Mutation sweep over the committed fixtures: 300 cases each for the first six files
/// (the verifier's original sequence, so case names stay comparable), then 150 each for
/// more formats.
///
/// Every case runs the ink-formats path (`read_info`, `load`, `positions`). The engine
/// (`into_document` + layout) runs on every 25th case: building an `HtmlDocument` loads
/// fonts in ink-html and costs ~0.25 s per case in a debug build, which would make the
/// sweep take 8 minutes. `INK_FUZZ_FULL=1` runs the engine on every case (used with
/// `--release` for evidence).
///
/// A panic in any case fails the sweep: on the device the release build has
/// `panic = "abort"`, so a caught panic here is a crash there. Each panicking case is
/// named on stderr (`FUZZ PANIC ...`). An abort (stack overflow, allocation cap) kills
/// the child; the case is in the marker file `$TMPDIR/ink-verify-fuzz-case.txt`.
fn run_fuzz_mutations() {
    let base = match std::env::var_os("INK_FORMATS_TESTDATA") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata"),
    }
    .join("formats");
    let names: [(&str, usize); 11] = [
        ("sample-calibre.azw3", 300),
        ("sample-calibre-old.mobi", 300),
        ("huffcdic.mobi", 300),
        ("kfx-container.kfx", 300),
        ("sample.fb2", 300),
        ("epub2-ncx.epub", 300),
        ("fonts-calibre.azw3", 150),
        ("sample-calibre-joint.mobi", 150),
        ("epub3-nav.epub", 150),
        ("sample.fb3", 150),
        ("sample.html", 150),
    ];
    let full = std::env::var_os("INK_FUZZ_FULL").is_some();
    let dir = tmp_dir();
    let mut state = 0x1234_5678_9abc_def0u64;
    // A stable path so the crashing case is readable after an abort.
    let crash_log = std::env::temp_dir().join("ink-verify-fuzz-case.txt");
    let mut cases = 0usize;
    let mut engine_cases = 0usize;
    let mut panics: Vec<String> = Vec::new();
    let mut slowest = (Duration::ZERO, String::new());
    for (name, iters) in names {
        let Ok(orig) = std::fs::read(base.join(name)) else { continue };
        if orig.is_empty() {
            continue;
        }
        for iter in 0..iters {
            let mut bytes = orig.clone();
            let flips = 1 + (next_rand(&mut state) % 40) as usize;
            for _ in 0..flips {
                let r = next_rand(&mut state) as usize;
                // Bias toward the first 1 KiB (headers).
                let idx = if r.is_multiple_of(2) { r % bytes.len().clamp(1, 1024) } else { r % bytes.len() };
                bytes[idx] ^= (next_rand(&mut state) & 0xFF) as u8;
            }
            let ext = name.rsplit('.').next().unwrap();
            let f = dir.join(format!("fuzz.{ext}"));
            std::fs::write(&f, &bytes).unwrap();
            let case = format!("{name} iter {iter} flips {flips}");
            std::fs::write(&crash_log, format!("{case}\n")).unwrap();
            let engine = full || cases.is_multiple_of(25);
            let t = Instant::now();
            let r = std::panic::catch_unwind(|| if engine { open_both(&f) } else { open_formats(&f) });
            let dt = t.elapsed();
            if r.is_err() {
                eprintln!("FUZZ PANIC {case}");
                panics.push(case.clone());
            }
            if dt > slowest.0 {
                slowest = (dt, case);
            }
            cases += 1;
            engine_cases += engine as usize;
        }
    }
    eprintln!("FUZZ cases={cases} engine={engine_cases} panics={} slowest={:?} ({})", panics.len(), slowest.0, slowest.1);
    assert!(panics.is_empty(), "{} fuzz cases panicked: {panics:?}", panics.len());
}

// ================================================================ harness

/// Runs one payload in a child process; returns (success, tail of stderr, elapsed).
fn run_child(name: &str, deadline: Duration) -> (bool, String, Duration) {
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["--ignored", "--exact", "--nocapture", "--test-threads=1", "run_payload"])
        .env("INK_VERIFY_PAYLOAD", name)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Both pipes are drained while the child runs: a child that fills a 64 KiB pipe
    // would otherwise block and look like a hang.
    let drain = |p: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out_t = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let err_t = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let tail_of = |s: &str| s.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
    let start = Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                let err = err_t.join().unwrap_or_default();
                let out = out_t.join().unwrap_or_default();
                let tail = tail_of(&err);
                if status.success() {
                    return (true, tail, start.elapsed());
                }
                // How the child ended (exit code or signal) and both tails, so a crash
                // with an empty stderr can still be told apart (SIGKILL = OOM killer or
                // an outside kill, SIGSEGV/SIGBUS = stack or memory, exit 101 = panic).
                return (false, format!("child {}\n-- stderr tail --\n{tail}\n-- stdout tail --\n{}", describe_status(&status), tail_of(&out)), start.elapsed());
            }
            None => {
                if start.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (false, format!("TIMEOUT after {:?} (hang)", deadline), start.elapsed());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// "exit code N" or "killed by signal N (NAME)".
fn describe_status(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let name = match sig {
                4 => "SIGILL",
                6 => "SIGABRT",
                7 => "SIGBUS",
                9 => "SIGKILL",
                11 => "SIGSEGV",
                15 => "SIGTERM",
                _ => "?",
            };
            return format!("killed by signal {sig} ({name}){}", if status.core_dumped() { ", core dumped" } else { "" });
        }
    }
    match status.code() {
        Some(c) => format!("exit code {c}"),
        None => format!("{status}"),
    }
}

/// Assert the payload finishes cleanly within the deadline; prints peak heap on pass.
fn expect_ok(name: &str) {
    expect_ok_within(name, Duration::from_secs(45));
}

/// [`expect_ok`] with its own deadline, multiplied by `INK_VERIFY_DEADLINE_SCALE` (an
/// integer, default 1; the emulated ARM guest runs these 10-30 times slower).
fn expect_ok_within(name: &str, deadline: Duration) {
    let scale = std::env::var("INK_VERIFY_DEADLINE_SCALE").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(1).max(1);
    let (ok, tail, dt) = run_child(name, deadline * scale);
    assert!(ok, "payload `{name}` crashed/hung in {dt:?}:\n{tail}");
    let summary: Vec<&str> = tail.lines().filter(|l| l.contains("PEAK") || l.contains("FUZZ") || l.starts_with("TIME")).collect();
    println!("payload `{name}` ok in {dt:?}; {}", summary.join("; "));
}

// ---------------------------------------------------------------- child entry

/// The in-child entry point. Normal `cargo test` skips it (ignored); the parent runs it
/// explicitly with `INK_VERIFY_PAYLOAD` set.
#[test]
#[ignore = "child process entry point; driven by the attack tests"]
fn run_payload() {
    // Only meaningful as a child with the payload named in the environment; a bare
    // `--ignored` run (no env) is a no-op so the suite stays clean.
    let Ok(name) = std::env::var("INK_VERIFY_PAYLOAD") else { return };
    let handle = std::thread::Builder::new().stack_size(8 * MIB).spawn(move || payload(&name)).unwrap();
    let res = handle.join();
    eprintln!("PEAK={} MiB", PEAK.load(Ordering::Relaxed) / MIB);
    if res.is_err() {
        std::process::exit(101);
    }
}

// ================================================================ attack tests
// Each spawns the child and asserts a clean finish.

#[test]
fn mobi_bad_offsets() {
    expect_ok("mobi_bad_offsets");
}

#[test]
fn kfx_empty() {
    expect_ok("kfx_empty");
}

#[test]
fn epub_no_container() {
    expect_ok("epub_no_container");
}

#[test]
fn epub_broken_opf() {
    expect_ok("epub_broken_opf");
}

#[test]
fn epub_spine_missing_items() {
    expect_ok("epub_spine_missing_items");
}

#[test]
fn epub_path_traversal() {
    expect_ok("epub_path_traversal");
}

#[test]
fn epub_broken_encryption_xml() {
    expect_ok("epub_broken_encryption_xml");
}

#[test]
fn txt_cp1252() {
    expect_ok("txt_cp1252");
}

#[test]
fn txt_utf16_nobom() {
    expect_ok("txt_utf16_nobom");
}

#[test]
fn txt_shiftjis() {
    expect_ok("txt_shiftjis");
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn txt_50mb() {
    expect_ok("txt_50mb");
}

#[test]
fn empty_files() {
    expect_ok("empty_files");
}

/// The harness itself: a failed child is reported with its exit code and both tails
/// (the guest's fuzz_mutations failure once came back with nothing to go on).
#[test]
fn run_child_reports_how_a_child_ended() {
    let (ok, tail, _) = run_child("__no_such_payload__", Duration::from_secs(120));
    assert!(!ok);
    assert!(tail.contains("child exit code 101") && tail.contains("unknown payload: __no_such_payload__"), "{tail}");
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn fuzz_mutations() {
    // 2550 cases: about 40 s in a debug build on an idle host, so the deadline is
    // longer than the 45 s of the single-file attacks (shared build machines are slow).
    expect_ok_within("fuzz_mutations", Duration::from_secs(300));
}

// --- verifier cycle 1 crashes (fixed) ---

#[test]
fn mobi_font_huge_usize() {
    expect_ok("mobi_font_huge_usize");
}

#[test]
fn mobi_text_length_huge() {
    expect_ok("mobi_text_length_huge");
}

#[test]
fn mobi_indx_data_count_huge() {
    expect_ok("mobi_indx_data_count_huge");
}

#[test]
fn mobi_cdic_count_huge() {
    expect_ok("mobi_cdic_count_huge");
}

#[test]
fn kf8_deep_part() {
    expect_ok("kf8_deep_part");
}

#[test]
fn epub_deep_chapter() {
    expect_ok("epub_deep_chapter");
}

#[test]
fn html_formatting_reconstruct() {
    expect_ok("html_formatting_reconstruct");
}

#[test]
fn mobi_fdst_count_huge() {
    expect_ok("mobi_fdst_count_huge");
}

#[test]
fn mobi_indx_count_huge() {
    expect_ok("mobi_indx_count_huge");
}

#[test]
fn kfx_deep_annotations() {
    expect_ok("kfx_deep_annotations");
}

/// The linear Ion builder writes the same bytes as the verifier's copying builder.
#[test]
fn ion_builder_is_linear_and_unchanged() {
    // 1-, 2- and 3-byte body lengths (the 3-byte VarUInt starts near depth 3280).
    for depth in [0, 1, 2, 15, 16, 17, 31, 32, 33, 100, 3000, 4000] {
        assert_eq!(ion_nested_annotations(depth), ion_nested_annotations_copying(depth), "depth {depth}");
    }
}

#[test]
fn fb2_deep_sections() {
    expect_ok("fb2_deep_sections");
}

#[test]
fn html_deep_divs() {
    expect_ok("html_deep_divs");
}

#[test]
fn xml_unclosed_quadratic() {
    expect_ok("xml_unclosed_quadratic");
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn epub_zip_bomb_chapter() {
    expect_ok("epub_zip_bomb_chapter");
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn epub_zip_bomb_cover() {
    expect_ok("epub_zip_bomb_cover");
}
