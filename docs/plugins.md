# Plugins

rinky has one plugin API. First-party plugins are Rust crates compiled into the
binary; user plugins are Lua 5.4 scripts in a folder on the e-reader. Both implement the same
`ink_plugin::Plugin` trait (a Lua plugin is a `ink_plugin::lua::LuaPlugin`), go through the
same `PluginManager`, registry and host services, and get the same permissions checks.

| Part | Where |
| --- | --- |
| API: trait, registry, host services, manifest, Lua host, Markdown parser, network worker | `crates/ink-plugin` |
| App side: loading, storage on disk, network, UI presenter (ink-ui widgets) | `crates/ink-app/src/plugin_host.rs` |
| Sample Lua plugin (word count and reading timer) | `plugins/sample-lua/plugin.toml`, `main.lua` |
| Lua plugins on the device | `/mnt/us/rinky/plugins/<name>/` (Kobo: `/mnt/onboard/.adds/rinky/plugins/<name>/`) |
| Compiled-in Rust plugins | `plugins/<name>/` crates, one line each in `crates/ink-app/src/plugins.rs` (kosync, opds, calibre, dictionary, stats, insightful, wikipedia, translate, usbnet, ssh) |

Contents: [Lua quick start](#lua-quick-start) - [plugin.toml](#plugintoml) -
[Hooks](#hooks) - [Registries](#registries) - [The `ink` table](#the-ink-table) -
[Storage](#storage) - [Network](#network) - [Limits, sandbox, errors](#limits-sandbox-and-errors) -
[Rust plugins](#rust-plugins) - [For screen authors](#for-screen-authors) - [Testing](#testing)

## Lua quick start

A plugin is a folder with two files. The folder name is the plugin name.

```text
/mnt/us/rinky/plugins/hello/
  plugin.toml
  main.lua
```

```toml
# plugin.toml
name = "hello"            # must equal the folder name
version = "0.1.0"
description = "Says hello"
permissions = ["ui"]
```

```lua
-- main.lua: return a table of hooks
local p = {}

function p.on_load(reg)
  reg.menu_item("hi", "Say hello", 10)        -- appears in the reader menu
end

function p.on_book_open()
  ink.ui.toast("Hello")
end

function p.on_action(id)
  if id == "hi" then ink.ui.toast("Hello again") end
end

return p
```

rinky loads every folder under `/mnt/us/rinky/plugins/` (on a Kobo
`/mnt/onboard/.adds/rinky/plugins/`) that has a `plugin.toml` when it starts
(`PluginHost::load`, called from `App::new`). `RINKY_HOME` moves the root
(`$RINKY_HOME/plugins`). A plugin with a bad manifest or a Lua syntax error is
skipped and reported (toast, notification list, `rinky.log`); the others still load.
A Lua plugin cannot take the name of a compiled-in plugin.

Install the sample: copy `plugins/sample-lua/plugin.toml` and `main.lua` (not the Rust
files) to `/mnt/us/rinky/plugins/sample-lua/`.

## plugin.toml

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | required | `[a-z0-9_-]`, at most 64 characters; equal to the folder name |
| `version` | required | any string |
| `description` | `""` | one line for the plugin list |
| `permissions` | `[]` | any of `document`, `ui`, `storage`, `network` |
| `entry` | `main.lua` | Lua file in the folder |
| `time_limit_ms` | 1000 (500 for `on_page_change`) | time per call, at most 10000 |
| `memory_mb` | 32 | Lua heap limit, at most 128 |
| `hosts` | `[]` (any) | hosts for `network`; `example.com` also allows its subdomains |

| Permission | Gives |
| --- | --- |
| `document` | `ink.doc.*`: metadata, TOC, search, text around a position, links, position, selection, page text |
| `ui` | `ink.ui.*`: toasts, notifications, dialogs, menus, the Markdown viewer, keyboard input, status-bar text |
| `storage` | `ink.settings.*`, `ink.book.*`, `ink.files.*`, `io.open` |
| `network` | `ink.http.*` (HTTP and HTTPS, streaming) |

Registration (`ink.register.*`), `ink.log`/`print`, `ink.json` and plain Lua need no
permission. Calling a function without its permission raises a Lua error that names the
missing permission.

## Hooks

The same hooks exist in Rust (`Plugin` trait methods, `cx: &mut HostCtx`) and Lua
(functions in the returned table). Pages are 0-based in Rust and 1-based in Lua.

| Hook | Lua arguments | Called |
| --- | --- | --- |
| `on_load` | `reg` (= `ink.register`) | once at start, after `main.lua` ran; register entries here (or at the top level of `main.lua`) |
| `on_book_open` | - | once the book's first page is on the screen (the reader defers it so the book opens fast); `on_page_change` for that page follows |
| `on_page_change` | `page` | for every new page (also the first), just before it is drawn, so a status text set here appears with the page; keep it fast (500 ms limit) |
| `on_selection` | `sel` = `{text, start, ["end"], page}` | the user selected text |
| `on_suspend` | - | before the device sleeps (screensaver); running `ink.http` requests are cancelled first and end with an `error` event (`interrupted: the device went to sleep`) |
| `on_resume` | - | after it woke up and the page is back |
| `on_close` | - | the book is closed (reader exits or opens another book) |
| `on_exit` | - (Rust only) | once, when rinky exits normally ("Exit to Kindle", `SIGTERM`): stop own processes and give back devices so the stock UI finds them as before; not called after a crash |
| `on_action` | `id` | a registered entry without its own handler was triggered |
| `on_library_action` | `id, book_path` | a library action without its own handler (falls back to `on_action(id)`) |
| `on_ui_result` | `id, result` | answer to a UI request made without a callback |
| `on_http` | `id, event` | network event of a request made without a callback |
| `on_tick` | - (Rust only) | about every `tick_interval()` while that returns `Some` (see [Rust plugins](#rust-plugins)) |

All hooks are optional. A Lua `main.lua` may also set a global table `plugin` instead of
returning one.

## Registries

Entries carry the plugin name and an action id. The app shows them and calls the plugin
when the user picks one. Use distinct ids across all registries of one plugin.

| Lua | Rust (`Registrar`) | Shown by |
| --- | --- | --- |
| `reg.menu_item(id, label, order[, fn])` | `menu_item(id, label, order)` | reader menu, sorted by `order` |
| `reg.selection_action(id, label[, fn])` | `selection_action(id, label)` | selection popup |
| `reg.gesture(id, gesture, label[, fn])` | `gesture(id, gesture, label)` | the reader, when the gesture happens |
| `reg.action(id, label[, fn])` | `action(id, label)` | Settings, Gestures: the user can put it on any reader gesture ([below](#gesture-map-actions)) |
| `reg.settings_page(id, title[, fn])` | `settings_page(id, title)` | settings screen; opening calls action `settings:<id>`, and the menu the plugin answers with is shown as a full settings page ([below](#settings-menus)) |
| `reg.library_action(id, label[, fn])` | `library_action(id, label)` | a book's long-press menu in the library; `fn(book_path, id)` |
| `reg.status_widget(id, slot, text)` | `status_widget(id, StatusSlot, text)` | reader status bar; slot `left`, `center` or `right` |
| `reg.control_tile(id, label, icon[, fn])` | `control_tile(id, label, icon)` | a tile the user can add to the control center ([below](#customizable-lists-control-center-and-navigation-bar)) |
| `reg.library_entry(id, label, icon[, fn])` | `library_entry(id, label, icon)` | the library's overflow menu, and the navigation bar when the user picks it |

In Lua, an optional last argument `fn` is called instead of `on_action` for that entry.
Gesture names (`ink_plugin::registry::GESTURES`): `two_finger_tap`, `double_tap`,
`long_press`, `swipe_up_from_bottom`, `swipe_down_from_top`,
`corner_hold:top_left|top_right|bottom_left|bottom_right`, `pinch_in`, `pinch_out`.
Holds (`long_press`, `corner_hold:*`, a tile's `on_hold`) fire after the app's one hold
time, `ink_ui::LONG_PRESS` (400 ms), like every other hold in the app; plugins get the
gesture, never raw touches, so they have no hold timer of their own.

**Dictionary lookup.** A selection action with the id `lookup`
(`ink_plugin::LOOKUP_ACTION`) makes the plugin a lookup provider. A long press (with the
gesture set to the dictionary) gives the selected word to the first provider, in plugin
order, that claims it: Rust plugins answer `Plugin::claims_lookup(text, lang)` (`lang`:
the book's language; default true), Lua plugins always claim. A provider for one
language placed before the general dictionary therefore takes only its language, and
the dictionary still gets every other word. With one provider nothing changes.

Status-widget text changes with `ink.ui.status(id, text)` (Rust:
`cx.ui()?.status(id, text)`); an empty text hides it.

## The `ink` table

Every function exists at all times, but works only while the plugin runs a hook (that is
always the case for code in hooks and callbacks). Functions saved in locals keep working.
While the plugin loads (the top level of `main.lua` and `on_load`) only `ink.register.*`,
`ink.log`/`print`, `ink.json.*` and `require` work; the others raise
`... is not available while the plugin loads`.
Failures that are not programming errors return `nil, message` (`ink.doc.page_text()`
on an engine without page text, a missing file, a refused request); permission problems
and wrong arguments raise errors.

### Document (`document`)

| Function | Returns |
| --- | --- |
| `ink.doc.metadata()` | `{title, authors = {...}, language, identifier, publisher, description, series, series_index}` |
| `ink.doc.book_id()` | KOReader partial MD5 of the file, or nil |
| `ink.doc.toc()` | `{ {title, level, position, page}, ... }` (level 0 = top) |
| `ink.doc.search(query[, limit=20])` | `{ {position, page, excerpt, match_start, match_end}, ... }`; `excerpt:sub(match_start, match_end)` is the match |
| `ink.doc.read_around([position][, chars=1000])` | about `chars` characters centred on `position` (default: current) |
| `ink.doc.links([position])` | `{ {text, internal, position or url, footnote}, ... }` on that page |
| `ink.doc.current_position()` | position string (crengine xpointer for reflowable books, `page:N;...` for PDF and CBZ) |
| `ink.doc.selection()` | `{text, start, ["end"], page}` or nil |
| `ink.doc.page()`, `ink.doc.page_count()` | current page (1-based) and page count |
| `ink.doc.page_text([page])` | text of a page, or `nil, "not supported: ..."` when the engine has no page text |

Positions are strings; pass them back unchanged. These are the five Insightful book tools
(`search_book`, `read_around`, `list_links`, `toc`, `current_position`).

### Gesture map actions

Settings, Gestures lists, under "Plugins" in each gesture's picker, the actions
(`action(id, label)`) and reader menu items of the enabled plugins. The user can put any
of them on any reader gesture (taps, corners, swipes, two-finger gestures, long press).
The gesture then calls `on_action(id)` (Lua: `fn(id)`) with the book open and no
selection. The label should name the plugin ("Insightful: Ask about the book"), since
the picker shows the actions of all plugins together. A menu item does not need a second
registration as an action. The saved action is `plugin:<plugin>:<id>` in `[gestures]`;
while its plugin is off, the gesture does nothing and the page shows `<id> (<plugin>,
off)`. This is the user's own map; `gesture(id, gesture, label)` instead binds a fixed
gesture, which runs only when the user's map has `none` for it. Used by: insightful
(`ask_book`, `chats`, and the menu item `book_menu`).

### Customizable lists: control center and navigation bar

The user chooses the control-center buttons and the navigation-bar items in Settings
(Control center buttons, Navigation bar). Plugin entries are listed there next to the
built-in ones, only while their plugin is enabled.

- **Control tile** (`control_tile(id, label, icon)`): a tile with the caption `label`, the
  icon `icon` (a name of the app's icon set, any case: `sync`, `download`, `library`,
  `chart`, `wifi`, ...; unknown names draw the info icon) and a state line under the
  caption. A tap calls `on_action(id)`. The returned `ControlTile` takes
  `.needs_wifi("hint")` (with Wi-Fi off a tap shows the hint and does not call the
  plugin), `.working("text")` (status line after a tap; default the caption),
  `.state("text")` (initial state line) and `.on_hold("action")` (Rust only: three dots
  in the corner, and a long press calls `on_action(action)`; usbnet and ssh use
  `settings:<page>` to open their settings page over the control center).
- **State line**: Rust plugins implement `Plugin::tile_state(&self, id) -> Option<TileState>`
  (`text`, `on: Some(bool)` for a filled toggle look, `None` for an outlined action tile).
  The control center calls it each time it draws or refreshes the tile, so it must read
  only the plugin's own memory. Lua plugins (and Rust plugins that return `None`) set
  the text with `ink.ui.tile_state(id, text)` / `cx.ui()?.tile_state(id, text)`.
- **Library entry** (`library_entry(id, label, icon)`): no book is involved. A pick
  calls `on_action(id)` (Lua: `fn(id)`).
- **Saved ids**: `settings.toml` stores built-in items by name (`wifi`, `stats`) and
  plugin items as `plugin:<plugin>:<id>` (`ink_plugin::registry::saved_id`), in
  `ui.control_buttons` and `ui.nav_items`. An id no registered entry answers (plugin
  removed or turned off) is skipped with a log line (`control center: button ... is not
  available`, `navigation bar: item ... is not available`); it stays in the file until the
  user changes the list on its page. The old built-in `sync` button reads as
  `plugin:kosync:sync_now`.
- Used by: kosync (tile `sync_now`, "Sync progress"), calibre (tile `toggle` "Calibre":
  tap connects or disconnects, the line shows the status; library entry `connect`,
  "Connect to calibre"), usbnet (tile `toggle` "USB network"), ssh (tile `toggle` "SSH
  server"). The OPDS navigation item is still the built-in name `opds`.

Updating one row of an open menu: `cx.ui()?.menu_label(menu_id, action, new_label)`
([`UiRequest::MenuLabel`]) changes the label of the item with `action` in the plugin's
open menu without opening a new one; only that row is repainted (GL16, one update). It is
ignored when the menu is not open or has no such item. Combine it with `on_tick` to show
live status in a menu (calibre's first row).

### UI (`ui`)

| Function | Shows | Answer |
| --- | --- | --- |
| `ink.ui.toast(text)` | short message at the bottom (2.5 s, a touch dismisses it) | - |
| `ink.ui.notify(text)` | like a toast (4 s), and kept in the notification list | - |
| `ink.ui.status(id, text)` | new text of a registered status widget | - |
| `ink.ui.tile_state(id, text)` | new state line of a registered control tile | - |
| `ink.ui.dialog{title, body, buttons = {{"Cancel","cancel"},{"OK","ok"}}}` | modal dialog | button action; `""` for the back key |
| `ink.ui.menu{title, items = {{"Label","action"}, "Plain"}}` | modal menu (pages itself when long) | item action; `""` when cancelled |
| `ink.ui.view{title, markdown}` | full-screen Markdown viewer with pages and a Close button | a tapped link URL (viewer stays open), then `"closed"` |
| `ink.ui.view{id = v, title, markdown}` | replaces the text of open viewer `v` (streamed answers) | - |
| `ink.ui.input{prompt, text}` | on-screen keyboard with a text field, Cancel and OK; over the plugin's own chat view (a viewer with `:::input`) the keyboard docks instead: the view's input bar sits on top of the keys (`prompt` as its placeholder, a send button) and the view scrolls to its newest lines above them | the text; `""` when cancelled (a docked keyboard put away keeps its text as a draft in the view's bar) |

`dialog`, `menu`, `view` and `input` return a request id at once and take an optional
callback `function(result)` (second argument or `callback =` field). Without a callback
the answer goes to `on_ui_result(id, result)`. Nothing blocks: the reader keeps running and
the answer arrives in a later call. Only one modal request shows at a time; later ones
wait. Several toasts at once show as the first one with `(+N more)`.

Markdown: CommonMark with GitHub tables (pulldown-cmark, `ink_plugin::markdown`):
headings, paragraphs, hard line breaks (two spaces or `\` at a line end), bullet and
numbered lists (nested items indented to their parent's text, 3 spaces under `1. `;
later paragraphs of an item stay indented), block quotes (a list in a quote shows as
quoted lines with their markers), fenced and indented code, rules, tables (with or
without outer pipes; columns wrap on narrow screens), `**bold**`, `*italic*`,
`***both***`, `~~struck~~`, `` `code` `` (on gray), `[text](url)` and `<https://url>`,
images as their alt text. HTML is shown as text. Bold and italics use the interface
font's own bold and italic faces (synthetic ones for a family without them); headings
and table headers are bold, quotes italic. The app bar, card labels, captions and sheet
rows stay light: send a title or label as plain text, not `**bold**`. Unclosed `**`
or `` ` `` (a streamed answer cut mid-word) shows as typed until it is closed.

### Storage (`storage`)

| Function | |
| --- | --- |
| `ink.settings.get(key)`, `ink.settings.set(key, value)` | plugin settings; `nil` removes a key |
| `ink.book.get(key)`, `ink.book.set(key, value)` | values for the open book (in a library action: the chosen book) |
| `ink.files.read(name)` | file content, or `nil, message` |
| `ink.files.write(name, data)`, `ink.files.append(name, data)` | `true`, or `nil, message`; at most 16 MiB per call |
| `ink.files.list([subdir])`, `ink.files.exists(name)`, `ink.files.remove(name)` | names (folders end in `/`), boolean, `true` |
| `io.open(name[, "r"|"w"|"a"])`, `io.lines(name)` | file handles with `read("l"/"L"/"n"/"a"/count)`, `lines`, `write`, `seek`, `close` (writes reach the file on `flush`/`close`) |

Values are anything JSON can hold (tables with keys `1..n` become arrays, an empty table
becomes `{}`). File names are relative to the plugin's data folder; `..`, absolute paths
and backslashes are refused.

### Network (`network`)

```lua
-- whole body
ink.http.get("https://example.com/feed.json", function(ev)
  if ev.type == "done" then local data = ink.json.decode(ev.body) end
  if ev.type == "error" then ink.ui.toast(ev.message) end
end)

-- streaming server-sent events (OpenAI/Anthropic style)
local id = ink.http.request({
  url = "https://api.example.com/v1/chat", method = "POST",
  headers = { Authorization = "Bearer " .. ink.settings.get("key") },
  body = { model = "x", stream = true },        -- a table is sent as JSON
  stream = "sse",                                -- or "chunks", or nil for the whole body
  timeout = 60,                                  -- seconds (default 30)
}, function(ev) ... end)
ink.http.cancel(id)
```

Events: `{type="response", status, headers}` (streaming modes), `{type="chunk", data}`,
`{type="sse", event, data, id}`, `{type="done", status, headers, body}` (`body` only
without `stream`), `{type="error", message}`. Requests run in worker threads; events
arrive while the reader keeps working (at most 8 requests at once).

### Other

`ink.log(...)` and `print(...)` write `plugin <name>: ...` to `rinky.log`.
`ink.json.encode(value)`, `ink.json.decode(text)` (`nil, message` on bad JSON).
`require("mod")` loads `mod.lua` (and `a.b` from `a/b.lua`) from the plugin folder.
`os.time`, `os.clock`, `os.date`, `os.difftime`. `ink.plugin.name`, `ink.plugin.version`.

## Storage

| What | File |
| --- | --- |
| Plugin settings | `plugins/<name>/settings.json` (written by `ink.settings.set`), over read-only defaults from `[plugins.<name>]` in `settings.toml` |
| Per-book data | the book's sidecar `<book>.sdr/rinky.<ext>.json` (for example `rinky.epub.json`), key `plugins.<name>` (load, merge, save on every write, so the reader's own saves are kept) |
| Files | `plugins/<name>/data/` |

Users may put defaults in `settings.toml`:

```toml
[plugins.sample-lua]
show_timer = false
```

## Limits, sandbox and errors

- **Time**: each call (hook, handler, callback) may run `time_limit_ms` of Lua time
  (default 1000 ms; `on_page_change` 500 ms). Time spent inside host functions
  (`ink.doc.search` on a big book, file writes) does not count, but no call runs longer
  than 5 x the limit of wall time, at most 5 s (or the limit itself when it is larger);
  this ceiling is also checked at every host call, so a loop of host calls stops too. The
  limit error cannot be caught by `pcall`, `xpcall` or `coroutine.resume`.
- **Memory**: each Lua plugin has its own Lua state with a heap limit (`memory_mb`).
  Per call at most 100 UI requests, 200 log lines (each cut at 2000 characters), 500
  settings/book writes and 64 MiB written to files; toast, notification and status texts
  are cut at 1000 characters; viewer and dialog texts may be 4 MiB. Going over raises a
  Lua error.
- **Sandbox**: no `os.execute`, `os.exit`, `os.getenv`, `os.remove`, `io.popen`, `debug`,
  `package`, `dofile`, `loadfile`, binary chunks, `collectgarbage("stop")`; `io` only for
  files in the data folder; `require` only for the plugin folder.
- **Errors**: a failing call is logged (`plugins: <name>: on_page_change: main.lua:12:
  ...` with a traceback), shown as a toast (`Plugin <name>: ...`) and kept in the
  notification list. The reader goes on. A plugin is disabled (its menu items etc.
  disappear, its network requests stop) after 5 errors, or at once when it runs out of
  time on a page turn.
- **Known gap**: one long C library call (for example a pathological `string.find`
  pattern on a huge string) is not interrupted by the time limit.

## Rust plugins

```rust
use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Registrar, Result, UiHost};

pub struct Hello(PluginManifest);

impl Hello {
    pub fn new() -> Self {
        Hello(PluginManifest::new("hello", env!("CARGO_PKG_VERSION"), &[Permission::Ui, Permission::Document]))
    }
}

impl Plugin for Hello {
    fn manifest(&self) -> &PluginManifest { &self.0 }
    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        reg.menu_item("hi", "Say hello", 10);
        Ok(())
    }
    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        let title = cx.document()?.metadata().title;
        cx.ui()?.dialog(1, "Hello", &title, &[("OK", "ok")]);
        Ok(())
    }
    fn on_ui_result(&mut self, _cx: &mut HostCtx, _id: u32, _result: &str) -> Result<()> { Ok(()) }
}
```

- Add the crate under `plugins/<name>/` (it is a workspace member through `plugins/*`)
  and one line `Box::new(hello::Hello::new()),` to `crates/ink-app/src/plugins.rs`.
- `HostCtx` accessors apply the manifest: `cx.document()`, `cx.ui()`, `cx.storage()`,
  `cx.data_file(name)`, `cx.http()` / `cx.send(req)` (blocking, for worker threads the
  plugin starts itself) and `cx.start_request(req, StreamMode::Sse)` (worker thread;
  events in `on_http(cx, id, &HttpEvent)`). The fields are public for trusted code; use
  the accessors so the permissions in the manifest stay true.
- `UiHost` helpers: `toast`, `notify`, `dialog`, `menu`, `view_markdown`, `input`,
  `status`, `tile_state`, `menu_label`, `rescan_library`. Request ids are the plugin's
  own; answers come in `on_ui_result`.
- `ink_plugin::serde_json` is the JSON crate of the storage API.
- Own worker threads (raw TCP/UDP, which the HTTP worker does not do): return
  `Some(interval)` from `Plugin::tick_interval` while the thread runs and collect its
  results in `on_tick(cx)` on the UI thread (toasts, menus, `rescan_library`). The host
  calls it from `PluginHost::poll` (every app loop turn), wakes the loop for it
  (`PluginHost::timeout`, at least 100 ms apart) and skips it while the device sleeps.
  Return `None` when idle so the loop can sleep. Example: `plugins/calibre` (calibre's
  Smart Device App protocol: a TCP session thread plus UDP discovery, books stored
  through `<file>.part` and a rename, tests against a mock calibre on localhost).
- Device access (trusted Rust code, no permission covers it): `plugins/usbnet` writes
  configfs and runs `modprobe`/`ifconfig`, `plugins/ssh` runs the bundled dropbear
  (`bin/dropbearmulti`, built by `scripts/build-dropbear.sh`) as a child process watched
  in `on_tick`. Both go through a small trait for files and commands, so their tests run
  on the host against a fake configfs tree and a shell-script dropbear; both undo what
  they started in `on_exit`.

### Text annotations (`Plugin::text_annotator`)

A Rust plugin can draw short texts above or below the text of reflowable books, laid
out like `<ruby>`: the annotated range is the base, the annotation is drawn centred
above it (`Annotation::over`, readings) or below it (`Annotation::under`, glosses) at
half its size. The two tiers are laid out separately with the same rules (overhang,
condensing, even pitch, no line break inside a base); one range can have both. Return an `Arc<dyn TextAnnotator>` from
`Plugin::text_annotator` (default `None`):

```rust
use ink_plugin::{Annotation, TextAnnotator};

struct Shout;

impl TextAnnotator for Shout {
    fn id(&self) -> &str { "shout" }
    fn version(&self) -> u64 { 1 }
    fn wants(&self, paragraph: &str, _lang: Option<&str>) -> bool { paragraph.contains('!') }
    fn annotate(&self, paragraph: &str, _lang: Option<&str>) -> Vec<Annotation> {
        paragraph.match_indices('!').map(|(i, _)| Annotation::over(i..i + 1, "loud")).collect()
    }
}

// in impl Plugin for MyPlugin:
fn text_annotator(&self) -> Option<Arc<dyn TextAnnotator>> { Some(self.annotator.clone()) }
```

- **Who is asked:** when a book opens the reader asks the enabled plugins in load order
  (compiled-in plugins in the order of `crates/ink-app/src/plugins.rs`) and uses every
  annotator it gets, in that order. The engine asks each about every paragraph and
  merges the results: where two annotations of the same position overlap, the earlier
  plugin's wins; one over a range and one under it both show. A plugin that panics
  there is skipped. With *Ruby annotations* off (Aa › Layout for the book, Settings › Default
  layout for new books) no annotator is used.
- **`on_book_open` comes first:** while an annotator is in use the reader sends
  `on_book_open` before the book's first page (otherwise it comes after it), then
  reads `version()` again and lays the book out anew if it moved. So a plugin can learn
  the book (its language, its own per-book settings) in `on_book_open`, set its
  annotations up and bump the version there, and the first page already shows them.
- **The text:** `wants` and `annotate` get one paragraph as it is laid out (whitespace
  collapsed, without the text of `<rt>` and `<rp>`) and its language (`lang`/`xml:lang`,
  else the book's), so an annotator for one language says no to the others in `wants`.
  Ranges are byte ranges of that text. Build annotations with `Annotation::over` (above
  the text) or `Annotation::under` (below it); the type is `#[non_exhaustive]` so fields
  can be added later. The engine drops ranges that are empty, out of bounds, inside a
  character, overlapping an earlier annotation of the same position (this annotator's
  or an earlier one's) or a `<ruby>` base of the book in that position (the book's ruby
  wins), and annotations with no text. It
  never breaks a line inside an annotated range.
- **Line rhythm:** every paragraph for which `wants` returns true gets the room of an
  annotation above all its lines (room below comes only with annotations under the
  text), even when `annotate` then returns nothing for it, so
  a page of text the annotator reads keeps one line pitch whether or not a paragraph
  has annotations. `wants` is therefore also the switch for that room: return false for
  paragraphs the annotator never annotates (another script, say). Book `<ruby>` keeps
  its own rule: only paragraphs with ruby get the room.
- **Drawing only:** annotations are not document text. Positions, selection, search,
  highlights and the text plugins read stay as without them.
- **Threads and cost:** the pagination threads call the annotator for every paragraph
  of the book, so it is `Send + Sync`, must not block, and `wants` should be a cheap
  test that lets most paragraphs through untouched.
- **Changing the output:** the output must be a function of (paragraph, language,
  version). To change it (a new mode, another word list), bump `version()` from any
  callback (`on_action`, `on_ui_result` for a dialog, card or settings menu answer,
  `on_tick`, `on_http`, `on_book_open`): the reader reads it before every frame and
  whenever it rebuilds its layout, and a new version paginates the book again, like a
  font size change. Returning a different annotator later has no effect until the book is
  opened again.
- **Failures:** a panic in `wants` or `annotate` is caught; the engine logs it once and
  lays the rest of the book out without that annotator's annotations. The other
  annotators go on.
- **Rust only:** a Lua plugin cannot offer an annotator. The annotator runs on the
  pagination threads, for every paragraph of the book, while the Lua state belongs to
  the UI thread and every Lua call goes through the host's sandbox and time limits;
  calling it per paragraph would make pagination many times slower.

### Example: Wikipedia (`plugins/wikipedia`)

A network plugin with a selection action. Worth reading for these patterns:

- **Requests on the worker**: `cx.start_request(req, StreamMode::Whole { limit })` per
  request, one in flight at a time; the id is kept and events of any other id are ignored
  (a new lookup or a closed card calls `cx.cancel_request`). The status comes from
  `HttpEvent::Response`, the decision (404 -> search API) is taken at `Done`.
- **Failures are cards, not errors**: a refused start, a network error and a bad body all
  show a card with a Retry button and return `Ok(())` (five `Err`s disable a plugin).
  The host has no Wi-Fi check for plugins (only control tiles have `needs_wifi`), so the
  plugin sorts the error text: `offline:` / `http:` -> "Turn on Wi-Fi ...", `timed out`,
  `tls:` (clock), `interrupted` (sleep, shown nowhere).
- **Late cards**: a card dialog is a modal layer; a later `Dialog` with the same id does
  not replace an open one (only viewer text replaces in place). So the plugin shows a
  toast while loading (the next modal replaces it) and sends the card when the answer
  arrives; a swipe on a paged card closes it and the plugin answers with the next page.
- **Selection action id**: not `lookup` (that id makes a plugin a dictionary lookup
  provider, see [Registries](#registries)).
- Tests use a fake `NetHost` that records `Request`s and feed `on_http` by hand; no thread
  and no network. The card text is checked against `CardDialog::parse` in
  `crates/ink-app/src/plugin_card.rs`.

## For screen authors

The hook API is `cx.plugins` (the `PluginHost`) and `crate::plugin_host::hooks`
(functions taking the screen's `AppCtx`).
The app loop (`app.rs`) already does events, overlay painting, timers, network delivery
and suspend/resume for every screen:

- before `handle`: `hooks::app_handle` (a plugin dialog, menu, viewer or keyboard takes
  the event; the screen does not see it);
- instead of `paint`: `hooks::app_paint` (the screen paints, then the plugin overlay is
  drawn over what the screen changed: its returned damage and anything it showed itself
  through `cx.screen`);
- `timeout`: `hooks::app_timeout(top.timeout(), plugins)`;
- power: `PowerEvent::GoingToScreenSaver` runs the plugins' `on_suspend`,
  `OutOfScreenSaver` their `on_resume` (once each; RTC maintenance wakes do not resume).

So screens **must not** call `PluginHost::handle_event` or `PluginHost::paint_overlay`
themselves. A screen with an open book implements

```rust
fn document(&mut self) -> Option<&mut dyn ink_doc::Document> {
    self.doc.as_deref_mut().map(|d| d as &mut dyn ink_doc::Document)
}
```

(`AppScreen::document`, default `None`) so plugins see the book when they answer a
dialog, get a network result or suspend. Prefer returning damage from `paint` over
calling `cx.screen.update` yourself: both work, but a self-shown page is refreshed once
without and once with the overlay.

### Reader (`screens/reader`)

| When | Call |
| --- | --- |
| after the first page is on the panel (before it, while a plugin annotator is in use; see [Text annotations](#text-annotations-plugintext_annotator)) | `cx.plugins.book_open(doc, &path)`, then `cx.plugins.page_change` for that page |
| `paint` of a new page, after `doc.set_current_page(page)` and before `render_page` (status texts then land in the same update) | `cx.plugins.page_change(doc, page)` |
| a selection was made | `cx.plugins.selection(doc, &sel)`; the popup adds `cx.plugins.selection_actions()` |
| reader menu | add `hooks::entries(cx.plugins, EntryKind::ReaderMenu)` (show `entry.label`) |
| an entry was picked | `hooks::run_entry(cx, &entry, Some(doc), None)` |
| a gesture the reader does not use | `if hooks::gesture(cx, &g, Some(doc)) { /* used */ }` |
| status bar | `hooks::status_text(cx, StatusSlot::Left/Center/Right)` |
| `on_exit` | `cx.plugins.book_close(doc)` |
| `AppScreen::document` | return the open book |

Suspend/resume need no reader call (the app loop does them with `document()`);
`cx.plugins.suspend`/`resume` exist for screens that sleep on their own and are no-ops
when repeated.

### Library and home (`screens/library`)

- The navigation bar items and the overflow menu rows of plugin library entries come
  from `cx.plugins.library_entries()`; saved ids are read with
  `plugin_entries::nav_chosen`. A pick runs `cx.plugins.trigger(plugin, id, None)`.
- A book's long-press menu adds `hooks::entries(cx.plugins, EntryKind::Library)`; a pick calls
  `hooks::run_entry(cx, &entry, None, Some(&book_path))`.

### Settings (`screens/settings`)

- The control-center buttons page and the navigation page list
  `plugin_entries::control_choices(host)` / `nav_choices(host)` after the built-in names
  (`plugin_entries::merged`). The control center draws a plugin tile from
  `cx.plugins.control_tile(plugin, id)` and runs a tap with `cx.plugins.trigger`.
- List `hooks::settings_pages(cx)` under "Plugins" (`entry.label` is the page title);
  opening one calls `hooks::run_entry(cx, &entry, None, None)`; the plugin answers with a
  menu or dialog, which the app loop draws and routes.
- Plugin list: `cx.plugins.plugins()` (`PluginInfo`: manifest, `kind` `rust`/`lua`,
  `enabled`, `errors`). Enable/disable (stored in `plugins/disabled.txt`, applied at the
  next start): `plugin_host::set_user_enabled(&paths, name, on)`; on the command line
  `rinky plugins [list|enable NAME|disable NAME]`.
- Notifications: `cx.plugins.notifications()`, `cx.plugins.unread_notifications()`,
  `cx.plugins.mark_notifications_read()`.

### Overlay behaviour

- Layers: one modal (dialog, menu, viewer, keyboard) plus a toast on top. A layer keeps
  the pixels under it and puts them back when it closes; with `below_redrawn = true` it
  takes them again from the new page.
- Refresh classes of the returned damage: toasts and keyboard typing DU, dialogs and menus
  GL16, the full-screen viewer REAGL when it opens and closes (no flash; see
  `docs/DESIGN.md`, "Flash policy").
- `handle_event` passes the power key and timers through; a touch dismisses a toast and
  is not used by it (the page still turns); a tap outside a menu cancels it; the back key
  cancels any modal layer.
- `cx.plugins.modal_open()` tells whether a modal layer is showing.
- Chat views (viewers with a `:::input` bar) scroll instead of paging: they open at
  their end, follow a replaced text's end while at the end (`:::top` opens at the
  start), and a swipe, a tap on a side or a page key moves them by a screenful less a
  line (REAGL on the text below the app bar, no flash); a thin bar shows the position.
  `:::you LINK | CAPTION` makes a user bubble tappable with a muted caption.
- A keyboard request over the plugin's chat view docks under it
  (`InputBox::docked`): no new panel with a prompt and Cancel/OK, but the view's input
  bar lifted onto the keyboard (`ink_ui::keyboard::lift_above`); the view lays out above
  it at its end (REAGL on the text, GL16 on the panel: no full-screen flash), typing
  refreshes the keys and the field only, a tap on the view puts the keyboard away (the
  draft stays in the bar), swipes over it scroll the view. A layer repainted with
  `partial` rectangles only puts those on the canvas, so layers above them that they do
  not reach are not drawn again.

## Testing

- Unit tests: `ink_plugin::lua::LuaPlugin::from_source(manifest, dir, source)` plus a fake
  `DocumentAccess` and `HostCtx::new(&mut UiQueue, &mut MemoryStorage, &perms)`; see
  `crates/ink-plugin/src/lua/tests.rs` and `plugins/sample-lua/lib.rs`.
- Host and overlay tests: `cargo test -p ink-app plugin_host`; with
  `INK_PLUGIN_SHOTS=DIR` they write the overlay screenshots (sample plugin on
  `testdata/juliet.epub`, every layer kind).
- Simulator: the sim reads plugins from `$TMPDIR/rinky-sim/plugins`. Copy
  `plugins/sample-lua/plugin.toml` and `main.lua` to `$TMPDIR/rinky-sim/plugins/sample-lua/`,
  then `cargo run -p ink-app -- sim testdata/juliet.epub --headless --script
  plugins/sample-lua/sim.txt --out OUT`: `04-welcome-back.png` shows the toast from the
  sample's `on_resume` after the scripted `power goingToScreenSaver` /
  `power outOfScreenSaver`. Headless `wait` steps advance a virtual clock, so toasts do
  not expire there (a tap dismisses them).
- `rinky plugins` (with `RINKY_HOME`) lists what the app would load.
- Emulator: `tests/e2e/plugin-*.sh` (run with `scripts/e2e/run.sh NAME OUTDIR 'plugin-*'`)
  install the sample plugin and two broken plugins, run the reader and check
  `rinky.log`, the sidecar and the plugin's data files.

## Settings menus

A plugin's settings are the menu it sends for `settings:<id>` (`ui.menu`). The app
shows that menu with the settings rows of the built-in settings: in Settings, Plugins as a
full page (app bar with back), when a control tile with `on_hold("settings:<id>")` is
held as the control center's settings popup, anywhere else as the same popup over the
screen. Item labels map to rows:

| Label | Row |
| --- | --- |
| `"\u{2713} Name"` | a chosen choice (the other plain items of the menu are choices too) |
| `"Name\ton"` / `"Name\toff"` | a switch; a tap answers the item's action |
| `"Name\tvalue"` | a row with the value on the right |
| action `back` | a back row (on the page, the app bar's back answers `back`) |
| empty action | a note (not tappable) |

A tap answers the menu (`on_ui_result(id, action)`); send the menu again with the new
state and the page or popup updates in place (only the changed rows repaint).
`ui.menu_label` changes one row. Back on the page cancels the menu (`""`) unless it has a
`back` item.

