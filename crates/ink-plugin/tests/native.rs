//! Native plugin libraries loaded through the C ABI: the in-tree `sample-native` and the
//! `native-faulty` fixture, built by cargo as `cdylib`s next to this test binary.

use ink_plugin::host::{HostCtx, MemoryStorage, PluginStorage, UiQueue, UiRequest};
use ink_plugin::manifest::Permission;
use ink_plugin::native::{self, NativePlugin};
use ink_plugin::testing::{FakeBook, TempDir};
use ink_plugin::{PluginError, PluginManager};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The library of workspace crate `crate_name` built for this test run.
fn library(crate_name: &str) -> PathBuf {
    let deps = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    let lib = deps.join(format!("{}{}{}", std::env::consts::DLL_PREFIX, crate_name.replace('-', "_"), std::env::consts::DLL_SUFFIX));
    assert!(lib.is_file(), "{} is missing (built by cargo as a dev-dependency)", lib.display());
    lib
}

/// Installs the library of `crate_name` as plugin `name` in `root` with `perms`.
fn install(root: &Path, crate_name: &str, name: &str, perms: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let stem = format!("lib{}", crate_name.replace('-', "_"));
    std::fs::write(dir.join("plugin.toml"), format!("name = \"{name}\"\nversion = \"1\"\npermissions = [{perms}]\nnative = \"{stem}.so\"\n")).unwrap();
    std::fs::copy(library(crate_name), dir.join(format!("{stem}{}", std::env::consts::DLL_SUFFIX))).unwrap();
    dir
}

fn load(root: &Path) -> PluginManager {
    let (plugins, errors) = native::load_dir(root, |_| false);
    assert!(errors.is_empty(), "{errors:?}");
    let mut mgr = PluginManager::new();
    mgr.load(plugins);
    mgr
}

#[test]
fn the_sample_plugin_loads_registers_and_answers() {
    let root = TempDir::new("native-sample");
    install(&root, "sample-native", "sample-native", "\"document\", \"ui\", \"storage\"");
    let mut mgr = load(&root);
    assert_eq!(mgr.info()[0].kind, "native");
    assert!(mgr.errors.is_empty(), "{:?}", mgr.errors);
    assert_eq!(mgr.registry.menu_items.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["count"]);
    assert_eq!(mgr.tile_state("sample-native", "count").map(|t| t.text), Some("0 this session".into()));

    let (mut ui, mut st) = (UiQueue::default(), MemoryStorage::default());
    let mut book = FakeBook { page: Some(0), pages: Some(1), page_texts: vec!["one two three".into()], ..FakeBook::default() };
    let perms = [Permission::Document, Permission::Ui, Permission::Storage];
    let mut cx = HostCtx::new(&mut ui, &mut st, &perms).with_doc(Some(&mut book));
    mgr.action("sample-native", "count", &mut cx).unwrap();
    assert!(matches!(&ui.0[..], [UiRequest::TextViewer { id: 1, markdown, .. }] if markdown.starts_with("**3** words")), "{:?}", ui.0);
    assert_eq!(st.get("total"), Some(serde_json::json!(1)));
    assert_eq!(mgr.tile_state("sample-native", "count").map(|t| t.text), Some("1 this session".into()));
    let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
    let err = mgr.action("sample-native", "nope", &mut cx).unwrap_err();
    assert!(err.to_string().contains("unknown action nope"), "{err}");
    assert!(mgr.is_enabled("sample-native"), "an ordinary error does not turn it off");
}

#[test]
fn host_services_permissions_and_callbacks_cross_the_boundary() {
    let root = TempDir::new("native-services");
    install(&root, "native-faulty", "native-faulty", "\"ui\", \"storage\"");
    let mut mgr = load(&root);
    assert_eq!(mgr.registry.status_widgets.len(), 1);
    assert_eq!(mgr.registry.control_tiles[0].icon, "info", "an empty icon gets the default");

    let (mut ui, mut st) = (UiQueue::default(), MemoryStorage::default());
    let perms = [Permission::Ui, Permission::Storage];
    let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
    mgr.action("native-faulty", "ui", &mut cx).unwrap();
    assert!(matches!(&ui.0[0], UiRequest::Toast { text } if text == "hello from a library"));
    assert!(matches!(&ui.0[1], UiRequest::Menu { id: 1, items, .. } if items.len() == 2 && items[1] == ("Two".to_string(), "two".to_string())));
    assert!(matches!(&ui.0[2], UiRequest::Status { id, text } if id == "w" && text == "menu 1"));
    assert!(matches!(&ui.0[3], UiRequest::TextViewer { id: 2, .. }));
    assert_eq!(st.get("menu"), Some(serde_json::json!(1)));

    // Without the document permission, and without network.
    let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
    assert!(matches!(mgr.action("native-faulty", "page", &mut cx), Err(PluginError::Permission(_))));
    let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
    assert!(matches!(mgr.action("native-faulty", "denied", &mut cx), Err(PluginError::Permission(_))));

    ui.0.clear();
    let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
    mgr.ui_result("native-faulty", 1, "two", &mut cx).unwrap();
    mgr.http_event("native-faulty", 9, &ink_plugin::HttpEvent::Done { body: Some(b"ok".to_vec()) }, &mut cx).unwrap();
    assert_eq!(ui.0[0], UiRequest::Toast { text: "result 1=two".into() });
    assert!(matches!(&ui.0[1], UiRequest::Toast { text } if text.starts_with("http 9 Done") && text.contains("[111, 107]")), "{:?}", ui.0[1]);

    // Lookup, word bounds, annotations.
    assert_eq!(mgr.lookup_provider("zebra", None).as_deref(), Some("native-faulty"));
    assert_eq!(mgr.word_bounds("abc", 1, Some("en")), Some(1..2));
    assert_eq!(mgr.word_bounds("a", 0, None), None);
    let anns = mgr.text_annotators();
    let a = &anns[0].1;
    assert_eq!((a.id(), a.version()), ("faulty-marks", 7));
    assert_eq!(a.positions().len(), 2);
    let got = a.annotate("héllo", Some("en"));
    assert_eq!(got.len(), 1, "ranges past the end and empty ranges are dropped: {got:?}");
    assert_eq!((got[0].range.clone(), got[0].text.as_str()), (0..1, "1st"));
    assert_eq!(got[0].position, ink_plugin::AnnotationPosition::Under);
    assert!(mgr.is_enabled("native-faulty"));
}

#[test]
fn a_panic_inside_a_plugin_is_caught_and_turns_it_off() {
    let root = TempDir::new("native-panic");
    install(&root, "native-faulty", "native-faulty", "\"ui\"");
    let mut mgr = load(&root);
    let (mut ui, mut st) = (UiQueue::default(), MemoryStorage::default());
    let mut cx = HostCtx::new(&mut ui, &mut st, &[]);
    let err = mgr.action("native-faulty", "panic", &mut cx).unwrap_err();
    assert!(matches!(&err, PluginError::Crashed(m) if m.contains("the fixture panics on purpose")), "{err}");
    assert!(!mgr.is_enabled("native-faulty"), "turned off at once");
    assert!(mgr.registry.menu_items.is_empty());
    assert!(mgr.errors.iter().any(|(p, e)| p == "native-faulty" && e == "disabled: it crashed"), "{:?}", mgr.errors);
}

#[test]
fn a_panic_outside_the_hooks_turns_the_plugin_off_too() {
    // A panicking lookup claim: no claim, and the manager turns the plugin off.
    let root = TempDir::new("native-panic-claim");
    install(&root, "native-faulty", "native-faulty", "");
    let mut mgr = load(&root);
    assert_eq!(mgr.lookup_provider("boom", None), None);
    assert_eq!(mgr.disable_crashed(), ["native-faulty"]);
    assert!(!mgr.is_enabled("native-faulty"));
    assert!(mgr.errors.iter().any(|(_, e)| e == "disabled: it crashed (claims_lookup panicked)"), "{:?}", mgr.errors);

    // A panicking annotator on another thread: it returns nothing from then on, is not
    // called again, and the plugin is turned off at the next check.
    let root = TempDir::new("native-panic-annotator");
    install(&root, "native-faulty", "native-faulty", "");
    let mut mgr = load(&root);
    let a = mgr.text_annotators().remove(0).1;
    let worker = std::thread::spawn(move || (a.annotate("boom", None).len(), a.annotate("fine", None).len(), a.wants("fine", None)));
    assert_eq!(worker.join().unwrap(), (0, 0, false));
    assert_eq!(mgr.disable_crashed(), ["native-faulty"]);
    assert!(mgr.text_annotators().is_empty());
    assert!(mgr.disable_crashed().is_empty(), "once");
}

#[test]
fn broken_native_plugins_are_reported_not_loaded() {
    let root = TempDir::new("native-broken");
    // The library calls itself native-faulty.
    install(&root, "native-faulty", "renamed", "");
    // Not a library at all.
    let junk = root.join("junk");
    std::fs::create_dir_all(&junk).unwrap();
    std::fs::write(junk.join("plugin.toml"), "name = \"junk\"\nversion = \"1\"\nnative = \"libjunk.so\"\n").unwrap();
    std::fs::write(junk.join(format!("libjunk{}", std::env::consts::DLL_SUFFIX)), b"not a library").unwrap();
    // The library is missing.
    let gone = root.join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    std::fs::write(gone.join("plugin.toml"), "name = \"gone\"\nversion = \"1\"\nnative = \"libgone.so\"\n").unwrap();
    // A Lua plugin next to them is not touched by the native loader, and the Lua loader
    // skips the native folders.
    let lua = root.join("hello");
    std::fs::create_dir_all(&lua).unwrap();
    std::fs::write(lua.join("plugin.toml"), "name = \"hello\"\nversion = \"1\"\n").unwrap();
    std::fs::write(lua.join("main.lua"), "return {}").unwrap();

    let (plugins, mut errors) = native::load_dir(&root, |_| false);
    assert!(plugins.is_empty());
    errors.sort();
    let names: Vec<_> = errors.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["gone", "junk", "renamed"], "{errors:?}");
    assert!(errors[2].1.contains("calls itself \"native-faulty\""), "{}", errors[2].1);
    let (lua_plugins, lua_errors) = ink_plugin::lua::load_dir(&root);
    assert_eq!((lua_plugins.len(), lua_errors.len()), (1, 0), "{lua_errors:?}");
    assert!(NativePlugin::load(&lua).is_err());
    // A plugin that is turned off is listed without opening its library (the junk one
    // would fail to open).
    let (plugins, errors) = native::load_dir(&root, |n| n == "junk");
    assert_eq!(plugins.iter().map(|p| (p.manifest().name.as_str(), p.kind())).collect::<Vec<_>>(), [("junk", "native")]);
    assert_eq!(errors.len(), 2);
}

// ---------------------------------------------------------------------------------------
// Crashes, in a child process (this test binary run again with one test selected).

/// Runs test `name` of this binary in a child with `env`; returns its status and output.
fn child(name: &str, env: &[(&str, &str)]) -> (std::process::ExitStatus, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .envs(env.iter().copied())
        .output()
        .unwrap();
    (out.status, format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[cfg(unix)]
fn signal_of(s: std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&s)
}

/// The child half of the crash tests: loads the fixture with the fatal-signal handlers
/// and crash marker set up as the app does, then calls `action`.
#[test]
fn crash_child() {
    let (Ok(action), Ok(root)) = (std::env::var("NATIVE_CRASH_ACTION"), std::env::var("NATIVE_CRASH_ROOT")) else { return };
    let root = PathBuf::from(root);
    ink_device::trace::install_fatal_handlers();
    ink_device::trace::set_crash_file(&root.join("plugin-crashed"));
    if std::env::var_os("NATIVE_CRASH_EXITING").is_some() {
        ink_device::trace::set_exiting();
    }
    if action == "kill-abrt" {
        // SIGABRT from outside, not from abort(): the handler must still end the process.
        let me = std::process::id().to_string();
        let _ = Command::new("kill").args(["-ABRT", &me]).status();
        std::thread::sleep(std::time::Duration::from_secs(2));
        println!("child: still running after {action}");
        return;
    }
    let mut mgr = load(&root.join("plugins"));
    let (mut ui, mut st) = (UiQueue::default(), MemoryStorage::default());
    let _ = mgr.action("native-faulty", &action, &mut HostCtx::new(&mut ui, &mut st, &[]));
    println!("child: still running after {action}");
}

#[cfg(unix)]
#[test]
fn a_segfault_or_abort_in_a_plugin_ends_the_process_and_names_the_plugin() {
    for (action, signals) in [("segfault", [libc_sig::SEGV, libc_sig::BUS]), ("abort", [libc_sig::ABRT, libc_sig::ABRT])] {
        let root = TempDir::new(&format!("native-crash-{action}"));
        install(&root.join("plugins"), "native-faulty", "native-faulty", "");
        let (status, out) = child("crash_child", &[("NATIVE_CRASH_ACTION", action), ("NATIVE_CRASH_ROOT", root.to_str().unwrap())]);
        let sig = signal_of(status);
        assert!(sig.is_some_and(|s| signals.contains(&s)), "{action}: {status:?}\n{out}");
        assert!(!out.contains("still running"), "{out}");
        assert!(out.contains("inside native plugin native-faulty"), "{out}");
        assert_eq!(std::fs::read_to_string(root.join("plugin-crashed")).unwrap(), "native-faulty\n", "{out}");
    }
    // While rinky is exiting the marker says so (the launcher then does not restart).
    let root = TempDir::new("native-crash-exiting");
    install(&root.join("plugins"), "native-faulty", "native-faulty", "");
    let (status, out) = child("crash_child", &[("NATIVE_CRASH_ACTION", "abort"), ("NATIVE_CRASH_ROOT", root.to_str().unwrap()), ("NATIVE_CRASH_EXITING", "1")]);
    assert_eq!(signal_of(status), Some(libc_sig::ABRT), "{out}");
    assert_eq!(std::fs::read_to_string(root.join("plugin-crashed")).unwrap(), "native-faulty\nexiting\n");
    // A SIGABRT sent from outside still ends the process; outside a plugin call no
    // marker is written.
    let root = TempDir::new("native-crash-kill");
    install(&root.join("plugins"), "native-faulty", "native-faulty", "");
    let (status, out) = child("crash_child", &[("NATIVE_CRASH_ACTION", "kill-abrt"), ("NATIVE_CRASH_ROOT", root.to_str().unwrap())]);
    assert_eq!(signal_of(status), Some(libc_sig::ABRT), "{status:?}\n{out}");
    assert!(!out.contains("still running"), "{out}");
    assert!(!root.join("plugin-crashed").exists());
    // Outside a plugin call no marker is written.
    let root = TempDir::new("native-crash-none");
    install(&root.join("plugins"), "native-faulty", "native-faulty", "");
    let (status, _) = child("crash_child", &[("NATIVE_CRASH_ACTION", "panic"), ("NATIVE_CRASH_ROOT", root.to_str().unwrap())]);
    assert!(status.success());
    assert!(!root.join("plugin-crashed").exists());
}

/// Signal numbers (the test does not depend on libc).
#[cfg(unix)]
mod libc_sig {
    pub const ABRT: i32 = 6;
    pub const SEGV: i32 = 11;
    #[cfg(target_os = "macos")]
    pub const BUS: i32 = 10;
    #[cfg(not(target_os = "macos"))]
    pub const BUS: i32 = 7;
}

/// The child half of the foreign-exception test.
#[test]
fn unwind_child() {
    if std::env::var("NATIVE_UNWIND_CHILD").is_err() {
        return;
    }
    // SAFETY: the fixture library and its symbol of this type.
    unsafe {
        let lib = libloading::Library::new(library("native-faulty")).unwrap();
        let f: libloading::Symbol<extern "C-unwind" fn() -> u32> = lib.get(b"native_faulty_unwind_across").unwrap();
        let caught = std::panic::catch_unwind(|| f());
        println!("child: catch_unwind returned (caught: {})", caught.is_err());
    }
}

/// Why every exported hook catches panics inside the plugin: a panic that unwinds out
/// of a plugin library into rinky is a foreign exception to rinky's copy of the
/// standard library, which aborts even inside `catch_unwind`.
#[cfg(unix)]
#[test]
fn a_panic_unwinding_out_of_a_library_cannot_be_caught_by_the_host() {
    let (status, out) = child("unwind_child", &[("NATIVE_UNWIND_CHILD", "1")]);
    assert_eq!(signal_of(status), Some(libc_sig::ABRT), "{status:?}\n{out}");
    assert!(out.contains("Rust cannot catch foreign exceptions"), "{out}");
    assert!(!out.contains("catch_unwind returned"), "{out}");
}
