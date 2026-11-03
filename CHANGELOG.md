# Changelog

All notable changes to rinky are listed here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rinky follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.3.0]: https://github.com/jyliuu/rinky/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/jyliuu/rinky/releases/tag/v0.2.0
[#1]: https://github.com/jyliuu/rinky/pull/1
[#2]: https://github.com/jyliuu/rinky/pull/2
[#3]: https://github.com/jyliuu/rinky/pull/3
[#4]: https://github.com/jyliuu/rinky/pull/4
[#5]: https://github.com/jyliuu/rinky/pull/5
