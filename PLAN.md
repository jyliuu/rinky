# rinky plan: full reader for the Kindle PW12

Status on 2026-10-24: a working prototype (EPUB text, justified layout, MTK refresh, tap page
turns, saved position) runs in the eink-emulator on firmware 5.19.5. This plan grows it into a
complete reading system that replaces the Kindle UI while it runs.

## Decisions (made 2026-10-24)

| Topic | Choice |
| --- | --- |
| Rendering | Hybrid. Own Rust HTML/CSS engine for EPUB, FB2, MOBI/AZW3 (DRM-free), TXT, HTML. MuPDF over C FFI for PDF and CBZ. |
| Plugins | First-party plugins are Rust crates behind one plugin API. User plugins are Lua 5.4 (embedded with `mlua`, vendored) using the same API. |
| Run mode | Replace the framework: `stop lab126_gui` on start, own sleep, light, Wi-Fi, USB and screensaver, `start lab126_gui` on exit or crash. |
| License | **Needs the user's OK.** Linking MuPDF (AGPL-3.0) forces AGPL-3.0-or-later for the whole app; Insightful (GPL-3.0) is compatible. `Cargo.toml` still says MIT and must change once agreed. Noto fonts stay OFL. |

## Facts measured on firmware 5.19.5 (emulator instance `pw12-5195-jb`)

- 2 ARMv7 cores, 980 MB RAM (618 MB available with the framework running), 3.8 GB free userstore.
- `/dev/fb0`: 1272×1696, 8 bpp, stride 1272, 2 buffers. MTK refresh ioctl `0x4060462e` works.
- Touch: the real device reports `pt_mt` (event1, read over SSH from the user's Kindle); the
  emulator reports `fts_ts`. Power key `bd71828-pwrkey` (event0) on both.
- `lipc` properties available:
  - `com.lab126.powerd`: `flIntensity` (0–24), `currentAmberLevel`, `screenSaverTimeout`,
    `powerButton`, `battLevel`, `isCharging`, `battStateInfo`, `deferSuspend`, `rtcWakeup`, `state`.
  - `com.lab126.wifid`: `enable`, `scan`, `scanList`, `cmConnect`, `cmDisconnect`, `createProfile`,
    `profileData`, `cmState`, `signalStrength`, `currentEssid`.
  - `com.lab126.cmd`: `wirelessEnable`, `activeInterface`. `com.lab126.volumd`: USB/MTP state.
  - `com.lab126.blanket`: screensaver/usb modules. `com.lab126.deviced`: `enable_touch`.
- Stock reader: `/app/bin/KPPMainAppV2` (47 MB native) plus a JavaScript bundle in
  `/app/KPPMainApp/js`, with `/app/kpp_app_cmds.json` listing debug commands.
- Framework stop, tested in the emulator: after `stop lab126_gui` only `KPPMainApp`, `pillowd`
  and the `kb` keyboard stop. `powerd`, `wifid`, `cmd`, `volumd`, `blanket`, `deviced`, X and
  `awesome` keep running. `lipc-get-prop` on powerd/wifid/cmd/volumd still answers, and
  `lipc-set-prop com.lab126.powerd flIntensity 10` changed the backlight (sysfs 1037). Free RAM
  rose to about 410 MB free + 300 MB cache. `start lab126_gui` brought the UI back.
- KOReader sync format: `kosync` sends `progress` = crengine xpointer for reflowable books
  (`ReaderRolling:getLastProgress` returns `self.xpointer`) and the document id = partial MD5
  (`util.partialMD5`: MD5 over 1024-byte samples at offsets `1024 << 2i`, i = -1..10).
- `liblipc` is a glibc shared library. Our static musl binary cannot load it, so phase 0 starts by
  running `lipc-get-prop` / `lipc-set-prop` / `lipc-wait-event` as child processes.

## Workspace layout (target)

```
rinky/
  crates/
    ink-device/   Device trait: screen, input, light, battery, power, wifi, usb, clock
                  impls: kindle_mtk (fb0 + evdev + lipc), sim (host window), headless (PNG)
    ink-ui/       widgets, layout boxes, e-ink refresh scheduler, gestures, keyboard, dialogs
    ink-text/     font loading, shaping (rustybuzz), glyph cache, hyphenation, line breaking
    ink-doc/      Document trait, positions, selection, search, TOC, links
    ink-html/     XHTML/HTML parse, CSS subset cascade, block/inline layout, pagination cache
    ink-formats/  epub, fb2, txt, html, mobi/azw3 → ink-html; cbz
    ink-mupdf/    MuPDF FFI for PDF and CBZ (zoom, crop, page text for search/selection)
    ink-store/    per-book sidecar (`book.sdr/rinky.<ext>.json`), library index, settings
    ink-net/      HTTPS (ureq + rustls), SSE streaming, OPDS, KOSync
    ink-plugin/   plugin API, event bus, action/menu/gesture registry, Lua host
    ink-app/      screens: Library, Reader, Settings, Search, TOC, Notes, Wi-Fi, Keyboard
  plugins/
    insightful/  dictionary/  stats/  vocab/  opds/  kosync/  calibre/  wikipedia/
  scripts/        emu.sh, device.sh, package.sh, golden-update.sh
  tests/golden/   reference PNG pages for layout regression tests
```

## Phases

Token figures are rough totals for the agent work, input and output combined. A daily-usable
reader exists after phase 5.

### Phase 0: Foundations (3–6M tokens)

- Split the prototype into the workspace above. Keep `render` and `run` working.
- C cross toolchain: install `zig`, build with `cargo zigbuild` for
  `armv7-unknown-linux-musleabihf`. Needed for MuPDF, rustls/ring and vendored Lua.
- Host simulator: a window (`minifb`) at 1272×1696, mouse = touch, keys = power and page keys.
  It applies a simple e-ink look (gray quantization, optional ghosting) so refresh bugs show.
- Test harness:
  - unit tests per crate;
  - golden-page tests: render fixed books to PNG and compare with `tests/golden/`;
  - emulator E2E: `scripts/emu.sh` runs scripted taps plus QMP screenshots and checks the log;
  - device smoke script over SSH for the real Kindle.
- `lipc` wrapper in `ink-device` (get, set, wait for events through child processes).
- Framework-stop launcher plus watchdog: `stop lab126_gui` → run rinky → `start lab126_gui`
  on any exit, including a crash or kill. All later phases are tested in this mode.
- Suspend/resume spike, with the framework stopped: power key → screensaver image → sleep →
  wake on power key → screen and touch still correct. Record which powerd events arrive
  (`lipc-wait-event -m com.lab126.powerd '*'`). This is the main risk, so it is proven first.

### Phase 1: UI toolkit and e-ink refresh (8–15M tokens)

- Retained widget tree: label, button, icon, list, menu, tabs, slider, toggle, progress bar,
  dialog, toast, scroll view, text input, popup, bottom sheet (the Kindle "Aa" panel style).
- Damage tracking. Each widget update yields a rectangle plus a refresh class (DU for taps,
  GL16 for UI, REAGL for pages, GC16 flash every N pages or on request).
- Gestures: tap, double tap, long press, swipe, two-finger pinch/spread, two-finger tap, corner
  holds. Handle the PW12 chip dropping very short taps: accept a touch-down and touch-up that
  arrive in the same `SYN` frame as a tap.
- Text: shaping with `rustybuzz`, kerning and ligatures, fallback fonts (CJK, symbols), bold
  and italic synthesis. Use system fonts in `/usr/java/lib/fonts` if present, plus user fonts in
  `/mnt/us/fonts`.
- On-screen keyboard (the stock `kb` process is gone once the framework stops): QWERTY plus
  symbols and accents, long-press alternates, a few layouts, key preview, repeat on hold.
- Night mode through the framebuffer grayscale flag plus MTK night waveforms (as in KOReader).
- Rotation: 4 orientations, input coordinates remapped.

### Phase 2: Reflowable engine v2 (15–30M tokens)

- HTML5 parser for broken markup (`html5ever`), keeping XHTML namespaces.
- CSS subset: selectors (type, class, id, descendant, child, attribute, `:first-child`),
  cascade and inheritance, `font-*`, `text-align`, `text-indent`, margins, padding, borders,
  `line-height`, `display` (block, inline, none, list-item, table basics), `page-break-*`,
  `white-space`, `vertical-align` sub/sup, `@font-face` embedded fonts. User overrides on top
  (font, size, spacing, margins, alignment, "publisher styles on/off").
- Images (`image` crate: JPEG, PNG, GIF, WebP, SVG via `resvg`), scaled to fit and dithered.
- Tables (simple grid), lists, block quotes, `<pre>`, footnote and endnote detection.
- Hyphenation (`hyphenation` crate, language from OPF/`lang`).
- Positions: a DOM path plus character offset, written in crengine xpointer syntax
  (`/body/DocFragment[N]/body/div/p[3]/text().42`) so it round-trips with KOReader's
  `progress` field and KOReader highlight positions. Stable across font changes; used for
  progress, bookmarks, highlights, sync and Insightful. Test the round trip against xpointers
  saved by KOReader for the same EPUB.
- Book identity: partial MD5 exactly as KOReader computes it, implemented once in `ink-store`.
- Incremental pagination per spine item with an on-disk page-map cache keyed by
  (book hash, layout settings). Open a book in under 1 s on the device and paginate the rest in
  a background thread.
- Kindle-style "location" numbers (128 bytes per location) and page labels from the EPUB page
  list when present.

### Phase 3: Reader experience (12–25M tokens)

Items from the stock reader and KOReader:

- Tap zones (configurable), swipes, side-tap modes, left-handed mode.
- Top and bottom bars: title, clock, battery, Wi-Fi; progress as page, location, percent,
  "time left in chapter/book" from measured reading speed.
- "Aa" panel: font, size, bold level, line spacing, margins, alignment, hyphenation, orientation,
  dark mode, refresh frequency, reading progress style. Save themes/presets.
- Go to: TOC (nested, current chapter marked), page, location, percent, beginning, cover.
- Page Flip / skim: thumbnail strip and scrubber that keeps the reading position.
- Search in book, with results list and jumping between hits.
- Selection by long press and drag handles, then actions: highlight (styles), note, copy, look
  up, search, translate, share to plugins (Insightful).
- Dictionary: StarDict (the installed NOAD conversion and the Oxford Advanced Learner's archive),
  with a popup and a full view.
- Footnote popups, internal links with a back button, external links shown but not opened.
- Bookmarks and a Notes list (filter, edit, delete, jump). Export to `My Clippings.txt` format,
  Markdown and JSON.
- Book end screen: "mark finished", rate, next book.

### Phase 4: Library and home (6–12M tokens)

- Scan `/mnt/us/Books` and `/mnt/us/documents` (configurable). Read metadata (OPF, FB2, MOBI
  EXTH, PDF info) and extract covers into a thumbnail cache.
- Home: continue reading, recent books, collections. Library: grid and list, sort (recent, title,
  author, date added), filter (unread, reading, finished, format), search.
- Collections (create, rename, add/remove books), book details page, delete with confirmation.
- File browser for other paths. Reading status and progress bars on covers.
- Migrate from KOReader: read `<book>.sdr/metadata.<ext>.lua` (position, highlights, notes,
  bookmarks, status, rating), `settings/statistics.sqlite3` and the vocabulary database. Import
  is read-only and never changes KOReader files.

### Phase 5: Device ownership (10–20M tokens)

- Start: `stop lab126_gui`, wait for the framework to finish, then take the screen and input.
  Exit: "Exit to Kindle" restores with `start lab126_gui`. A watchdog script restarts the
  framework if rinky crashes, so the device never stays unusable.
- Power key: sleep with a screensaver (book cover, a chosen image, or the current page),
  wake on the key or the hall sensor (cover). Use powerd suspend events; investigate first
  (`lipc-wait-event com.lab126.powerd`, REA on the stock app if needed).
- Auto-sleep after inactivity (setting), defer suspend while downloading.
- Frontlight brightness and warmth (`flIntensity`, `currentAmberLevel`), amber schedule, quick
  light panel by swiping down from the top.
- Wi-Fi: on/off, scan, join with password from the on-screen keyboard, saved profiles, status
  icon, airplane mode (`wifid`, `cmd`). Keep NTP time.
- Battery percent and charging state; low-battery warning.
- USB: on cable insert offer "USB drive mode" (unmount userstore via volumd, show a lock screen,
  remount and rescan the library afterwards) or "charge only".
- Settings screen covering all of the above, plus date/time, language, reset settings,
  screenshots (two-finger tap in corners), device info.
- Before work starts, list which daemons keep running after `stop lab126_gui` (powerd, wifid,
  cmd, volumd) and whether their `lipc` properties still respond.

### Phase 6: Other formats (10–20M tokens)

- MuPDF (C, built with zig) for PDF and CBZ: fit width/page, zoom and pan, margin crop,
  columns, page text for search and selection, highlights stored by page plus quads, contrast.
- FB2 and FB3 to the HTML engine. TXT with encoding detection and chapter guessing.
- MOBI/AZW3 (DRM-free only): PalmDB/MOBI/KF8 parser to HTML. KFX and DRM files are listed but
  refused with a message.
- Comics: CBZ page view, two-page spreads in landscape.

### Phase 7: Plugin system (6–12M tokens)

- One API for Rust and Lua plugins:
  - lifecycle: `on_load`, `on_book_open`, `on_page_change`, `on_selection`, `on_suspend`,
    `on_resume`, `on_close`;
  - registries: reader menu items, selection actions, gestures, settings pages, library actions,
    status-bar widgets;
  - document access: `toc()`, `search(query, limit)`, `read_around(position, chars)`,
    `links(position)`, `current_position()`, `selection()`;
  - UI: dialogs, menus, text viewer with Markdown, keyboard input, toasts, notifications;
  - storage: per-plugin settings, per-book data, files in `plugins/<name>/data`;
  - network: HTTPS requests with streaming, behind a per-plugin permission in its manifest.
- Lua host: `mlua` with vendored Lua 5.4, a sandbox (no `os.execute`, limited `io`), CPU/time
  limits per call, errors shown without crashing the reader.
- Plugin folder `/mnt/us/rinky/plugins/<name>/` with `plugin.toml` plus `main.lua`.
- Documentation and a sample Lua plugin (word count / reading timer).

### Phase 8: Insightful in Rust (8–15M tokens)

Port of `insightful.koplugin` v0.7.0 as a first-party plugin on the phase 7 API:

- Providers: OpenAI, DeepSeek, OpenRouter (Chat Completions), Anthropic (Messages), with
  streaming (SSE) and model lists. Read the existing `configuration.lua` provider table once
  and convert it to `insightful.toml`, so keys are not retyped.
- The 5 book tools with the same limits: `search_book`, `read_around`, `list_links`, `toc`,
  `current_position`. Shown in the chat as **AGENT ACTION** blocks.
- Selection actions: Explain, Give examples, Context/history, People/characters, Ask AI…
- Prompts loaded from the same Markdown files with the same `<tag>` substitution.
- Chats per book (several, list, delete), follow-up question links, retry, model label per
  answer, token and cost statistics.
- Chat view: Markdown renderer (headings, lists, quotes, code, tables, links) on ink-ui, a
  keyboard-friendly input bar, text arriving in batches.

### Phase 9: Network features (6–12M tokens)

- OPDS catalogs (browse, search, download to the library), Calibre wireless connection,
  KOSync progress sync (compatible with KOReader servers), Wikipedia and translation lookup
  from a selection, Wallabag (optional).

### Phase 10: Statistics, vocabulary, polish (6–12M tokens)

- Reading statistics: time per book and day, pages per hour, calendar view, streaks.
- Vocabulary builder from dictionary lookups, with flashcards.
- Performance: book open time, page turn time under 150 ms to start the refresh, memory cap,
  idle CPU near zero. Battery check over a reading session.
- Accessibility and i18n: UI strings in a catalog, at least English and Danish.

### Phase 11: Packaging (2–4M tokens)

- KUAL extension and a single install package. In-app update from a GitHub release (checksum
  verified). Uninstall leaves the Kindle unchanged. User documentation.

Total estimate: about 90–180M tokens across all phases.

## Coverage of stock Kindle features

Built from the stock reader's English catalog (`/app/KPPMainApp/locale/en/LC_MESSAGES/mainapp.mo`,
5906 strings; groups `settingsapp.*`, `aamenu.*`, `library.*`, ...).

| Stock feature | rinky |
| --- | --- |
| Aa menu: font, size, bold, line/word/character/paragraph spacing, margins, alignment, orientation, columns, page color, contrast, crop margins, themes, reading progress, clock, refresh every page, page turn animation | Phase 3 (crop margins and columns also phase 6 for PDF) |
| Screen and brightness, light/dark mode, warmth schedule, display & text size | Phases 1, 5 |
| Sleep timer, power saver, screenshots | Phase 5 |
| Wi-Fi networks, airplane mode, date and time, language, device name, device info, storage management | Phase 5 |
| Restart device, reset settings | Phase 5 (rinky settings; factory reset stays in the Kindle UI) |
| Device PIN / lock screen | Phase 5 (own PIN screen on wake) |
| Keyboards and keyboard languages | Phase 1 |
| Dictionary languages, vocabulary builder, translation | Phases 3, 9, 10 |
| Library: collections, sort, filter, search, downloaded items | Phase 4 |
| Annotations, export notes | Phase 3 |
| Bluetooth, VoiceView, audio, TTS reader | Not planned (needs Amazon's audio stack); "Exit to Kindle" for these |
| Amazon account, store, Whispersync, Send to Kindle, Goodreads, household, profiles, parental controls, Kindle Unlimited, Alexa+ | Not possible without Amazon's framework |
| KFX / DRM books (the Amazon library) | Not readable; listed and refused with a message |
| X-Ray, Word Wise, Recaps, popular highlights, book mentions | Not possible (Amazon data); Insightful covers some of the same need |
| Notebooks, handwriting | Not applicable (no stylus on PW12) |

## Use of REA on the stock Kindle app

REA is used only where `lipc-probe` and the KOReader source do not answer the question:

1. `/app/KPPMainApp/js/KPPMainApp.js.hbc` (Hermes bytecode; `analyze_javascript_application`): list the stock reader's
   reading settings, progress modes, X-Ray/Recaps/export behavior and Page Flip, to check the
   phase 3 feature list against it.
2. How the framework handles suspend, screensaver and the hall sensor: which `lipc` events
   powerd sends and which calls the framework makes in response (phase 5).
3. Wi-Fi profile creation and password handling through `wifid` (phase 5).

Firmware files come from the emulator's extracted 5.19.5 images, not from the user's device.

## Testing rules for every phase

- Host: `cargo test --workspace`, golden pages unchanged (or updated on purpose with a diff shown).
- Emulator: deploy with `scripts/emu.sh deploy`, run the phase's scripted E2E taps, check
  screenshots and the log. Framework-stop mode is tested in a `-snapshot` run so the instance
  stays clean.
- Real device: a short smoke test at the end of each phase, only after an emulator pass. The
  user starts and stops the reader on the device; the agent never overwrites KOReader files or
  the device's Insightful `configuration.lua`.

## Main risks

| Risk | Plan |
| --- | --- |
| Suspend/resume without the framework misbehaves (no wake, drained battery) | Prototype it first in phase 5; keep the watchdog that restarts the framework |
| Emulator touch (`fts_ts`) differs from device (`pt_mt`) | Device smoke test each phase; mapping options in `RINKY_TOUCH` |
| HTML/CSS engine scope grows without end | Fixed CSS subset; golden tests on a set of 20 real EPUBs; anything else falls back to defaults |
| Large books paginate slowly on 2 ARM cores | Incremental pagination plus page-map cache; measure in phase 2 |
| `liblipc` cannot be linked into a static musl binary | Child processes first; later a small glibc helper built with zig if latency matters |
| MuPDF build with zig fails | Fall back to a prebuilt MuPDF from KOReader's build system for armhf |
