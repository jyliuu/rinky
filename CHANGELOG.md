# Changelog

All notable changes to rinky are listed here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rinky follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.1] - 2026-11-04

### Fixed
- The Kindle no longer freezes or falls back to sleep after waking. Stopping the Kindle
  UI left some Amazon programs running unmanaged; one of them (fastmetrics) got stuck,
  and the Kindle's power service then took about 32 s to handle each cover or power
  button event. rinky's launcher now ends such leftovers. ([#12])
- A cover that moves in a bag no longer wakes the Kindle. The cover must stay open for
  0.4 s before the Kindle wakes, a wake from full sleep with the cover still closed
  goes back to sleep, and a cover wake that nobody touches sleeps again after 3
  minutes. ([#12])

### Changed
- While rinky runs, the Amazon background services it does not use (cloud sync, usage
  reporting, log uploads, audio, text-to-speech, Bluetooth) are stopped, and they start
  again when you leave rinky. Power management, Wi-Fi, USB and battery protection keep
  running. `RINKY_QUIET=0` in `rinky.env` leaves them all running. ([#12])

## [0.4.0] - 2026-11-03

### Added
- Insightful works like insightful.koplugin. Each book has a current chat, and
  highlighted actions (Explain, Give examples, Context / history, People / characters,
  Ask AI…) continue it, so related questions stay together. "New chat for highlighted
  actions" in the reader menu's Insightful page starts a new chat each time instead
  (hold it to make it the default for other books), and a checkbox in the actions
  ("Start a new chat" / "Start in existing chat") changes it for one highlight.
- Insightful's reader-menu page: Continue current chat, Chats, Start new chat, the
  setting above, Provider, Statistics (General, Current book, All books) and Gesture
  shortcuts. Gestures can show the current chat, show the chat list or start a chat.
- A Stop button while Insightful writes an answer.
- Plugin viewers: a long press on a link reports `hold:LINK`, and a sheet row whose
  link starts with `stay:` leaves the sheet open.

### Changed
- The Insightful chat no longer scrolls along while an answer streams in. A question
  you send moves to the top and its answer fills in below it. Page turns move a whole
  screen, never part of one, so you keep your place.
- Closing the Insightful chat no longer cancels the answer: it is written and saved
  to its chat, and a notice says when it is ready.
- Ask AI… opens the chat with the keyboard closed; tap "Message Insightful…" to type.
  The selected passage goes with your message.
- Insightful's chat list starts with "Start new chat" and marks the current chat; hold
  a chat to delete it.
- Dragging a selection to the bottom (or top) edge of the page now moves half a page
  at a time instead of a whole page, so the text you are selecting into appears in the
  middle of the screen with the text before it still in view. When the selection card
  closes, the reader shows the page the selection ended on.

### Fixed
- Retry is offered only when sending the question again can work (not after a missing
  or rejected API key).
- Going to a percentage right after opening a book landed far too early while the book
  was still being paginated. ([#9])
- Tables in Insightful answers (and other plugin text) are laid out in columns that
  wrap on the Kindle's narrow screen, including tables written without outer pipes.
  Plugin Markdown is now parsed by a CommonMark parser (pulldown-cmark). ([#10])
- CI is green: tests no longer depend on the runner's speed, thread count or float
  rounding. ([#9])

## [0.3.0] - 2026-11-03

### Added
- Frontlight gestures in the reader, as in KOReader: slide up or down the left edge
  to change brightness and the right edge to change warmth. Settings > Gestures >
  "Edge slides (light)" sets each edge to Off, Brightness or Warmth. ([#1])
- `light_toggle` and `warmth_toggle` actions: the first tap turns the light (or
  warmth) off, the next restores the previous level. ([#1])
- A bottom-center tap zone that takes any gesture action. ([#1])

### Changed
- Tapping the bottom-left corner now toggles the light and the bottom-right corner
  toggles warmth, instead of turning the page. Set a corner to "Nothing" to turn
  pages there again. ([#1])
- Titles and headings in the UI use the regular face, so they read light. The
  embedded Noto Sans Bold is removed, which makes the binary smaller. ([#5])

### Fixed
- The library's Unread filter now lists books that were opened but are still at
  0 %, matching the Unread badge on their covers. ([#2])
- USB networking no longer fails with "USB port busy": the bind is retried while
  MTP lets go of the port, a legacy `g_ether` module holding the port is unloaded,
  and errors name whatever holds the port. ([#4])
- `deploy.sh` builds through `package.sh`, so deploys pass its build-path check. ([#4])
- The log names the built-in UI font Noto Sans, not Noto Serif. ([#5])
- Outdated insightful plugin tests. ([#3])
- The live preview in the Aa sheet is drawn with the text weight (Normal, Darker,
  Darkest), so it darkens along with the page.

### Internal
- CI runs clippy and the test suite on every push to main and every pull request.
  The workspace is clippy-clean, and tests that relied on sleeps or shared temp
  directories are now deterministic.

## [0.2.0] - 2026-11-03

First public release.

[0.4.1]: https://github.com/jyliuu/rinky/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/jyliuu/rinky/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jyliuu/rinky/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jyliuu/rinky/releases/tag/v0.2.0
[#1]: https://github.com/jyliuu/rinky/pull/1
[#2]: https://github.com/jyliuu/rinky/pull/2
[#3]: https://github.com/jyliuu/rinky/pull/3
[#4]: https://github.com/jyliuu/rinky/pull/4
[#5]: https://github.com/jyliuu/rinky/pull/5
[#9]: https://github.com/jyliuu/rinky/pull/9
[#10]: https://github.com/jyliuu/rinky/pull/10
[#12]: https://github.com/jyliuu/rinky/pull/12
