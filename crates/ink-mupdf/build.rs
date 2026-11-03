//! Builds MuPDF 1.27.2 as two static libraries (`libmupdf.a`, `libmupdf-third.a`) plus the
//! FFI shim (`csrc/shim.c`) for the target being compiled, and links them.
//!
//! Steps, all automatic:
//! 1. Cache directory: `$INK_MUPDF_CACHE`, else `<dir>/inkreader-wt/.cache` found next to
//!    or above the workspace, else `<target dir>/mupdf-cache`.
//! 2. Tarball `mupdf-1.27.2-source.tar.lz` (the release KOReader pins): `$INK_MUPDF_TARBALL`,
//!    else `<cache>/mupdf/`, else downloaded with curl from GitHub or mupdf.com. The SHA-256
//!    is always checked.
//! 3. Extract once into `<cache>/mupdf/src/`, apply two small text patches (no ARM assembly;
//!    CBZ archives skip `__MACOSX/` folders and files whose name starts with a dot).
//! 4. `make libs` with zig as the C compiler, for the Kindle and for the host:
//!    - Kindle: the `zig cc` wrapper that `cargo zigbuild` sets in
//!      `CC_armv7_unknown_linux_musleabihf`; without it, this script's own `zig cc` wrapper.
//!    - Host: this script's `zig cc -target <host>` wrapper when `zig` runs and no
//!      `CC_<target>`/`HOST_CC`/`CC` is set; else the system compiler (with a warning).
//!    - `INK_MUPDF_CC=zig` forces this script's zig wrapper, `INK_MUPDF_CC=system` the cc
//!      crate's choice.
//!
//!    The libraries go to `<cache>/mupdf/build/<version>-<target>-<hash>/`, where the hash
//!    covers the compiler's identity (zig version or `--version` line, not its path), flags,
//!    make features and patches. Later builds, other profiles and other worktrees reuse that
//!    directory; `BUILD.txt` there names the compiler. A build goes to a temporary directory
//!    first and is renamed into place, so parallel builds cannot clash.
//! 5. Compile the shim with the same compiler, link shim, mupdf, mupdf-third.
//!
//! `$INK_MUPDF_LIB_DIR` skips steps 2-4 and links the libraries found there (headers are still
//! needed for the shim: the source tree from step 3).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = "1.27.2";
const TARBALL: &str = "mupdf-1.27.2-source.tar.lz";
const SHA256: &str = "35049dcd0b1a541be2cca1eb4bbd6b41a51e0ed44799333b7bde4528461e2848";
const URLS: &[&str] = &[
    "https://github.com/ArtifexSoftware/mupdf-downloads/releases/download/1.27.2/mupdf-1.27.2-source.tar.lz",
    "https://mupdf.com/downloads/archive/mupdf-1.27.2-source.tar.lz",
];
/// Bumped when the recipe changes in a way the flag hash does not see.
const RECIPE: &str = "r2";

/// Feature switches: no JavaScript, no HTML/EPUB engine (rinky has its own), no SVG
/// documents, no OCR/office output, no Noto/CJK/SIL fonts (the URW base-14 fonts stay, so
/// PDFs with unembedded Helvetica/Times still show text), no hyphenation tables.
const MAKE_FEATURES: &[&str] = &[
    "mujs=no",
    "html=no",
    "svg=no",
    "brotli=no",
    "barcode=no",
    "tesseract=no",
    "extract=no",
    "archive=no",
    "commercial=no",
    "threading=no",
    "HAVE_X11=no",
    "HAVE_GLUT=no",
    "HAVE_OBJCOPY=no",
    "HAVE_LIBCRYPTO=no",
    "HAVE_PTHREAD=no",
    "USE_SYSTEM_LIBS=no",
    "shared=no",
    "build=release",
    "verbose=no",
    "USE_ARGUMENT_FILE=no",
    // Not Linux/Darwin: no objcopy font embedding, no xcrun overrides, no pkg-config probes
    // that could pull host libraries into a cross build.
    "OS=Other",
];
const XCFLAGS: &[&str] = &[
    "-DTOFU",
    "-DTOFU_CJK",
    "-DTOFU_SIL",
    "-DFZ_ENABLE_HYPHEN=0",
    "-DFZ_ENABLE_JS=0",
    "-DFZ_ENABLE_BROTLI=0",
    "-DFZ_ENABLE_OCR_OUTPUT=0",
    "-DFZ_ENABLE_DOCX_OUTPUT=0",
    "-DFZ_ENABLE_ODT_OUTPUT=0",
    "-DFZ_ENABLE_SPOT_RENDERING=0",
    "-DFZ_PLOTTERS_CMYK=0",
];

/// Text patches applied to the extracted tree: (file, original, replacement).
const PATCHES: &[(&str, &str, &str)] = &[
    (
        "include/mupdf/fitz/system.h",
        "/* ARCH_ARM is only used for 32bit ARM stuff. */\n#if defined(__arm__) || defined(__thumb__)\n",
        "/* ARCH_ARM is only used for 32bit ARM stuff. */\n/* rinky: MuPDF's 32-bit ARM assembly stays off (as in KOReader). */\n#if 0\n",
    ),
    (
        "source/cbz/mucbz.c",
        "\t\tconst char *ext = name ? strrchr(name, '.') : NULL;\n",
        concat!(
            "\t\tconst char *ext = name ? strrchr(name, '.') : NULL;\n",
            "\t\t/* rinky: skip macOS resource forks (__MACOSX folders, ._ files) and hidden\n",
            "\t\t * files (a file name starting with '.'). \"./001.png\" and \".hack/001.png\" stay. */\n",
            "\t\tif (name)\n",
            "\t\t{\n",
            "\t\t\tconst char *base = name, *p;\n",
            "\t\t\tfor (p = name; *p; p++)\n",
            "\t\t\t\tif (*p == '/' || *p == '\\\\')\n",
            "\t\t\t\t\tbase = p + 1;\n",
            "\t\t\tif (base[0] == '.' || !strncmp(name, \"__MACOSX/\", 9) || strstr(name, \"/__MACOSX/\"))\n",
            "\t\t\t\tcontinue;\n",
            "\t\t}\n",
        ),
    ),
];

fn main() {
    for v in ["INK_MUPDF_CACHE", "INK_MUPDF_TARBALL", "INK_MUPDF_LIB_DIR", "INK_MUPDF_JOBS", "INK_MUPDF_CC"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc/shim.c");
    println!("cargo:rerun-if-changed=csrc/shim.h");

    let target = env::var("TARGET").expect("TARGET");
    let host = env::var("HOST").expect("HOST");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    let cache = cache_dir(&manifest, &out);
    let tarball = tarball(&cache);
    let src = source_tree(&cache, &tarball);

    let zig = choose_compiler(&out, &target, &host);

    let mut cfg = cc::Build::new();
    cfg.opt_level(2).debug(false).pic(true).warnings(false).cargo_metadata(false);
    let compiler = cfg.get_compiler();
    let cc_args: Vec<String> = compiler.args().iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let cc_line = shell_join(&compiler.path().to_string_lossy(), &cc_args);
    let cc_id = compiler_id(&compiler.path().to_string_lossy(), zig.as_deref(), &out);
    let ar_line = match env_tool("AR", &target, &host) {
        Some(a) => a,
        None => cfg.get_archiver().get_program().to_string_lossy().into_owned(),
    };
    let ranlib = if host.contains("apple") && target.contains("apple") { Some("ranlib".to_string()) } else { None };

    let lib_dir = match env::var("INK_MUPDF_LIB_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => {
            // The compiler's identity (not its path: wrappers live in per-build OUT_DIRs),
            // so a zig build and a system cc build never share a cache directory.
            let mut hashed = vec![VERSION.to_string(), RECIPE.to_string(), target.clone(), cc_id.clone()];
            hashed.extend(cc_args.iter().cloned());
            hashed.extend(MAKE_FEATURES.iter().map(|s| s.to_string()));
            hashed.extend(XCFLAGS.iter().map(|s| s.to_string()));
            hashed.extend(PATCHES.iter().map(|p| p.2.to_string()));
            let key = format!("{VERSION}-{target}-{:016x}", fnv(&hashed.join("\n")));
            build_libs(&cache, &src, &key, &cc_line, &cc_id, &ar_line, ranlib.as_deref())
        }
    };

    // The shim: compiled with the same compiler, archived with the same archiver.
    let mut shim = cfg.clone();
    shim.file(manifest.join("csrc/shim.c")).include(src.join("include")).include(manifest.join("csrc"));
    let objects = shim.compile_intermediates();
    // The name must not start with "ink": zig cc reads `-link...` as one of its own options
    // (zig 0.17 then panics with "TODO").
    let shim_lib = out.join("libmupdf_shim_ink.a");
    let _ = fs::remove_file(&shim_lib);
    let objs: Vec<String> = objects.iter().map(|o| sh_quote(&o.to_string_lossy())).collect();
    run_sh(&format!("{ar_line} cr {} {}", sh_quote(&shim_lib.to_string_lossy()), objs.join(" ")), &out);
    if let Some(r) = &ranlib {
        run_sh(&format!("{r} {}", sh_quote(&shim_lib.to_string_lossy())), &out);
    }

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=mupdf_shim_ink");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=mupdf");
    println!("cargo:rustc-link-lib=static=mupdf-third");
    println!("cargo:rustc-env=INK_MUPDF_VERSION={VERSION}");
    println!("cargo:rustc-env=INK_MUPDF_LIBS={}", lib_dir.display());
    println!("cargo:include={}", src.join("include").display());
}

/// `CC_<target>`, `CC_<target_with_underscores>`, then `TARGET_CC`/`HOST_CC`, then `CC` for
/// native builds only (a host `CC` must not leak into a cross build).
fn env_tool(kind: &str, target: &str, host: &str) -> Option<String> {
    let mut keys = vec![format!("{kind}_{target}"), format!("{kind}_{}", target.replace('-', "_"))];
    if target == host {
        keys.push(format!("HOST_{kind}"));
        keys.push(kind.to_string());
    } else {
        keys.push(format!("TARGET_{kind}"));
    }
    for k in keys {
        println!("cargo:rerun-if-env-changed={k}");
        if let Ok(v) = env::var(&k) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Picks the C compiler for MuPDF and the shim and returns the zig version when zig is used
/// through this script's own wrapper.
///
/// - Cross builds for a musl target: `CC_<target>` when set (cargo zigbuild sets its zig
///   wrapper there), else this script's `zig cc` wrapper.
/// - `INK_MUPDF_CC=zig`: this script's `zig cc` wrapper, always (fails without zig).
/// - `INK_MUPDF_CC=system`: the cc crate's own choice (`CC_<target>`, `CC`, else `cc`).
/// - Unset, native build: an explicit `CC_<target>`/`HOST_CC`/`CC` wins; else `zig cc` when
///   `zig` runs; else the system compiler with a cargo warning.
fn choose_compiler(out: &Path, target: &str, host: &str) -> Option<String> {
    let mode = env::var("INK_MUPDF_CC").unwrap_or_default();
    let explicit = env_tool("CC", target, host).is_some();
    let use_zig = match mode.as_str() {
        "zig" => true,
        "system" => false,
        "" => {
            if target != host && target.contains("musl") {
                !explicit
            } else if explicit {
                false
            } else if zig_version().is_some() {
                true
            } else {
                println!("cargo:warning=ink-mupdf: zig not found, building MuPDF with the system C compiler");
                false
            }
        }
        other => panic!("ink-mupdf: INK_MUPDF_CC={other:?}: use zig or system"),
    };
    if !use_zig {
        return None;
    }
    let version = zig_version().unwrap_or_else(|| panic!("ink-mupdf: zig is needed for {target} but `zig version` does not run"));
    let (cc, ar) = zig_wrappers(out, target);
    let key = target.replace('-', "_");
    // SAFETY: build scripts are single-threaded at this point.
    env::set_var(format!("CC_{key}"), &cc);
    if mode == "zig" || env_tool("AR", target, host).is_none() {
        env::set_var(format!("AR_{key}"), &ar);
    }
    Some(version)
}

fn zig_version() -> Option<String> {
    let out = Command::new("zig").arg("version").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

/// The `zig cc -target` value for a Rust target triple.
fn zig_target(target: &str) -> Option<&'static str> {
    Some(match target {
        "armv7-unknown-linux-musleabihf" => "arm-linux-musleabihf -mcpu=generic+v7a+vfp3-d32+thumb2-neon",
        "aarch64-unknown-linux-musl" => "aarch64-linux-musl",
        "x86_64-unknown-linux-musl" => "x86_64-linux-musl",
        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",
        "x86_64-unknown-linux-gnu" => "x86_64-linux-gnu",
        "aarch64-apple-darwin" => "aarch64-macos",
        "x86_64-apple-darwin" => "x86_64-macos",
        _ => return None,
    })
}

/// Shell scripts in `out` that run `zig cc` / `zig ar` for `target`. The cc wrapper drops
/// the clang `--target=...` argument the cc crate adds (zig rejects Apple's `arm64` name and
/// takes its own `-target`).
fn zig_wrappers(out: &Path, target: &str) -> (PathBuf, PathBuf) {
    let zig_target = zig_target(target)
        .unwrap_or_else(|| panic!("ink-mupdf: no zig target for {target}; set INK_MUPDF_CC=system or CC_{target}"));
    let cc = out.join("zig-cc.sh");
    let ar = out.join("zig-ar.sh");
    write_exec(
        &cc,
        &format!(
            "#!/bin/sh\n# ink-mupdf: zig cc for {target}.\n# `zig cc --version` writes an empty a.o into the current directory: run it here.\ncase \" $* \" in *\" --version \"*) cd \"$(dirname \"$0\")\" || exit 1 ;; esac\nfor a in \"$@\"; do\n  shift\n  case \"$a\" in --target=*) ;; *) set -- \"$@\" \"$a\" ;; esac\ndone\nexec zig cc -target {zig_target} \"$@\"\n"
        ),
    );
    write_exec(&ar, "#!/bin/sh\nexec zig ar \"$@\"\n");
    (cc, ar)
}

/// What the compiler is, without its path: the zig version and target for this script's
/// wrapper, else the program's file name and the first line of its `--version`. Runs in
/// `out`: `zig cc --version` (also behind cargo zigbuild's wrapper) writes an empty `a.o`
/// into the current directory.
fn compiler_id(path: &str, zig: Option<&str>, out: &Path) -> String {
    let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let version = Command::new(path)
        .arg("--version")
        .current_dir(out)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().map(|l| l.trim().to_string()))
        .unwrap_or_default();
    match zig {
        Some(z) => format!("zig {z} ({name}: {version})"),
        None => format!("{name}: {version}"),
    }
}

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

fn cache_dir(manifest: &Path, out: &Path) -> PathBuf {
    if let Ok(d) = env::var("INK_MUPDF_CACHE") {
        return PathBuf::from(d);
    }
    for a in manifest.ancestors() {
        let sibling = a.join("inkreader-wt").join(".cache");
        if sibling.is_dir() {
            return sibling;
        }
        if a.file_name().is_some_and(|n| n == "inkreader-wt") && a.join(".cache").is_dir() {
            return a.join(".cache");
        }
    }
    // <target>/<profile>/build/ink-mupdf-<hash>/out -> <target>/mupdf-cache
    let target_dir = out.ancestors().nth(4).unwrap_or(out);
    target_dir.join("mupdf-cache")
}

fn tarball(cache: &Path) -> PathBuf {
    if let Ok(p) = env::var("INK_MUPDF_TARBALL") {
        let p = PathBuf::from(p);
        check_sha(&p);
        return p;
    }
    let dir = cache.join("mupdf");
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir {}: {e}", dir.display()));
    let path = dir.join(TARBALL);
    if path.is_file() {
        check_sha(&path);
        return path;
    }
    let tmp = dir.join(format!("{TARBALL}.part-{}", std::process::id()));
    let mut errors = Vec::new();
    for url in URLS {
        let st = Command::new("curl").args(["-fsSL", "--retry", "2", "-o"]).arg(&tmp).arg(url).status();
        match st {
            Ok(s) if s.success() => {
                check_sha(&tmp);
                fs::rename(&tmp, &path).expect("rename tarball");
                return path;
            }
            Ok(s) => errors.push(format!("{url}: curl {s}")),
            Err(e) => errors.push(format!("{url}: {e}")),
        }
    }
    let _ = fs::remove_file(&tmp);
    panic!("ink-mupdf: cannot download {TARBALL}: {}; put it into {}", errors.join("; "), dir.display());
}

fn check_sha(path: &Path) {
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output())
        .unwrap_or_else(|e| panic!("ink-mupdf: no shasum/sha256sum: {e}"));
    let text = String::from_utf8_lossy(&out.stdout);
    let got = text.split_whitespace().next().unwrap_or("");
    if got != SHA256 {
        panic!("ink-mupdf: {} has SHA-256 {got:?}, expected {SHA256}", path.display());
    }
}

/// Extracted and patched source tree (shared by every target; make only reads it).
fn source_tree(cache: &Path, tarball: &Path) -> PathBuf {
    let patch_hash = fnv(&PATCHES.iter().map(|p| format!("{}{}{}", p.0, p.1, p.2)).collect::<String>());
    let root = cache.join("mupdf").join("src");
    let dir = root.join(format!("mupdf-{VERSION}-{:08x}", patch_hash as u32));
    if dir.join("Makefile").is_file() {
        return dir;
    }
    fs::create_dir_all(&root).expect("mkdir src");
    let tmp = root.join(format!(".extract-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).expect("mkdir extract");
    // bsdtar (macOS) reads lzip; GNU tar needs the lzip program.
    let st = Command::new("tar").arg("-xf").arg(tarball).arg("-C").arg(&tmp).status().expect("run tar");
    if !st.success() {
        let st2 = Command::new("tar").arg("--lzip").arg("-xf").arg(tarball).arg("-C").arg(&tmp).status();
        if !st2.map(|s| s.success()).unwrap_or(false) {
            panic!("ink-mupdf: cannot extract {} (needs bsdtar or tar + lzip)", tarball.display());
        }
    }
    let inner = tmp.join(format!("mupdf-{VERSION}-source"));
    for (file, from, to) in PATCHES {
        let p = inner.join(file);
        let text = fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        assert_eq!(text.matches(from).count(), 1, "ink-mupdf: patch anchor not found exactly once in {file}");
        fs::write(&p, text.replacen(from, to, 1)).expect("write patch");
    }
    match fs::rename(&inner, &dir) {
        Ok(()) => {}
        Err(_) if dir.join("Makefile").is_file() => {} // another build won the race
        Err(e) => panic!("rename {} -> {}: {e}", inner.display(), dir.display()),
    }
    let _ = fs::remove_dir_all(&tmp);
    dir
}

fn build_libs(cache: &Path, src: &Path, key: &str, cc: &str, cc_id: &str, ar: &str, ranlib: Option<&str>) -> PathBuf {
    let root = cache.join("mupdf").join("build");
    let dir = root.join(key);
    let libs = ["libmupdf.a", "libmupdf-third.a"];
    if libs.iter().all(|l| dir.join(l).is_file()) {
        return dir;
    }
    fs::create_dir_all(&root).expect("mkdir build");
    let tmp = root.join(format!("{key}.tmp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let obj = tmp.join("obj");
    fs::create_dir_all(&obj).expect("mkdir obj");
    let jobs = env::var("INK_MUPDF_JOBS").or_else(|_| env::var("NUM_JOBS")).unwrap_or_else(|_| "2".into());
    let make = if Command::new("gmake").arg("--version").output().is_ok() { "gmake" } else { "make" };
    let mut cmd = Command::new(make);
    cmd.arg("-C").arg(src).arg(format!("-j{jobs}")).arg(format!("OUT={}", obj.display()));
    cmd.arg(format!("CC={cc}")).arg(format!("CXX={cc}")).arg(format!("AR={ar}"));
    if let Some(r) = ranlib {
        cmd.arg(format!("RANLIB={r}"));
    }
    cmd.args(MAKE_FEATURES).arg(format!("XCFLAGS={}", XCFLAGS.join(" "))).arg("libs");
    for v in ["MAKEFLAGS", "MFLAGS", "MAKELEVEL", "CFLAGS", "CPPFLAGS", "CXXFLAGS", "LDFLAGS", "XCFLAGS", "OS", "build"] {
        cmd.env_remove(v);
    }
    let t = std::time::Instant::now();
    let output = cmd.output().unwrap_or_else(|e| panic!("ink-mupdf: cannot run {make}: {e}"));
    let mut log = output.stdout.clone();
    log.extend_from_slice(&output.stderr);
    let _ = fs::write(tmp.join("build.log"), &log);
    if !output.status.success() {
        let text = String::from_utf8_lossy(&log);
        let tail: Vec<&str> = text.lines().rev().take(40).collect();
        panic!(
            "ink-mupdf: make libs failed ({}), log {}:\n{}",
            output.status,
            tmp.join("build.log").display(),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }
    let lib = tmp.join("lib");
    fs::create_dir_all(&lib).expect("mkdir lib");
    for l in libs {
        fs::copy(obj.join(l), lib.join(l)).unwrap_or_else(|e| panic!("copy {l}: {e}"));
    }
    fs::write(
        lib.join("BUILD.txt"),
        format!("mupdf {VERSION} {key}\nCC={cc}\nAR={ar}\ncompiler={cc_id}\nseconds={:.0}\n", t.elapsed().as_secs_f64()),
    )
    .expect("write BUILD.txt");
    let _ = fs::copy(tmp.join("build.log"), lib.join("build.log"));
    match fs::rename(&lib, &dir) {
        Ok(()) => {}
        Err(_) if libs.iter().all(|l| dir.join(l).is_file()) => {}
        Err(e) => panic!("rename {} -> {}: {e}", lib.display(), dir.display()),
    }
    let _ = fs::remove_dir_all(&tmp);
    dir
}

fn run_sh(line: &str, dir: &Path) {
    let st = Command::new("sh").arg("-c").arg(line).current_dir(dir).status().expect("run sh");
    assert!(st.success(), "ink-mupdf: `{line}` failed: {st}");
}

fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=+,:@%".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn shell_join(prog: &str, args: &[String]) -> String {
    let mut s = sh_quote(prog);
    for a in args {
        s.push(' ');
        s.push_str(&sh_quote(a));
    }
    s
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
