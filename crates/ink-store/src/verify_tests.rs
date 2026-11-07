//! Adversarial tests from the wave A verifier (cycle 1). Each test is one attack on the
//! store: boundary sizes for the partial MD5, corrupt and hostile files, crashes between
//! write and rename, two writers, read-only directories, non-UTF-8 paths. A test marked
//! `#[ignore = "verifier: ..."]` reproduces a defect that is still open; run it with
//! `cargo test -p ink-store verifier_ -- --ignored`.

use crate::koreader::{self, KoreaderDirs};
use crate::library::{BookEntry, Collection, LibraryIndex};
use crate::lua;
use crate::md5::{partial_md5, partial_md5_reader, vectors};
use crate::sidecar::{sidecar_dir, sidecar_path, BookState, Bookmark, Highlight, StateSource};
use crate::stats::{PageRecord, StatsBook, StatsDb};
use crate::testutil::TempDir;
use crate::{backup_path, write_atomic, Paths};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

/// True when the tests run as root (the emulator guest): permission bits do not stop
/// root, so the read-only tests are skipped there.
fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Makes a directory read-only and restores it on drop (also when an assertion fails).
struct ReadOnly(PathBuf);

impl ReadOnly {
    fn new(dir: &Path) -> ReadOnly {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        ReadOnly(dir.to_path_buf())
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------------------
// Partial MD5
// ---------------------------------------------------------------------------------------

/// A second, independent reading of KOReader's `util.partialMD5`: an explicit offset
/// list (no shift arithmetic), slices of an in-memory buffer, stop when the offset is at
/// or past the end (Lua's `file:read(1024)` returns nil there).
fn naive_partial_md5(data: &[u8]) -> String {
    use ::md5::{Digest, Md5};
    let offs: [u64; 12] = [0, 1024, 4096, 16384, 65536, 262144, 1 << 20, 4 << 20, 16 << 20, 64 << 20, 256 << 20, 1 << 30];
    let mut h = Md5::new();
    for o in offs {
        let o = o as usize;
        if o >= data.len() {
            break;
        }
        h.update(&data[o..(o + 1024).min(data.len())]);
    }
    format!("{:x}", h.finalize())
}

#[test]
fn verifier_md5_matches_naive_reference_at_every_boundary() {
    let max = (4 << 20) + 1024 + 3;
    let data = vectors::stream(max);
    let mut sizes = vec![0usize, 1, 2, 3, 511, 512, 513];
    for off in [1024usize, 4096, 16384, 65536, 262144, 1 << 20, 4 << 20] {
        for base in [off, off + 1024] {
            for d in -2i64..=2 {
                sizes.push((base as i64 + d) as usize);
            }
        }
    }
    sizes.retain(|&s| s <= max);
    for &n in &sizes {
        let got = partial_md5_reader(&mut Cursor::new(&data[..n])).unwrap();
        assert_eq!(got, naive_partial_md5(&data[..n]), "size {n}");
    }
    eprintln!("verifier md5: {} sizes agree with the naive reference", sizes.len());
}

/// A file of `len` bytes that does not exist anywhere: byte at offset `o` is
/// `((o * 2654435761) >> 13) & 0xFF`. Lets the 16 MiB .. 1 GiB samples and files above
/// 4 GiB be hashed without disk or memory.
struct Virtual {
    len: u64,
    pos: u64,
}

impl Read for Virtual {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(self.len - self.pos) as usize;
        for (k, b) in buf[..n].iter_mut().enumerate() {
            let o = (self.pos + k as u64) as u128;
            *b = ((o * 2654435761) >> 13) as u8;
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Virtual {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.pos = match pos {
            SeekFrom::Start(p) => p,
            SeekFrom::End(d) => (self.len as i64 + d) as u64,
            SeekFrom::Current(d) => (self.pos as i64 + d) as u64,
        };
        Ok(self.pos)
    }
}

/// Hashes printed by KOReader's `util.partialMD5`, its source lines 1094-1111 of
/// `koreader/frontend/util.lua` copied with sed, run under LuaJIT 2.1 with KOReader's
/// `ffi/sha2` md5, on sparse files that hold the same bytes as [`Virtual`] at every
/// sample offset (verifier evidence: out/evidence/store/verify-1/md5/).
const LUAJIT_VIRTUAL: &[(u64, &str)] = &[
    (0, "d41d8cd98f00b204e9800998ecf8427e"),
    (1, "93b885adfe0da089cdf634904fd59f71"),
    (1024, "57b49e462481dbca8bde8fb0d6ec968e"),
    (4097, "cb9ab55e6c5102b86d835c42d9d00d5b"),
    (1049599, "4c01e4c9d2c2c7bc32e025095d6fc828"),
    (1073741823, "10d78962a7ca64154b54c37ab2901335"),
    (1073741824, "10d78962a7ca64154b54c37ab2901335"),
    (1073741825, "9648d650ee9acdb658c357d8d378cf57"),
    (1073742847, "877dcfc5f721ad6e2a9bfebd9fc2437d"),
    (1073742848, "5dd03addd39cf3ad464be46d936cf224"),
    (1073742849, "5dd03addd39cf3ad464be46d936cf224"),
    (3221225472, "5dd03addd39cf3ad464be46d936cf224"),
    (4294967301, "5dd03addd39cf3ad464be46d936cf224"),
];

#[test]
fn verifier_md5_luajit_virtual_files_up_to_4gib() {
    for &(len, want) in LUAJIT_VIRTUAL {
        let got = partial_md5_reader(&mut Virtual { len, pos: 0 }).unwrap();
        assert_eq!(got, want, "virtual file of {len} bytes");
    }
}

#[test]
fn verifier_md5_tiny_files_on_disk() {
    let dir = TempDir::new("v-md5");
    let data = vectors::stream(1100);
    for n in [0usize, 1, 2, 1023, 1024, 1025] {
        let f = dir.path().join(format!("t{n}.epub"));
        std::fs::write(&f, &data[..n]).unwrap();
        assert_eq!(partial_md5(&f).unwrap(), naive_partial_md5(&data[..n]), "file of {n} bytes");
    }
    // A directory is not a book: an error, not the hash of nothing.
    assert!(partial_md5(dir.path()).is_err());
}

// ---------------------------------------------------------------------------------------
// Sidecar
// ---------------------------------------------------------------------------------------

/// The same defect reached through the KOReader import: `pboxes` with `inf` (dump.lua
/// writes LuaJIT's `inf`/`nan`) go into our sidecar, which then no longer loads.
#[test]
fn verifier_koreader_import_nonfinite_pboxes_keeps_sidecar_loadable() {
    let dir = TempDir::new("v-nan-import");
    let book = dir.path().join("scan.pdf");
    std::fs::write(&book, vectors::stream(5000)).unwrap();
    // Our own state first.
    let ours = BookState { position: Some("page:7".into()), bookmarks: vec![Bookmark { position: "page:7".into(), ..Default::default() }], ..Default::default() };
    ours.save(&book).unwrap();
    let meta = koreader::metadata_path(&book);
    std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
    std::fs::write(
        &meta,
        br#"return {
    ["annotations"] = {
        [1] = {
            ["datetime"] = "2024-01-01 00:00:00",
            ["page"] = 2,
            ["pos0"] = { ["page"] = 2, ["x"] = 1, ["y"] = 2 },
            ["pos1"] = { ["page"] = 2, ["x"] = 3, ["y"] = 4 },
            ["pboxes"] = { [1] = { ["x"] = inf, ["y"] = 0, ["w"] = 1, ["h"] = nan } },
            ["text"] = "boxed",
        },
    },
    ["doc_pages"] = 10,
    ["last_page"] = 2,
}
"#,
    )
    .unwrap();
    let before = std::fs::read(&meta).unwrap();
    let imported = koreader::import_book(&book, None, false);
    assert_eq!(std::fs::read(&meta).unwrap(), before, "KOReader's file is never written");
    let r = BookState::load_report(&book).unwrap();
    assert!(r.set_aside.is_empty(), "import returned {imported:?}, then load set aside {:?}: {:?}", r.set_aside, r.errors);
    assert_eq!(r.source, StateSource::Sidecar);
    assert_eq!(r.state.bookmarks.len(), 1, "our own bookmark survives the import");
}

/// Two formats of one title side by side (`Title.epub` and `Title.pdf`, the layout
/// Calibre writes for every book with several formats) share `Title.sdr`. KOReader keeps
/// them apart by naming the file after the suffix (`metadata.epub.lua`,
/// `metadata.pdf.lua`); `rinky.json` is one file for both, so opening one book loads
/// the other's position and highlights, and saving one overwrites the other.
#[test]
fn verifier_sidecar_two_formats_of_one_title_keep_their_own_state() {
    let dir = TempDir::new("v-formats");
    let epub = dir.path().join("Title - Author.epub");
    let pdf = dir.path().join("Title - Author.pdf");
    std::fs::write(&epub, b"epub bytes").unwrap();
    std::fs::write(&pdf, b"pdf bytes").unwrap();
    let mut e = BookState { position: Some("/body/DocFragment[9]/body/p[4]/text().17".into()), ..Default::default() };
    e.add_highlight(Highlight { start: "/body/DocFragment[9]/body/p[4]/text().0".into(), end: "/body/DocFragment[9]/body/p[4]/text().9".into(), text: "epub words".into(), ..Default::default() });
    e.save(&epub).unwrap();
    let p = BookState { position: Some("page:41".into()), ..Default::default() };
    p.save(&pdf).unwrap();
    let back_epub = BookState::load(&epub).unwrap();
    let back_pdf = BookState::load(&pdf).unwrap();
    assert_eq!(back_pdf.position.as_deref(), Some("page:41"));
    assert!(back_pdf.highlights.is_empty(), "the PDF shows the EPUB's highlights: {:?}", back_pdf.highlights);
    assert_eq!(back_epub.position, e.position, "the EPUB's position was overwritten by the PDF's");
    assert_eq!(back_epub.highlights.len(), 1);
}

/// A crash between write and rename leaves `rinky.json.tmp.<pid>.<n>` (partial
/// bytes) next to a good sidecar. Load must use the good file; save must still work.
#[test]
fn verifier_sidecar_crash_before_rename() {
    let dir = TempDir::new("v-crash");
    let book = dir.path().join("c.epub");
    BookState { position: Some("flow:1.0.0".into()), ..Default::default() }.save(&book).unwrap();
    let mut stale = sidecar_path(&book).into_os_string();
    stale.push(".tmp.999999.0");
    let stale = PathBuf::from(stale);
    std::fs::write(&stale, b"{\"version\": 2, \"posit").unwrap();
    let r = BookState::load_report(&book).unwrap();
    assert_eq!((r.source, r.state.position.as_deref()), (StateSource::Sidecar, Some("flow:1.0.0")));
    BookState { position: Some("flow:2.0.0".into()), ..Default::default() }.save(&book).unwrap();
    assert_eq!(BookState::load(&book).unwrap().position.as_deref(), Some("flow:2.0.0"));
    // The temp file of the crashed process is younger than `STALE_TEMP_AGE`, so it stays
    // (it might belong to a save still running); only our own temp files are gone.
    let names = names_in(&sidecar_dir(&book));
    assert_eq!(names.iter().filter(|n| n.contains(".tmp.")).count(), 1, "{names:?}");
}

/// Every truncation of a good sidecar (power loss on a file system without the fsync
/// ordering) loads without error: from the backup, never with a panic. The daily run cuts
/// at every length below 64 and every 13th after; the ignored one at every byte.
#[test]
fn verifier_sidecar_truncated_at_many_lengths() {
    sidecar_truncated(13);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn verifier_sidecar_truncated_at_every_byte() {
    sidecar_truncated(1);
}

fn sidecar_truncated(step: usize) {
    let dir = TempDir::new(&format!("v-trunc-{step}"));
    let book = dir.path().join("t.epub");
    let mut s = BookState { position: Some("/body/DocFragment[3]/body/p[2]/text().42".into()), rating: Some(5), ..Default::default() };
    s.add_highlight(Highlight { start: "a".into(), end: "b".into(), text: "ü \"q\" \u{1F600}".into(), note: Some("n".into()), ..Default::default() });
    s.save(&book).unwrap();
    s.save(&book).unwrap(); // second save: the first file becomes the .old backup
    let full = std::fs::read(sidecar_path(&book)).unwrap();
    let good_old = std::fs::read(backup_path(&sidecar_path(&book))).unwrap();
    for cut in (0..64.min(full.len())).chain((64..full.len()).step_by(step)) {
        std::fs::write(sidecar_path(&book), &full[..cut]).unwrap();
        std::fs::write(backup_path(&sidecar_path(&book)), &good_old).unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!(r.source, StateSource::Backup, "cut at {cut}");
        assert_eq!(r.state.position, s.position);
        for p in &r.set_aside {
            std::fs::remove_file(p).unwrap();
        }
    }
}

/// Hand-edited or foreign values of the wrong type: the file is kept aside byte for byte
/// and the defaults are used; nothing panics.
#[test]
fn verifier_sidecar_wrong_types_are_kept_aside() {
    let dir = TempDir::new("v-types");
    let book = dir.path().join("w.epub");
    std::fs::create_dir_all(sidecar_dir(&book)).unwrap();
    for bad in [
        &br#"{"rating": 300}"#[..],
        br#"{"page": -1}"#,
        br#"{"highlights": {"a": 1}}"#,
        br#"{"settings": {"crop": [1, 2]}}"#,
        br#"[1, 2, 3]"#,
        b"\xff\xfe{",
        b"null",
    ] {
        std::fs::write(sidecar_path(&book), bad).unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!(r.source, StateSource::Default, "{}", String::from_utf8_lossy(bad));
        assert_eq!(r.set_aside.len(), 1);
        assert_eq!(std::fs::read(&r.set_aside[0]).unwrap(), bad);
        std::fs::remove_file(&r.set_aside[0]).unwrap();
    }
}

/// Two writers and a reader on one sidecar at once (the reader thread and a plugin
/// thread saving the same book): every load sees a complete file, nothing is set aside,
/// no temp file is left. On the Kindle user store (fuse.fsp) the reader sometimes finds
/// no sidecar during a rename; loads now wait for the save (`read_settled`) or use its
/// complete temporary copy, so they never return the older `.old`.
#[test]
fn verifier_sidecar_two_writers_and_a_reader() {
    let waits_before = crate::load_waits();
    let dir = TempDir::new("v-writers");
    let book = dir.path().join("w.epub");
    BookState { position: Some("w0-0".into()), ..Default::default() }.save(&book).unwrap();
    std::thread::scope(|sc| {
        for w in 1..=2 {
            let book = &book;
            sc.spawn(move || {
                for i in 0..60 {
                    let mut s = BookState::load(book).unwrap();
                    s.position = Some(format!("w{w}-{i}"));
                    s.add_highlight(Highlight { start: format!("{w}-{i}"), end: format!("{w}-{i}"), ..Default::default() });
                    s.save(book).unwrap();
                }
            });
        }
        let book = &book;
        sc.spawn(move || {
            for _ in 0..200 {
                let r = BookState::load_report(book).unwrap();
                assert_eq!(r.source, StateSource::Sidecar, "{:?}", r.errors);
                assert!(r.set_aside.is_empty());
                assert!(r.state.position.as_deref().unwrap().starts_with('w'));
            }
        });
    });
    let names = names_in(&sidecar_dir(&book));
    assert!(names.iter().all(|n| !n.contains(".tmp.") && !n.contains(".corrupt-")), "{names:?}");
    eprintln!("verifier two writers: loads waited {} times for a save in progress (all tests of this process)", crate::load_waits() - waits_before);
}

/// A read-only `.sdr` directory (read-only mount, full store remounted ro): load still
/// works from the backup and reports the bad file under its own name; save fails cleanly
/// and leaves nothing behind.
#[test]
fn verifier_sidecar_read_only_directory() {
    if is_root() {
        eprintln!("skipped: root ignores permission bits");
        return;
    }
    let dir = TempDir::new("v-ro");
    let book = dir.path().join("r.epub");
    BookState { position: Some("flow:1.0.0".into()), ..Default::default() }.save(&book).unwrap();
    BookState { position: Some("flow:2.0.0".into()), ..Default::default() }.save(&book).unwrap();
    std::fs::write(sidecar_path(&book), b"{\"posi").unwrap();
    let sdr = sidecar_dir(&book);
    let before = names_in(&sdr);
    {
        let _ro = ReadOnly::new(&sdr);
        let r = BookState::load_report(&book).unwrap();
        assert_eq!((r.source, r.state.position.as_deref()), (StateSource::Backup, Some("flow:1.0.0")));
        assert_eq!(r.set_aside, [sidecar_path(&book)], "could not move it: listed under its own name");
        assert!(BookState { position: Some("flow:3.0.0".into()), ..Default::default() }.save(&book).is_err());
        assert_eq!(names_in(&sdr), before, "nothing created or removed");
        assert!(write_atomic(&sdr.join("x.json"), b"1").is_err());
        assert_eq!(names_in(&sdr), before);
    }
}

// ---------------------------------------------------------------------------------------
// Library index and paths
// ---------------------------------------------------------------------------------------

/// One book whose path is not UTF-8 must not make the whole index unsaveable (serde's
/// `Path` serializer refuses such paths, so every later save of collections, progress
/// and last-opened times fails).
#[test]
fn verifier_library_non_utf8_path_does_not_block_save() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new("v-lib-utf8");
    let file = dir.path().join("library.json");
    let mut lib = LibraryIndex::default();
    lib.books.push(BookEntry { path: PathBuf::from(OsStr::from_bytes(b"/mnt/us/Books/caf\xe9.epub")), title: "cafe".into(), format: "epub".into(), ..Default::default() });
    lib.books.push(BookEntry { path: PathBuf::from("/mnt/us/Books/ok.epub"), title: "ok".into(), format: "epub".into(), collections: vec!["Fav".into()], ..Default::default() });
    lib.collections.push(Collection { name: "Fav".into(), created: 1 });
    let saved = lib.save(&file);
    assert!(saved.is_ok(), "{saved:?}");
    let back = LibraryIndex::load(&file).unwrap();
    assert!(back.get(Path::new("/mnt/us/Books/ok.epub")).is_some());
    assert_eq!(back.collections.len(), 1);
}

/// The same on a real file system (Linux only: APFS refuses such names). Run on the
/// emulator's /tmp (tmpfs) and on the user store; skipped where the name is refused.
#[cfg(target_os = "linux")]
#[test]
fn verifier_library_scan_non_utf8_file_then_save() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new("v-lib-utf8-fs");
    let bad = dir.path().join(OsStr::from_bytes(b"caf\xe9.epub"));
    if let Err(e) = std::fs::write(&bad, b"x") {
        eprintln!("skipped: the file system refuses a non-UTF-8 name: {e}");
        return;
    }
    std::fs::write(dir.path().join("ok.epub"), b"y").unwrap();
    let mut lib = LibraryIndex::default();
    assert_eq!(lib.scan(&[dir.path()], 1), (2, 0));
    let file = dir.path().join("library.json");
    let saved = lib.save(&file);
    assert!(saved.is_ok(), "{saved:?}");
}

/// Same for the sidecar on a real file system: two non-UTF-8 books in one folder.
#[cfg(target_os = "linux")]
#[test]
fn verifier_sidecar_non_utf8_books_on_disk_keep_their_own_state() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = TempDir::new("v-sdr-utf8-fs");
    let a = dir.path().join(OsStr::from_bytes(b"caf\xe9.epub"));
    let b = dir.path().join(OsStr::from_bytes(b"na\xefve.epub"));
    if std::fs::write(&a, b"a").and_then(|_| std::fs::write(&b, b"b")).is_err() {
        eprintln!("skipped: the file system refuses a non-UTF-8 name");
        return;
    }
    BookState { position: Some("pos-a".into()), ..Default::default() }.save(&a).unwrap();
    BookState { position: Some("pos-b".into()), ..Default::default() }.save(&b).unwrap();
    assert_eq!(BookState::load(&a).unwrap().position.as_deref(), Some("pos-a"));
}

/// `Paths::cover(md5)` builds a path the library screen writes a PNG to (as root on the
/// Kindle). The md5 comes from files (`rinky.json`, KOReader's
/// `partial_md5_checksum` via `to_book_state`/`merge_into`, `library.json`); a value
/// that is not 32 hex digits must not lead outside the cover cache.
#[test]
fn verifier_cover_path_stays_in_the_cache() {
    let p = Paths::kindle();
    // The chain from a KOReader sidecar: its checksum would become the state's md5. A
    // checksum that is not 32 hex digits is now dropped when the file is read.
    let t = lua::parse_table(br#"return { partial_md5_checksum = "/../../var/local/x", last_page = 1 }"#).unwrap();
    let from_ko = koreader::metadata_from_table(Path::new("/b/x.sdr/metadata.pdf.lua"), &t, Some(true)).to_book_state().md5;
    assert_eq!(from_ko, None);
    for md5 in ["/../../var/local/x", "/mnt/us/documents/evil", "../../../../etc/x", "ab/../../../x", "", "a", "..", "/"] {
        let c = p.cover(md5);
        assert!(
            c.starts_with(p.covers()) && !c.components().any(|x| x == Component::ParentDir),
            "md5 {md5:?} -> {}",
            c.display()
        );
    }
}

#[test]
fn verifier_library_truncated_index_uses_backup() {
    library_truncated(53);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn verifier_library_truncated_index_uses_backup_every_7th_byte() {
    library_truncated(7);
}

/// Cuts at every length below 64 and every `step`-th length after.
fn library_truncated(step: usize) {
    let dir = TempDir::new(&format!("v-lib-trunc-{step}"));
    let file = dir.path().join("library.json");
    let a = crate::library::synthetic_index(5);
    a.save(&file).unwrap();
    a.save(&file).unwrap();
    let full = std::fs::read(&file).unwrap();
    let old = std::fs::read(backup_path(&file)).unwrap();
    for cut in (0..64.min(full.len())).chain((64..full.len()).step_by(step)) {
        std::fs::write(&file, &full[..cut]).unwrap();
        std::fs::write(backup_path(&file), &old).unwrap();
        let back = LibraryIndex::load(&file).unwrap();
        assert_eq!(back.books.len(), 5, "cut at {cut}");
        for e in std::fs::read_dir(dir.path()).unwrap() {
            let e = e.unwrap();
            if e.file_name().to_string_lossy().contains(".corrupt-") {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Lua literal reader and KOReader metadata
// ---------------------------------------------------------------------------------------

#[test]
fn verifier_lua_code_inside_strings_and_comments_is_data() {
    let src = br#"-- os.execute("touch /tmp/rinky-verifier-pwned")
return {
    ["a"] = "os.execute('touch /tmp/rinky-verifier-pwned')",
    ["b"] = [==[ require("ffi").C.system("reboot") ]==],
    --[[ dofile("/mnt/us/x.lua") ]]
    ["c"] = 'io.open("/etc/shadow"):read("*a")',
}"#;
    let t = lua::parse_table(src).unwrap();
    assert_eq!(t.str("a"), Some("os.execute('touch /tmp/rinky-verifier-pwned')"));
    assert_eq!(t.str("b"), Some(r#" require("ffi").C.system("reboot") "#));
    assert_eq!(t.str("c"), Some(r#"io.open("/etc/shadow"):read("*a")"#));
    assert_eq!(t.entries.len(), 3);
    for code in [
        &b"return os.execute('touch /tmp/rinky-verifier-pwned')"[..],
        b"return { a = os.execute('x') }",
        b"return { a = (function() return 1 end)() }",
        b"return setmetatable({}, {__index = os.exit})",
        b"return { [os.time()] = 1 }",
        b"return { a = 1 } or os.exit()",
        b"return { a = #'x' }",
        b"return { a = ~1 }",
        b"return { a = 1 }; os.exit()",
        b"local t = {} return t",
        b"return { a = \"x\" \"y\" }",
    ] {
        assert!(lua::parse(code).is_err(), "accepted {}", String::from_utf8_lossy(code));
    }
}

/// Nesting bombs in every form the grammar recurses on, parsed on a thread with the
/// default 2 MiB stack (a worker thread on the Kindle): errors, no stack overflow.
#[test]
fn verifier_lua_nesting_bombs_on_a_worker_thread() {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(|| {
            let n = 1_000_000;
            let bombs: Vec<Vec<u8>> = vec![
                [b"return ".to_vec(), b"{".repeat(n)].concat(),
                b"{[".repeat(n),
                b"{a=".repeat(n),
                b"{-".repeat(n),
                b"-{".repeat(n),
                [b"-".repeat(n), b"1".to_vec()].concat(),
                b"{{{{{[{{[-{".repeat(n / 10),
            ];
            for b in &bombs {
                assert!(lua::parse(b).is_err(), "accepted a bomb starting {:?}", String::from_utf8_lossy(&b[..20]));
            }
            // 100 levels (the limit) of the real shape still parse.
            let ok = [b"return ".to_vec(), b"{[\"k\"]=".repeat(lua::MAX_DEPTH), b"1".to_vec(), b"}".repeat(lua::MAX_DEPTH)].concat();
            assert!(lua::parse(&ok).is_ok());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn verifier_lua_fuzz_never_panics() {
    let base = include_bytes!("../tests/fixtures/nasty.lua");
    let paging = include_bytes!("../tests/fixtures/metadata.paging.pdf.lua");
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let start = Instant::now();
    let mut n = 0;
    for round in 0..20_000 {
        let src: &[u8] = if round % 2 == 0 { base } else { paging };
        let mut buf = src.to_vec();
        // Flip, insert or delete a few bytes, from the whole byte range.
        for _ in 0..(1 + next() % 6) {
            let at = (next() as usize) % buf.len().max(1);
            match next() % 3 {
                0 => buf[at] = next() as u8,
                1 => buf.insert(at, b"{}[]=,;\"'\\-\n0x.e[[]]--"[next() as usize % 22]),
                _ => {
                    buf.remove(at);
                }
            }
        }
        if let Ok(lua::Value::Table(t)) = lua::parse(&buf) {
            // Whatever parses must also go through the metadata reader.
            let _ = koreader::metadata_from_table(Path::new("/f/metadata.pdf.lua"), &t, None);
            let _ = lua::Value::Table(t).to_json();
        }
        n += 1;
    }
    // Pure random bytes too.
    for _ in 0..20_000 {
        let len = (next() % 200) as usize;
        let buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let _ = lua::parse(&buf);
        n += 1;
    }
    eprintln!("verifier lua fuzz: {n} inputs in {} ms", start.elapsed().as_millis());
}

#[test]
fn verifier_lua_unterminated_and_oversized_inputs() {
    for bad in [
        &b"return \"abc"[..],
        b"return 'abc\\",
        b"return \"\\x4",
        b"return \"\\u{41",
        b"return \"\\u{D800}\"", // a surrogate: accepted as U+FFFD below
        b"return [==[ abc ]=]",
        b"return { a = 1, --[[ ",
        b"return { [\"a\"] ",
        b"return { a = ",
        b"return -",
        b"return 0x1p",
        b"return 1e",
        b"return \xEF\xBB\xBF",
    ] {
        let r = lua::parse(bad);
        if bad == b"return \"\\u{D800}\"" {
            assert_eq!(r.unwrap(), lua::Value::Str("\u{FFFD}".into()));
        } else {
            assert!(r.is_err(), "accepted {}", String::from_utf8_lossy(bad));
        }
    }
    // MAX_INPUT + 1 bytes of white space: refused by size, quickly.
    let big = vec![b' '; lua::MAX_INPUT + 1];
    let t = Instant::now();
    assert!(lua::parse(&big).unwrap_err().msg.contains("larger than"));
    assert!(t.elapsed().as_millis() < 2_000);
    // A metadata file of that size is refused too (it is read into memory first).
    let dir = TempDir::new("v-lua-big");
    let f = dir.path().join("metadata.epub.lua");
    let file = std::fs::File::create(&f).unwrap();
    file.set_len(lua::MAX_INPUT as u64 + 1).unwrap();
    drop(file);
    assert!(koreader::read_metadata_file(&f, Some(false)).is_err());
}

/// A KOReader metadata file that is code, not data: refused, never run, KOReader's file
/// and our sidecar untouched.
#[test]
fn verifier_koreader_code_metadata_is_refused_and_left_alone() {
    let dir = TempDir::new("v-ko-code");
    let book = dir.path().join("evil.epub");
    std::fs::write(&book, b"not really an epub").unwrap();
    let marker = dir.path().join("pwned");
    let meta = koreader::metadata_path(&book);
    std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
    let code = format!("os.execute(\"touch {}\")\nreturn {{ last_xpointer = \"/body/DocFragment[1]\" }}\n", marker.display());
    std::fs::write(&meta, &code).unwrap();
    let r = koreader::import_book(&book, Some(&KoreaderDirs { root: dir.path().join("koreader") }), true);
    assert!(r.is_err(), "{r:?}");
    assert!(!marker.exists());
    assert_eq!(std::fs::read_to_string(&meta).unwrap(), code);
    assert!(!sidecar_path(&book).exists(), "nothing imported, nothing written");
}

/// Annotation tables of the wrong shape: no panic, nothing invented.
#[test]
fn verifier_koreader_wrong_shapes_never_panic() {
    for src in [
        &b"return { annotations = \"x\" }"[..],
        b"return { annotations = { 1, 2, \"x\", {} } }",
        b"return { annotations = { { pos0 = {}, pos1 = 3, page = \"p\" } } }",
        b"return { annotations = { { pos0 = \"a\", page = -5 } }, last_page = -1 }",
        b"return { annotations = { { page = 1e300, pboxes = { { x = 1 } } } } }",
        b"return { bookmarks = { { highlighted = true, page = {} } }, highlight = { [1] = \"x\", [2] = { 5 } } }",
        b"return { bookmarks = 5, highlight = { [-1] = { { pos0 = { x = 0 } } } } }",
        b"return { summary = \"x\", doc_props = 1, percent_finished = nan, last_page = 4294967296 }",
        b"return { summary = { rating = inf, status = {} }, partial_md5_checksum = 5 }",
    ] {
        let t = lua::parse_table(src).unwrap_or_else(|e| panic!("{}: {e}", String::from_utf8_lossy(src)));
        for paging in [None, Some(true), Some(false)] {
            let m = koreader::metadata_from_table(Path::new("/x/metadata.epub.lua"), &t, paging);
            let s = m.to_book_state();
            assert!(s.percent.is_none_or(|p| p.is_finite()));
            assert!(s.rating.is_none_or(|r| (1..=5).contains(&r)));
        }
    }
}

/// Matching of legacy bookmarks and highlights: time a large but possible library of
/// notes (2000 highlights, release numbers in the evidence).
#[test]
fn verifier_koreader_legacy_many_highlights_timing() {
    let n = 2000;
    let mut src = String::from("return {\n bookmarks = {\n");
    for i in 0..n {
        src.push_str(&format!(
            "  {{ datetime = \"2020-01-01 00:{:02}:{:02}\", highlighted = true, notes = \"text {i}\", page = \"/body/DocFragment[{i}]/body/p[1]/text().0\", pos0 = \"/body/DocFragment[{i}]/body/p[1]/text().0\", pos1 = \"/body/DocFragment[{i}]/body/p[1]/text().9\", text = \"note {i}\" }},\n",
            i / 60 % 60,
            i % 60
        ));
    }
    src.push_str(" },\n highlight = {\n");
    for i in 0..n {
        src.push_str(&format!(
            "  [{}] = {{ {{ datetime = \"2020-01-01 00:{:02}:{:02}\", drawer = \"lighten\", pos0 = \"/body/DocFragment[{i}]/body/p[1]/text().0\", pos1 = \"/body/DocFragment[{i}]/body/p[1]/text().9\", text = \"text {i}\" }} }},\n",
            i + 1,
            i / 60 % 60,
            i % 60
        ));
    }
    src.push_str(" },\n}\n");
    let t = Instant::now();
    let table = lua::parse_table(src.as_bytes()).unwrap();
    let m = koreader::metadata_from_table(Path::new("/x/metadata.epub.lua"), &table, Some(false));
    let s = m.to_book_state();
    let ms = t.elapsed().as_millis();
    assert_eq!(s.highlights.len(), n);
    assert!(s.highlights.iter().all(|h| h.style == "lighten" && h.note.is_some()));
    eprintln!("verifier legacy import: {n} bookmarks + {n} highlights in {ms} ms ({} KiB)", src.len() / 1024);
    assert!(ms < 60_000, "{ms} ms");
}

// ---------------------------------------------------------------------------------------
// SQLite
// ---------------------------------------------------------------------------------------

/// Deterministic form: another connection holds a write transaction for 300 ms (a
/// KOReader import on a worker thread, a plugin's own query through `connection()`).
/// `record_pages` starts a deferred transaction, reads (SHARED lock), then needs the
/// RESERVED lock; SQLite does not call the busy handler for a connection that already
/// holds a read transaction (`sqlite3BtreeBeginTrans`), so the 5 s busy timeout never
/// applies and the page record is lost with "database is locked" at once. A write
/// transaction started IMMEDIATE would wait the 300 ms and succeed.
#[test]
fn verifier_stats_record_pages_waits_for_another_writer() {
    let dir = TempDir::new("v-stats-busy");
    let db = dir.path().join("statistics.sqlite3");
    let mut s = StatsDb::open(&db).unwrap();
    let book = s.upsert_book(&StatsBook { md5: "a".repeat(32), title: "A".into(), ..Default::default() }, 1).unwrap();
    let session = s.start_session(book, 1_000).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let other = {
        let db = db.clone();
        std::thread::spawn(move || {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch("BEGIN; INSERT INTO book (title, authors, md5) VALUES ('other', 'x', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb');").unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            c.execute_batch("COMMIT").unwrap();
        })
    };
    ready_rx.recv().unwrap();
    let t = Instant::now();
    let r = s.record_pages(session, &[PageRecord { page: 1, total_pages: 10, start_time: 1_000, duration: 30 }]);
    let waited = t.elapsed().as_millis();
    other.join().unwrap();
    assert!(r.is_ok(), "record_pages after {waited} ms: {r:?}");
    assert_eq!(s.book_totals(&"a".repeat(32)).unwrap().unwrap().pages_read, 1);
}

/// Control for the test above: the same write as `BEGIN IMMEDIATE` on a connection with
/// the same 5 s busy timeout waits for the other writer and succeeds.
#[test]
fn verifier_stats_control_immediate_transaction_waits() {
    let dir = TempDir::new("v-stats-immediate");
    let db = dir.path().join("statistics.sqlite3");
    drop(StatsDb::open(&db).unwrap());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let other = {
        let db = db.clone();
        std::thread::spawn(move || {
            let c = rusqlite::Connection::open(&db).unwrap();
            c.execute_batch("BEGIN; INSERT INTO book (title, authors, md5) VALUES ('other', 'x', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb');").unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
            c.execute_batch("COMMIT").unwrap();
        })
    };
    ready_rx.recv().unwrap();
    let c = rusqlite::Connection::open(&db).unwrap();
    c.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    let t = Instant::now();
    c.execute_batch("BEGIN IMMEDIATE; SELECT count(*) FROM book; INSERT INTO book (title, authors, md5) VALUES ('mine', 'y', 'cccccccccccccccccccccccccccccccc'); COMMIT;").unwrap();
    let waited = t.elapsed().as_millis();
    other.join().unwrap();
    assert!(waited >= 200, "waited {waited} ms");
}

/// Two connections writing page records at once (the stats plugin on the reader thread
/// and a KOReader import or a second plugin on a worker thread). Same cause as above;
/// timing-dependent (failed in 1 of 3 host runs with 421 of 1600 records lost).
#[test]
fn verifier_stats_two_connections_write_at_once() {
    let dir = TempDir::new("v-stats");
    let db = dir.path().join("statistics.sqlite3");
    drop(StatsDb::open(&db).unwrap());
    let errors: Vec<String> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4u32)
            .map(|t| {
                let db = &db;
                sc.spawn(move || {
                    let mut errs = Vec::new();
                    let mut s = StatsDb::open(db).unwrap();
                    let book = s.upsert_book(&StatsBook { md5: format!("{t:032x}"), title: format!("Book {t}"), ..Default::default() }, 1).unwrap();
                    let session = s.start_session(book, 1_000).unwrap();
                    for i in 0..400u32 {
                        let rec = PageRecord { page: i + 1, total_pages: 200, start_time: 1_000 + 10 * i as u64, duration: 10 };
                        if let Err(e) = s.record_pages(session, &[rec]) {
                            errs.push(format!("thread {t} page {i}: {e}"));
                        }
                    }
                    if let Err(e) = s.end_session(session, 3_000) {
                        errs.push(format!("thread {t} end_session: {e}"));
                    }
                    errs
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    assert!(errors.is_empty(), "{} errors, first: {:?}", errors.len(), errors.first());
    let s = StatsDb::open(&db).unwrap();
    for t in 0..4u32 {
        assert_eq!(s.book_totals(&format!("{t:032x}")).unwrap().unwrap().pages_read, 400);
    }
}

/// KOReader's databases in a read-only folder (WAL mode, rows only in the -wal file):
/// read completely, and the folder is unchanged.
#[test]
fn verifier_koreader_databases_in_a_read_only_folder() {
    if is_root() {
        eprintln!("skipped: root ignores permission bits");
        return;
    }
    let dir = TempDir::new("v-ko-ro");
    let ko = dir.path().join("settings");
    let (stats, vocab) = koreader::sqlite::write_sample_databases(&ko, false, true).unwrap();
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let want_stats = koreader::read_statistics_db(&stats, Some(&scratch)).unwrap();
    let want_words = koreader::read_vocabulary(&vocab, Some(&scratch)).unwrap();
    assert!(!want_stats.pages.is_empty() && !want_words.is_empty());
    let before: Vec<(String, Vec<u8>)> = names_in(&ko).into_iter().map(|n| (n.clone(), std::fs::read(ko.join(&n)).unwrap())).collect();
    {
        let _ro = ReadOnly::new(&ko);
        assert_eq!(koreader::read_statistics_db(&stats, Some(&scratch)).unwrap(), want_stats);
        assert_eq!(koreader::read_vocabulary(&vocab, Some(&scratch)).unwrap(), want_words);
    }
    let after: Vec<(String, Vec<u8>)> = names_in(&ko).into_iter().map(|n| (n.clone(), std::fs::read(ko.join(&n)).unwrap())).collect();
    assert_eq!(before, after);
    assert!(names_in(&scratch).is_empty(), "scratch copies removed: {:?}", names_in(&scratch));
}

/// Garbage where a database should be: errors, never a panic, the file unchanged.
#[test]
fn verifier_koreader_garbage_databases() {
    let dir = TempDir::new("v-ko-garbage");
    let scratch = dir.path().join("scratch");
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("text", b"return { not = 'sqlite' }".to_vec()),
        ("random", vectors::stream(40_000)),
    ];
    // A real header with the rest cut off.
    let (stats, _) = koreader::sqlite::write_sample_databases(&dir.path().join("src"), false, false).unwrap();
    let real = std::fs::read(&stats).unwrap();
    cases.push(("truncated", real[..real.len().min(1500)].to_vec()));
    let mut flipped = real.clone();
    for i in (100..flipped.len()).step_by(97) {
        flipped[i] ^= 0x5a;
    }
    cases.push(("flipped", flipped));
    for (name, bytes) in cases {
        let f = dir.path().join(format!("{name}.sqlite3"));
        std::fs::write(&f, &bytes).unwrap();
        let s = koreader::read_statistics_db(&f, Some(&scratch));
        let v = koreader::read_vocabulary(&f, Some(&scratch));
        assert!(s.is_err() || name == "flipped", "{name}: statistics read {s:?}");
        assert!(v.is_err(), "{name}: vocabulary read");
        assert_eq!(std::fs::read(&f).unwrap(), bytes, "{name} unchanged");
    }
}

// ---------------------------------------------------------------------------------------
// Verifier cycle 1, second run: attacks not covered above.
// ---------------------------------------------------------------------------------------

/// Peak resident memory of this process in bytes (`getrusage`; macOS reports bytes,
/// Linux KiB).
fn max_rss_bytes() -> u64 {
    // SAFETY: `ru` is a valid, writable rusage; getrusage(RUSAGE_SELF) only fills it.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    assert_eq!(r, 0);
    let v = ru.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        v
    } else {
        v * 1024
    }
}

/// Makes a directory unreadable (mode 000) and restores it on drop.
struct NoAccess(PathBuf);

impl NoAccess {
    fn new(dir: &Path) -> NoAccess {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        NoAccess(dir.to_path_buf())
    }
}

impl Drop for NoAccess {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// `write_atomic` promises "either the old or the new file". One thread renames new
/// versions over a file while another opens it: the path must never be missing. On the
/// Kindle user store (`fuse.fsp` on 5.19.5) a reader sees `NotFound` during the rename.
/// This is a property of the file system, which the store cannot change; the store's
/// loads handle it (see `store_load_never_misses_a_file_being_renamed`). Passes on APFS,
/// ext4 and tmpfs; run with TMPDIR on the user store to see it
/// (`tests/e2e/store-verify.sh` logs the count).
#[test]
#[ignore = "file-system property: on the Kindle user store (fuse.fsp) a file renamed over is missing for a moment; the store's loads wait for it (store_load_never_misses_a_file_being_renamed)"]
fn verifier_write_atomic_target_never_missing_for_a_reader() {
    let dir = TempDir::new("v-rename");
    let f = dir.path().join("rinky.json");
    write_atomic(&f, b"{\"n\":0}").unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let (missing, reads) = std::thread::scope(|sc| {
        let (f, stop) = (&f, &stop);
        sc.spawn(move || {
            for i in 1..=150 {
                write_atomic(f, format!("{{\"n\":{i}}}").as_bytes()).unwrap();
            }
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let r = sc.spawn(move || {
            let (mut missing, mut reads) = (0u32, 0u32);
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                match std::fs::read(f) {
                    Ok(b) => assert!(b.starts_with(b"{\"n\":") && b.ends_with(b"}"), "partial file: {:?}", String::from_utf8_lossy(&b)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing += 1,
                    Err(e) => panic!("read: {e}"),
                }
                reads += 1;
            }
            (missing, reads)
        });
        r.join().unwrap()
    });
    eprintln!("verifier rename: {missing} of {reads} reads found no file at {}", dir.path().display());
    assert_eq!(missing, 0, "{missing} of {reads} reads during 150 renames found no file");
}

/// The store's side of the test above: while one thread saves new versions of a file
/// (`save_with_backup`, as `BookState::save` does), another loads it with
/// `load_recovering` (what every load uses). Each load must give the newest complete
/// version or a newer one, never the older backup and never "missing", also on the
/// user store where plain reads during a rename find no file (counted for the log).
#[test]
fn store_load_never_misses_a_file_being_renamed() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
    let dir = TempDir::new("v-rename-load");
    let f = dir.path().join("rinky.epub.json");
    let valid = |b: &[u8]| serde_json::from_slice::<serde_json::Value>(b).is_ok();
    let parse = |b: &[u8]| serde_json::from_slice::<serde_json::Value>(b).map_err(|e| e.to_string());
    crate::save_with_backup(&f, b"{\"n\":0}", valid).unwrap();
    crate::save_with_backup(&f, b"{\"n\":0}", valid).unwrap(); // makes the .old backup
    let stop = AtomicBool::new(false);
    let written = AtomicU64::new(0);
    let waits_before = crate::load_waits();
    let (raw_missing, raw_reads, loads, temps) = std::thread::scope(|sc| {
        let (f, stop, written) = (&f, &stop, &written);
        sc.spawn(move || {
            for i in 1..=150u64 {
                crate::save_with_backup(f, format!("{{\"n\":{i}}}").as_bytes(), valid).unwrap();
                written.store(i, SeqCst);
            }
            stop.store(true, SeqCst);
        });
        let r = sc.spawn(move || {
            let (mut raw_missing, mut raw_reads, mut loads, mut temps) = (0u32, 0u32, 0u32, 0u32);
            let mut last = 0u64;
            while !stop.load(SeqCst) {
                match std::fs::read(f) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => raw_missing += 1,
                    _ => {}
                }
                raw_reads += 1;
                let done_before = written.load(SeqCst);
                let r = crate::load_recovering(f, parse).unwrap();
                loads += 1;
                assert!(r.set_aside.is_empty(), "{:?}", r.errors);
                assert!(matches!(r.source, crate::LoadSource::Primary | crate::LoadSource::Temp), "load {loads}: {:?}", r.source);
                temps += (r.source == crate::LoadSource::Temp) as u32;
                let n = r.value.unwrap()["n"].as_u64().unwrap();
                assert!(n >= done_before && n >= last, "load {loads}: version {n}, but {done_before} was saved before it started (last seen {last})");
                last = n;
            }
            (raw_missing, raw_reads, loads, temps)
        });
        r.join().unwrap()
    });
    eprintln!(
        "store rename: {raw_missing} of {raw_reads} plain reads found no file; {loads} loads all current ({temps} from a temporary copy), {} waits at {}",
        crate::load_waits() - waits_before,
        dir.path().display()
    );
}

/// The chain the library screen is told to run: KOReader's
/// `partial_md5_checksum` becomes `BookState::md5` (`import_book` keeps it when the book
/// itself cannot be hashed; `to_book_state` always), `LibraryIndex::update_from_state`
/// copies it into `BookEntry::md5`, and the cover PNG is written to
/// `Paths::cover(entry.md5)`. A checksum from a file must not steer that path.
#[test]
fn verifier_cover_path_from_koreader_checksum_via_library() {
    let dir = TempDir::new("v-cover-chain");
    let paths = Paths { root: dir.path().join("rinky") };
    let book = dir.path().join("Books/x.epub");
    let meta = koreader::metadata_path(&book);
    std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
    std::fs::write(&meta, br#"return { ["partial_md5_checksum"] = "../../../../../../tmp/rinky-verifier", ["last_xpointer"] = "/body/DocFragment[1]/body/p[1]/text().0" }"#).unwrap();
    // The book file is not readable (here: not there yet), so import_book cannot hash it.
    assert!(koreader::import_book(&book, None, false).unwrap().is_some());
    let imported = BookState::load(&book).unwrap();
    let mut lib = LibraryIndex::default();
    lib.books.push(BookEntry { path: book.clone(), title: "x".into(), format: "epub".into(), ..Default::default() });
    for state in [imported, koreader::read_book_metadata(&book).unwrap().to_book_state()] {
        assert!(lib.update_from_state(&book, &state));
        let md5 = lib.get(&book).unwrap().md5.clone().unwrap_or_default();
        let cover = paths.cover(&md5);
        assert!(
            cover.starts_with(paths.covers()) && !cover.components().any(|c| c == Component::ParentDir),
            "BookEntry::md5 {md5:?} -> cover {}",
            cover.display()
        );
    }
}

/// A folder that cannot be read during a rescan (EIO on the FUSE user store, a rescan
/// while the store is being remounted after USB drive mode, a permission change) is
/// treated as an empty folder: its books are dropped from the index with their
/// collections. Collections live only in library.json, so they do not come back when
/// the folder is readable again. Skipped as root (the emulator guest).
#[test]
fn verifier_library_rescan_with_unreadable_folder_keeps_books() {
    if is_root() {
        eprintln!("skipped: root ignores permission bits");
        return;
    }
    let dir = TempDir::new("v-lib-unreadable");
    let sub = dir.path().join("Series");
    std::fs::create_dir_all(&sub).unwrap();
    let book = sub.join("one.epub");
    std::fs::write(&book, b"x").unwrap();
    std::fs::write(dir.path().join("top.epub"), b"y").unwrap();
    let mut lib = LibraryIndex::default();
    assert_eq!(lib.scan(&[dir.path()], 1), (2, 0));
    lib.add_to_collection(&book, "Favourites", 2).unwrap();
    let removed = {
        let _guard = NoAccess::new(&sub);
        lib.scan(&[dir.path()], 3).1
    };
    lib.scan(&[dir.path()], 4);
    let collections = lib.get(&book).map(|e| e.collections.clone());
    assert_eq!(removed, 0, "the unreadable folder's books were dropped; afterwards: {collections:?}");
    assert_eq!(collections, Some(vec!["Favourites".to_string()]));
}

/// KOReader's databases with mode 0444 (a restored backup, files copied read-only):
/// `std::fs::copy` keeps the mode, and the copy is opened read-write so SQLite can
/// replay the WAL. All rows (only in the -wal file) must still be read, the files must
/// stay as they were and the scratch copy must be gone.
#[test]
fn verifier_koreader_read_only_database_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("v-ko-ro-files");
    let ko = dir.path().join("settings");
    let (stats, vocab) = koreader::sqlite::write_sample_databases(&ko, false, true).unwrap();
    let scratch = dir.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let want_stats = koreader::read_statistics_db(&stats, Some(&scratch)).unwrap();
    let want_words = koreader::read_vocabulary(&vocab, Some(&scratch)).unwrap();
    assert!(!want_stats.pages.is_empty() && !want_words.is_empty());
    assert!(names_in(&ko).iter().any(|n| n.ends_with("-wal")), "{:?}", names_in(&ko));
    for n in names_in(&ko) {
        std::fs::set_permissions(ko.join(n), std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let before: Vec<(String, Vec<u8>)> = names_in(&ko).into_iter().map(|n| (n.clone(), std::fs::read(ko.join(&n)).unwrap())).collect();
    let got_stats = koreader::read_statistics_db(&stats, Some(&scratch));
    let got_words = koreader::read_vocabulary(&vocab, Some(&scratch));
    let after: Vec<(String, Vec<u8>)> = names_in(&ko).into_iter().map(|n| (n.clone(), std::fs::read(ko.join(&n)).unwrap())).collect();
    assert_eq!(got_stats.unwrap(), want_stats);
    assert_eq!(got_words.unwrap(), want_words);
    assert_eq!(before, after);
    assert!(names_in(&scratch).is_empty(), "scratch copies removed: {:?}", names_in(&scratch));
}

/// A KOReader metadata file that `lua::MAX_INPUT` accepts but that is all small
/// entries (`1,` = 2 bytes each) must not need more memory than a Kindle has to spare
/// (about 400 MB free with the framework stopped, less rinky's own use): budget
/// 256 MiB of peak growth. It either parses (n up to `lua::MAX_VALUES`) or is refused
/// with an error by the value limit; it never aborts. Heavy and it measures the process's
/// peak memory, so it is ignored in normal runs; run it alone:
/// `INK_VERIFY_LUA_ENTRIES=<n> cargo test -p ink-store --release
/// verify_tests::verifier_lua_near_limit_literal_memory -- --ignored --exact --nocapture`
/// (default n fills MAX_INPUT). On the emulator `tests/e2e/store-verify.sh` runs it
/// under `ulimit -v`.
#[test]
#[ignore = "heavy: measures the peak memory of the whole process; run alone (see the doc comment)"]
fn verifier_lua_near_limit_literal_memory() {
    let n: usize = std::env::var("INK_VERIFY_LUA_ENTRIES").ok().and_then(|v| v.parse().ok()).unwrap_or((lua::MAX_INPUT - 16) / 2);
    let mut src = Vec::with_capacity(n * 2 + 16);
    src.extend_from_slice(b"return {");
    for _ in 0..n {
        src.extend_from_slice(b"1,");
    }
    src.extend_from_slice(b"}");
    let before = max_rss_bytes();
    let t = Instant::now();
    let r = lua::parse_table(&src);
    let ms = t.elapsed().as_millis();
    let grew = max_rss_bytes().saturating_sub(before);
    let outcome = match &r {
        Ok(t) => format!("parsed {} entries", t.entries.len()),
        Err(e) => format!("refused: {e}"),
    };
    let entries = r.as_ref().map(|t| t.entries.len()).ok();
    let err = r.as_ref().err().map(|e| e.msg.clone());
    drop(r);
    eprintln!(
        "verifier lua near limit: {n} entries, {} bytes of input: {outcome} in {ms} ms, peak memory grew {} MiB ({} bytes per entry)",
        src.len(),
        grew >> 20,
        grew / n.max(1) as u64
    );
    match (entries, err) {
        (Some(got), _) => assert_eq!(got, n),
        (None, Some(msg)) => assert!(
            (n + 1 > lua::MAX_VALUES && msg.contains("values")) || (src.len() > lua::MAX_INPUT && msg.contains("larger than")),
            "{n} entries refused for another reason: {msg}"
        ),
        (None, None) => unreachable!(),
    }
    assert!(grew < 256 << 20, "peak memory grew {} MiB for an input of {} MiB", grew >> 20, src.len() >> 20);
}

/// The store on a read-only file system (`EROFS`, as `/` on the Kindle): every write
/// returns an error, nothing panics, loads give defaults. Needs `INK_VERIFY_RO_DIR` (an
/// existing directory on a read-only mount); skipped without it.
#[test]
fn verifier_read_only_file_system_errors_cleanly() {
    let Some(ro) = std::env::var_os("INK_VERIFY_RO_DIR").map(PathBuf::from) else {
        eprintln!("skipped: set INK_VERIFY_RO_DIR to a directory on a read-only mount");
        return;
    };
    let probe = ro.join("rinky-verifier-probe.json");
    let e = write_atomic(&probe, b"1").unwrap_err();
    assert!(matches!(&e, crate::StoreError::Io(io) if io.raw_os_error() == Some(libc::EROFS)), "{ro:?} is not read-only: {e}");
    assert!(!probe.exists());
    let book = ro.join("rinky-verifier.epub");
    let r = BookState::load_report(&book).unwrap();
    assert_eq!(r.source, StateSource::Default);
    assert!(BookState { position: Some("p".into()), ..Default::default() }.save(&book).is_err());
    assert!(LibraryIndex::default().save(&ro.join("library.json")).is_err());
    assert!(crate::Settings::default().save(&ro.join("settings.toml")).is_err());
    assert!(StatsDb::open(&ro.join("rinky-verifier-stats.sqlite3")).is_err());
    let paths = Paths { root: ro.join("rinky-verifier-root") };
    let mut st = crate::PluginStore::open(&paths, "stats", None).unwrap();
    assert!(st.set("k", serde_json::json!(1)).is_err());
    assert!(crate::plugin::ensure_data_dir(&paths, "stats").is_err());
    assert!(!paths.root.exists());
}

/// Several connections opening a new statistics database at the same moment (the
/// reader and a worker thread at start-up): every open succeeds.
#[test]
fn stats_open_at_once_on_a_new_file() {
    for round in 0..30 {
        let dir = TempDir::new("stats-open");
        let db = dir.path().join("statistics.sqlite3");
        let barrier = std::sync::Barrier::new(6);
        let errors: Vec<String> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..6)
                .map(|_| {
                    let (db, barrier) = (&db, &barrier);
                    sc.spawn(move || {
                        barrier.wait();
                        StatsDb::open(db).err().map(|e| e.to_string())
                    })
                })
                .collect();
            hs.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(errors.is_empty(), "round {round}: {errors:?}");
    }
}

/// Probe for the emulator (not a check): does the file system support
/// `renameat2(RENAME_EXCHANGE)` (swap two names in one step), and does a reader then
/// always find the file while new versions are swapped in? If the Kindle's user store
/// supported it, `write_atomic` could close fuse.fsp's rename gap at the source.
/// Logged by `tests/e2e/store-verify.sh`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "probe: run on the emulator by tests/e2e/store-verify.sh"]
fn probe_rename_exchange_keeps_the_file_visible() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let exchange = |a: &Path, b: &Path| -> std::io::Result<()> {
        let (ca, cb) = (CString::new(a.as_os_str().as_bytes()).unwrap(), CString::new(b.as_os_str().as_bytes()).unwrap());
        // SAFETY: both paths are NUL-terminated and outlive the call; renameat2 takes
        // (olddirfd, oldpath, newdirfd, newpath, flags).
        let r = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, ca.as_ptr(), libc::AT_FDCWD, cb.as_ptr(), libc::RENAME_EXCHANGE) };
        if r == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    let dir = TempDir::new("probe-exchange");
    let f = dir.path().join("state.json");
    let tmp = dir.path().join("state.json.tmp.probe");
    std::fs::write(&f, b"{\"n\":0}").unwrap();
    std::fs::write(&tmp, b"{\"n\":1}").unwrap();
    if let Err(e) = exchange(&tmp, &f) {
        eprintln!("probe rename exchange: not supported at {}: {e}", dir.path().display());
        return;
    }
    assert_eq!(std::fs::read(&f).unwrap(), b"{\"n\":1}");
    let stop = AtomicBool::new(false);
    let (missing, reads) = std::thread::scope(|sc| {
        let (f, tmp, stop) = (&f, &tmp, &stop);
        sc.spawn(move || {
            for i in 2..=150 {
                let mut file = std::fs::File::create(tmp).unwrap();
                std::io::Write::write_all(&mut file, format!("{{\"n\":{i}}}").as_bytes()).unwrap();
                file.sync_all().unwrap();
                drop(file);
                exchange(tmp, f).unwrap();
            }
            stop.store(true, SeqCst);
        });
        sc.spawn(move || {
            let (mut missing, mut reads) = (0u32, 0u32);
            while !stop.load(SeqCst) {
                if std::fs::read(f).is_err() {
                    missing += 1;
                }
                reads += 1;
            }
            (missing, reads)
        })
        .join()
        .unwrap()
    });
    eprintln!("probe rename exchange: supported at {}; {missing} of {reads} reads found no file", dir.path().display());
}
