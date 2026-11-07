# rinky roadmap

What is still to do. Shipped work is in `CHANGELOG.md`; how the code is organised is
in `docs/ARCHITECTURE.md`. Each item names where the work would go and what exists
today, so it can be picked up without digging.

## Reader

- **Fixed-layout view options in the reader.** `MupdfDocument::set_options` already
  takes fit, zoom, crop, columns, contrast, gamma, rotation, spreads and right-to-left
  (`ink_mupdf::FixedOptions`), but the reader offers only pinch zoom
  (`Document::zoom_step`). Today the other options come from the `RINKY_FIXED`
  environment variable. They need a place in the Aa panel and a per-book entry in the
  sidecar.
- **Right-to-left books.** Page-turn direction, tap zones and the page-turn wave
  assume left-to-right (`ReaderScreen::turn` always passes `false` to
  `Wave::for_turn`).
- **Skim view.** A thumbnail strip with a scrubber that keeps the reading position
  (the stock reader's Page Flip). The reader has a page slider in the menu, nothing
  more.
- **More export formats.** Highlights, notes and bookmarks export as Markdown
  (`ink_store::export`, `rinky export-highlights`). `My Clippings.txt` and JSON are
  not written yet.
- **Book end screen.** Mark the book finished, rate it, open the next one. The status
  exists in the sidecar (`ink_store::sidecar::ReadingStatus`) and can be set from the
  library's book details.

## Device

- **Kobo models not yet driven.** The sunxi models (Elipsa, Sage) need their own
  display backend (ION buffer, display layers, G2D rotation). The i.MX6SL models
  (Glo HD, Aura ONE) need the `mxcfb` v1 refresh struct and touch protocol A. Notes
  on both are in `docs/kobo-port.md`.
- **Colour on Kaleido panels.** The Clara Colour, Libra Colour and Tolino colour
  models show grayscale because the framebuffer runs at 8 bpp. Colour needs 32 bpp
  and a second pixel path in `copy_to_panel`.
- **Kobo warmth schedule and USB storage.** The Kindle has both; on a Kobo there is
  no warmth schedule yet, and USB storage means exiting to Nickel.
- **Real hardware.** Kobo support and several Kindle models have only run in the
  emulator. The user guide lists which ones.
- **Keep the device awake during downloads.** `ink_net::connectivity::{KeepAwake,
  NetActivity}` exist but the app's sleep path does not consult them yet; a sleep
  cancels running plugin requests instead (`NetWorker::interrupt_all`).
- **Device PIN.** An optional PIN screen on wake.

## Plugins and network

- **Translation and Wallabag plugins.** Wikipedia lookup exists (`plugins/wikipedia`);
  a translation lookup from a selection and a Wallabag client do not.
- **Lua time limit inside C calls.** One long C library call (a pathological
  `string.find` on a huge string) is not interrupted by the time limit
  (`docs/plugins.md`, "Limits, sandbox and errors").

## Interface

- **Translations of the UI.** UI strings are English literals in the code. A catalog
  and at least a Danish translation are still to come. The keyboard already has
  Danish, German, French, Spanish, Swedish and Norwegian layouts.

## Out of scope

These need Amazon's or Kobo's own software and stay with it. "Exit to Kindle" (or
Kobo) is the way to reach them.

| Feature | Why |
| --- | --- |
| Bluetooth, VoiceView, audio, text to speech | needs Amazon's audio stack |
| Store, account, Whispersync, Send to Kindle, Goodreads, profiles, parental controls | needs Amazon's framework |
| KFX and DRM books | listed in the library and refused with a message |
| X-Ray, Word Wise, Recaps, popular highlights | Amazon's data; the Insightful plugin covers some of the same need |
| Notebooks, handwriting | no stylus support |
| Kindles below 300 ppi (212 and 167 ppi models) | the UI is laid out in 300 ppi pixels |
