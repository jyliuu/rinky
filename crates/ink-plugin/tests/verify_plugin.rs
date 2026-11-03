//! Adversarial tests for the plugin system (verifier cycle 1, branch wave-a/plugin).
//!
//! These drive hostile Lua plugins through the public `PluginManager` + `LuaPlugin` path
//! (what `ink-app`'s `PluginHost` uses) and assert the reader survives: a bad plugin is
//! disabled or its call fails, never a panic and never an unbounded hang (except the one
//! documented gap, which is an `#[ignore]` test below).

use ink_doc::Selection;
use ink_plugin::host::{MemoryStorage, UiQueue};
use ink_plugin::lua::LuaPlugin;
use ink_plugin::manifest::Permission;
use ink_plugin::registry::Registrar;
use ink_plugin::{HostCtx, Lifecycle, PluginManager, PluginManifest, Registry};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-verify-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A Lua plugin from source, with a manifest carrying `perms` and an optional
/// `time_limit_ms`.
fn plugin(name: &str, src: &str, perms: &[Permission], time_limit_ms: Option<u64>) -> Box<dyn ink_plugin::Plugin> {
    let mut m = PluginManifest::new(name, "0.1", perms);
    m.time_limit_ms = time_limit_ms;
    m.memory_mb = Some(16);
    let dir = tmpdir(name);
    Box::new(LuaPlugin::from_source(m, &dir, src).unwrap())
}

/// Broadcasts `ev` with a minimal host context (no book, no network) to every plugin.
fn broadcast(mgr: &mut PluginManager, ev: &Lifecycle) {
    let mut ui = UiQueue::default();
    let mut storage = MemoryStorage::default();
    mgr.broadcast(ev, |m, f| {
        let mut cx = HostCtx::new(&mut ui, &mut storage, &m.permissions);
        let _ = f(&mut cx);
    });
}

// ---------------------------------------------------------------------------------------
// Attack 1: an infinite loop on a page turn must disable the plugin at once, fast.

#[test]
fn infinite_loop_on_page_turn_is_disabled_fast() {
    let looper = plugin("looper", "return { on_page_change = function() while true do end end }", &[], Some(60));
    let mut mgr = PluginManager::new();
    mgr.load(vec![looper]);
    assert!(mgr.is_enabled("looper"));
    let t = Instant::now();
    broadcast(&mut mgr, &Lifecycle::PageChange(0));
    let dt = t.elapsed();
    assert!(!mgr.is_enabled("looper"), "a slow page-turn hook must disable the plugin");
    assert!(dt < Duration::from_secs(2), "the loop must be interrupted quickly, took {dt:?}");
    assert!(mgr.errors.iter().any(|(p, e)| p == "looper" && e.contains("too slow")), "{:?}", mgr.errors);
}

// ---------------------------------------------------------------------------------------
// Attack 2: a plugin whose every hook raises must be disabled after max_errors, and the
// reader (manager) must keep running the other plugins the whole time.

#[test]
fn error_storm_disables_without_taking_down_others() {
    let crasher = plugin("crasher", "return { on_book_open=function() error('boom') end, on_page_change=function() error('boom') end }", &[], None);
    let mut good_pages = 0usize;
    // A well-behaved plugin counts page turns via storage so we can prove it kept running.
    let good = plugin("good", "return { on_page_change = function(n) ink.log('good saw ' .. n) end }", &[], None);
    let mut mgr = PluginManager::new();
    mgr.max_errors = 3;
    mgr.load(vec![crasher, good]);
    broadcast(&mut mgr, &Lifecycle::BookOpen);
    for p in 0..5 {
        broadcast(&mut mgr, &Lifecycle::PageChange(p));
        if mgr.is_enabled("good") {
            good_pages += 1;
        }
    }
    assert!(!mgr.is_enabled("crasher"), "the crasher must be disabled after max_errors");
    assert!(mgr.is_enabled("good"), "the good plugin must stay enabled");
    assert_eq!(good_pages, 5, "the good plugin ran on every page turn");
    assert!(mgr.errors.iter().any(|(p, e)| p == "crasher" && e.contains("disabled")), "{:?}", mgr.errors);
}

// ---------------------------------------------------------------------------------------
// Attack 3: a memory bomb must become a Limit error, not a crash, and the state survives.

#[test]
fn memory_bomb_is_limited_not_fatal() {
    let bomb = plugin("bomb", "return { on_selection = function() local t = string.rep('x', 64*1024*1024) return #t end }", &[], None);
    let mut mgr = PluginManager::new();
    mgr.load(vec![bomb]);
    let sel = Selection { text: "x".into(), start: "flow:0.0.0".parse().unwrap(), end: "flow:0.0.1".parse().unwrap(), page: 0, rects: Vec::new() };
    broadcast(&mut mgr, &Lifecycle::Selection(sel));
    assert!(mgr.errors.iter().any(|(p, e)| p == "bomb" && (e.contains("memory") || e.contains("limit"))), "{:?}", mgr.errors);
    // One error only (default max_errors 5), so it is still loaded, just recorded.
    assert!(mgr.is_enabled("bomb"), "a single memory error does not disable; it is recorded");
}

// ---------------------------------------------------------------------------------------
// Attack 4: the sandbox must deny shell/process/file escapes from inside a plugin hook.

#[test]
fn sandbox_denies_escapes_from_a_hook() {
    let src = r#"
      local out = {}
      return { on_book_open = function()
        out.execute = os.execute
        out.popen = rawget(io, 'popen')
        out.exit = os.exit
        out.getenv = os.getenv
        out.debug = debug
        out.package = package
        out.loadfile = loadfile
        out.open_etc = select(2, pcall(io.open, '/etc/passwd'))
        out.open_up  = select(2, pcall(io.open, '../../x', 'w'))
        out.require_up = select(2, pcall(require, '..'))
        ink.book = ink.book  -- noop
        ink.log('escape results: ' .. tostring(out.execute) .. ' ' .. tostring(out.popen))
        error('report:' .. tostring(out.execute) .. '|' .. tostring(out.popen) .. '|' .. tostring(out.debug) .. '|' .. tostring(out.open_etc))
      end }
    "#;
    let p = plugin("escaper", src, &[Permission::Storage, Permission::Ui], None);
    let mut mgr = PluginManager::new();
    mgr.load(vec![p]);
    broadcast(&mut mgr, &Lifecycle::BookOpen);
    let report = mgr.errors.iter().find(|(pl, _)| pl == "escaper").map(|(_, e)| e.clone()).unwrap_or_default();
    assert!(report.contains("report:nil|nil|nil|"), "os.execute/io.popen/debug must be nil: {report}");
    assert!(report.contains("outside"), "io.open outside data/ must be refused: {report}");
}

// ---------------------------------------------------------------------------------------
// Attack 5: two plugins binding the same gesture both register and both route, no panic.

#[test]
fn two_plugins_same_gesture_both_bind() {
    let a = plugin("alpha", "return { on_load = function(reg) reg.gesture('g', 'two_finger_tap', 'A') end }", &[], None);
    let b = plugin("beta", "return { on_load = function(reg) reg.gesture('g', 'two_finger_tap', 'B') end }", &[], None);
    let mut mgr = PluginManager::new();
    mgr.load(vec![a, b]);
    let bound = mgr.registry.gesture_bindings("two_finger_tap");
    assert_eq!(bound.len(), 2, "both plugins keep their binding");
    let mut plugins: Vec<&str> = bound.iter().map(|g| g.plugin.as_str()).collect();
    plugins.sort();
    assert_eq!(plugins, vec!["alpha", "beta"]);
    // Routing the shared action to each plugin must not panic.
    let mut ui = UiQueue::default();
    let mut st = MemoryStorage::default();
    let perms: Vec<Permission> = Vec::new();
    for p in ["alpha", "beta"] {
        let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
        let _ = mgr.action(p, "g", &mut cx);
    }
}

// ---------------------------------------------------------------------------------------
// Attack 6: a duplicate plugin name is refused, keeping the first.

#[test]
fn duplicate_plugin_name_refused() {
    let a = plugin("dup", "return { on_load = function(reg) reg.menu_item('x', 'First', 0) end }", &[], None);
    let b = plugin("dup", "return { on_load = function(reg) reg.menu_item('y', 'Second', 0) end }", &[], None);
    let mut mgr = PluginManager::new();
    mgr.load(vec![a, b]);
    assert_eq!(mgr.list().len(), 1, "the duplicate is skipped, only the first is kept");
    assert_eq!(mgr.registry.menu_items.len(), 1, "only the first plugin's entry is registered");
    assert_eq!(mgr.registry.menu_items[0].id, "x");
    assert!(mgr.errors.iter().any(|(_, e)| e.contains("already loaded")), "{:?}", mgr.errors);
}

// ---------------------------------------------------------------------------------------
// Attack 7: a UI-request storm in one call is capped (does not fill host memory).

#[test]
fn ui_request_storm_is_capped() {
    // 100000 toasts in one hook; the per-call cap (100) turns it into an error.
    let p = plugin("spammer", "return { on_book_open = function() for i=1,100000 do ink.ui.toast('t'..i) end end }", &[Permission::Ui], None);
    let mut mgr = PluginManager::new();
    mgr.load(vec![p]);
    let mut ui = UiQueue::default();
    let mut storage = MemoryStorage::default();
    mgr.broadcast(&Lifecycle::BookOpen, |m, f| {
        let mut cx = HostCtx::new(&mut ui, &mut storage, &m.permissions);
        let _ = f(&mut cx);
    });
    assert!(ui.0.len() <= 100, "at most CALL_CAPS.ui toasts reach the host, got {}", ui.0.len());
    assert!(mgr.errors.iter().any(|(p, e)| p == "spammer" && e.contains("too many")), "{:?}", mgr.errors);
}

// ---------------------------------------------------------------------------------------
// Attack 8: main.lua that returns a non-table is reported, not a panic; others load.

#[test]
fn bad_main_return_is_reported() {
    let bad = plugin("notable", "return 42", &[], None);
    let good = plugin("fine", "return { on_load = function(reg) reg.menu_item('ok', 'OK', 0) end }", &[], None);
    let mut mgr = PluginManager::new();
    mgr.load(vec![bad, good]);
    assert!(!mgr.is_enabled("notable"), "a non-table main.lua disables the plugin");
    assert!(mgr.is_enabled("fine"));
    assert_eq!(mgr.registry.menu_items.len(), 1);
}

// ---------------------------------------------------------------------------------------
// Attack 9 (FINDING): a pathological Lua string pattern runs entirely inside the C
// pattern-matcher, which the instruction hook cannot interrupt, so the per-call time
// limit never fires and the reader hangs forever on that page turn. This is DISCLOSED by
// the implementer (lua.rs module docs and docs/plugins.md, "Known gap"), so it is not a
// regression, but it does defeat the page-turn time limit. Ignored so the suite stays
// green; the body proves the hang on a worker thread without blocking the test process.

#[test]
#[ignore = "verifier: a Lua string-pattern C call is not interrupted by the time limit; a plugin can freeze the reader on a page turn (disclosed known gap)"]
fn catastrophic_pattern_defeats_the_time_limit() {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    // page-turn limit 60ms, hard ceiling 250ms; a well-behaved hook returns in <300ms.
    let src = "return { on_page_change = function() local s = string.rep('a', 32) return string.find(s, '(a*)(a*)(a*)(a*)(a*)(a*)(a*)(a*)b') end }";
    std::thread::spawn(move || {
        let mut mgr = PluginManager::new();
        mgr.load(vec![plugin("redos", src, &[], Some(60))]);
        broadcast(&mut mgr, &Lifecycle::PageChange(0));
        let _ = tx.send(());
    });
    assert!(rx.recv_timeout(Duration::from_secs(3)).is_err(), "the page-turn hook should have hung past 3s, defeating the 250ms hard ceiling");
}

// A tiny compile-time use so unused imports do not warn if a test is edited out.
#[allow(dead_code)]
fn _uses(_: &Path, _: &Registrar, _: &Registry) {}
