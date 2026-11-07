# rinky architecture

rinky is one static ARMv7 binary that replaces the stock reading software on Kindles
and Kobos while it runs. This page is a map: which crate and module holds each part,
how a page turn travels through them, and the device behaviour you need to know before
changing the launcher, sleep or the screen code. Each crate's `lib.rs` documents its
own modules in more detail.

The main choices behind the layout:

- **Two document engines.** An own HTML/CSS engine lays out EPUB, FB2/FB3, MOBI/AZW3,
  HTML and TXT, so positions match KOReader's crengine xpointers. PDF, CBZ and XPS go
  through MuPDF over C FFI.
- **One plugin API.** First-party plugins are Rust crates compiled in; user plugins are
  Lua 5.4 scripts on the device. Both implement the same `Plugin` trait.
- **The app owns the device.** On start the launcher stops the stock UI (the Kindle
  framework or Kobo's Nickel). rinky then handles sleep, light, Wi-Fi, USB and the
  screensaver itself, and the stock UI comes back on exit or crash.
- **AGPL-3.0-or-later**, because MuPDF is AGPL.

## Where things are

| Concept | Crate | Module or file |
| --- | --- | --- |
| Pixels: 8-bit gray canvas, geometry, rotation | ink-device | `canvas.rs`, `geom.rs`, `screen.rs` (`copy_to_panel`) |
| Screen, input and service traits; `Device` | ink-device | `screen.rs`, `input.rs`, `services.rs`, `lib.rs` |
| Kindle and Kobo backend: framebuffer, refresh ioctls, model table | ink-device | `ereader/fb.rs`, `ereader/model.rs`, `ereader/mod.rs` |
| Touch decoding (evdev protocols A and B, Kobo "snow") | ink-device | `evdev.rs`, `ereader/input.rs` |
| Sleep, screensaver, cover sensor, kernel suspend | ink-device | `sleep.rs`, `screensaver.rs`, `events.rs`, `ereader/system.rs` |
| Kindle system services through `lipc` | ink-device | `lipc.rs`, `ereader/kindle.rs` |
| Kobo light, battery, Wi-Fi without Nickel | ink-device | `ereader/kobo/` |
| Undoing device changes after a crash | ink-device | `journal.rs` (`rinky device restore`) |
| Host window and headless backends; scripted input | ink-device | `sim.rs`, `headless.rs`, `script.rs` |
| Kindle or Kobo, and the paths that follow from it | ink-platform | `lib.rs` (`Platform`, `install_root`, `font_dirs`) |
| Fonts, shaping, line breaking, hyphenation | ink-text | `fonts.rs`, `shape.rs`, `linebreak.rs`, `hyphen.rs` |
| Widgets, design tokens, on-screen keyboard | ink-ui | `widget.rs`, `controls.rs`, `theme.rs`, `keyboard.rs` (see `docs/DESIGN.md`) |
| Gestures and the event loop's input step | ink-ui | `gesture.rs`, `pump.rs` |
| Refresh classes per change, flash policy | ink-ui | `refresh.rs` (`RefreshScheduler`, `Change`) |
| `Document` and `Provider` traits, positions | ink-doc | `lib.rs`, `position.rs` |
| Reflowable layout: DOM, CSS, pages, page-map cache | ink-html | `parse.rs`, `style.rs`, `layout.rs`, `paginate.rs` |
| Book formats into the reflowable engine | ink-formats | `epub.rs`, `fb2.rs`, `mobi/`, `txt.rs`, `book.rs` (`BookDocument`) |
| PDF, CBZ, XPS | ink-mupdf | `document.rs` (`MupdfDocument`), `build.rs` |
| Book identity (KOReader partial MD5) | ink-store | `md5.rs` |
| Reading position, highlights, bookmarks per book | ink-store | `sidecar.rs` (`<book>.sdr/rinky.<ext>.json`) |
| Library index, settings, statistics, vocabulary | ink-store | `library.rs`, `settings.rs`, `stats.rs`, `vocab.rs` |
| KOReader import (read-only) | ink-store | `koreader/`, `lua.rs` |
| StarDict dictionaries | ink-dict | `stardict.rs`, `library.rs`, `markdown.rs` |
| HTTP(S), SSE, downloads, OPDS, KOSync | ink-net | `http.rs`, `sse.rs`, `download.rs`, `opds.rs`, `kosync.rs` |
| Plugin trait, registry, host services, Lua host | ink-plugin | `lib.rs`, `registry.rs`, `host.rs`, `lua/` |
| Event loop and screen stack | ink-app | `app.rs`, `router.rs` |
| Screens: reader, library, settings, statistics, widget gallery | ink-app | `screens/<name>/` |
| What the panel shows (to avoid flashes) | ink-app | `shown.rs` |
| Plugin host: loading, UI overlays, network worker | ink-app | `plugin_host.rs`, `plugin_card.rs`, `plugin_entries.rs` |
| Compiled-in plugins | plugins/* | one line each in `crates/ink-app/src/plugins.rs` |
| Command line (`rinky run`, `sim`, `render`, `selftest`, ...) | ink-app | `cli.rs`, `main.rs` |
| In-app update | ink-app | `update.rs` |
| Kindle launcher and watchdog | scripts | `rinky.sh`, `rinky-watchdog.sh`, `packaging/rinky-ctl.sh` |
| Kobo launcher and NickelMenu entry | packaging | `kobo/rinky.sh`, `kobo/nm-rinky` |
| Install packages | scripts, packaging | `scripts/package.sh`, `packaging/install.sh` |

First-party plugins in `plugins/`: `kosync` (progress sync), `opds` (catalogs),
`calibre` (wireless connection), `dictionary` (look up, with ink-dict), `stats` (crate
`ink-stats`: reading statistics and the vocabulary table), `insightful` (AI chat about
the book), `wikipedia`, `usbnet` (USB Ethernet gadget), `ssh` (bundled dropbear), and
`sample-lua`, a crate that embeds and tests the sample Lua plugin.

### Dependencies between crates

```text
ink-platform                       (no ink dependencies)
ink-device   <- ink-platform
ink-text     <- ink-device
ink-ui       <- ink-device, ink-text
ink-doc      <- ink-device
ink-html     <- ink-platform, ink-device, ink-text, ink-doc
ink-formats  <- ink-device, ink-doc, ink-html
ink-mupdf    <- ink-device, ink-doc
ink-dict     <- ink-html            (its HTML parser and CSS cascade, for definitions)
ink-store    <- ink-platform
ink-net                            (no ink dependencies)
ink-plugin   <- ink-device, ink-doc, ink-net
plugins/*    <- ink-plugin, plus ink-net, ink-store, ink-doc, ink-dict, ink-stats as needed
ink-app      <- everything
```

Dev-dependencies only: ink-html uses ink-formats and ink-store in its tests (the golden
pages open real books), ink-formats uses ink-store. ink-net knows nothing of the
device: Wi-Fi state and keeping the device awake reach it through its `Connectivity`
and `KeepAwake` hooks. So far only `rinky selftest --net` wires them (with `--lipc`);
the app's sleep path does not consult them yet (`PLAN.md`).

## A page turn, end to end

What happens between a tap on the right side of a page and the next page on the panel:

1. **Touch.** The kernel reports the contact on the touch evdev node. The e-reader
   backend's input loop (`ink-device/src/ereader/input.rs`) decodes it with
   `evdev::TouchDecoder` into a `TouchFrame` in panel pixels, delivered as
   `InputEvent::Touch`. A down and up in one frame (`TouchPhase::DownUp`) is the PW12's
   short tap.
2. **Gesture.** The app loop (`App::run_loop` in `ink-app/src/app.rs`) calls
   `ink_ui::pump::next_events`, which feeds the frame to the `GestureRecognizer`. It
   maps panel to logical coordinates through the current rotation and emits
   `UiEvent::Touch` notifications and, on lift, `UiEvent::Gesture(Gesture::Tap)`.
3. **Plugin overlays first.** `plugin_host::hooks::app_handle` offers the event to an
   open plugin dialog, menu or keyboard. None is open, so the event goes to the top
   screen's `AppScreen::handle`.
4. **Gesture map.** `ReaderScreen::handle_reading` passes the tap to
   `screens/reader/actions.rs`, which classifies it into a trigger (`tap_right`) and
   looks up the user's action in `settings.gestures` (default: next page).
5. **State change, no drawing.** `ReaderScreen::turn` moves `self.page`, asks
   `RefreshScheduler::page_turn()` for the refresh class (REAGL, or GC16 every N turns,
   on image pages, or when ghosting debt is due) and, when the page-turn wave setting
   is on, the direction of the wave. `handle` returns `Transition::None`.
6. **Paint.** The app loop calls `plugin_host::hooks::app_paint`, which calls
   `ReaderScreen::paint`. It sets the document's current page, tells the plugins
   (`PluginHost::page_change`, so their status texts are ready), renders the page with
   `Document::render_page` into the canvas, draws highlights, the bookmark corner and
   the status bars (`screens/reader/footer.rs`), and keeps a copy of the page so an
   overlay can close without rendering again.
7. **Layout, if needed.** For a reflowable book `render_page` runs in ink-html. The
   page map says where each page starts; a background thread builds it and caches it
   on disk (`paginate.rs`). For a PDF, ink-mupdf renders the page tile through MuPDF.
8. **One refresh.** The reader sends the whole page and its bars in one
   `Screen::update_wave` (or `update`) call: no animation frames and no separate bar
   update. The screen is wrapped by `shown::ShownScreen`, which keeps a copy of what
   the panel shows.
9. **To the panel.** `ereader/fb.rs` rotates the canvas into the framebuffer
   (`copy_to_panel`, which also inverts for night mode where the controller does
   not), builds the refresh struct for this model's controller (`update_data_epdc`)
   and sends the ioctl. It does not wait for a FULL update to finish; it waits only
   before it next writes into the same rectangle.
10. **Saving the position.** The page shown is noted at once, and written to the
    sidecar (`<book>.sdr/rinky.<ext>.json`, through `ink_store::write_atomic`) after
    two idle seconds or at most ten seconds later (`POSITION_SAVE_IDLE`,
    `POSITION_SAVE_MAX`), so fast page turns do not each wait for the user store.

The app loop then waits for the next event with the shortest timeout any screen,
plugin or timer asked for, so an idle reader uses no CPU.

## Opening a book

The library pushes `Route::Reader(path)`. The reader asks each provider in
`app::PROVIDERS` (`FormatsProvider`, then `MupdfProvider`) whether it can open the file:

- `FormatsProvider` converts the format into XHTML chapters (`ink_formats::load`) and
  returns a `BookDocument`: the ink-html `HtmlDocument` plus the book's own table of
  contents, page list and metadata. KFX, Topaz and DRM files are refused with a message.
- `MupdfProvider` returns a `MupdfDocument`. A page index there is a screen (a PDF page
  can be split into tiles), and positions are `page:N;x0,y0,x1,y1`.

The reader calls `set_layout` with the user's settings, then `set_page_cache` so
pagination is cached per book and layout, then shows the saved position. Pagination
continues on a background thread; `page_count()` returns `complete: false` until it is
done, and the status bars show an estimate meanwhile. The plugins hear `on_book_open`
after the first page is on the panel.

## The `Document` trait

`ink_doc::Document` is how every screen and plugin sees a book. Rules that are easy to
get wrong:

- **Positions are strings everywhere they are stored** (sidecars, plugins, sync):
  crengine xpointers such as `/body/DocFragment[3]/body/p[2]/text().42` for
  reflowable books, `page:12;x0,y0,x1,y1` for fixed layout. Old `flow:` positions from
  the first prototype engine are still read. Pass positions back unchanged.
- `set_layout` keeps the reading position; read `current_page()` afterwards.
  `set_current_page(p)` moves the position only when `p` differs from the current page.
- `page_count()` may be incomplete while pagination runs; call `wait_paginated` where an
  exact total is needed (tests do).
- **A new trait method with a default body must also be delegated in
  `ink_formats::BookDocument`**, or reflowable books will silently use the default.
- Fixed-layout options (fit, crop, columns, contrast) are inherent methods of
  `MupdfDocument` (`set_options`), not part of the trait; `RINKY_FIXED` sets them for
  now.

## Device layer

`ink_device::Device` is a struct with public boxed fields (`screen`, `input`, `light`,
`battery`, `power`, `wifi`, `usb`, `clock`), so the app can borrow screen and input at
the same time. Every service method defaults to `DeviceError::Unsupported`, so a
backend implements what it has. There are three backends: `ereader::open` (Kindle and
Kobo, Linux only), `sim::open` (a minifb window, feature `sim`) and `headless::open`
(PNG frames, used by tests).

**Models.** The model table (`ereader/model.rs`) holds what differs between devices:
the e-ink controller interface (`Epdc`: MediaTek for the PW5 and newer, three i.MX
layouts for older Kindles, `KoboMxcfb` and `KoboMtk` for Kobos), warmth, page keys,
touch transforms, the upright framebuffer rotation and whether the driver's wait ioctl
can be trusted. A Kindle's model comes from the serial number (`/proc/usid`), a Kobo's
from the last digits of `/mnt/onboard/.kobo/version`; `RINKY_MODEL` overrides. An
unknown Kindle's controller is probed with a refresh of an unchanged corner. The UI is
laid out in 300 ppi pixels, so lower-density models are not supported; `rinky sim
--panel WxH` renders other sizes.

**Night mode and rotation.** Rotation is done in software (`copy_to_panel`); refresh
rectangles are in panel coordinates. Night mode depends on the controller
(`Epdc::night`): on MediaTek Kindles (PW5 and newer) the driver inverts in hardware
and uses night waveforms; i.MX Kobos set an inversion flag on each update; on i.MX
Kindles and MediaTek Kobos it is software inversion with the day waveforms. i.MX
Kindles have no page-turn sweep.

**The journal.** Every system setting rinky changes (lipc properties, framebuffer
flags such as night mode, rotation and bit depth) is written to `/tmp/rinky/restore`
before it changes. A clean exit undoes the changes itself; after a crash the launcher
and the watchdog replay the file (`rinky device restore`), so the device is not left
changed.

**Sleep.** With the stock UI stopped, nothing else puts the device to sleep. The
`SleepController` in `sleep.rs` owns the power key, the cover sensor and inactivity:
it draws the screensaver, turns the light off and suspends the kernel with an RTC
alarm (`ereader/system.rs`), and on wake puts the page back. The app receives
`PowerEvent::GoingToScreenSaver` and `OutOfScreenSaver`, and the plugin host runs
`on_suspend` and `on_resume` from them. The cover sensor also fires when the device
moves in a bag, so a cover open wakes only once it stayed open for 400 ms
(`RINKY_COVER_SETTLE_MS`), and a wake that no touch or key confirms sleeps again
after 3 minutes (`RINKY_COVER_UNUSED_S`). `RINKY_COVER=0` ignores the cover.

## Running on the Kindle

`scripts/rinky.sh` is the launcher, started from KUAL. It detaches (`setsid`), starts
`rinky-watchdog.sh`, stops the framework (`stop lab126_gui`, ignoring SIGTERM
meanwhile as KOReader does), waits until `KPPMainApp` and `pillowd` are gone, and runs
`rinky run`. On every exit path it restarts appmgrd and runs `start lab126_gui`.

The watchdog polls once a second. If the launcher disappears it stops rinky and starts
the framework; if rinky disappears and the launcher does not finish within 15 s, it
starts the framework itself.

A restart without the framework works through a file: `rinky-ctl.sh restart` (used
by `scripts/deploy.sh`) and the in-app update (`update::request_restart`) write
`/tmp/rinky/restart` and end rinky. The launcher sees the file, undoes the journal
and starts the installed binary again without bringing the framework back. A launcher
announces that it can do this with `/tmp/rinky/restart-capable`.

Runtime files are in `/tmp/rinky` (`lock/`, `launcher.pid`, `rinky.pid`, `done`,
`heartbeat`, `restore`, `quieted`, `restart-capable`). Logs are
`/mnt/us/rinky/rinky.log` and `watchdog.log`.

The PW12 on 5.19.5, for scale: two ARMv7 cores, 980 MB RAM (about 618 MB available
with the framework running; with it stopped about 410 MB free plus 300 MB cache),
`/dev/fb0` 1272 x 1696 at 8 bpp. The device's touch controller is `pt_mt`, the
emulator's `fts_ts`; the power key is `bd71828-pwrkey` on both.

### Sleep and the Kindle framework

Firmware 5.19.5 behaves as follows while rinky runs with `lab126_gui` stopped.

- **powerd ignores the power button.** On a press, powerd asks the Java framework
  (`com.lab126.kaf`) for `frameworkStarted`; with the framework gone the call fails and
  powerd logs `Splash screen is on.  Ignoring power button`. It sends no
  `goingToScreenSaver` or `readyToSuspend` and never suspends. So rinky reads
  `KEY_POWER` from the `bd71828-pwrkey` evdev node itself, draws its own screensaver and
  suspends the kernel directly (`crates/ink-device/src/sleep.rs`,
  `ereader/system.rs`).
- **powerd events.** With the framework running, powerd publishes the events KOReader
  handles (`goingToScreenSaver`, `outOfScreenSaver`, `exitingScreenSaver`,
  `readyToSuspend`, `wakeupFromSuspend`, `charging`, `notCharging`) plus
  `notReadyToSuspend` and `t1TimerReset`. `t1TimerReset` arrives on every touch, also
  with the framework stopped. The first argument of `goingToScreenSaver` and
  `outOfScreenSaver` is the source (1 = button wake, 2 = button).
- **Injected key presses.** powerd ignores key events injected into evdev;
  `lipc-set-prop -i com.lab126.powerd powerButton 1` is its own path for a press.
- **Suspend can be refused.** Writing `mem` to `/sys/power/state` fails at once with
  `EBUSY` while a wakeup source is active, for example the USB controller while a cable
  or USB networking is up. The emulator always has that source active, so real sleep and
  wake can only be tested on a device.
- **The cover arrives three ways.** As a kernel uevent `HALLSENSOR=` from `eink_hall`,
  as udev's `com.lab126.hal magSensorClosed/Opened`, and as powerd's
  `goingToScreenSaver 4` / `outOfScreenSaver 6`. `events::cover_from_published` and
  `events::classify` map them; `SleepController::cover_from` logs each with its source
  and drops powerd relays that contradict what the sensor last said. Reading
  `/sys/devices/platform/eink_hall/hall_test_trigger` injects a fake "cover opened"
  event: never read it.
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
  survive, reparented to init. On a PW12 an orphaned `fastmetrics` got stuck; powerd
  sends it a metric before it handles each cover or power event, and each send waited
  about 32 s for a timeout (`FM Metrics Emission:status :4` in `/var/log/messages`).
  So every powerd event came 32 s late, and every lipc read of a powerd property timed
  out. The launcher records the process tree of every running job before the
  framework stop and afterwards kills what a stopped job left behind (`fastmetrics`,
  `wifilocker`, `minerva_service` were seen).
- **Quieted services.** The launcher also stops the Amazon services rinky has no use
  for (`QUIET_JOBS` in `scripts/rinky.sh`: cloud sync, metrics, log uploaders, audio and
  TTS, Bluetooth, appmgrd), lists them in `/tmp/rinky/quieted`, and the launcher or the
  watchdog starts them again before the framework. Kept: powerd, lipcd, deviced, cmd,
  wifid, wifis, volumd, mtp, tmd, battstressd, X and the system jobs. `RINKY_QUIET=0`
  leaves them all running; `RINKY_QUIET_JOBS` replaces the list.

`rinky spike-suspend` is a small test screen for these paths: the first power press
draws a screensaver and tries to suspend, the next one wakes. Scenario:
`tests/e2e/arch-suspend-spike.sh`.

### Kindle system services

rinky's static musl binary cannot load the glibc `liblipc`, so `ink_device::lipc` runs
`lipc-get-prop`, `lipc-set-prop` and `lipc-wait-event` as child processes, each with a
timeout. On Linux every child gets `PR_SET_PDEATHSIG(SIGKILL)` and dies with the thread
that spawned it, so start watchers from long-lived threads.

After `stop lab126_gui`, only `KPPMainApp`, `pillowd` and the `kb` keyboard stop.
`powerd`, `wifid`, `cmd`, `volumd`, `blanket`, `deviced`, X and `awesome` keep running,
and the lipc services among them keep answering. The properties rinky uses:

| Service | Properties |
| --- | --- |
| `com.lab126.powerd` | `flIntensity` (0-24), `currentAmberLevel`, `screenSaverTimeout`, `powerButton`, `battLevel`, `isCharging`, `battStateInfo`, `deferSuspend`, `rtcWakeup`, `state` |
| `com.lab126.wifid` | `enable`, `scan`, `scanList`, `cmConnect`, `cmDisconnect`, `createProfile`, `profileData`, `cmState`, `signalStrength`, `currentEssid` |
| `com.lab126.cmd` | `wirelessEnable`, `activeInterface` |
| `com.lab126.volumd` | USB and MTP state |
| `com.lab126.blanket` | screensaver and USB modules |
| `com.lab126.deviced` | `enable_touch` |

## Running on the Kobo

The same binary runs on Kobos; `ink_platform::Platform::detect` chooses Kobo when
`/mnt/onboard/.kobo` exists (`RINKY_PLATFORM` overrides). `packaging/kobo/rinky.sh`,
started from NickelMenu, copies itself and the binary to tmpfs, stops Nickel and its
helpers, runs rinky, and on every exit path restores the framebuffer and starts Nickel
again, as KOReader's `koreader.sh` does.

There is no lipc on a Kobo. `ereader/kobo/` drives the frontlight and warmth through
sysfs, reads the battery from `power_supply`, and runs Wi-Fi with `wpa_supplicant` on
Nickel's own configuration, as KOReader's scripts do. Suspend writes
`state-extended` before `mem`, with Wi-Fi off first. The framebuffer boots at 16 or
32 bpp and is switched to 8 bpp gray while rinky runs. Touch panels of the "snow" kind
never send `TRACKING_ID -1`; `BTN_TOUCH 0` ends the contact. USB storage stays
Nickel's. Background and the models not yet supported: `docs/kobo-port.md`.

## Plugins

A plugin implements `ink_plugin::Plugin`. In `on_load` it registers what it adds:
reader menu items, selection actions, gestures, gesture-map actions, settings pages,
library actions, status-bar widgets, control-center tiles and library entries. The app
calls `on_action` when the user picks one. Rust plugins are crates in `plugins/<name>`
with one line in `crates/ink-app/src/plugins.rs`. Lua plugins are folders with
`plugin.toml` and `main.lua` in the install folder's `plugins/`, loaded by
`ink_plugin::lua::LuaHost` through the same registry and host services.

Plugins never block the reader: UI requests are queued and answered in `on_ui_result`,
network requests run in worker threads and report through `on_http`. A plugin that
fails five times, or runs out of time during a page turn, is disabled. Permissions in
the manifest (`document`, `ui`, `storage`, `network`) gate the host services.

The app loop does the plugin calls for every screen (event routing, overlay painting,
timers, suspend and resume). `docs/plugins.md` is the full API for plugin authors and
for screen authors.

## Extending rinky

**A screen.** Create `crates/ink-app/src/screens/<name>/mod.rs` with a type that
implements `router::AppScreen` (`name`, `handle`, `paint`, and optionally `on_enter`,
`on_exit`, `timeout`, `document`). Add a `register` function, a `Route` variant, and
the `pub mod` line and `register` call in `screens/mod.rs`. A screen draws into
`cx.canvas` and returns `Damage` (a rectangle and a refresh class), or calls
`cx.screen.update` itself for full-page content. Follow `docs/DESIGN.md` for widgets
and refresh classes.

**A Document method.** Add it to `ink_doc::Document` with a default body, implement it
in `HtmlDocument` or `MupdfDocument`, and delegate it in `ink_formats::BookDocument`.

**A public API change.** Several people work on different crates at once. Prefer an
additive change (a new method with a default, a new function) over changing or
removing a public item. When a change is unavoidable, update every caller in the same
commit.

**A dependency.** Add it to `[workspace.dependencies]` in the root `Cargo.toml` with an
exact version and `default-features = false` where the defaults pull in much, and use
`foo.workspace = true` in the crate. It must build for the ARM target with zig; try
`cargo zigbuild` before relying on it.

## Build and toolchain

- Rust 1.91 (`workspace.package.rust-version`). `.cargo/config.toml` sets
  `resolver.incompatible-rust-versions = "fallback"`, so new dependencies stay
  compatible with it.
- Device build: `cargo zigbuild --release --target armv7-unknown-linux-musleabihf`.
  zig is the C compiler and linker, so the C code (MuPDF, ring, vendored Lua, bundled
  SQLite) cross-compiles without a GNU toolchain. The result is one static hard-float
  musl binary for every supported device, built for the Cortex-A9 with NEON, the
  oldest CPU among them (`.cargo/config.toml` explains why not newer).
- ink-mupdf's `build.rs` builds MuPDF 1.27.2 from the pinned, checksummed tarball with
  `zig cc` for the device and the system compiler for the host. It needs `zig`,
  `curl` (first build only), `shasum`, a `tar` that reads lzip, and `make`.
  `INK_MUPDF_CACHE` sets the cache directory for the tarball and the built libraries;
  the other variables are listed at the top of `build.rs`.
- Profiles: `dev` keeps only line tables, `release` is fat LTO with one codegen unit
  and `panic = "unwind"` (a panicking pagination thread does not take the reader
  down), `deploy` and `emu` are release without fat LTO for quick installs.
- `scripts/package.sh` builds the Kindle and Kobo packages into `out/package` and
  refuses a tree with uncommitted changes unless `--allow-dirty`. `scripts/deploy.sh`
  installs the current tree on your own device; see `docs/TESTING.md`.
