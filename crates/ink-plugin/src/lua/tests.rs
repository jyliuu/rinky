use super::*;
use crate::host::{DocumentAccess, MemoryStorage, UiQueue};
use crate::manifest::Permission;
use crate::net::NetWorker;
use crate::registry::Registry;
use crate::{Lifecycle, PluginManager};
use ink_doc::{Link, SearchHit, TocEntry};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-lua-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn pos(s: &str) -> Position {
    s.parse().unwrap()
}

/// A book of three "pages" of known text.
struct FakeDoc {
    page: usize,
    selection: Option<Selection>,
    /// `search` takes 20 ms.
    slow: bool,
}

const PAGES: [&str; 3] = ["It was the best of times", "it was the worst of times, it was the age of wisdom", "it was the age of foolishness"];

impl DocumentAccess for FakeDoc {
    fn metadata(&self) -> Metadata {
        Metadata { title: "A Tale".into(), authors: vec!["Dickens".into()], ..Metadata::default() }
    }
    fn book_id(&self) -> Option<String> {
        Some("0123456789abcdef0123456789abcdef".into())
    }
    fn toc(&self) -> Vec<TocEntry> {
        vec![TocEntry { title: "Book the First".into(), level: 0, position: pos("/body/DocFragment[2]/body/h1/text().0"), page: Some(0) }]
    }
    fn search(&mut self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        if self.slow {
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(PAGES
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.find(query).map(|at| (i, t, at)))
            .take(limit)
            .map(|(i, t, at)| SearchHit { position: pos(&format!("/body/DocFragment[2]/body/p[{}]/text().{at}", i + 1)), page: Some(i), excerpt: t.to_string(), matched: at..at + query.len() })
            .collect())
    }
    fn read_around(&mut self, _pos: &Position, chars: usize) -> Result<String> {
        Ok(PAGES.join(" ").chars().take(chars).collect())
    }
    fn links(&mut self, _pos: &Position) -> Result<Vec<Link>> {
        Ok(vec![Link { text: "note 1".into(), target: LinkTarget::External("https://e.org".into()), rect: None, footnote: true }])
    }
    fn current_position(&self) -> Result<Position> {
        Ok(pos(&format!("/body/DocFragment[2]/body/p[{}]/text().0", self.page + 1)))
    }
    fn selection(&self) -> Option<Selection> {
        self.selection.clone()
    }
    fn current_page(&self) -> Option<usize> {
        Some(self.page)
    }
    fn page_count(&self) -> Option<usize> {
        Some(PAGES.len())
    }
    fn page_text(&mut self, page: usize) -> Result<String> {
        PAGES.get(page).map(|s| s.to_string()).ok_or_else(|| PluginError::Failed("no page".into()))
    }
}

struct Env {
    ui: UiQueue,
    storage: MemoryStorage,
    doc: FakeDoc,
    net: NetWorker,
}

impl Env {
    fn new(dir: &Path) -> Self {
        Env {
            ui: UiQueue::default(),
            storage: MemoryStorage { dir: dir.join("data"), ..Default::default() },
            doc: FakeDoc { page: 0, selection: None, slow: false },
            net: NetWorker::new(crate::net::tests::fake()),
        }
    }

    /// Runs `f` with a full context for `m`.
    fn with<R>(&mut self, m: &PluginManifest, f: impl FnOnce(&mut HostCtx) -> R) -> R {
        let mut jobs = self.net.for_plugin(&m.name);
        let mut cx = HostCtx::new(&mut self.ui, &mut self.storage, &m.permissions)
            .with_doc(Some(&mut self.doc))
            .with_jobs(Some(&mut jobs))
            .with_hosts(&m.hosts);
        f(&mut cx)
    }
}

fn manifest(perms: &[Permission]) -> PluginManifest {
    PluginManifest::new("t", "0.1", perms)
}

fn plugin(src: &str, perms: &[Permission]) -> (LuaPlugin, Registry) {
    plugin_in(&tmpdir("src"), src, perms)
}

fn plugin_in(dir: &Path, src: &str, perms: &[Permission]) -> (LuaPlugin, Registry) {
    let mut p = LuaPlugin::from_source(manifest(perms), dir, src).unwrap();
    let mut registry = Registry::default();
    p.on_load(&mut Registrar { plugin: "t", registry: &mut registry }).unwrap();
    (p, registry)
}

/// A plugin with manifest `m`, loaded (registrations dropped).
fn loaded(m: &PluginManifest, dir: &Path, src: &str) -> LuaPlugin {
    let mut p = LuaPlugin::from_source(m.clone(), dir, src).unwrap();
    p.on_load(&mut Registrar { plugin: "t", registry: &mut Registry::default() }).unwrap();
    p
}

const ALL: [Permission; 4] = [Permission::Document, Permission::Ui, Permission::Storage, Permission::Network];

#[test]
fn eval_sandbox_and_time_limit() {
    let mut h = LuaHost::new().unwrap();
    assert_eq!(h.version(), "Lua 5.4");
    assert_eq!(h.eval("return 6 * 7").unwrap(), "42");
    assert_eq!(h.eval("return string.upper('ink')").unwrap(), "INK");
    assert_eq!(h.eval("return type(os.time())").unwrap(), "number");
    assert_eq!(h.eval("return type(os.date('%Y'))").unwrap(), "string");
    for blocked in [
        "return io.open('/etc/passwd')",
        "return os.execute('ls')",
        "return os.exit(1)",
        "return os.getenv('HOME')",
        "return os.remove('/tmp/x')",
        "return require('x')",
        "return dofile('/x')",
        "return loadfile('/x')",
        "return debug.getinfo(1)",
        "return package.loaded",
        "return collectgarbage('stop')",
    ] {
        assert!(h.eval(blocked).is_err(), "{blocked} must fail");
    }
    assert_eq!(h.eval("return load(string.dump(function() end))").unwrap(), "nil");
    assert_eq!(h.eval("return load('return 5')()").unwrap(), "5");
    assert_eq!(h.eval("return collectgarbage('count') > 0").unwrap(), "true");
    h.limit = Duration::from_millis(100);
    for spin in [
        "while true do end",
        "local co = coroutine.wrap(function() while true do end end) co()",
        "local co = coroutine.create(function() while true do end end) return coroutine.resume(co)",
        "while true do pcall(function() while true do end end) end",
        "while true do xpcall(function() while true do end end, function(e) return e end) end",
    ] {
        let t = Instant::now();
        let err = h.eval(spin).unwrap_err();
        assert!(matches!(err, PluginError::Limit(_)), "{spin}: {err}");
        assert!(t.elapsed() < Duration::from_secs(2), "{spin} took {:?}", t.elapsed());
    }
    // The state stays usable after an error; ordinary pcall still works.
    assert_eq!(h.eval("return 1 + 1").unwrap(), "2");
    assert_eq!(h.eval("return select(2, pcall(error, 'x', 0))").unwrap(), "x");
    // Heap limit.
    h.set_memory_limit(8 << 20).unwrap();
    let err = h.eval("return #string.rep('x', 64 * 1024 * 1024)").unwrap_err();
    assert!(matches!(err, PluginError::Limit(_)), "{err}");
    assert_eq!(h.eval("return #string.rep('x', 1000)").unwrap(), "1000");
}

#[test]
fn load_plugin_checks_folder() {
    let root = tmpdir("check");
    let dir = root.join("wc");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), "name = \"wc\"\nversion = \"0.1\"\n").unwrap();
    std::fs::write(dir.join("main.lua"), "return {}").unwrap();
    let h = LuaHost::new().unwrap();
    assert_eq!(h.load_plugin(&dir).unwrap().name, "wc");
    std::fs::write(dir.join("main.lua"), "return {").unwrap();
    assert!(matches!(h.load_plugin(&dir), Err(PluginError::Lua(m)) if m.contains("main.lua")));
    assert!(matches!(h.load_plugin(&root.join("missing")), Err(PluginError::Manifest(_))));
    // Name must match the folder.
    let other = root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("plugin.toml"), "name = \"wc\"\nversion = \"0.1\"\n").unwrap();
    std::fs::write(other.join("main.lua"), "return {}").unwrap();
    assert!(matches!(LuaPlugin::load(&other), Err(PluginError::Manifest(m)) if m.contains("folder")));
    // load_dir: one good, two bad, one ignored folder without plugin.toml.
    std::fs::write(dir.join("main.lua"), "return {}").unwrap();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    std::fs::create_dir_all(root.join("Bad Name")).unwrap();
    std::fs::write(root.join("Bad Name/plugin.toml"), "").unwrap();
    let (plugins, errors) = load_dir(&root);
    assert_eq!(plugins.iter().map(|p| p.manifest().name.clone()).collect::<Vec<_>>(), vec!["wc"]);
    let names: Vec<&str> = errors.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["Bad Name", "other"]);
    assert!(load_dir(&root.join("none")).0.is_empty());
}

const FULL: &str = r##"
local p = {}
local log = {}
local toast = ink.ui.toast            -- saved at load time, used later
ink.register.menu_item("count", "Count words", 5)
ink.register.selection_action("sel", "Count selection", function() p.sel_handler = true end)
ink.register.gesture("tft", "two_finger_tap", "Show timer")
ink.register.settings_page("main", "Word count", function() ink.ui.menu({ title = "Settings", items = { {"On", "on"}, "Off" } }, function(r) log[#log+1] = "menu:" .. r end) end)
ink.register.library_action("lib", "Reading time", function(book, id) ink.ui.toast(id .. " " .. book) end)
ink.register.status_widget("timer", "right", "0 min")
ink.register.control_tile("tile", "Words", "chat", function(id) ink.ui.tile_state(id, "tapped") end)
ink.register.library_entry("entry", "Count all", "chart", function(id) ink.ui.toast("entry " .. id) end)
ink.register.action("act", "Words: Count the page", function(id) ink.ui.toast("act " .. id) end)
function p.on_load(reg)
  reg.menu_item("late", "Late item", 9)
end
function p.on_book_open()
  local m = ink.doc.metadata()
  toast(m.title .. " by " .. m.authors[1])
  log[#log+1] = "open:" .. ink.doc.book_id()
end
function p.on_page_change(page)
  local text = ink.doc.page_text()
  local n = 0
  for _ in text:gmatch("%S+") do n = n + 1 end
  ink.ui.status("timer", page .. "/" .. ink.doc.page_count() .. " " .. n .. " words")
  ink.book.set("pages", (ink.book.get("pages") or 0) + 1)
end
function p.on_selection(sel) log[#log+1] = "sel:" .. sel.text .. "@" .. sel.page end
function p.on_suspend() log[#log+1] = "suspend" end
function p.on_resume() log[#log+1] = "resume" end
function p.on_close()
  ink.settings.set("log", log)
  local f = assert(io.open("sessions.txt", "a"))
  f:write("closed ", #log, "\n")
  f:close()
end
function p.on_action(id)
  if id == "count" then
    local toc = ink.doc.toc()
    local hits = ink.doc.search("age", 5)
    local links = ink.doc.links()
    local around = ink.doc.read_around(ink.doc.current_position(), 12)
    ink.ui.view({ title = "Stats", markdown = "# " .. toc[1].title .. "\n" .. #hits .. " hits, first on page " .. hits[1].page
      .. " at " .. hits[1].match_start .. "-" .. hits[1].match_end .. "\n" .. links[1].url .. " " .. around }, function(r) log[#log+1] = "view:" .. r end)
    ink.ui.dialog({ title = "Q", body = "Sure?", buttons = { {"Yes", "yes"}, {"No", "no"} } }, function(r) log[#log+1] = "dialog:" .. r end)
    ink.ui.input({ prompt = "Note" }, function(t) log[#log+1] = "input:" .. t end)
    ink.ui.notify("counted")
  elseif id == "tft" then
    toast("timer")
  end
end
return p
"##;

#[test]
fn full_plugin_through_the_manager() {
    let dir = tmpdir("full");
    let m = manifest(&ALL);
    let p = LuaPlugin::from_source(m.clone(), &dir, FULL).unwrap();
    let mut mgr = PluginManager::new();
    mgr.load(vec![Box::new(p)]);
    assert!(mgr.errors.is_empty(), "{:?}", mgr.errors);
    let r = &mgr.registry;
    assert_eq!(r.menu_items.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["count", "late"]);
    assert_eq!((r.selection_actions.len(), r.gestures.len(), r.settings_pages.len(), r.library_actions.len(), r.status_widgets.len()), (1, 1, 1, 1, 1));
    assert_eq!(r.gestures[0].gesture, "two_finger_tap");

    let mut env = Env::new(&dir);
    let broadcast = |env: &mut Env, mgr: &mut PluginManager, ev: Lifecycle| {
        mgr.broadcast(&ev, |m, f| {
            env.with(m, |cx| {
                let _ = f(cx);
            })
        });
    };
    broadcast(&mut env, &mut mgr, Lifecycle::BookOpen);
    env.doc.page = 1;
    broadcast(&mut env, &mut mgr, Lifecycle::PageChange(1));
    broadcast(&mut env, &mut mgr, Lifecycle::PageChange(1));
    let sel = Selection { start: pos("/body/p[1]/text().0"), end: pos("/body/p[1]/text().5"), text: "best".into(), page: 0, rects: vec![] };
    broadcast(&mut env, &mut mgr, Lifecycle::Selection(sel));
    broadcast(&mut env, &mut mgr, Lifecycle::Suspend);
    broadcast(&mut env, &mut mgr, Lifecycle::Resume);
    assert!(mgr.errors.is_empty(), "{:?}", mgr.errors);
    assert_eq!(env.ui.0[0], UiRequest::Toast { text: "A Tale by Dickens".into() });
    assert_eq!(env.ui.0[1], UiRequest::Status { id: "timer".into(), text: "2/3 12 words".into() });
    assert_eq!(env.storage.book.get("pages"), Some(&serde_json::json!(2)));

    env.ui.0.clear();
    env.with(&m, |cx| mgr.action("t", "count", cx)).unwrap();
    let reqs = std::mem::take(&mut env.ui.0);
    assert_eq!(reqs.len(), 4, "{reqs:?}");
    let UiRequest::TextViewer { id: view_id, markdown, .. } = &reqs[0] else { panic!("{reqs:?}") };
    assert_eq!(markdown, "# Book the First\n2 hits, first on page 2 at 39-41\nhttps://e.org It was the b");
    let UiRequest::Dialog { id: dialog_id, buttons, .. } = &reqs[1] else { panic!() };
    assert_eq!(buttons[1], ("No".to_string(), "no".to_string()));
    let UiRequest::TextInput { id: input_id, .. } = &reqs[2] else { panic!() };
    assert_eq!(reqs[3], UiRequest::Notification { text: "counted".into() });
    for (id, result) in [(*view_id, "https://e.org"), (*dialog_id, "no"), (*input_id, "a note"), (*view_id, "closed"), (*view_id, "late")] {
        env.with(&m, |cx| mgr.ui_result("t", id, result, cx)).unwrap();
    }
    env.with(&m, |cx| mgr.action("t", "settings:main", cx)).unwrap();
    let UiRequest::Menu { id: menu_id, items, .. } = &env.ui.0[0] else { panic!("{:?}", env.ui.0) };
    assert_eq!(items, &vec![("On".to_string(), "on".to_string()), ("Off".to_string(), "Off".to_string())]);
    let menu_id = *menu_id;
    env.with(&m, |cx| mgr.ui_result("t", menu_id, "on", cx)).unwrap();
    env.with(&m, |cx| mgr.action("t", "sel", cx)).unwrap();
    env.with(&m, |cx| mgr.library_action("t", "lib", Path::new("/mnt/us/Books/x.epub"), cx)).unwrap();
    assert_eq!(env.ui.0.last(), Some(&UiRequest::Toast { text: "lib /mnt/us/Books/x.epub".into() }));
    assert_eq!((mgr.registry.control_tiles.len(), mgr.registry.library_entries[0].icon.as_str()), (1, "chart"));
    env.ui.0.clear();
    env.with(&m, |cx| mgr.action("t", "tile", cx)).unwrap();
    assert_eq!(env.ui.0, vec![UiRequest::TileState { id: "tile".into(), text: "tapped".into() }]);
    env.with(&m, |cx| mgr.action("t", "entry", cx)).unwrap();
    assert_eq!(env.ui.0.last(), Some(&UiRequest::Toast { text: "entry entry".into() }));
    assert_eq!(mgr.registry.actions[0].label, "Words: Count the page");
    env.with(&m, |cx| mgr.action("t", "act", cx)).unwrap();
    assert_eq!(env.ui.0.last(), Some(&UiRequest::Toast { text: "act act".into() }));
    broadcast(&mut env, &mut mgr, Lifecycle::Close);
    assert!(mgr.errors.is_empty(), "{:?}", mgr.errors);
    let log = env.storage.settings.get("log").cloned().unwrap();
    assert_eq!(
        log,
        serde_json::json!([
            "open:0123456789abcdef0123456789abcdef",
            "sel:best@1",
            "suspend",
            "resume",
            "view:https://e.org",
            "dialog:no",
            "input:a note",
            "view:closed",
            "menu:on"
        ])
    );
    assert_eq!(std::fs::read_to_string(dir.join("data/sessions.txt")).unwrap(), "closed 9\n");
}

#[test]
fn errors_limits_and_permissions() {
    // Runtime error with file and line.
    let (mut p, _) = plugin("local p = {}\nfunction p.on_book_open()\n  local x = nil\n  return x.y\nend\nreturn p", &ALL);
    let dir = tmpdir("err");
    let mut env = Env::new(&dir);
    let m = manifest(&ALL);
    let err = env.with(&m, |cx| p.on_book_open(cx)).unwrap_err();
    assert!(matches!(&err, PluginError::Lua(msg) if msg.starts_with("main.lua:4:")), "{err}");

    // Endless page-turn hook: page-turn limit (500 ms default; 50 ms here).
    let mut m2 = manifest(&ALL);
    m2.time_limit_ms = Some(50);
    let mut p = loaded(&m2, &dir, "return { on_page_change = function() while true do end end }");
    let t = Instant::now();
    let err = env.with(&m2, |cx| p.on_page_change(cx, 0)).unwrap_err();
    assert!(matches!(err, PluginError::Limit(_)), "{err}");
    assert!(t.elapsed() < Duration::from_secs(1));

    // A host call does not eat the plugin's time: a slow search inside the limit is fine.
    // (Time inside host functions is subtracted.)

    // Without permissions.
    let (mut p, _) = plugin(
        "return { on_action = function(id)
            if id == 'doc' then return ink.doc.toc() end
            if id == 'ui' then return ink.ui.toast('x') end
            if id == 'store' then return ink.settings.set('a', 1) end
            if id == 'file' then return io.open('a.txt', 'w') end
            if id == 'net' then return ink.http.get('https://x.org/text', print) end
            if id == 'reg' then return ink.register.menu_item('x', 'X') end
            if id == 'nope' then return ink.nope() end
          end }",
        &[],
    );
    let none = manifest(&[]);
    for (id, want) in [("doc", "document"), ("ui", "ui"), ("store", "storage"), ("file", "storage"), ("net", "network"), ("reg", "only while the plugin loads"), ("nope", "nil")] {
        let err = env.with(&none, |cx| p.on_action(cx, id)).unwrap_err().to_string();
        assert!(err.contains(want), "{id}: {err}");
    }

    // Syntax error at load.
    assert!(matches!(LuaPlugin::from_source(manifest(&[]), &dir, "return {"), Err(PluginError::Lua(_))));
    // main.lua must return a table.
    let mut p = LuaPlugin::from_source(manifest(&[]), &dir, "return 5").unwrap();
    assert!(p.on_load(&mut Registrar { plugin: "t", registry: &mut Registry::default() }).is_err());
    // A global `plugin` table works too.
    let (mut p, _) = plugin("plugin = { on_action = function() ink.ui.toast('global') end }", &ALL);
    env.with(&m, |cx| p.on_action(cx, "x")).unwrap();
    assert_eq!(env.ui.0.last(), Some(&UiRequest::Toast { text: "global".into() }));
}

#[test]
fn files_io_require_and_json() {
    let dir = tmpdir("files");
    std::fs::write(dir.join("util.lua"), "return { twice = function(x) return 2 * x end }").unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/deep.lua"), "return 'deep'").unwrap();
    let src = r#"
      local util = require("util")
      return { on_action = function(id)
        local r = {}
        r.twice = util.twice(21)
        r.deep = require("lib.deep")
        r.same = require("util") == util
        r.bad_mod = select(2, pcall(require, "../x"))
        assert(ink.files.write("notes/a.txt", "line1\nline2\n42\n"))
        assert(ink.files.append("notes/a.txt", "tail"))
        local f = assert(io.open("notes/a.txt"))
        r.l1 = f:read("l")
        r.l2, r.n = f:read("L", "n")
        r.rest = f:read("a")
        f:close()
        local lines = {}
        for l in io.lines("notes/a.txt") do lines[#lines + 1] = l end
        r.lines = #lines
        r.list = ink.files.list()
        r.sub = ink.files.list("notes")
        r.exists = ink.files.exists("notes/a.txt")
        r.missing = select(2, ink.files.read("missing.txt")) ~= nil
        r.escape1 = select(2, pcall(io.open, "../../x", "w"))
        r.escape2 = select(2, pcall(io.open, "/etc/passwd"))
        r.removed = ink.files.remove("notes/a.txt")
        r.json = ink.json.encode({ a = { 1, 2, 3 }, b = "x", c = true, d = {} })
        local back = ink.json.decode('{"k":[1,{"z":null}],"f":1.5}')
        r.decoded = back.k[1] + back.f
        r.bad_json = select(2, ink.json.decode("{"))
        r.encode_fn = select(2, pcall(ink.json.encode, { f = print }))
        ink.settings.set("result", r)
      end }"#;
    let (mut p, _) = plugin_in(&dir, src, &ALL);
    let mut env = Env::new(&dir);
    let m = manifest(&ALL);
    env.with(&m, |cx| p.on_action(cx, "x")).unwrap();
    let r = env.storage.settings.get("result").cloned().unwrap();
    assert_eq!(r["twice"], 42);
    assert_eq!(r["deep"], "deep");
    assert_eq!(r["same"], true);
    assert!(r["bad_mod"].as_str().unwrap().contains("module name"));
    assert_eq!(r["l1"], "line1");
    assert_eq!(r["l2"], "line2\n");
    assert_eq!(r["n"], 42);
    assert_eq!(r["rest"], "\ntail");
    assert_eq!(r["lines"], 4);
    assert_eq!(r["list"], serde_json::json!(["notes/"]));
    assert_eq!(r["sub"], serde_json::json!(["a.txt"]));
    assert_eq!(r["exists"], true);
    assert_eq!(r["missing"], true);
    assert!(r["escape1"].as_str().unwrap().contains("outside"), "{r}");
    assert!(r["escape2"].as_str().unwrap().contains("outside"), "{r}");
    assert_eq!(r["removed"], true);
    let j: serde_json::Value = serde_json::from_str(r["json"].as_str().unwrap()).unwrap();
    assert_eq!(j, serde_json::json!({ "a": [1, 2, 3], "b": "x", "c": true, "d": {} }));
    assert_eq!(r["decoded"], 2.5);
    assert!(r["bad_json"].as_str().unwrap().contains("bad JSON"));
    assert!(r["encode_fn"].as_str().unwrap().contains("function"));
    assert!(!dir.join("data/notes/a.txt").exists());
}

#[test]
fn json_of_nested_arrays_is_linear() {
    // Each level of arrays used to be converted twice (as an object, then as an array),
    // so a tree of depth 14 took 37 s on the host (release) inside one host call, where
    // the Lua time limit does not count.
    let h = LuaHost::new().unwrap();
    let tree: Value = h.lua().load("local function t(d) if d == 0 then return 1 end return { t(d - 1), t(d - 1) } end return t(14)").eval().unwrap();
    let t = Instant::now();
    let j = lua_to_json(&tree).unwrap();
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(j[1][0][1][0][1][0][1][0][1][0][1][0][1][0], 1);
    let mixed: Value = h.lua().load("return { 1, 2, x = { 3 }, [1.5] = true }").eval().unwrap();
    assert_eq!(lua_to_json(&mixed).unwrap(), serde_json::json!({ "1": 1, "2": 2, "x": [3], "1.5": true }));
}

#[test]
fn http_streaming_through_on_http() {
    let dir = tmpdir("http");
    let src = r#"
      local got = {}
      return { on_action = function(id)
        if id == "go" then
          ink.http.request({ url = "https://x.org/sse", stream = "sse" }, function(ev)
            got[#got + 1] = ev.type .. ":" .. (ev.data or ev.status or "")
          end)
          ink.http.get("https://x.org/text", function(ev)
            got[#got + 1] = "whole:" .. ev.type .. ":" .. ev.status .. ":" .. ev.body
          end)
          ink.http.request({ url = "https://x.org/missing" }, function(ev) got[#got + 1] = ev.type end)
        elseif id == "deny" then
          ink.http.request({ url = "https://other.org/" }, print)
        else
          ink.settings.set("got", got)
        end
      end }"#;
    let mut m = manifest(&ALL);
    m.hosts = vec!["x.org".into()];
    let mut p = loaded(&m, &dir, src);
    let mut env = Env::new(&dir);
    let err = env.with(&m, |cx| p.on_action(cx, "deny")).unwrap_err();
    assert!(err.to_string().contains("not in hosts"), "{err}");
    env.with(&m, |cx| p.on_action(cx, "go")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while env.net.in_flight() > 0 && Instant::now() < deadline {
        for (plugin, id, ev) in env.net.wait(Duration::from_millis(50)) {
            assert_eq!(plugin, "t");
            env.with(&m, |cx| p.on_http(cx, id, &ev)).unwrap();
        }
    }
    env.with(&m, |cx| p.on_action(cx, "done")).unwrap();
    let mut got: Vec<String> = serde_json::from_value(env.storage.settings["got"].clone()).unwrap();
    got.sort();
    assert_eq!(got, vec!["done:200", "error", "response:200", "sse:one", "sse:two", "whole:done:200:hello streaming world"]);
    m.hosts.clear();
    assert!(env.with(&m, |cx| p.on_action(cx, "deny")).is_ok(), "any host when hosts is empty");
}

#[test]
fn host_calls_and_host_memory_are_bounded() {
    let dir = tmpdir("bounded");
    let mut env = Env::new(&dir);
    let mut m = manifest(&ALL);
    m.time_limit_ms = Some(100);
    let src = r#"return { on_action = function(id)
        if id == "search" then while true do ink.doc.search("age", 1) end end
        if id == "psearch" then while true do pcall(ink.doc.search, "age", 1) end end
        if id == "toasts" then while true do ink.ui.toast(string.rep("x", 4096)) end end
        if id == "logs" then while true do print(string.rep("y", 10000)) end end
        if id == "stores" then local i = 0 while true do i = i + 1 ink.settings.set("k", i) end end
        if id == "big" then ink.ui.view({ title = "t", markdown = string.rep("z", 5 * 1024 * 1024) }) end
        if id == "fine" then for i = 1, 50 do ink.ui.status("w", "x") end ink.ui.toast(string.rep("a", 3000)) end
      end }"#;
    let mut p = loaded(&m, &dir, src);
    env.doc.slow = true;
    for id in ["search", "psearch"] {
        let t = Instant::now();
        let err = env.with(&m, |cx| p.on_action(cx, id)).unwrap_err();
        assert!(matches!(err, PluginError::Limit(_)), "{id}: {err}");
        assert!(t.elapsed() < Duration::from_millis(1500), "{id}: a loop of slow host calls ran {:?}", t.elapsed());
        assert!(t.elapsed() >= hard_limit(Duration::from_millis(100)), "{id}: stopped before the wall-clock ceiling");
    }
    env.doc.slow = false;
    // The caps, with time to reach them (slow emulated ARM included).
    m.time_limit_ms = Some(5000);
    let mut p = loaded(&m, &dir, src);
    for (id, want) in [("toasts", "too many UI requests"), ("logs", "too many log lines"), ("stores", "too many settings and book writes"), ("big", "larger than")] {
        env.ui.0.clear();
        let err = env.with(&m, |cx| p.on_action(cx, id)).unwrap_err().to_string();
        assert!(err.contains(want), "{id}: {err}");
        assert!(env.ui.0.len() <= CALL_CAPS.ui as usize);
    }
    env.ui.0.clear();
    env.with(&m, |cx| p.on_action(cx, "fine")).unwrap();
    assert_eq!(env.ui.0.len(), 51);
    let UiRequest::Toast { text } = &env.ui.0[50] else { panic!() };
    assert_eq!(text.chars().count(), MAX_SHORT_TEXT + 3, "long toasts are clipped");
    // The counters start again with every call.
    env.with(&m, |cx| p.on_action(cx, "fine")).unwrap();
    assert_eq!(hard_limit(Duration::from_millis(500)), Duration::from_millis(2500));
    assert_eq!(hard_limit(Duration::from_secs(1)), Duration::from_secs(5));
    assert_eq!(hard_limit(Duration::from_secs(10)), Duration::from_secs(10));
}
