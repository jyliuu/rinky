//! Lua 5.4 host (mlua, vendored) for user plugins in `/mnt/us/rinky/plugins/<name>/`.
//!
//! - [`LuaHost`]: one sandboxed Lua state with a time limit per call and a heap limit.
//! - [`LuaPlugin`]: a plugin folder (`plugin.toml` + `main.lua`) as a [`Plugin`]; every
//!   hook of the Rust API is a function in the table `main.lua` returns, and the host
//!   services are the global `ink` table (see `docs/plugins.md` for the full list).
//!
//! Sandbox: `base` without `dofile`, `loadfile` and binary chunks in `load`;
//! `collectgarbage` limited to `count`/`collect`/`step`; `table`, `string`, `math`,
//! `utf8`, `coroutine`; `os` has only `time`, `clock`, `date` and `difftime`; `io` has
//! only `open`/`lines` on files inside the plugin's `data/` folder (needs the `storage`
//! permission) and `write` (to the log); `require` loads `.lua` files from the plugin
//! folder only. No `debug`, no `package`, no `os.execute`/`os.exit`/`io.popen`.
//!
//! Limits: every call runs under an instruction hook that raises `time limit exceeded`
//! once the call has used its time (time spent inside host functions such as
//! `ink.doc.search` does not count), or once the call has run [`hard_limit`] of wall
//! time (host functions included; also checked at every host call). `pcall`, `xpcall`
//! and `coroutine.resume` re-raise that error, so a plugin cannot catch it and loop on.
//! The heap limit turns runaway allocations (`string.rep("x", 1e9)`) into a Lua error;
//! per-call caps on UI requests, log lines and storage writes ([`CALL_CAPS`]) keep a
//! plugin from filling the reader's own memory or the disk. Known gap: one C library call
//! (for example a pathological `string.find` pattern) is not interrupted.

use crate::host::{data_path, HostCtx, UiHost};
use crate::manifest::{valid_name, PluginManifest};
use crate::net::{HttpEvent, StreamMode, WHOLE_LIMIT};
use crate::registry::{Registrar, StatusSlot};
use crate::{Plugin, PluginError, Result, UiRequest};
use ink_doc::{LinkTarget, Metadata, Position, Selection};
use mlua::{Function, HookTriggers, Lua, LuaOptions, MultiValue, StdLib, Table, Value, VmState};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Text of the error raised when a call runs out of time.
pub const TIME_LIMIT_MSG: &str = "time limit exceeded";

/// Shared between the instruction hook and the calls.
struct Limits {
    base: Instant,
    active: AtomicBool,
    start_ns: AtomicU64,
    limit_ns: AtomicU64,
    hard_ns: AtomicU64,
    host_ns: AtomicU64,
}

/// Wall-clock ceiling of a call whose Lua time limit is `limit`: time inside host
/// functions does not count against `limit`, but a call never runs longer than this.
pub fn hard_limit(limit: Duration) -> Duration {
    limit.max((limit * 5).min(Duration::from_secs(5)))
}

impl Limits {
    fn now_ns(&self) -> u64 {
        self.base.elapsed().as_nanos() as u64
    }
    fn exceeded(&self) -> bool {
        if !self.active.load(Ordering::Relaxed) {
            return false;
        }
        let wall = self.now_ns().saturating_sub(self.start_ns.load(Ordering::Relaxed));
        let used = wall.saturating_sub(self.host_ns.load(Ordering::Relaxed));
        used > self.limit_ns.load(Ordering::Relaxed) || wall > self.hard_ns.load(Ordering::Relaxed)
    }
    fn add_host(&self, d: Duration) {
        self.host_ns.fetch_add(d.as_nanos() as u64, Ordering::Relaxed);
    }
}

/// A sandboxed Lua state.
pub struct LuaHost {
    lua: Lua,
    limits: Arc<Limits>,
    /// Time limit per [`LuaHost::eval`] call.
    pub limit: Duration,
}

/// Formats an mlua error as `message` plus the traceback on later lines.
fn describe(e: &mlua::Error) -> String {
    match e {
        mlua::Error::CallbackError { cause, traceback } => {
            let inner = describe(cause);
            if traceback.is_empty() || inner.contains("stack traceback") {
                inner
            } else {
                format!("{inner}\n{traceback}")
            }
        }
        mlua::Error::RuntimeError(m) => m.clone(),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::MemoryError(m) => format!("memory limit: {m}"),
        mlua::Error::WithContext { context, cause } => format!("{context}: {}", describe(cause)),
        other => other.to_string(),
    }
}

/// Maps an mlua error to a [`PluginError`]: time and memory limits become
/// [`PluginError::Limit`], everything else [`PluginError::Lua`].
pub fn lua_err(e: mlua::Error) -> PluginError {
    let text = describe(&e);
    if text.contains(TIME_LIMIT_MSG) {
        return PluginError::Limit(TIME_LIMIT_MSG.into());
    }
    if matches!(e, mlua::Error::MemoryError(_)) || text.starts_with("memory limit") || text.contains("not enough memory") {
        return PluginError::Limit("memory limit exceeded".into());
    }
    PluginError::Lua(text)
}

/// Lua run inside the fresh state: wrappers that re-raise limit errors, a text-only
/// `load`, a limited `collectgarbage`.
const SANDBOX: &str = r#"
local LIMIT = ...
local tostring, error, sfind = tostring, error, string.find
local pack, unpack = table.pack, table.unpack
local function is_limit(e) return sfind(tostring(e), LIMIT, 1, true) ~= nil end
local _pcall, _xpcall, _resume = pcall, xpcall, coroutine.resume
pcall = function(f, ...)
  local r = pack(_pcall(f, ...))
  if not r[1] and is_limit(r[2]) then error(r[2], 0) end
  return unpack(r, 1, r.n)
end
xpcall = function(f, handler, ...)
  local limited
  local r = pack(_xpcall(f, function(e)
    if is_limit(e) then limited = e return e end
    return handler(e)
  end, ...))
  if limited ~= nil then error(limited, 0) end
  return unpack(r, 1, r.n)
end
coroutine.resume = function(co, ...)
  local r = pack(_resume(co, ...))
  if not r[1] and is_limit(r[2]) then error(r[2], 0) end
  return unpack(r, 1, r.n)
end
local _load = load
load = function(chunk, name, mode, env)
  if env == nil then return _load(chunk, name, "t") end
  return _load(chunk, name, "t", env)
end
local _gc = collectgarbage
collectgarbage = function(opt, ...)
  opt = opt or "collect"
  if opt == "count" or opt == "collect" or opt == "step" then return _gc(opt, ...) end
  error("collectgarbage('" .. tostring(opt) .. "') is not available to plugins", 2)
end
local _os = os
os = { time = _os.time, clock = _os.clock, date = _os.date, difftime = _os.difftime }
dofile, loadfile, require = nil, nil, nil
"#;

impl LuaHost {
    /// A new sandboxed state with a 2 s limit per [`LuaHost::eval`] call and no heap limit.
    pub fn new() -> Result<Self> {
        let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE | StdLib::OS;
        let lua = Lua::new_with(libs, LuaOptions::default()).map_err(lua_err)?;
        lua.load(SANDBOX).set_name("=sandbox").call::<()>(TIME_LIMIT_MSG).map_err(lua_err)?;
        let limits = Arc::new(Limits {
            base: Instant::now(),
            active: AtomicBool::new(false),
            start_ns: AtomicU64::new(0),
            limit_ns: AtomicU64::new(0),
            hard_ns: AtomicU64::new(0),
            host_ns: AtomicU64::new(0),
        });
        let l = limits.clone();
        // A global hook reaches coroutines too: Lua copies the hook into new threads.
        lua.set_global_hook(HookTriggers::new().every_nth_instruction(1000), move |_, _| {
            if l.exceeded() {
                Err(mlua::Error::RuntimeError(TIME_LIMIT_MSG.into()))
            } else {
                Ok(VmState::Continue)
            }
        })
        .map_err(lua_err)?;
        Ok(LuaHost { lua, limits, limit: Duration::from_secs(2) })
    }

    /// Sets the heap limit in bytes.
    pub fn set_memory_limit(&self, bytes: usize) -> Result<()> {
        self.lua.set_memory_limit(bytes).map(|_| ()).map_err(lua_err)
    }

    /// The Lua state (for hosts that add their own functions).
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Runs `f` under `limit`; nested calls keep the outer deadline.
    pub fn limited<R>(&self, limit: Duration, f: impl FnOnce() -> mlua::Result<R>) -> Result<R> {
        let outer = self.limits.active.swap(true, Ordering::Relaxed);
        if !outer {
            self.limits.start_ns.store(self.limits.now_ns(), Ordering::Relaxed);
            self.limits.host_ns.store(0, Ordering::Relaxed);
            self.limits.limit_ns.store(limit.as_nanos() as u64, Ordering::Relaxed);
            self.limits.hard_ns.store(hard_limit(limit).as_nanos() as u64, Ordering::Relaxed);
        }
        let r = f();
        if !outer {
            self.limits.active.store(false, Ordering::Relaxed);
        }
        r.map_err(lua_err)
    }

    /// Runs `code` under the time limit and returns its first result as a string.
    pub fn eval(&self, code: &str) -> Result<String> {
        let v = self.limited(self.limit, || self.lua.load(code).set_name("=plugin").eval::<Value>())?;
        Ok(match v {
            Value::Nil => "nil".into(),
            Value::Boolean(b) => b.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.to_string_lossy(),
            other => other.type_name().to_string(),
        })
    }

    /// Checks a plugin folder: parses `plugin.toml` and compiles the entry file in this
    /// state (nothing runs). Returns the manifest.
    pub fn load_plugin(&self, dir: &Path) -> Result<PluginManifest> {
        let (manifest, source) = read_plugin(dir)?;
        self.lua.load(&source).set_name(format!("@{}", manifest.entry_file())).into_function().map_err(lua_err)?;
        Ok(manifest)
    }

    /// Lua version string (e.g. `Lua 5.4`).
    pub fn version(&self) -> String {
        self.lua.globals().get::<String>("_VERSION").unwrap_or_default()
    }
}

/// Reads `dir/plugin.toml` and the entry file; the manifest name must equal the folder
/// name.
fn read_plugin(dir: &Path) -> Result<(PluginManifest, String)> {
    let text = std::fs::read_to_string(dir.join("plugin.toml")).map_err(|e| PluginError::Manifest(format!("{}: {e}", dir.join("plugin.toml").display())))?;
    let manifest = PluginManifest::parse(&text)?;
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if folder != manifest.name {
        return Err(PluginError::Manifest(format!("plugin name {:?} differs from its folder name {folder:?}", manifest.name)));
    }
    let entry = dir.join(manifest.entry_file());
    let source = std::fs::read_to_string(&entry).map_err(|e| PluginError::Manifest(format!("{}: {e}", entry.display())))?;
    Ok((manifest, source))
}

// ---------------------------------------------------------------------------------------
// Lua <-> JSON

/// Converts a Lua value to JSON. Tables with keys `1..n` only become arrays (an empty
/// table becomes `{}`), other tables objects with string keys; error values become their
/// message; functions and userdata are errors.
pub fn lua_to_json(v: &Value) -> mlua::Result<serde_json::Value> {
    to_json(v, 0)
}

fn to_json(v: &Value, depth: usize) -> mlua::Result<serde_json::Value> {
    use serde_json::Value as J;
    if depth > 64 {
        return Err(mlua::Error::RuntimeError("value nested too deeply (or a cycle)".into()));
    }
    Ok(match v {
        Value::Nil => J::Null,
        Value::Boolean(b) => J::Bool(*b),
        Value::Integer(i) => J::from(*i),
        Value::Number(n) => serde_json::Number::from_f64(*n).map(J::Number).ok_or_else(|| mlua::Error::RuntimeError(format!("{n} has no JSON form")))?,
        Value::String(s) => J::String(s.to_string_lossy()),
        // Error objects (from pcall) are kept as their message.
        Value::Error(e) => J::String(describe(e).lines().next().unwrap_or_default().to_string()),
        Value::Table(t) => {
            let len = t.raw_len();
            let mut count = 0usize;
            let mut obj = serde_json::Map::new();
            let mut all_seq = true;
            for pair in t.clone().pairs::<Value, Value>() {
                let (k, val) = pair?;
                count += 1;
                let key = match &k {
                    Value::Integer(i) => {
                        if *i < 1 || *i as usize > len {
                            all_seq = false;
                        }
                        i.to_string()
                    }
                    Value::String(s) => {
                        all_seq = false;
                        s.to_string_lossy()
                    }
                    Value::Number(n) => {
                        all_seq = false;
                        n.to_string()
                    }
                    other => return Err(mlua::Error::RuntimeError(format!("table key of type {} has no JSON form", other.type_name()))),
                };
                obj.insert(key, to_json(&val, depth + 1)?);
            }
            if all_seq && count == len && len > 0 {
                let mut arr = Vec::with_capacity(len);
                for i in 1..=len {
                    arr.push(to_json(&t.raw_get::<Value>(i)?, depth + 1)?);
                }
                J::Array(arr)
            } else {
                J::Object(obj)
            }
        }
        other => return Err(mlua::Error::RuntimeError(format!("a {} has no JSON form", other.type_name()))),
    })
}

/// Converts JSON to a Lua value (`null` becomes `nil`).
pub fn json_to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Value::Nil,
        J::Bool(b) => Value::Boolean(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        J::String(s) => Value::String(lua.create_string(s)?),
        J::Array(a) => {
            let t = lua.create_table_with_capacity(a.len(), 0)?;
            for (i, x) in a.iter().enumerate() {
                t.raw_set(i + 1, json_to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
        J::Object(o) => {
            let t = lua.create_table_with_capacity(0, o.len())?;
            for (k, x) in o {
                t.raw_set(k.as_str(), json_to_lua(lua, x)?)?;
            }
            Value::Table(t)
        }
    })
}

// ---------------------------------------------------------------------------------------
// The `ink` table

/// Persistent Lua side of the API: every function forwards to `H.call`, which the host
/// sets to a scoped Rust function during each call, so functions saved in locals keep
/// working in later calls.
const PRELUDE: &str = r##"
local H, name, version = ...
local pack, unpack, concat = table.pack, table.unpack, table.concat
local type, tostring, select, error = type, tostring, select, error
local function call(fname)
  return function(...)
    local c = H.call
    if c == nil then error("ink." .. fname .. " is only available while the plugin is running a hook", 2) end
    return c(fname, ...)
  end
end
local function group(prefix, names)
  local t = {}
  for _, n in ipairs(names) do t[n] = call(prefix .. "." .. n) end
  return t
end
ink = {
  plugin = { name = name, version = version },
  log = call("log"),
  register = group("register", { "menu_item", "selection_action", "gesture", "settings_page", "library_action", "status_widget", "control_tile", "library_entry", "action" }),
  doc = group("doc", { "metadata", "book_id", "toc", "search", "read_around", "links", "current_position", "selection", "page", "page_count", "page_text" }),
  ui = group("ui", { "toast", "notify", "status", "tile_state", "dialog", "menu", "view", "input" }),
  settings = group("settings", { "get", "set" }),
  book = group("book", { "get", "set" }),
  files = group("files", { "read", "write", "append", "list", "remove", "exists" }),
  http = group("http", { "request", "cancel" }),
  json = group("json", { "encode", "decode" }),
}
ink.http.get = function(url, callback) return ink.http.request({ url = url }, callback) end
print = function(...)
  local t = pack(...)
  for i = 1, t.n do t[i] = tostring(t[i]) end
  ink.log(concat(t, "\t", 1, t.n))
end
warn = function(...) ink.log("warning: " .. concat({ ... })) end

-- require: modules from the plugin folder only.
local loaded, source_of = {}, call("require")
require = function(mod)
  if type(mod) ~= "string" then error("require needs a module name", 2) end
  local v = loaded[mod]
  if v ~= nil then return v end
  local src, chunk = source_of(mod)
  local fn, err = load(src, "@" .. chunk, "t")
  if not fn then error(err, 2) end
  local r = fn(mod)
  if r == nil then r = true end
  loaded[mod] = r
  return r
end

-- io: files in the plugin's data folder only.
local files = ink.files
local function reader(data)
  local h = { pos = 1 }
  function h:read(...)
    local fmts = pack(...)
    if fmts.n == 0 then fmts = { "l", n = 1 } end
    local out = {}
    for i = 1, fmts.n do
      local f = fmts[i]
      if type(f) == "string" then f = f:gsub("^%*", "") end
      local v
      if f == "a" then
        v = data:sub(self.pos); self.pos = #data + 1
      elseif self.pos > #data then
        v = nil
      elseif f == "l" or f == "L" then
        local s = data:find("\n", self.pos, true)
        if s then v = data:sub(self.pos, f == "L" and s or s - 1); self.pos = s + 1
        else v = data:sub(self.pos); self.pos = #data + 1 end
      elseif f == "n" then
        local s, e = data:find("^%s*[-+]?%d*%.?%d+", self.pos)
        if s then v = tonumber(data:sub(s, e)); self.pos = e + 1 end
      elseif type(f) == "number" then
        v = data:sub(self.pos, self.pos + f - 1); self.pos = self.pos + f
      else
        error("bad read format " .. tostring(f), 2)
      end
      out[i] = v
      if v == nil then return unpack(out, 1, i) end
    end
    return unpack(out, 1, fmts.n)
  end
  function h:lines(fmt) return function() return self:read(fmt or "l") end end
  function h:seek(whence, off)
    whence, off = whence or "cur", off or 0
    local base = whence == "set" and 1 or whence == "end" and #data + 1 or self.pos
    self.pos = math.max(1, base + off)
    return self.pos - 1
  end
  function h:write() return nil, "file opened for reading" end
  function h:close() return true end
  return h
end
local function writer(fname, append)
  if not append then
    local ok, err = files.write(fname, "")
    if not ok then return nil, err end
  end
  local h = { buf = {} }
  function h:write(...)
    for i = 1, select("#", ...) do self.buf[#self.buf + 1] = tostring((select(i, ...))) end
    return self
  end
  function h:flush()
    if #self.buf > 0 then
      local data = concat(self.buf); self.buf = {}
      local ok, err = files.append(fname, data)
      if not ok then return nil, err end
    end
    return true
  end
  function h:close() return self:flush() end
  function h:read() return nil, "file opened for writing" end
  function h:lines() error("file opened for writing", 2) end
  function h:seek() return nil, "seek is not supported on files opened for writing" end
  return h
end
io = {}
function io.open(fname, mode)
  mode = (mode or "r"):gsub("b", "")
  if mode == "r" then
    local data, err = files.read(fname)
    if data == nil then return nil, err end
    return reader(data)
  elseif mode == "w" or mode == "a" then
    return writer(fname, mode == "a")
  end
  return nil, "mode " .. mode .. " is not supported (r, w, a)"
end
function io.lines(fname, ...)
  local f, err = io.open(fname, "r")
  if not f then error(err, 2) end
  return f:lines(...)
end
function io.write(...)
  local t = pack(...)
  for i = 1, t.n do t[i] = tostring(t[i]) end
  ink.log(concat(t, "", 1, t.n))
  return io
end
"##;

/// What a pending UI callback was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiKind {
    /// One answer (dialog, menu, input).
    Once,
    /// Link taps until `closed` (viewer).
    Viewer,
}

struct HttpCall {
    callback: Option<Function>,
    whole: bool,
    status: Option<u16>,
    headers: Vec<(String, String)>,
}

/// Plugin state the dispatcher changes.
struct State {
    manifest: PluginManifest,
    dir: PathBuf,
    handlers: HashMap<String, Function>,
    ui: HashMap<u32, (UiKind, Function)>,
    http: HashMap<u32, HttpCall>,
    next_ui: u32,
    /// What the current call has used.
    spent: Spent,
}

/// Per-call caps on host resources that the Lua heap limit does not cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallCaps {
    /// UI requests (toasts, dialogs, status texts, ...).
    pub ui: u32,
    /// `ink.log` / `print` lines.
    pub log: u32,
    /// Settings and per-book writes.
    pub stores: u32,
    /// Bytes written to files.
    pub file_bytes: u64,
}

/// The caps every call gets.
pub const CALL_CAPS: CallCaps = CallCaps { ui: 100, log: 200, stores: 500, file_bytes: 64 << 20 };
/// Longest text of a toast, notification or status widget (characters).
pub const MAX_SHORT_TEXT: usize = 1000;
/// Largest Markdown text of the viewer or dialog body (bytes).
pub const MAX_LONG_TEXT: usize = 4 << 20;
/// Longest log line (characters).
pub const MAX_LOG_LINE: usize = 2000;

#[derive(Clone, Copy, Debug, Default)]
struct Spent {
    ui: u32,
    log: u32,
    stores: u32,
    file_bytes: u64,
}

fn spend(counter: &mut u32, cap: u32, what: &str) -> mlua::Result<()> {
    *counter += 1;
    if *counter > cap {
        return Err(rt(format!("too many {what} in one call (at most {cap})")));
    }
    Ok(())
}

fn clip(text: String, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text
    } else {
        format!("{}...", text.chars().take(max_chars).collect::<String>())
    }
}

/// What a call can reach.
enum Cx<'a, 'b> {
    /// Running `main.lua` and `on_load`: registration.
    Load(&'a mut Registrar<'b>),
    /// A hook with host services.
    Host(&'a mut HostCtx<'b>),
}

/// A Lua plugin folder as a [`Plugin`].
pub struct LuaPlugin {
    host: LuaHost,
    state: State,
    /// The private table whose `call` field is the scoped dispatcher.
    h: Table,
    /// The compiled entry file; run in `on_load`.
    main: Option<Function>,
    /// The table `main.lua` returned (hooks).
    hooks: Option<Table>,
}

fn rt(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::RuntimeError(msg.into())
}

impl LuaPlugin {
    /// Loads `dir/plugin.toml` and compiles the entry file. Nothing runs until
    /// [`Plugin::on_load`].
    pub fn load(dir: &Path) -> Result<Self> {
        let (manifest, source) = read_plugin(dir)?;
        Self::from_source(manifest, dir, &source)
    }

    /// A plugin from a manifest and entry source (`dir` is where `require` and nothing
    /// else looks; tests pass any folder).
    pub fn from_source(manifest: PluginManifest, dir: &Path, source: &str) -> Result<Self> {
        let host = LuaHost::new()?;
        let lua = host.lua();
        let h = lua.create_table().map_err(lua_err)?;
        lua.load(PRELUDE)
            .set_name("=ink")
            .call::<()>((h.clone(), manifest.name.clone(), manifest.version.clone()))
            .map_err(lua_err)?;
        let main = lua.load(source).set_name(format!("@{}", manifest.entry_file())).into_function().map_err(lua_err)?;
        host.set_memory_limit(manifest.memory_limit())?;
        let state = State { manifest, dir: dir.to_path_buf(), handlers: HashMap::new(), ui: HashMap::new(), http: HashMap::new(), next_ui: 1, spent: Spent::default() };
        Ok(LuaPlugin { host, state, h, main: Some(main), hooks: None })
    }

    /// Runs `f` with `H.call` set to the dispatcher for `cx`, under the time limit.
    fn run<R>(&mut self, cx: Cx, page_turn: bool, f: impl FnOnce(&Lua) -> mlua::Result<R>) -> Result<R> {
        let limit = self.state.manifest.time_limit(page_turn);
        self.state.spent = Spent::default();
        let LuaPlugin { host, state, h, .. } = self;
        let limits = host.limits.clone();
        let lua = &host.lua;
        host.limited(limit, || {
            lua.scope(|scope| {
                let mut cx = cx;
                let disp = scope.create_function_mut(move |lua, (name, args): (String, MultiValue)| {
                    // A loop of host calls must stop too (the hook only sees Lua instructions).
                    if limits.exceeded() {
                        return Err(rt(TIME_LIMIT_MSG));
                    }
                    let t = Instant::now();
                    let r = dispatch(lua, state, &mut cx, &name, args.into_vec());
                    limits.add_host(t.elapsed());
                    r
                })?;
                h.raw_set("call", disp)?;
                let r = f(lua);
                h.raw_set("call", Value::Nil)?;
                r
            })
        })
    }

    fn hook(&self, name: &str) -> Option<Function> {
        self.hooks.as_ref().and_then(|t| t.get::<Option<Function>>(name).ok().flatten())
    }

    /// Calls hook `name` with `args` if the plugin defines it.
    fn call_hook(&mut self, cx: &mut HostCtx, name: &str, page_turn: bool, args: impl FnOnce(&Lua) -> mlua::Result<MultiValue>) -> Result<()> {
        let Some(f) = self.hook(name) else { return Ok(()) };
        self.run(Cx::Host(cx), page_turn, |lua| f.call::<()>(args(lua)?))
    }

    fn call_fn(&mut self, cx: &mut HostCtx, f: Function, args: impl FnOnce(&Lua) -> mlua::Result<MultiValue>) -> Result<()> {
        self.run(Cx::Host(cx), false, |lua| f.call::<()>(args(lua)?))
    }
}

fn mv(values: Vec<Value>) -> MultiValue {
    MultiValue::from_vec(values)
}

fn no_args(_: &Lua) -> mlua::Result<MultiValue> {
    Ok(MultiValue::new())
}

fn selection_table(lua: &Lua, s: &Selection) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("text", s.text.as_str())?;
    t.set("start", s.start.to_string())?;
    t.set("end", s.end.to_string())?;
    t.set("page", s.page + 1)?;
    Ok(t)
}

impl Plugin for LuaPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.state.manifest
    }

    fn kind(&self) -> &'static str {
        "lua"
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        let Some(main) = self.main.take() else { return Ok(()) };
        let hooks: Option<Table> = self.run(Cx::Load(reg), false, |lua| {
            let ret: Value = main.call(())?;
            let hooks = match ret {
                Value::Table(t) => Some(t),
                Value::Nil => lua.globals().get::<Option<Table>>("plugin")?,
                other => return Err(rt(format!("main.lua must return a table of hooks, not a {}", other.type_name()))),
            };
            if let Some(on_load) = hooks.as_ref().and_then(|t| t.get::<Option<Function>>("on_load").ok().flatten()) {
                let ink: Table = lua.globals().get("ink")?;
                on_load.call::<()>(ink.get::<Table>("register")?)?;
            }
            Ok(hooks)
        })?;
        self.hooks = hooks;
        Ok(())
    }

    fn on_book_open(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.call_hook(cx, "on_book_open", false, no_args)
    }

    fn on_page_change(&mut self, cx: &mut HostCtx, page: usize) -> Result<()> {
        self.call_hook(cx, "on_page_change", true, |_| Ok(mv(vec![Value::Integer(page as i64 + 1)])))
    }

    fn on_selection(&mut self, cx: &mut HostCtx, sel: &Selection) -> Result<()> {
        let sel = sel.clone();
        self.call_hook(cx, "on_selection", false, move |lua| Ok(mv(vec![Value::Table(selection_table(lua, &sel)?)])))
    }

    fn on_suspend(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.call_hook(cx, "on_suspend", false, no_args)
    }

    fn on_resume(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.call_hook(cx, "on_resume", false, no_args)
    }

    fn on_close(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.call_hook(cx, "on_close", false, no_args)
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        let id_s = id.to_string();
        if let Some(f) = self.state.handlers.get(id).cloned() {
            return self.call_fn(cx, f, move |lua| Ok(mv(vec![Value::String(lua.create_string(&id_s)?)])));
        }
        self.call_hook(cx, "on_action", false, move |lua| Ok(mv(vec![Value::String(lua.create_string(&id_s)?)])))
    }

    fn on_library_action(&mut self, cx: &mut HostCtx, id: &str, book: &Path) -> Result<()> {
        let (id_s, book_s) = (id.to_string(), book.display().to_string());
        let args = move |lua: &Lua| Ok(mv(vec![Value::String(lua.create_string(&book_s)?), Value::String(lua.create_string(&id_s)?)]));
        if let Some(f) = self.state.handlers.get(id).cloned() {
            return self.call_fn(cx, f, args);
        }
        if self.hook("on_library_action").is_some() {
            return self.call_hook(cx, "on_library_action", false, move |lua| {
                let mut a = args(lua)?.into_vec();
                a.swap(0, 1);
                Ok(mv(a))
            });
        }
        self.on_action(cx, id)
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        let r = result.to_string();
        let kind = self.state.ui.get(&id).map(|(k, _)| *k);
        let callback = match kind {
            Some(UiKind::Viewer) if result != "closed" => self.state.ui.get(&id).map(|(_, f)| f.clone()),
            Some(_) => self.state.ui.remove(&id).map(|(_, f)| f),
            None => None,
        };
        match callback {
            Some(f) => self.call_fn(cx, f, move |lua| Ok(mv(vec![Value::String(lua.create_string(&r)?)]))),
            None => self.call_hook(cx, "on_ui_result", false, move |lua| Ok(mv(vec![Value::Integer(id as i64), Value::String(lua.create_string(&r)?)]))),
        }
    }

    fn on_http(&mut self, cx: &mut HostCtx, id: u32, ev: &HttpEvent) -> Result<()> {
        let Some(call) = self.state.http.get_mut(&id) else {
            let ev = ev.clone();
            return self.call_hook(cx, "on_http", false, move |lua| Ok(mv(vec![Value::Integer(id as i64), Value::Table(http_event_table(lua, &ev, None, &[])?)])));
        };
        if let HttpEvent::Response { status, headers } = ev {
            call.status = Some(*status);
            call.headers = headers.clone();
            if call.whole {
                return Ok(());
            }
        }
        let (status, headers) = (call.status, call.headers.clone());
        let callback = call.callback.clone();
        if ev.is_last() {
            self.state.http.remove(&id);
        }
        let Some(f) = callback else { return Ok(()) };
        let ev = ev.clone();
        self.call_fn(cx, f, move |lua| Ok(mv(vec![Value::Table(http_event_table(lua, &ev, status, &headers)?)])))
    }
}

fn http_event_table(lua: &Lua, ev: &HttpEvent, status: Option<u16>, headers: &[(String, String)]) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let header_table = |hs: &[(String, String)]| -> mlua::Result<Table> {
        let h = lua.create_table()?;
        for (k, v) in hs {
            h.set(k.as_str(), v.as_str())?;
        }
        Ok(h)
    };
    match ev {
        HttpEvent::Response { status, headers } => {
            t.set("type", "response")?;
            t.set("status", *status)?;
            t.set("headers", header_table(headers)?)?;
        }
        HttpEvent::Chunk(data) => {
            t.set("type", "chunk")?;
            t.set("data", lua.create_string(data)?)?;
        }
        HttpEvent::Sse(e) => {
            t.set("type", "sse")?;
            t.set("event", if e.event.is_empty() { "message" } else { e.event.as_str() })?;
            t.set("data", e.data.as_str())?;
            t.set("id", e.id.clone())?;
        }
        HttpEvent::Done { body } => {
            t.set("type", "done")?;
            if let Some(b) = body {
                t.set("body", lua.create_string(b)?)?;
            }
        }
        HttpEvent::Error(m) => {
            t.set("type", "error")?;
            t.set("message", m.as_str())?;
        }
    }
    if !matches!(ev, HttpEvent::Response { .. }) {
        if let Some(s) = status {
            t.set("status", s)?;
            t.set("headers", header_table(headers)?)?;
        }
    }
    Ok(t)
}

// ---------------------------------------------------------------------------------------
// Dispatcher

/// Positional arguments of one API call.
struct Args<'n> {
    v: Vec<Value>,
    name: &'n str,
}

impl Args<'_> {
    fn get(&self, i: usize) -> &Value {
        self.v.get(i).unwrap_or(&Value::Nil)
    }
    fn bad(&self, i: usize, want: &str) -> mlua::Error {
        rt(format!("ink.{}: argument {} must be {want}, got {}", self.name, i + 1, self.get(i).type_name()))
    }
    fn str(&self, i: usize) -> mlua::Result<String> {
        match self.get(i) {
            Value::String(s) => Ok(s.to_string_lossy()),
            Value::Integer(n) => Ok(n.to_string()),
            Value::Number(n) => Ok(n.to_string()),
            _ => Err(self.bad(i, "a string")),
        }
    }
    fn bytes(&self, i: usize) -> mlua::Result<Vec<u8>> {
        match self.get(i) {
            Value::String(s) => Ok(s.as_bytes().to_vec()),
            Value::Integer(_) | Value::Number(_) => Ok(self.str(i)?.into_bytes()),
            _ => Err(self.bad(i, "a string")),
        }
    }
    fn opt_str(&self, i: usize) -> mlua::Result<Option<String>> {
        match self.get(i) {
            Value::Nil => Ok(None),
            _ => self.str(i).map(Some),
        }
    }
    fn opt_int(&self, i: usize) -> mlua::Result<Option<i64>> {
        match self.get(i) {
            Value::Nil => Ok(None),
            Value::Integer(n) => Ok(Some(*n)),
            Value::Number(n) if n.is_finite() => Ok(Some(*n as i64)),
            _ => Err(self.bad(i, "a number")),
        }
    }
    fn opt_fn(&self, i: usize) -> mlua::Result<Option<Function>> {
        match self.get(i) {
            Value::Nil => Ok(None),
            Value::Function(f) => Ok(Some(f.clone())),
            _ => Err(self.bad(i, "a function")),
        }
    }
    fn table(&self, i: usize) -> mlua::Result<Table> {
        match self.get(i) {
            Value::Table(t) => Ok(t.clone()),
            _ => Err(self.bad(i, "a table")),
        }
    }
}

/// Turns a host error into Lua results: `nil, message` for "not available" and
/// failures, a raised error for permission problems.
fn soft<T>(lua: &Lua, r: Result<T>, ok: impl FnOnce(&Lua, T) -> mlua::Result<MultiValue>) -> mlua::Result<MultiValue> {
    match r {
        Ok(v) => ok(lua, v),
        Err(e @ (PluginError::Unsupported(_) | PluginError::Failed(_))) => nil_msg(lua, e.to_string()),
        Err(e) => Err(rt(e.to_string())),
    }
}

fn one(v: impl mlua::IntoLua, lua: &Lua) -> mlua::Result<MultiValue> {
    Ok(mv(vec![v.into_lua(lua)?]))
}

/// The Lua convention for a soft failure: `nil, message`.
fn nil_msg(lua: &Lua, msg: impl AsRef<[u8]>) -> mlua::Result<MultiValue> {
    Ok(mv(vec![Value::Nil, Value::String(lua.create_string(msg)?)]))
}

fn host<'c, 'a, 'b>(cx: &'c mut Cx<'a, 'b>, fname: &str) -> mlua::Result<&'c mut HostCtx<'b>> {
    match cx {
        Cx::Host(h) => Ok(&mut **h),
        Cx::Load(_) => Err(rt(format!("ink.{fname} is not available while the plugin loads; call it from a hook"))),
    }
}

/// The UI for one request, counted against [`CallCaps::ui`].
fn ui<'c>(cx: &'c mut Cx<'_, '_>, spent: &mut Spent, fname: &str) -> mlua::Result<&'c mut dyn UiHost> {
    spent.ui += 1;
    if spent.ui > CALL_CAPS.ui {
        return Err(rt(format!("ink.{fname}: too many UI requests in one call (at most {})", CALL_CAPS.ui)));
    }
    host(cx, fname)?.ui().map_err(|e| rt(format!("ink.{fname}: {e}")))
}

fn pos_arg(cx: &mut Cx, a: &Args, i: usize) -> mlua::Result<Result<Position>> {
    Ok(match a.opt_str(i)? {
        Some(s) => s.parse::<Position>().map_err(|e| PluginError::Failed(format!("bad position {s:?}: {e}"))),
        None => match host(cx, a.name)?.document() {
            Ok(d) => d.current_position(),
            Err(e) => Err(e),
        },
    })
}

fn metadata_table(lua: &Lua, m: &Metadata) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("title", m.title.as_str())?;
    t.set("authors", lua.create_sequence_from(m.authors.iter().map(String::as_str))?)?;
    t.set("language", m.language.clone())?;
    t.set("identifier", m.identifier.clone())?;
    t.set("publisher", m.publisher.clone())?;
    t.set("description", m.description.clone())?;
    if let Some((name, index)) = &m.series {
        t.set("series", name.as_str())?;
        t.set("series_index", *index)?;
    }
    Ok(t)
}

fn next_ui(state: &mut State, kind: UiKind, f: Option<Function>) -> u32 {
    let id = state.next_ui;
    state.next_ui = state.next_ui.wrapping_add(1).max(1);
    if let Some(f) = f {
        state.ui.insert(id, (kind, f));
    }
    id
}

/// `(label, action)` pairs from `{ {"Label", "action"}, "Label", {label=, action=} }`.
fn pairs_arg(t: Option<Table>) -> mlua::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let Some(t) = t else { return Ok(out) };
    for v in t.sequence_values::<Value>() {
        match v? {
            Value::String(s) => {
                let s = s.to_string_lossy();
                out.push((s.clone(), s));
            }
            Value::Table(p) => {
                let label: String = p.get::<Option<String>>(1)?.or(p.get::<Option<String>>("label")?).unwrap_or_default();
                let action: String = p.get::<Option<String>>(2)?.or(p.get::<Option<String>>("action")?).unwrap_or_else(|| label.clone());
                out.push((label, action));
            }
            other => return Err(rt(format!("menu/dialog entries must be strings or {{label, action}} tables, got {}", other.type_name()))),
        }
    }
    Ok(out)
}

fn module_path(dir: &Path, module: &str) -> Result<(PathBuf, String)> {
    let ok = !module.is_empty() && module.split('.').all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    if !ok {
        return Err(PluginError::Permission(format!("module name {module:?} (use letters, digits, _ and dots)")));
    }
    let rel = format!("{}.lua", module.replace('.', "/"));
    Ok((dir.join(&rel), rel))
}

const MAX_WRITE: usize = 16 << 20;

fn dispatch(lua: &Lua, state: &mut State, cx: &mut Cx, name: &str, args: Vec<Value>) -> mlua::Result<MultiValue> {
    let a = Args { v: args, name };
    match name {
        "log" => {
            spend(&mut state.spent.log, CALL_CAPS.log, "log lines")?;
            let text: Vec<String> = a.v.iter().map(|v| match v {
                Value::String(s) => s.to_string_lossy(),
                Value::Integer(n) => n.to_string(),
                Value::Number(n) => n.to_string(),
                Value::Boolean(b) => b.to_string(),
                Value::Nil => "nil".into(),
                other => other.type_name().to_string(),
            }).collect();
            log::info!("plugin {}: {}", state.manifest.name, clip(text.join(" "), MAX_LOG_LINE));
            Ok(mv(vec![]))
        }
        "require" => {
            let module = a.str(0)?;
            let (path, rel) = module_path(&state.dir, &module).map_err(|e| rt(e.to_string()))?;
            let src = std::fs::read_to_string(&path).map_err(|e| rt(format!("require {module:?}: {}: {e}", path.display())))?;
            Ok(mv(vec![Value::String(lua.create_string(&src)?), Value::String(lua.create_string(&rel)?)]))
        }

        // Registration (main.lua top level and on_load).
        n if n.starts_with("register.") => {
            let Cx::Load(reg) = cx else {
                return Err(rt(format!("ink.{n} works only while the plugin loads (main.lua or on_load)")));
            };
            let id = a.str(0)?;
            // Each call takes (id, label or slot, ...) and an optional handler function at
            // the index it returns.
            let handler_at = match n {
                "register.menu_item" => {
                    let order = a.opt_int(2)?.unwrap_or(0) as i32;
                    reg.menu_item(&id, &a.str(1)?, order);
                    3
                }
                "register.selection_action" => {
                    reg.selection_action(&id, &a.str(1)?);
                    2
                }
                "register.gesture" => {
                    let label = a.opt_str(2)?.unwrap_or_else(|| id.clone());
                    reg.gesture(&id, &a.str(1)?, &label);
                    3
                }
                "register.action" => {
                    reg.action(&id, &a.str(1)?);
                    2
                }
                "register.settings_page" => {
                    reg.settings_page(&id, &a.str(1)?);
                    2
                }
                "register.library_action" => {
                    reg.library_action(&id, &a.str(1)?);
                    2
                }
                "register.control_tile" => {
                    let icon = a.opt_str(2)?.unwrap_or_else(|| "info".into());
                    reg.control_tile(&id, &a.str(1)?, &icon);
                    3
                }
                "register.library_entry" => {
                    let icon = a.opt_str(2)?.unwrap_or_else(|| "more".into());
                    reg.library_entry(&id, &a.str(1)?, &icon);
                    3
                }
                "register.status_widget" => {
                    let slot_name = a.opt_str(1)?.unwrap_or_else(|| "right".into());
                    let slot = StatusSlot::parse(&slot_name).ok_or_else(|| rt(format!("ink.{n}: slot must be left, center or right, not {slot_name:?}")))?;
                    reg.status_widget(&id, slot, &a.opt_str(2)?.unwrap_or_default());
                    return Ok(mv(vec![]));
                }
                _ => return Err(rt(format!("ink.{n} does not exist"))),
            };
            if let Some(f) = a.opt_fn(handler_at)? {
                // A settings page is opened as the action `settings:<id>`.
                let key = if n == "register.settings_page" { format!("settings:{id}") } else { id };
                state.handlers.insert(key, f);
            }
            Ok(mv(vec![]))
        }

        "json.encode" => {
            let j = lua_to_json(a.get(0))?;
            one(j.to_string(), lua)
        }
        "json.decode" => {
            let s = a.str(0)?;
            match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(j) => Ok(mv(vec![json_to_lua(lua, &j)?])),
                Err(e) => nil_msg(lua, format!("bad JSON: {e}")),
            }
        }

        // Document.
        "doc.metadata" => {
            let r = host(cx, name)?.document().map(|d| d.metadata());
            soft(lua, r, |lua, m| one(metadata_table(lua, &m)?, lua))
        }
        "doc.book_id" => {
            let r = host(cx, name)?.document().map(|d| d.book_id());
            soft(lua, r, |lua, id| one(id, lua))
        }
        "doc.toc" => {
            let r = host(cx, name)?.document().map(|d| d.toc());
            soft(lua, r, |lua, toc| {
                let t = lua.create_table()?;
                for (i, e) in toc.iter().enumerate() {
                    let row = lua.create_table()?;
                    row.set("title", e.title.as_str())?;
                    row.set("level", e.level)?;
                    row.set("position", e.position.to_string())?;
                    row.set("page", e.page.map(|p| p + 1))?;
                    t.raw_set(i + 1, row)?;
                }
                one(t, lua)
            })
        }
        "doc.search" => {
            let q = a.str(0)?;
            let limit = a.opt_int(1)?.unwrap_or(20).clamp(1, 500) as usize;
            let r = host(cx, name)?.document().and_then(|d| d.search(&q, limit));
            soft(lua, r, |lua, hits| {
                let t = lua.create_table()?;
                for (i, h) in hits.iter().enumerate() {
                    let row = lua.create_table()?;
                    row.set("position", h.position.to_string())?;
                    row.set("page", h.page.map(|p| p + 1))?;
                    row.set("excerpt", h.excerpt.as_str())?;
                    row.set("match_start", h.matched.start + 1)?;
                    row.set("match_end", h.matched.end)?;
                    t.raw_set(i + 1, row)?;
                }
                one(t, lua)
            })
        }
        "doc.read_around" => {
            let chars = a.opt_int(1)?.unwrap_or(1000).clamp(1, 100_000) as usize;
            let r = match pos_arg(cx, &a, 0)? {
                Ok(pos) => host(cx, name)?.document().and_then(|d| d.read_around(&pos, chars)),
                Err(e) => Err(e),
            };
            soft(lua, r, |lua, s| one(s, lua))
        }
        "doc.links" => {
            let r = match pos_arg(cx, &a, 0)? {
                Ok(pos) => host(cx, name)?.document().and_then(|d| d.links(&pos)),
                Err(e) => Err(e),
            };
            soft(lua, r, |lua, links| {
                let t = lua.create_table()?;
                for (i, l) in links.iter().enumerate() {
                    let row = lua.create_table()?;
                    row.set("text", l.text.as_str())?;
                    row.set("footnote", l.footnote)?;
                    match &l.target {
                        LinkTarget::Internal(p) => {
                            row.set("internal", true)?;
                            row.set("position", p.to_string())?;
                        }
                        LinkTarget::External(u) => {
                            row.set("internal", false)?;
                            row.set("url", u.as_str())?;
                        }
                    }
                    t.raw_set(i + 1, row)?;
                }
                one(t, lua)
            })
        }
        "doc.current_position" => {
            let r = host(cx, name)?.document().and_then(|d| d.current_position());
            soft(lua, r, |lua, p| one(p.to_string(), lua))
        }
        "doc.selection" => {
            let r = host(cx, name)?.document().map(|d| d.selection());
            soft(lua, r, |lua, s| match s {
                Some(s) => one(selection_table(lua, &s)?, lua),
                None => Ok(mv(vec![Value::Nil])),
            })
        }
        "doc.page" => {
            let r = host(cx, name)?.document().map(|d| d.current_page().map(|p| p + 1));
            soft(lua, r, |lua, p| one(p, lua))
        }
        "doc.page_count" => {
            let r = host(cx, name)?.document().map(|d| d.page_count());
            soft(lua, r, |lua, p| one(p, lua))
        }
        "doc.page_text" => {
            let page = a.opt_int(0)?;
            let r = host(cx, name)?.document().and_then(|d| {
                let page = match page {
                    Some(p) if p >= 1 => p as usize - 1,
                    Some(p) => return Err(PluginError::Failed(format!("no page {p}"))),
                    None => d.current_page().ok_or_else(|| PluginError::Unsupported("current page".into()))?,
                };
                d.page_text(page)
            });
            soft(lua, r, |lua, s| one(s, lua))
        }

        // UI.
        "ui.toast" => {
            let text = clip(a.str(0)?, MAX_SHORT_TEXT);
            ui(cx, &mut state.spent, name)?.toast(&text);
            Ok(mv(vec![]))
        }
        "ui.notify" => {
            let text = clip(a.str(0)?, MAX_SHORT_TEXT);
            ui(cx, &mut state.spent, name)?.notify(&text);
            Ok(mv(vec![]))
        }
        "ui.status" => {
            let (id, text) = (a.str(0)?, clip(a.opt_str(1)?.unwrap_or_default(), MAX_SHORT_TEXT));
            ui(cx, &mut state.spent, name)?.status(&id, &text);
            Ok(mv(vec![]))
        }
        "ui.tile_state" => {
            let (id, text) = (a.str(0)?, clip(a.opt_str(1)?.unwrap_or_default(), MAX_SHORT_TEXT));
            ui(cx, &mut state.spent, name)?.tile_state(&id, &text);
            Ok(mv(vec![]))
        }
        "ui.dialog" | "ui.menu" | "ui.view" | "ui.input" => {
            let opts = a.table(0)?;
            let f = a.opt_fn(1)?.or(opts.get::<Option<Function>>("callback")?);
            let title = clip(opts.get::<Option<String>>("title")?.unwrap_or_default(), MAX_SHORT_TEXT);
            let long = |t: String| -> mlua::Result<String> {
                if t.len() > MAX_LONG_TEXT {
                    return Err(rt(format!("ink.{name}: text larger than {MAX_LONG_TEXT} bytes")));
                }
                Ok(t)
            };
            let u = ui(cx, &mut state.spent, name)?;
            let (kind, req): (UiKind, Box<dyn FnOnce(u32) -> UiRequest>) = match name {
                "ui.dialog" => {
                    let body = long(opts.get::<Option<String>>("body")?.or(opts.get::<Option<String>>("text")?).unwrap_or_default())?;
                    let mut buttons = pairs_arg(opts.get("buttons")?)?;
                    if buttons.is_empty() {
                        buttons.push(("OK".into(), "ok".into()));
                    }
                    (UiKind::Once, Box::new(move |id| UiRequest::Dialog { id, title, body, buttons }))
                }
                "ui.menu" => {
                    let items = pairs_arg(opts.get("items")?)?;
                    if items.len() > 1000 {
                        return Err(rt("ink.ui.menu: more than 1000 items"));
                    }
                    (UiKind::Once, Box::new(move |id| UiRequest::Menu { id, title, items }))
                }
                "ui.view" => {
                    let markdown = long(opts.get::<Option<String>>("markdown")?.or(opts.get::<Option<String>>("text")?).unwrap_or_default())?;
                    // Updating an open viewer keeps its id and callback.
                    if let Some(id) = opts.get::<Option<u32>>("id")? {
                        u.request(UiRequest::TextViewer { id, title, markdown });
                        return one(id, lua);
                    }
                    (UiKind::Viewer, Box::new(move |id| UiRequest::TextViewer { id, title, markdown }))
                }
                _ => {
                    let prompt = opts.get::<Option<String>>("prompt")?.unwrap_or(title);
                    let initial = opts.get::<Option<String>>("text")?.or(opts.get::<Option<String>>("initial")?).unwrap_or_default();
                    (UiKind::Once, Box::new(move |id| UiRequest::TextInput { id, prompt, initial }))
                }
            };
            let id = next_ui(state, kind, f);
            u.request(req(id));
            one(id, lua)
        }

        // Storage.
        "settings.get" | "book.get" => {
            let key = a.str(0)?;
            let book = name == "book.get";
            let r = host(cx, name)?.storage().map(|s| if book { s.book_get(&key) } else { s.get(&key) });
            match r {
                Ok(Some(v)) => Ok(mv(vec![json_to_lua(lua, &v)?])),
                Ok(None) => Ok(mv(vec![Value::Nil])),
                Err(e) => Err(rt(format!("ink.{name}: {e}"))),
            }
        }
        "settings.set" | "book.set" => {
            spend(&mut state.spent.stores, CALL_CAPS.stores, "settings and book writes")?;
            let key = a.str(0)?;
            let value = lua_to_json(a.get(1))?;
            let book = name == "book.set";
            let r = host(cx, name)?.storage().and_then(|s| if book { s.book_set(&key, value) } else { s.set(&key, value) });
            match r {
                Ok(()) => one(true, lua),
                Err(e @ PluginError::Permission(_)) => Err(rt(format!("ink.{name}: {e}"))),
                Err(e) => nil_msg(lua, e.to_string()),
            }
        }
        "files.read" | "files.write" | "files.append" | "files.remove" | "files.exists" | "files.list" => {
            let fname = if name == "files.list" { a.opt_str(0)? } else { Some(a.str(0)?) };
            let hc = host(cx, name)?;
            let dir = match hc.storage() {
                Ok(s) => s.data_dir(),
                Err(e) => return Err(rt(format!("ink.{name}: {e}"))),
            };
            let path = match fname.as_deref() {
                Some(f) => data_path(&dir, f).map_err(|e| rt(format!("ink.{name}: {e}")))?,
                None => dir.clone(),
            };
            let io = |r: std::io::Result<MultiValue>| -> mlua::Result<MultiValue> {
                r.or_else(|e| nil_msg(lua, format!("{}: {e}", fname.as_deref().unwrap_or("data"))))
            };
            match name {
                "files.read" => match std::fs::read(&path) {
                    Ok(b) => one(lua.create_string(&b)?, lua),
                    Err(e) => io(Err(e)),
                },
                "files.exists" => one(path.exists(), lua),
                "files.remove" => io(std::fs::remove_file(&path).map(|_| mv(vec![Value::Boolean(true)]))),
                "files.list" => {
                    let mut names: Vec<String> = match std::fs::read_dir(&path) {
                        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| {
                            let n = e.file_name().to_string_lossy().into_owned();
                            if e.path().is_dir() { format!("{n}/") } else { n }
                        }).collect(),
                        Err(_) => Vec::new(),
                    };
                    names.sort();
                    one(lua.create_sequence_from(names)?, lua)
                }
                _ => {
                    let data = a.bytes(1)?;
                    if data.len() > MAX_WRITE {
                        return nil_msg(lua, "write larger than 16 MiB");
                    }
                    state.spent.file_bytes += data.len() as u64;
                    if state.spent.file_bytes > CALL_CAPS.file_bytes {
                        return Err(rt(format!("ink.{name}: more than {} MiB written in one call", CALL_CAPS.file_bytes >> 20)));
                    }
                    let append = name == "files.append";
                    io((|| {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        let mut f = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&path)?;
                        std::io::Write::write_all(&mut f, &data)?;
                        Ok(mv(vec![Value::Boolean(true)]))
                    })())
                }
            }
        }

        // Network.
        "http.request" => {
            let opts = a.table(0)?;
            let f = a.opt_fn(1)?.or(opts.get::<Option<Function>>("callback")?);
            let url: String = opts.get::<Option<String>>("url")?.ok_or_else(|| rt("ink.http.request: url is missing"))?;
            let mut req = ink_net::Request::get(url);
            req.method = match opts.get::<Option<String>>("method")?.unwrap_or_else(|| "GET".into()).to_ascii_uppercase().as_str() {
                "GET" => ink_net::Method::Get,
                "POST" => ink_net::Method::Post,
                "PUT" => ink_net::Method::Put,
                "DELETE" => ink_net::Method::Delete,
                other => return Err(rt(format!("ink.http.request: method {other} is not supported"))),
            };
            if let Some(h) = opts.get::<Option<Table>>("headers")? {
                for pair in h.pairs::<String, String>() {
                    let (k, v) = pair?;
                    req.headers.push((k, v));
                }
            }
            match opts.get::<Value>("body")? {
                Value::Nil => {}
                Value::String(s) => req.body = s.as_bytes().to_vec(),
                Value::Table(t) => {
                    req.body = lua_to_json(&Value::Table(t))?.to_string().into_bytes();
                    if !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                        req.headers.push(("Content-Type".into(), "application/json".into()));
                    }
                }
                other => return Err(rt(format!("ink.http.request: body must be a string or a table, not {}", other.type_name()))),
            }
            if let Some(t) = opts.get::<Option<f64>>("timeout")? {
                req.timeout = Duration::from_secs_f64(t.clamp(1.0, 600.0));
            }
            let stream = opts.get::<Option<String>>("stream")?;
            let mode = match stream.as_deref() {
                None => StreamMode::Whole { limit: opts.get::<Option<usize>>("limit")?.unwrap_or(WHOLE_LIMIT).min(64 << 20) },
                Some("chunks") => StreamMode::Chunks,
                Some("sse") => StreamMode::Sse,
                Some(other) => return Err(rt(format!("ink.http.request: stream must be \"chunks\" or \"sse\", not {other:?}"))),
            };
            let r = host(cx, name)?.start_request(req, mode);
            match r {
                Ok(id) => {
                    state.http.insert(id, HttpCall { callback: f, whole: matches!(mode, StreamMode::Whole { .. }), status: None, headers: Vec::new() });
                    one(id, lua)
                }
                Err(e @ PluginError::Permission(_)) => Err(rt(format!("ink.http.request: {e}"))),
                Err(e) => nil_msg(lua, e.to_string()),
            }
        }
        "http.cancel" => {
            let id = a.opt_int(0)?.unwrap_or(0) as u32;
            state.http.remove(&id);
            host(cx, name)?.cancel_request(id);
            Ok(mv(vec![]))
        }
        other => Err(rt(format!("ink.{other} does not exist"))),
    }
}

/// Every plugin folder under `root` (folders holding a `plugin.toml`), sorted by name.
pub fn plugin_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.join("plugin.toml").is_file()).collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    dirs
}

/// Loads every Lua plugin under `root`: `(plugins, errors)` where an error is
/// `(folder name, message)` for a plugin that could not be built.
pub fn load_dir(root: &Path) -> (Vec<LuaPlugin>, Vec<(String, String)>) {
    let mut plugins = Vec::new();
    let mut errors = Vec::new();
    for dir in plugin_dirs(root) {
        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !valid_name(&folder) {
            errors.push((folder.clone(), format!("folder name {folder:?} is not a valid plugin name (a-z, 0-9, _ and -)")));
            continue;
        }
        match LuaPlugin::load(&dir) {
            Ok(p) => plugins.push(p),
            Err(e) => errors.push((folder, e.to_string())),
        }
    }
    (plugins, errors)
}

#[cfg(test)]
mod tests;
