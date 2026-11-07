# rinky architecture

Written by the phase 0 architect (2026-10-24). Later agents code against the APIs
described here. API changes go through the integrator (see "Changing a shared API").
Updated by the integrator on 2026-10-30 after merging the ten wave A branches (p0fix,
device, ui, store, net, html, formats, mupdf, plugin, packaging).

## Crate map

```text
crates/
  ink-device   Canvas, geometry, Screen/Input/Light/Battery/Power/Wifi/Usb/Clock traits,
               evdev decoder, lipc runner; backends kindle, sim (minifb), headless
  ink-text     Fonts (fontdue), glyph cache, draw_text, shaping, line breaking, Hyphenator
  ink-ui       WidgetTree, Damage, RefreshScheduler, GestureRecognizer, keyboard,
               Dialog/Toast/Menu/BottomSheet, Theme, message screen
  ink-doc      Document + Provider traits, Position (xpointer / page / flow anchor),
               LayoutSettings, TOC/search/link/selection types, ResourceLoader
  ink-html     phase 2 reflowable engine behind HtmlDocument: html5ever DOM, CSS cascade,
               images (image/resvg), tables, notes, hyphenation, crengine xpointers,
               background pagination with an on-disk page-map cache
  ink-formats  EPUB, FB2/FB3, MOBI/AZW3 (KF8), HTML, TXT (encoding detection) into the
               engine; KFX/Topaz/DRM refused with a message; read_info; FormatsProvider
  ink-mupdf    PDF/CBZ/XPS through MuPDF 1.27.2 (built with zig): fixed layout, zoom,
               columns, text, links, outline
  ink-store    partial MD5, <stem>.sdr/rinky.<ext>.json sidecars, library index,
               settings.toml, plugin storage, statistics DB, KOReader import (read-only),
               vocab (dictionary lookup log, table `lookup` in vocabulary.sqlite3)
  ink-dict     StarDict reader (.ifo/.idx[.gz]/.dict[.dz]/.syn), lookup forms,
               definition markup to text (no other ink crate)
  ink-net      HttpClient + UreqClient (rustls), SSE, downloads, connectivity hooks,
               OPDS/KOSync clients, HTTP(S) test server
  ink-plugin   Plugin trait, registries, host services, manifests, Lua 5.4 host
  ink-app      binary `rinky`: App event loop, router, screens, plugin host, CLI,
               in-app update (update.rs)
plugins/       first-party plugin crates (wave C), workspace members via plugins/*;
               plugins/sample-lua is a small crate that embeds and tests the sample Lua
               plugin (a folder without Cargo.toml would break the members glob).
               Compiled in (crates/ink-app/src/plugins.rs): kosync, opds (KOSync and
               OPDS), dictionary (ink-dict), stats (crate ink-stats: statistics recorder,
               report and the `vocabulary` table the Statistics/Vocabulary screens
               read), insightful (AI chat), usbnet (USB Ethernet gadget), ssh
               (bundled dropbear; scripts/build-dropbear.sh, packaging/dropbear/)
packaging/     KUAL extension, install/uninstall scripts, package tests
scripts/       emulator, launcher, watchdog, device, golden and E2E scripts
tests/golden/  reference PNG pages        tests/e2e/  emulator scenarios
tests/sim/     scripted input for the simulator
testdata/      juliet.epub (public domain), synthetic.epub (generated)
docs/          this file, DESIGN.md, TESTING.md, plugins.md, user-guide.md, logo/,
               screenshots/
```

Dependency direction (no cycles except one dev-dependency, see "Golden tests"):

```text
ink-device <- ink-text <- ink-ui
ink-device <- ink-doc  <- ink-html <- ink-formats      (ink-html also uses ink-text)
                       <- ink-mupdf                    (also ink-device)
ink-device, ink-doc, ink-net <- ink-plugin
ink-plugin, ink-store, ink-dict, ink-stats <- plugins/* (dictionary also ink-stats)
everything                   <- ink-app
ink-store and ink-net depend on no other ink crate.
Dev-dependencies only: ink-html -> ink-formats, ink-store; ink-formats -> ink-store.
```

## Traits and main types

### ink-device

- `Canvas`: 8-bit gray, `width * height` bytes, 0 = black. Everything draws into it.
- `geom::{Point, Size, Rect, Rotation}`. The *panel* is portrait 1272 x 1696;
  *logical* coordinates are after rotation (`Rotation::to_panel` / `from_panel`).
- `Screen`: `panel_size`, `rotation`/`set_rotation`, `size` (logical), `night_mode`,
  `update(&Canvas, Rect, RefreshClass)`, `update_all`, `refresh_all`, `screenshot`.
  `RefreshClass::{Du, Gl16, Reagl, Gc16}`; GC16 flashes. Rotation and night inversion are
  applied in `screen::copy_to_panel`, shared by all backends.
- `Input::next_event(timeout) -> Option<InputEvent>`; `InputEvent::{Touch(TouchFrame),
  Key(KeyEvent), Power(PowerEvent), Usb(bool), Quit}`. A `TouchFrame` is one evdev
  `SYN_REPORT` with contacts in panel pixels and phases `Down`, `Move`, `Up`, `DownUp`
  (down and up in one frame: the PW12 short tap).
- `Light`, `Battery`, `Power`, `Wifi`, `Usb`, `Clock`: every method defaults to
  `DeviceError::Unsupported`. `services::Unsupported` implements all of them.
- `Device` is a struct with public boxed fields (`screen`, `input`, `light`, ...), so the
  app can borrow screen and input at the same time.
- `evdev::TouchDecoder`: raw 16-byte ARM32 `input_event`s to frames (protocol A and B,
  ignores pointer emulation). Host-testable.
- `lipc::Lipc`: `get_int/get_str/set_int/set_str/wait_event/watch` through
  `lipc-get-prop`, `lipc-set-prop`, `lipc-wait-event` child processes, each with a
  timeout (the child is killed). `LipcWatcher` streams `-m` events. On Linux every child
  gets `PR_SET_PDEATHSIG(SIGKILL)`, so it dies with the thread that spawned it (spawn
  watchers from long-lived threads).
- `Power::suspend(wake_after)`: kernel suspend with an RTC alarm, bypassing powerd (see
  "Sleep and the Kindle framework").
- Backends: `kindle::open(KindleOptions)` (Linux), `sim::open(SimOptions)` (feature
  `sim`, host only), `headless::open(HeadlessOptions)` (always; returns a probe).
- Kindle models (`kindle::model`): the model comes from the device code in the serial
  (`/proc/usid`, KOReader's rule; `RINKY_MODEL` overrides). The table holds what differs:
  the refresh interface (`Epdc`: MTK for the PW5 and newer, the i.MX Rex, Zelda and
  original `mxcfb_update_data` for the PW4, Oasis 2/3 and Voyage/PW3/Oasis 1), warmth,
  page keys and their direction, the Voyage's pressure keys, the hall sensor folder.
  `KindleFb` builds each refresh struct (`update_data_epdc`); on i.MX night mode is
  software inversion with day waveforms and there is no page-turn sweep or fast mode;
  the sweep is on only for the PW5 and PW12 (`Model::swipe`, KOReader's
  `canDoSwipeAnimation`: the Kindle 2024 driver voids swept updates).
  An unknown model's interface is probed with a GC16 PARTIAL of an unchanged corner. A
  framebuffer left in landscape (accelerometer models) is turned upright and the old
  `rotate` journaled. The UI is laid out in 300 ppi pixels and adapts to the panel
  size, so the 212 and 167 ppi models are marked unsupported. `rinky sim --panel WxH`
  renders other sizes.
- Kobo (`kobo::open`, chosen when `platform::Platform::detect()` finds
  `/mnt/onboard/.kobo`; `RINKY_PLATFORM` overrides): the same binary and the same
  screen, input and sleep code as the Kindle backend. Kobo models are in the same
  table (`kindle::model::KOBO_MODELS`, by the number at the end of
  `/mnt/onboard/.kobo/version`), with what a Kobo adds: the `rotate` value that is
  upright (FBInk's rotation map), touch transforms (KOReader's `touch_switch_xy`,
  `touch_mirrored_*`), the cover key (`KEY_F1`) and whether the driver's wait ioctl
  can be trusted. Refresh: `Epdc::KoboMxcfb` (72-byte v2 struct; REAGL FULL; night
  mode is the per-update inversion flag with the eclipse waveforms GLKW16/GCK16; no
  waits on the Libra 2 and the others KOReader flags) and `Epdc::KoboMtk` (36-byte
  `hwtcon_update_data`). The framebuffer boots at 16 or 32 bpp and is switched to 8 bpp
  gray (on MTK after `Y8` to the hwtcon `mdp_src_format`), and back on exit. Touch:
  "snow" panels never send `TRACKING_ID -1`; `BTN_TOUCH 0` ends the contact. There is
  no lipc: `kobo::services` drives light (`mxc_msp430.0` brightness, the mixer's
  `color`), battery (`power_supply`), Wi-Fi (`kobo::wifi`: modules, `wpa_supplicant`
  on Nickel's config, `wpa_cli`, `dhcpcd`/`udhcpc`, as KOReader's scripts) and suspend
  (`state-extended`, no `wakeup_count`, Wi-Fi off first). USB storage stays Nickel's.
  `packaging/kobo/rinky.sh` stops Nickel and starts it again (KOReader's `koreader.sh`
  and `nickel.sh`). Investigation notes: `docs/kobo-port.md`.
- `script`: the scripted-input language used by headless and sim runs.
- `eink::EinkPanel`: 16-level quantization, DU to 2 levels, optional ghosting.
- Wave A (`device`): the Kindle backend implements
  every service. New trait methods with defaults: `Light::{amber_schedule,
  set_amber_schedule}`, `Battery::info`, `Power::{set_screensaver, set_auto_sleep,
  auto_sleep, sleep_now, sleep_state}`, `Wifi::{scan_list, profiles, create_profile,
  delete_profile, airplane_mode, set_airplane_mode}`, `Usb::{cable, set_charge_only,
  drive_mode}`, `Clock::{local_now, sync_ntp}`. `PowerEvent` gained `LowBattery`, `Cover`,
  `DriveMode`, `LibraryRescan`, `TimeChanged`, `WifiState`, `Lipc {..}` (keep a `_` arm).
  With `KindleOptions::sleep = Some(..)` (default) the device layer owns the power key,
  cover and inactivity (screensaver + suspend) and delivers `InputEvent::Power`; the
  spike opts out. Cover close/open arrives three ways (kernel uevent `HALLSENSOR=` from
  `eink_hall`, udev's `com.lab126.hal magSensorClosed/Opened`, powerd
  `goingToScreenSaver 4`/`outOfScreenSaver 6`); `events::cover_from_published` and
  `classify` map them, `SleepController::cover_from` logs each with its source and
  keeps a change inside the debounce window for later. The hall sensor also fires when
  the device moves in a bag, so a cover open wakes only after it stayed open for
  `cover_settle` (400 ms; `RINKY_COVER_SETTLE_MS`), a resume with the cover closed
  stays asleep, a cover or resume wake that no touch or key follows sleeps again after
  3 min (`RINKY_COVER_UNUSED_S`, 0 = off), and powerd's relays that contradict the
  sensor's last report are dropped as stale. Start-up logs
  `/sys/devices/platform/eink_hall/hall_enable` (read only); `RINKY_COVER=0` turns
  the cover off. Settings changed on the device are journaled in `/tmp/rinky/restore`
  and undone by `rinky device restore` (launcher and watchdog). `devtool`
  (`rinky device ...`) and `selftest` (`rinky selftest --device`) are the
  device-layer test tools. `Canvas::{scaled, fit_into}`; `Canvas::load_png` refuses
  images over 4096 x 4096. `evdev::AppContacts`; `script` has `down/move/up/twotap/pinch`.

### ink-text

`Fonts` (three embedded Noto Serif faces; `advance`, `text_width`, `vmetrics`, `glyph`,
`draw_text`), `Style::{Regular, Bold, Italic}`,
`linebreak::{break_opportunities, greedy}`, `Hyphenator` + `NoHyphenation`,
`registry::scan_font_dirs`. Golden pages depend on `Fonts`' exact math.
Wave A (`ui`): shaped text (`Fonts::shape`, `draw_run`, `draw_run_clipped`, fallback faces
via `Fonts::load_fallbacks(&registry::KINDLE_FONT_DIRS)`), `Style::BoldItalic`,
`linebreak` items with Knuth-Plass (`knuth_plass`, `greedy_items`, `wrap`) and
`hyphen::PatternHyphenator` (`hyphenator_for("da")`, 82 languages through `hyphenation`
with `embed_all`). The prototype path (`text_width`, `draw_text`) keeps
its exact math.

### ink-ui

`WidgetTree` (add/remove/set_bounds/mark_dirty/invalidate/paint/dispatch/get_mut; a
removed or invalidated area is cleared, the widgets over it repainted and the area
returned as damage, so screens call `invalidate(screen, Gc16)` when they appear), `Widget`,
`Label`, `Button`, `PaintCx`, `Damage` + `refresh::merge`, `RefreshScheduler`
(`page_turn()` = REAGL, GC16 every N), `GestureRecognizer` (+ `ScreenSpace`),
`Gesture::{Tap, DoubleTap, LongPress, Swipe, TwoFingerTap, Pinch}`, `keyboard`,
`dialog::{Dialog, Toast, Menu, BottomSheet}`, `Theme`, `message::draw_message`,
`UiEvent` and `translate(&mut GestureRecognizer, InputEvent)`.
Wave A (`ui`): the event loop step is
`ink_ui::pump::next_events` (gesture timers, rotation-aware coordinates).
`UiEvent::Touch(Touch)` carries per-contact notifications (`TouchKind::{Down, Move, Hold,
Up, Cancel}`); `Gesture::CornerHold { corner, pos }` fires while a finger rests in a corner.
Every hold in the app (long press, corner hold, key alternates, the first key repeat)
waits `ink_ui::LONG_PRESS` (400 ms), the one hold time; no screen sets its own.
The keyboard types on `Touch` events (not on `Tap` gestures): feed it every `UiEvent`,
then drain `Keyboard::take_actions()`; key rectangles are inside `Keyboard::keys_area(b)`.
`WidgetTree::paint` clips each widget's text to its bounds. More widgets: lists, tabs,
slider, toggle, progress bar, scroll view, text input, popup, bottom sheet, icons.

### ink-doc

```rust
pub trait Document {
    fn metadata(&self) -> &Metadata;
    fn kind(&self) -> DocKind;                         // Reflowable | Fixed
    fn set_layout(&mut self, &LayoutSettings) -> Result<()>;
    fn layout(&self) -> &LayoutSettings;
    fn page_count(&self) -> PageCount;                 // { pages, complete }
    fn render_page(&mut self, page, &mut Canvas) -> Result<()>;   // content only
    fn position_of_page(&self, page) -> Result<Position>;
    fn page_of(&self, &Position) -> Result<usize>;
    fn current_page(&self) -> usize; fn set_current_page(&mut self, usize);
    fn current_position(&self) -> Result<Position>;    // default
    fn toc(&self) -> Vec<TocEntry>;                    // defaults below
    fn search(&mut self, query, limit) -> Result<Vec<SearchHit>>;
    fn read_around(&mut self, &Position, chars) -> Result<String>;
    fn links(&mut self, &Position) -> Result<Vec<Link>>;
    fn select(&mut self, page, Point, Point) -> Result<Option<Selection>>;
    fn selection(&self) -> Option<&Selection>; fn clear_selection(&mut self);
    fn set_selection(&mut self, Selection);            // default: no-op (cross-page selections)
    fn location(&self, &Position) -> Option<Location>; fn location_count(&self) -> Option<Location>;
    fn page_labels(&self) -> Vec<(String, Position)>;
    // wave A (html), all with defaults:
    fn note(&mut self, &Position) -> Result<Option<Note>>;            // footnote popups
    fn set_page_cache(&mut self, dir: &Path, book_id: &str);          // page-map cache
    fn wait_paginated(&mut self, Duration) -> bool;                   // background pagination
}
pub trait Provider { fn name; fn extensions; fn can_open(&Path); fn open(&Path) -> Result<Box<dyn Document>>; }
```

`Position::{XPointer(XPointer), Page(PagePosition), Flow(FlowAnchor)}` with a string form
used everywhere (sidecars, plugins): `/body/DocFragment[3]/body/p[2]/text().42`,
`page:12;x0,y0,x1,y1`, `flow:spine.block.word`. `XPointer` parses and prints crengine
syntax exactly. `FlowAnchor` is the phase 0 engine's anchor and is not portable; the
phase 2 engine produces xpointers instead. `Location` = 128 bytes per location.
`LayoutSettings::for_screen(w, h)` = the prototype defaults; `cache_key()` keys the
page-map cache. Unimplemented features return `DocError::Unsupported("... phase N")`.
Wave A (`html`): `XPointer` has `point: Option<u32>` (crengine element points such as
`/body/DocFragment[2].0`; struct literals add `point: None`); `Note`, `NoteKind`;
`ResourceLoader::shared()` (default `None`). Reflowable positions are crengine xpointers;
phase 0 `flow:` positions still resolve. `set_current_page(p)` moves the position only
when `p` differs from `current_page()`, and `set_layout` keeps the reading position: after
`set_layout` read `current_page()`. `page_count()` is `{ complete: false }` while the
background thread paginates; call `wait_paginated` where an exact total is needed.

### ink-html

`HtmlDocument::new(metadata, chapters, loader)`, `set_navigation(Navigation { toc,
page_list })` (NCX / EPUB 3 nav from `nav::from_opf`; hrefs are book paths with
`#fragment`), `chapter_count`, `page_layout`. html5ever DOM, CSS cascade (lengths and
nesting are clamped), images through `image`/`resvg` (big PNG/JPEG decoded with bounded
memory by `png` row by row and `zune-jpeg`), tables, lists, notes, hyphenation, crengine
xpointers, background pagination and a page-map cache (`ENGINE_VERSION` 2;
`tests/golden/pagemaps.txt` forces a bump when page breaks change). Faces are parsed on
first use. Binary size: hyphenation patterns 3.0 MB, extra fonts 0.5 MB.

### ink-formats

`open(path) -> HtmlDocument` (EPUB: through `epub::open`, which also hands the engine
its navigation), `open_book(path) -> BookDocument` (the engine plus the book's own TOC,
page list and `BookInfo`; every other `Document` method, including `note`,
`set_page_cache` and `wait_paginated`, is delegated: a new `Document` method with a
default body must be delegated there too), `load(path) -> Book`, `read_info(path) ->
BookInfo` (metadata, identifiers, cover, DRM state, for the library), `detect`, `sniff`,
`format_of`, `FormatsProvider` (returns `BookDocument`; extensions include `kepub`, `fbz`,
`pdb`, `kfx-zip`, `tpz`, `azw1`). Formats: EPUB, FB2/FB3 (footnotes as
`epub:type="noteref"/"footnote"`), MOBI 6 and KF8, HTML, TXT (`encoding_rs` +
`chardetng`); KFX, Topaz and DRM files are refused with a message. Limits: XML depth 256,
HTML repair at most 256 open elements, zip entries 64 MiB, MOBI text 64 MiB.

### ink-mupdf

MuPDF 1.27.2, compiled by `build.rs` with zig for the Kindle and the host (tarball cached
in `inkreader-wt/.cache/mupdf`, SHA-256 checked; `INK_MUPDF_CACHE`, `INK_MUPDF_CC`).
`MupdfProvider` opens PDF, CBZ and XPS. `MupdfDocument` (fixed layout): a page index is a
*screen*; positions are `page:N;x0,y0,x1,y1` (PDF page plus the tile's cell). Inherent
methods (not reachable through `dyn Document` yet): `set_options(FixedOptions)` (fit,
zoom, crop, columns, contrast, gamma, rotation, spreads, rtl), `render_page_at`,
`highlight_rects`, `prefetch`, `thumbnail`; `cover_thumbnail(path, w, h)`. Until the
reader exposes options, `RINKY_FIXED="fit=width,crop=1,..."` sets them. The static
shim library is named `mupdf_shim_ink` (zig 0.17 `cc` panics on `-link...` names).

### ink-store

`partial_md5(path)` (KOReader `util.partialMD5`, checked against LuaJIT vectors and a
real sidecar), `BookState` v2 in `<stem>.sdr/rinky.<ext>.json` (one file per format,
like KOReader's `metadata.<ext>.lua`; `sidecar::sidecar_path(book)` gives the name; atomic
write with fsync, `.old` backup, corrupt files moved aside; an old shared
`<stem>.sdr/rinky.json` is still read, never written; falls back to the prototype's
`BOOK.rinky`), `LibraryIndex` (metadata, covers, collections, 10k books,
`scan_report`), `Settings` (+ `load_or_recover`) and `PluginStore`, `stats::StatsDb`
(rusqlite, bundled SQLite), `koreader::{read_book_metadata, import_book,
read_statistics_db, read_vocabulary}` (read-only; Lua literal parser `lua`, never
executes), `Paths` (`/mnt/us/rinky/...`), `write_atomic`, `selftest` (run by
`rinky selftest`). On 5.19.5 `/mnt/us` is `fuse.fsp`: set file times by path
(`utimensat`), keep SQLite writes off the UI thread (about 70 ms per transaction).

### ink-net

`HttpClient::send(Request) -> Response` (streaming body, `url` after redirects; head /
per-read / total timeouts; fakes use `Response::new(status, headers, body, url)`),
`UreqClient` (+ `ClientConfig`, `TlsRoots`; ureq 3, rustls with ring, webpki-roots),
`sse::{SseParser, SseReader, EventSource}` (4 MiB per event by default),
`download::{Download, Hooks, Checksum}` (`.part` + `Range` resume, `<dest>.part.lock`,
`NetError::Busy`), `connectivity::{Connectivity, KeepAwake, NetActivity, wifi_rule}`,
`opds::{parse_feed, parse_opds2, parse_any, OpdsClient, fill_template}`,
`kosync::{KoSyncClient, KoSyncAccount, Progress, SyncQueue, decide}`,
`testserver::TestServer` (HTTP/HTTPS mock, also for plugin tests), `selftest`
(`rinky selftest --net URL`). Power-loop wiring for the app (not done yet): on
`PowerEvent::ReadyToSuspend` call `NetActivity::on_ready_to_suspend(&keep)`, ask
`may_auto_suspend()` before an idle sleep, `interrupt_all()` before a power-key sleep.

### ink-plugin

`Plugin` (manifest, on_load(&mut Registrar), on_book_open, on_page_change,
on_selection, on_suspend, on_resume, on_close, on_action, on_ui_result),
`Registry`/`Registrar` (menu items, selection actions, gestures, settings pages,
library actions, status widgets), `HostCtx { doc, ui, storage, net, permissions }`
with `http()` and `document()` permission checks, `DocumentAccess` (+ `DocAccess` over
any `Document`), `UiRequest` queue, `PluginStorage`, `PluginManifest` + `Permission`,
`PluginManager` (load/broadcast/action; a failing plugin is disabled and its errors are
collected), `lua::LuaHost` (sandboxed Lua 5.4 with a per-call time limit).
Wave A (`plugin`; the full API is in `docs/plugins.md`): `HostCtx` is `#[non_exhaustive]`,
built with `HostCtx::new(ui, storage, perms).with_doc(..).with_net(..).with_jobs(..)
.with_hosts(..)`, checked accessors `document()`, `ui()`, `storage()`, `data_file()`,
`http()`, `send()`, `start_request()`. New trait methods with defaults: `Plugin::kind`,
`on_library_action`, `on_http`. `PluginManager::{library_action, ui_result, http_event,
disable, info, manifest, is_enabled, record_error, max_errors}`, `PluginError::Limit`,
`UiRequest::Status`, `ink_plugin::{net, markdown}`, `lua::{LuaPlugin, load_dir}`;
re-exports `serde_json`. Plugin settings live in `plugins/<name>/settings.json` over
read-only defaults from `[plugins.<name>]` in `settings.toml`.

### ink-app

`App::new(Device, Settings, Paths).run(Route)`, `router::{Route, Transition, AppScreen,
AppCtx, ScreenRegistry}`, `screens::{reader, library, settings, demo}`, `plugin_host`,
`plugins::compiled_in()`, `cli`, `selftest`, `spike`, `update`.
- `App::new` builds the plugin host with `PluginHost::load(compiled_in(), &settings,
  &paths)` (also loads Lua plugins from `/mnt/us/rinky/plugins`). `PluginHost::ui` is a
  `PluginUi` overlay presenter (toasts, dialogs, menus, Markdown viewer, keyboard input);
  screens call `handle_event` first and `paint_overlay` last (`docs/plugins.md`, "For
  screen authors"). An open input box receives `UiEvent::Touch` too.
- CLI subcommands added in wave A: `device ...` (ink-device devtool), `update [check|install]`
  (in-app update from a GitHub release, `update.rs`, flock on
  `/mnt/us/rinky/.update.lock`), `selftest [--device [--write] [--ntp S]] [--net URL
  [--net-tls URL --ca PEM] [--net-scratch DIR]]` (store checks always run). An option
  that needs a value and has none is an error (`option_value`); `main` reads
  `args_os()` and refuses a non-UTF-8 argument with an error instead of a panic; the
  logger and `main`'s error line never panic on an unwritable stderr.

## Data flow

```text
evdev / lipc-wait-event / minifb / script
        |  InputEvent (panel coordinates)
        v
App::run --translate--> GestureRecognizer (rotation-aware) --> UiEvent (logical)
        |                                                        |
        |                         top AppScreen::handle(ev, AppCtx) -> Transition
        |                                                        |
        |                    Push/Replace/Pop/Exit on the screen stack
        v
AppScreen::paint(AppCtx) -> canvas + Vec<Damage> -> Screen::update(rect, class)

Reader: Provider::open(path) -> Box<dyn Document> -> set_layout -> render_page(i)
        + footer; RefreshScheduler::page_turn() picks REAGL/GC16;
        BookState saved after each page; PluginHost::page_change(doc, i).
```

## Ownership map

Phase 0 (architect) owned the whole repository. From wave A on, each agent edits only
its paths. **The integrator owns the root `Cargo.toml`, `Cargo.lock` and every other
shared file** (README, docs/ARCHITECTURE.md, .cargo/, scripts not listed below,
crates/ink-app outside the screen/plugin paths, tests/sim). That includes
`crates/ink-app/src/main.rs`, `cli.rs`, `selftest.rs`, `app.rs` and
`crates/ink-app/tests/verify_cli.rs` (added by the p0fix verifier); `scripts/emu-slot.sh`,
`scripts/e2e/` (run.sh, lib.sh, slot and launcher race tests, attack tools, written by
p0fix) are integrator-owned from now on.

| Wave | Key | Owns |
| --- | --- | --- |
| A | `ui` | `crates/ink-text`, `crates/ink-ui` (also `crates/ink-app/src/screens/demo/`) |
| A | `html` | `crates/ink-html`, `crates/ink-doc`, `tests/golden` |
| A | `store` | `crates/ink-store` |
| A | `device` | `crates/ink-device`, `scripts/rinky.sh`, `scripts/rinky-watchdog.sh`, `scripts/device.sh`, `tests/e2e/device-helpers/` |
| A | `mupdf` | `crates/ink-mupdf` |
| A | `formats` | `crates/ink-formats`, `testdata/formats` |
| A | `net` | `crates/ink-net` |
| B | `reader` | `crates/ink-app/src/screens/reader`, `crates/ink-app/tests/footer_golden.rs`, `crates/ink-app/tests/golden/footer-*.png` |
| B | `library` | `crates/ink-app/src/screens/library` |
| B | `settings` | `crates/ink-app/src/screens/settings` |
| B | `plugin` | `crates/ink-plugin`, `crates/ink-app/src/plugin_host.rs`, `docs/plugins.md`, `plugins/sample-lua`, `tests/e2e/plugin-fixtures/` |
| C | `insightful`, `dictionary`, `stats`, `vocab`, `opds`, `kosync`, `calibre`, `wikipedia` | `plugins/<name>`; each adds one line to `crates/ink-app/src/plugins.rs` |
| C | `polish` | i18n catalogs and performance fixes |
| C | `packaging` | `scripts/package.sh`, `packaging/`, `docs/user-guide.md`, `crates/ink-app/src/update.rs` |

Shared-by-convention files:

- `crates/ink-app/src/plugins.rs`: wave C agents each add exactly one line.
- `tests/e2e/<key>-*.sh`: each agent adds only files with its own key prefix.
- `crates/ink-html/tests/golden.rs` and `tests/golden/`: the `html` agent. Others who
  change rendering on purpose regenerate with
  `scripts/golden-update.sh out/evidence/<key>/golden-diff` (a new or empty directory:
  one that holds other files is refused, never emptied) and list the diff images as
  evidence; the integrator re-checks after the merge. `scripts/golden-update.sh` also
  regenerates the footer goldens (`crates/ink-app/tests/golden/`, owner `reader`).

## How to add a screen

1. Create `crates/ink-app/src/screens/<name>/mod.rs` with a type implementing
   `router::AppScreen` (`name`, `on_enter`, `handle`, `paint`, optional `on_exit`,
   `timeout`).
2. Add `pub fn register(reg: &mut ScreenRegistry) { reg.register("<name>", |route| ...) }`.
3. The integrator adds a `Route` variant (if needed), the `pub mod <name>;` line and the
   `<name>::register(reg)` call in `screens/mod.rs` (ask through an api-request).
4. Screens draw into `cx.canvas` and return `Damage`; or, for full-page content, call
   `cx.screen.update(...)` themselves with the class from `cx.refresh`.

## How plugins register

- Rust: a crate in `plugins/<name>` depending on `ink-plugin` implements `Plugin`. In
  `on_load(reg)` it calls `reg.menu_item(id, label, order)`,
  `reg.selection_action(id, label)`, `reg.gesture(id, gesture, label)`,
  `reg.action(id, label)` (offered in the user's gesture map),
  `reg.settings_page(id, title)`, `reg.library_action(id, label)`,
  `reg.status_widget(id, slot, text)`. When the user triggers an entry the app calls
  `on_action(cx, id)`. One line in `crates/ink-app/src/plugins.rs` compiles it in.
- Lua (phase 7): `/mnt/us/rinky/plugins/<name>/plugin.toml` + `main.lua`, loaded by
  `ink_plugin::lua::LuaHost`; the same registry and host services, exposed as Lua tables.
- Permissions come from the manifest: `document`, `ui`, `storage`, `network`.
  `HostCtx::http()` and `HostCtx::document()` refuse without them.
- UI is asynchronous: plugins queue `UiRequest`s and get answers in `on_ui_result`.
- The full plugin API (Rust and Lua), the host calls screens must make, and the sample
  Lua plugin are documented in `docs/plugins.md`.

## Changing a shared API

1. Do not change or remove an existing public item in a crate you do not own.
2. Make the smallest additive change in your own crate. If you need something in another
   crate, write it down for the integrator: crate, item, signature,
   why, and a default implementation if it is a trait method (so nothing breaks).
3. New third-party dependencies: add them to your own crate's `Cargo.toml` with an
   explicit version and list them in your report; the integrator hoists them into
   `[workspace.dependencies]`.
4. The integrator applies requests when merging, then runs
   `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
   the golden tests and `scripts/e2e/run.sh`.

## Toolchain

- Rust 1.91 (`workspace.package.rust-version`); `.cargo/config.toml` sets
  `resolver.incompatible-rust-versions = "fallback"` so new dependencies stay
  compatible.
- Kindle build: `cargo zigbuild --release --target armv7-unknown-linux-musleabihf`
  (zig 0.17.0, cargo-zigbuild 0.24.0; do not reinstall). The result is a static ARMv7
  hard-float musl binary (`target/armv7-unknown-linux-musleabihf/release/rinky`;
  4.5 MB at phase 0, 19,979,512 bytes after the wave A merge on 2026-10-30: SQLite,
  MuPDF, hyphenation patterns, html5ever, image decoders). The old `rust-lld` route is commented in `.cargo/config.toml`; it
  cannot build C dependencies.
- Every crate in `[workspace.dependencies]` was built and linked for the ARM target on
  2026-10-24 with a throwaway crate that calls into each of them (mlua 0.12 vendored
  Lua 5.4, rusqlite 0.37 bundled SQLite 3.50, ureq 3.5 + rustls/ring + webpki-roots,
  html5ever 0.39 + markup5ever_rcdom 0.39, cssparser 0.35, selectors 0.31, rustybuzz
  0.20, ttf-parser 0.25, fontdue 0.9, unicode-linebreak, unicode-segmentation,
  hyphenation 0.8 (embed_all: +3.0 MB), image 0.25 (jpeg/png/gif/webp), resvg/usvg 0.49,
  tiny-skia 0.12, zip 9, quick-xml 0.37, png 0.18, md-5 0.10, serde, serde_json,
  toml 1.1, encoding_rs, chardetng, crossbeam-channel, log, env_logger, anyhow,
  thiserror, libc). None failed. minifb 0.28 is host-only (target-gated).
- Added to `[workspace.dependencies]` in wave A: `rustls`, `ring`, `flate2`
  (`default-features = false`; members add features), `zune-jpeg`, `zune-core`.
  `cc` stays a build-dependency of ink-mupdf only.
- ink-mupdf's `build.rs` needs `zig`, `curl` (first build only), `shasum`, a `tar` that
  reads lzip and `make`; the MuPDF tarball and built libraries are cached in
  `inkreader-wt/.cache/mupdf` (about 1 min per target on a cold cache).
- `scripts/package.sh [--no-build] [--allow-dirty] [--version V] [--out DIR]` builds the
  install package (`out/package/rinky-V-kindlehf.tar.gz`); it refuses a tree with
  uncommitted changes unless `--allow-dirty`.
- `export CARGO_BUILD_JOBS=2` in every shell (shared machine).
- Dev profile keeps only line tables, dependencies without debug info (disk).

## Emulator slots

Use only `scripts/emu-slot.sh` (never `./eink run` by hand, never `pw12-5195-jb`, never
ports 4022/4023):

```sh
scripts/emu-slot.sh up <key>-jb        # acquire a slot, create from the 5.19.5 base with --jbsh,
                                       # boot with -snapshot (~5 min), wait for SSH, jailbreak, Home
scripts/emu-slot.sh deploy <key>-jb    # zigbuild + copy binary, launcher, watchdog, test books
                                       # (--no-build: copy the last build only)
scripts/emu-slot.sh ssh <key>-jb 'cmd' # also: push, pull, shot, tap, swipe, hold, power, log, wifi
scripts/emu-slot.sh restart <key>-jb   # fresh boot in the same slot (the slot stays held)
scripts/e2e/run.sh <key>-jb OUTDIR 'key-*'
scripts/emu-slot.sh down <key>-jb      # QMP quit (then TERM/KILL), release the slot (also on failure!)
scripts/emu-slot.sh status | cleanup | port NAME | slotdir NAME
```

- At most 4 slots (`../inkreader-wt/.emu-slots/slotN`).
  Slot N: SSH port 4110+10N, Wi-Fi SSH port 4111+10N. If no slot is free, do host-side
  work and try again later (a loop that retries `up` every few minutes is fine: a failed
  `up` gives back a slot it took itself).
- A slot is a directory made with `mkdir` (atomic) holding `owner`, `acquired` (epoch
  s), `acquirer` (pid); after `up` also `qemu.pid`, `boot_s` (QEMU start to Home, s) and
  `fw_cycles` (framework cycles since that boot, counted by `scripts/e2e/run.sh`).
  `up`/`restart` keep `up.pid` while they work.
- `acquire`, `release` and `cleanup` run under an `flock` on `.emu-slots/.lock`
  (perl `flock`, fd 9; the kernel drops it when a holder dies). So several callers at
  once get distinct slots, one name acquired by several callers gets one slot, and
  `cleanup` cannot remove a lock an `acquire` is still writing.
- `down`, and a failed `up` that gives back the slot it took, remove the slot directory
  only under that lock and only while its `owner` is still NAME: if another agent's
  `cleanup` dropped the lock (QEMU gone) and its `acquire` took the same slot in the
  meantime, the new owner's lock stays.
- `up` and `restart` of one name are serialized by `.emu-slots/.up-NAME.lock` (fd 8,
  held until the process exits; QEMU and `eink create` start without it). A second
  `up NAME` waits for the first (up to `EMU_UP_WAIT_S`, default boot deadline + 20
  min) and then reuses the running QEMU; it never starts a second one.
- `cleanup` never kills a process. It keeps a lock whose QEMU runs (and repairs a wrong
  `qemu.pid` when the owner's QEMU runs under another pid), a lock whose `up`/`restart`
  still runs (`up.pid`), and a lock younger than 20 minutes without a QEMU (age from
  `acquired`, else the directory mtime). It removes a lock whose QEMU is gone.
- `up` of an instance that already runs in its slot reuses it; `up` refuses when the
  instance runs as a QEMU outside its slot, or when another program listens on the
  slot's ports. `down` stops only NAME's own QEMU (QMP quit, then SIGTERM, then
  SIGKILL, each after `EMU_QUIT_WAIT_S`), also one without a slot record.
- Environment: `EMU_TIME_SCALE` (factor for every deadline), `EMU_BOOT_DEADLINE_S`
  (900), `EMU_SLOT_GRACE_S` (1200), `EMU_QUIT_WAIT_S` (30), `EMU_UP_WAIT_S`,
  `EMU_SLOTS`, `EMU_SLOT_PORT_BASE` and `EMU` (tests point these at a private directory
  and a fake emulator: `scripts/e2e/slot-test.sh`, `scripts/e2e/verify-slot-races.sh`,
  and main's original `scripts/emu-slot-test.sh`).
- `scripts/e2e/run.sh` copes with guest degradation and a slow host: scenarios declare
  `# e2e-cycles: N`; before a scenario that would take the boot past `E2E_MAX_CYCLES`
  (12) framework cycles it runs `restart` + `deploy --no-build`; a scenario that exits
  75 (guest not ready: lib.sh `not_ready`) or hits its time limit is retried once after
  a restart (`E2E_RETRY`); `E2E_RESTART=always` restarts before every scenario;
  `E2E_TIME_SCALE` (default auto = max(1, boot_s/300, 0.5 + load/CPUs), at most 4)
  scales every deadline (never below 1x). A scenario that checked nothing exits 77
  (lib.sh `skip`, e.g. `plugin-tls` without `INK_E2E_INTERNET=1`); run.sh lists it as
  SKIP, not PASS. `scripts/e2e/run-test.sh` tests these rules with a fake emulator.
- Scenarios that need extra ARM build products build them (incrementally) themselves:
  `html-render`/`html-selftest` the `inkhtml` example, `formats-selftest` and the
  `*-arm-tests` scenarios their test binaries. `deploy` builds only `ink-app`.
- `scripts/e2e/lib.sh` `wait_home` / `wait_clean_home` save as `NN-LABEL.png` the
  screendump that passed the check (`keep_probe`), not a second screendump.
- Every run uses `-- -snapshot`: jb.sh repeats on every boot and guest writes vanish when
  QEMU exits. Boot once, then redeploy many times.
- `tap`/`swipe`/`hold` inject QMP absolute-pointer events (the emulator's FT5536G model
  turns them into `fts_ts` multi-touch). `power` runs `rinky inject power` in the
  guest, which writes `EV_KEY KEY_POWER` press/release records into the
  `bd71828-pwrkey` evdev node (apps that read evdev see it), and sets powerd's
  `powerButton 1` (powerd ignores injected evdev events; it reacts 20-90 s later, and not
  at all while the framework is stopped). QEMU's PW6 board model has no power-button
  property, and the emulated USB cable keeps the kernel from suspending
  ("Sleep and the Kindle framework").
- `wifi` joins the open `Kindle-QEMU` AP with `wpa_cli`; the guest then reaches the
  host at `10.0.2.2` (mock HTTP servers bound to 127.0.0.1 on the host). Without Wi-Fi
  the host is `192.168.15.201` over USB networking. On a fresh boot the supplicant can
  stay in `SCANNING` (ap_scan 2); `wifi` then runs `wpa_cli reassociate` after 10 s and
  `ap_scan 1` + `select_network` + `reassociate` after 20 s. The device scenarios join
  through rinky's own wifid calls instead (`tests/e2e/device-helpers/lib.sh`).
- Home detection (`scripts/emu-screen.py home`) looks for the 5.19.5 Home tab bar;
  `appmgrd activeApp` says `com.lab126.KPPMainApp` for Home and books alike.

## Evidence layout

`out/evidence/<key>/...` (outside git): one
directory per acceptance check or scenario, holding screenshots (`NN-label.png`), logs
(`*.log`, `*.txt`) and `summary.txt` from `scripts/e2e/run.sh`. Report every path in the
structured result; look at screenshots before claiming what they show.

## Test commands

```sh
export CARGO_BUILD_JOBS=2
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p ink-html --test golden            # golden pages (exact match)
scripts/golden-update.sh [DIFF_DIR]             # regenerate goldens + diff images (DIFF_DIR new or empty)
cargo zigbuild --release --target armv7-unknown-linux-musleabihf --workspace
rinky render testdata/juliet.epub OUT 6     # host PNGs (prototype-identical)
rinky sim testdata/juliet.epub [--ghost] [--script tests/sim/turn-pages.txt]
rinky sim BOOK --headless --script FILE --out DIR [--frames]
rinky demo --headless --script tests/sim/demo.txt --out DIR
cargo test -p ink-app --test screens            # every screen headless with scripted input
scripts/png-compare.py A B                      # pixel comparison of PNGs or directories
scripts/e2e/slot-test.sh                        # slot logic (private lock dir, fake emulator)
scripts/e2e/verify-slot-races.sh                # down vs cleanup+acquire, simultaneous up, killed restart/down
scripts/emu-slot-test.sh                        # main's original lock-logic test (still passes)
scripts/clippy-arm.sh                           # clippy for the ARM target (zig cc for C deps)
scripts/e2e/run.sh NAME OUTDIR [PATTERN]        # emulator scenarios
scripts/e2e/run-test.sh [OUTDIR]                # run.sh recovery rules (fake emulator)
scripts/e2e/launcher-race-test.sh [OUTDIR] [N]  # launcher + watchdog with stub upstart (host)
scripts/e2e/attack-inputs.sh OUTDIR [BIN]       # malformed books / odd arguments (host)
scripts/e2e/phase0-host.sh OUTDIR               # all host-side phase 0 checks, steps.txt
rinky selftest --lipc --book B --url U      # on the guest
```

## Golden tests

Two sets, both exact matches:

- Page content: `crates/ink-html/tests/golden.rs` opens books through `ink-formats` (a
  dev-dependency: `ink-formats` depends on `ink-html`; integration tests link the same
  library, so this cycle is fine) and renders page content only at 1272 x 1696 against
  `tests/golden/*.png`.
- Reader footer: `crates/ink-app/tests/footer_golden.rs` draws the footer
  (`screens::reader::draw_footer`) for fixed page/total pairs and two font sizes and
  compares the bottom-margin band with `crates/ink-app/tests/golden/footer-*.png`
  (independent of the HTML engine). It also checks that `screens::reader::render_page`
  on juliet and synthetic equals content-only rendering above the band and content +
  footer inside it. The `reader` agent regenerates these when the footer changes.

`INK_GOLDEN_UPDATE=1` rewrites, `INK_GOLDEN_OUT` is where actual and diff images go,
`INK_GOLDEN_DIR` reads page goldens elsewhere. `scripts/golden-update.sh [DIFF_DIR]`
regenerates both sets and lists the diff images. DIFF_DIR must be new or empty; the
default `target/golden-diff` is cleared first.

## Run mode on the Kindle

`scripts/rinky.sh` detaches (setsid), starts `rinky-watchdog.sh`, runs
`stop lab126_gui` (ignoring SIGTERM meanwhile, as KOReader does), waits until
`KPPMainApp` and `pillowd` are gone, runs `rinky run BOOK`, and on every exit path
runs `start lab126_gui` (skipped when `status lab126_gui` already says `start/`), after
restarting appmgrd (otherwise 5.19.5 shows an "Application Error" dialog on Home;
`RINKY_RESTART_APPMGRD=0` disables the restart). The
watchdog polls once a second; if the launcher disappears it stops rinky and starts
the framework; if rinky disappears and the launcher does not finish within 15 s it
starts the framework itself. Runtime files: `/tmp/rinky/{lock,launcher.pid,
rinky.pid,done}`; logs: `/mnt/us/rinky/{rinky.log,watchdog.log}`.
Sleep and wake: see "Sleep and the Kindle framework" below.

## Sleep and the Kindle framework

Firmware 5.19.5 behaves as follows while rinky runs with `lab126_gui` stopped.

- **powerd ignores the power button.** On a press, powerd asks the Java framework
  (`com.lab126.kaf`) for `frameworkStarted`; with the framework gone the call fails and
  powerd logs `Splash screen is on.  Ignoring power button`. It sends no
  `goingToScreenSaver` or `readyToSuspend` and never suspends. So rinky reads
  `KEY_POWER` from the `bd71828-pwrkey` evdev node itself, draws its own screensaver and
  suspends the kernel directly (`crates/ink-device/src/sleep.rs`,
  `kindle/system.rs`).
- **powerd events.** With the framework running, powerd publishes the events KOReader
  handles (`goingToScreenSaver`, `outOfScreenSaver`, `exitingScreenSaver`,
  `readyToSuspend`, `wakeupFromSuspend`, `charging`, `notCharging`) plus
  `notReadyToSuspend` and `t1TimerReset`. `t1TimerReset` arrives on every touch, also
  with the framework stopped. The first argument of `goingToScreenSaver` /
  `outOfScreenSaver` is the source (1 = button wake, 2 = button).
- **Injected key presses.** powerd ignores key events injected into evdev;
  `lipc-set-prop -i com.lab126.powerd powerButton 1` is its own path for a press.
- **Suspend can be refused.** Writing `mem` to `/sys/power/state` fails at once with
  `EBUSY` while a wakeup source is active, for example the USB controller while a cable
  or USB networking is up. The emulator always has that source active, so real sleep and
  wake can only be tested on a device.
- **"Application Error" on Home.** After `start lab126_gui`, Home can show
  "Application Error: The selected application could not be started", also for a plain
  stop and start without rinky. KPPMainApp's post-stop hook reports every stop to
  appmgrd as `backgroundAppUnLoaded`; appmgrd treats that as an abnormal exit and asks
  pillow to show the alert, which is visible when the request reaches pillow after the
  framework is back. Restarting appmgrd before starting the framework avoids it; the
  launcher and the watchdog do that (`RINKY_RESTART_APPMGRD=0` turns it off).

- **Orphaned framework daemons stall powerd.** Several upstart jobs run their daemon
  through a shell (`exec fastmetrics 2>&1 | logger`, `su framework -c ...`). When
  `stop lab126_gui` stops such a job, upstart kills the shell but the daemon can
  survive, reparented to init and no longer managed by the job. On a PW12 an orphaned
  `fastmetrics` got stuck. powerd sends it a metric before it handles each cover or
  power event, and each send waited about 32 s for a timeout
  (`FM Metrics Emission:status :4` in `/var/log/messages`). So every powerd event came
  32 s late, and every lipc read of a powerd property timed out. The launcher records
  the process tree of every running job before the framework stop and afterwards kills
  what a stopped job left behind (`fastmetrics`, `wifilocker`, `minerva_service` were
  seen). It also stops the Amazon services rinky has no use for (`QUIET_JOBS` in
  `scripts/rinky.sh`: cloud sync, metrics, log uploaders, audio and TTS, Bluetooth,
  appmgrd), lists them in `/tmp/rinky/quieted`, and the launcher or the watchdog
  starts them again before the framework. Kept: powerd, lipcd, deviced, cmd, wifid,
  wifis, volumd, mtp, tmd, battstressd, X and the system jobs. `RINKY_QUIET=0` leaves
  them all running; `RINKY_QUIET_JOBS` replaces the list. Reading
  `/sys/devices/platform/eink_hall/hall_test_trigger` injects a fake "cover opened"
  event: never read it.

`rinky spike-suspend` is a small test screen for these paths: the first power press
draws a screensaver and tries to suspend, the next one wakes. Scenario:
`tests/e2e/arch-suspend-spike.sh`.
