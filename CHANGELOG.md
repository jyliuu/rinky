# Changelog

All notable changes to rinky are listed here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and rinky follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Annotations: readings that a book marks up as ruby (furigana in Japanese books,
  readings in Chinese textbooks) are drawn in small type centred above their
  characters (or below them, where the book asks for that) instead of after them, and
  are left out of search, selected text and look-ups.
  The characters keep an even spacing: a reading wider than its character reaches
  over the characters beside it where their readings leave room, readings that still
  do not fit are set a little smaller (down to 70 %, the same size for the whole
  paragraph), and only after that do the characters move apart, all by the same
  amount. Lines with readings get the room they need.
- *Annotations* in Aa › Layout shows or hides those annotations for the open book,
  and in Settings › Default layout for the books you open from then on. It also
  shows or hides annotations that plugins add above or below the text: compiled-in
  plugins can now annotate the text of reflowable books (several at once). Their
  annotations are worked out in the background while you read, never hold up
  opening a book or turning a page, and appear on a page as soon as they are ready.
- A long press can look a word up with a plugin made for the book's language while
  the dictionary still takes every other word: the first plugin that takes the word
  gets it.

### Fixed
- Chinese, Japanese and Korean text in menus, cards, messages and title bars shows
  in the Kindle's own sans fonts instead of as empty boxes.

## [0.7.0] - 2026-11-12

### Added
- Translate: select a word or passage and tap *Translate* to see it translated by
  Google Translate (with Wi-Fi), with a word's meanings and other translations in the
  full view. The languages to translate from and into are under Settings › Plugins ›
  Translate.

## [0.6.1] - 2026-11-09

### Changed
- A footnote opens in a panel across the bottom of the screen, as in KOReader, as
  high as the note (at most half the screen; a longer note turns a page at a time
  with a tap on its text), with *Go to note* at its right; a tap above the panel
  closes it. The panel shows the note as the book prints it, its number included,
  with its italics and bold, and no "Footnote 57" heading. A number and the period
  after it stay on the note's first line instead of a line of their own, a note of
  several paragraphs keeps its paragraphs, and a note that runs on over paragraphs
  without a container of its own is shown whole.

### Fixed
- A footnote in an endnotes list (`<li>` items, as Pandoc and many publishers write
  them) shows that one note, not the whole list.
- The hide-keyboard key puts the keyboard away in a plugin's chat (Insightful) and
  other plugin text boxes. It used to open the keyboard again at once, so the screen
  only flickered.
- In the library, the hide-keyboard and Enter keys of the search and collection name
  keyboards no longer also tap what lies under them once the keyboard is gone (the
  hide key opened Settings from the navigation bar).
- Bold and italics now show in every plugin view (Insightful answers, Wikipedia,
  the dictionary, help pages), whatever the interface font: before, they looked like
  the rest of the text in Insightful and most other views. An interface font without
  bold or italic files gets slanted and heavier letters made from its regular ones.
  Labels, titles and list rows keep their light look.
- Plugin views show more of what Insightful's answers and other Markdown contain:
  bold italic text, italics in headings, a gray background behind `code`, struck
  text, line breaks within a paragraph, later paragraphs of a list item indented
  with it, and lists inside quotes.

## [0.6.0] - 2026-11-08

### Added
- The end of a book: turning past the last page shows a card with the book's title.
  It marks the book as finished, takes a rating of one to five stars (a rating
  imported from KOReader shows there too), suggests what to read next (the next book of the series,
  or else the unfinished book opened last) and goes back to the library. Back, or
  *Back to book*, returns to the last page. The card appears with one flash; taps on
  it do not flash.
- PDF, comic (CBZ) and XPS books have a **View** tab in *Reading settings* (Aa), in
  place of the font and margin tabs that did nothing to them: fit the whole page or
  the page width, read two-column papers one column at a time, darker contrast for
  faint scans, two pages side by side, crop the white margins, and right to left for
  manga, where a tap on the left or a swipe to the right turns to the next page. The
  choices are kept for each book.
- Highlights can be exported as Kindle clippings or JSON as well as Markdown. Choose
  under Settings › KOReader import › Export format; the reader's and the library's
  *Export highlights* use the same choice. Kindle clippings use the layout of a
  Kindle's `My Clippings.txt`, so Readwise and similar tools read them; *Export all
  highlights* then puts every book in one `exports/My Clippings.txt`. Over SSH,
  `rinky export-highlights --format clippings|json` does the same.

### Changed
- Books finish counting their pages sooner after opening and after a font-size change,
  and rinky uses about 35 MB less memory while reading: each font is now read once and
  shared, where the page counter read it again for itself.
- Counting a book's pages (after opening it for the first time or changing the font
  size) takes about half as long as before, and needs about half the memory while a
  book with long chapters is counted.
- AZW3 books whose cover page is an SVG around one picture (Calibre's usual cover)
  open several times faster: the picture is decoded straight to its size on screen.
- The library takes in new books about twice as fast: the cover thumbnail of each book
  is made with much less work.
- A book opened for the first time shows its first page without waiting for the
  pages after it to be counted.
- The library shows its books at once when rinky starts and reads the book folders
  in the background; books added or removed meanwhile show up a moment later.
- Reading settings (Aa): a tap on a control no longer redraws the whole sheet when the
  finger goes down and again when it lifts, so the sheet answers taps sooner. The
  sheet also opens faster.
- PDF and comic pages draw about 10 % faster: rinky no longer runs colour management
  for a gray screen. Gray pages look exactly as before; in colour pages, saturated
  colours now come out darker (pure red is 77 of 255 instead of 129).
- PDF and comic page turns are near instant when you rest on a page for a moment:
  rinky draws the next page (or the previous one, after turning back) while you read.
- rinky starts faster: the first screen no longer waits for the interface font to be
  fully loaded.
- Long EPUB books open sooner, above all when opened again: rinky unpacks the chapter
  it shows first and the others while you read, not every chapter before the first page.
- On Kindles with 512 MB of memory, scanned PDFs and comics take about 20 MB less
  memory while reading, with no slower page turns.
- Zooming a PDF or comic page with a pinch shows the zoomed page at once, a little
  coarse, and sharpens it a moment after your fingers lift.
- Wi-Fi connects by itself more reliably and says what it is doing. rinky remembers
  that Wi-Fi was on and turns it on again when it starts and after the e-reader wakes
  up (on a Kobo, which turns Wi-Fi off to sleep, it stayed off until you turned it on
  again). It looks for a saved network at once after a wake or turning Wi-Fi on,
  then less often while none is found.
- A short message tells you when Wi-Fi connects ("Connected to Home"), when joining a
  network fails ("Couldn't connect to Home", once an hour for the same network) and
  when no saved network is in range after a wake or start ("No saved network found",
  once until Wi-Fi connects or is turned off).
- The Wi-Fi icon in the status bars changes as soon as the connection does, and has
  three looks: connected, connecting, and off or not connected (before, it showed
  nothing unless connected). Settings › Wi-Fi and the control center's Wi-Fi button
  follow the connection too ("Connecting to Home…", "Connected to Home"), and joining
  a network or scanning no longer freezes the screen.

### Fixed
- Reading settings (Aa): a preset or a font size step now also refreshes the preview
  and the other controls it changes (the size, the preset chips, Justify), which
  could stay showing the old values on the screen.
- rinky said it was connected to a network it was only trying to join. It now counts
  Wi-Fi as connected only once the e-reader has an address on the network.
- Kobo: a network joined later (after a wake, or on coming into range) got no
  address, and a connection Nickel left half set up when rinky started stayed
  unusable. rinky now gets the address whenever Wi-Fi is joined without one.
- Kindle: with Wi-Fi on but no network joined, rinky did not look for saved networks
  when wifid reported "pending", which it does in that state.

## [0.5.3] - 2026-11-07

### Changed
- Settings › Battery shows estimates much sooner. It counts the time up to now (not
  only up to the last note), gives a rate once a point of charge is used in a quarter
  of an hour, and, before any point is used, an upper bound after an hour ("under
  0.2 % per hour", "over 16 days"). rinky also notes the charge each time it drops,
  which times the rate better than the 10-minute notes alone.
- Dictionary full view: entries in markup rinky has no parser of its own for keep the
  dictionary's formatting (numbered and nested senses, bold and italics, paragraphs,
  quotes). The definition is read by the book engine's HTML parser and styled with
  the dictionary's style sheet (`<name>.css` next to its `.ifo`, as KOReader reads
  it), where before it was flattened to plain lines. Bold and italics show in
  the interface font's own bold and italic faces (other UI text keeps its light
  titles).
- Settings › Fonts › Interface font lists only families with bold and italic faces
  (a family chosen before stays listed). The built-in Noto Sans now has its bold and
  italic faces (+0.7 MB).

### Fixed
- A long press on a word hyphenated at the end of a line selected only the part on
  the line pressed ("Char" of "Char-acteristically"). It now selects the whole word,
  pressed on either part, so the dictionary and plugins get all of it.

## [0.5.2] - 2026-11-07

### Added
- Settings › Battery: the use per hour awake and asleep, the use since the last
  charge, and how long the charge left may last. rinky notes the charge every 10
  minutes while awake and at each sleep and wake. The page opens from the battery
  card in Device info and from a new Battery button for the control center.

### Changed
- With Wi-Fi on but not connected, rinky looks for saved networks by itself and joins
  the strongest one in range. It scans every 15 s at first, then less often, up to
  every 5 minutes; waking up or turning Wi-Fi on starts over. A network that fails to
  join is skipped for 10 minutes. This replaces the single scan after a wake from 0.5.1.
- Keyboard: a pressed key gets a thick border instead of turning black, which shows
  sooner on e-ink.

### Fixed
- After a font size change (a pinch), the status bars kept an estimated page total
  until the next page turn. They now show the counted total as soon as pagination
  finishes. The estimate was written "~340", which read as a minus sign at
  status-bar size; it is now "about 340".

## [0.5.1] - 2026-11-07

### Fixed
- After waking from sleep with Wi-Fi on but not connected (for example after the
  e-reader was carried away from its network), rinky now scans for networks, so
  Settings › Wi-Fi lists the networks around and a saved one in range can be joined.
- Insightful: closing a chat started with Explain (or another highlight action) took
  two taps. The chat opened twice, one copy under the other; it now opens once.

## [0.5.0] - 2026-11-06

### Added
- rinky runs on more Kindles: the Paperwhite 3, 4 and 5, Voyage, Oasis 1 to 3, Kindle
  11th and 12th gen, Scribe and Colorsoft, besides the Paperwhite 12th gen. The
  Paperwhite 3 and 4, Oasis 3 and Kindle 12th gen were tried in an emulator with
  Amazon's firmware; the others are untested. The user guide lists them. rinky reads
  the model from the serial number and uses that model's display interface, page keys
  (Voyage, Oasis) and lights. On a Kindle it does not know, it tries each display
  interface in turn.
- Settings › Device shows the Kindle model, with the firmware under it.
- The user guide explains how to get each Kindle ready (jailbreak, KUAL, USB drive or
  MTP) and what differs between models.
- `rinky sim --panel WxH` renders the simulator at another screen size.
- rinky runs on Kobo e-readers with a 300 ppi screen: Libra 2, Clara HD, Clara 2E,
  Forma, Libra H2O, Clara BW and Colour, Libra Colour, and the Tolinos made by Kobo.
  It is the same binary; a Kobo package (`rinky-<version>-kobo.zip`, `.tar.gz`)
  installs it in `.adds/rinky` with a NickelMenu entry. The launcher stops Kobo's own
  software while rinky runs and starts it again afterwards. Light, warmth, battery,
  sleep and Wi-Fi (with the Kobo's saved networks) work without it; the in-app update
  fetches the Kobo package. Tried in an emulator on the Libra 2 (Kobo firmware 4.38)
  and the Tolino Shine 5 (the Clara BW inside); untested on real Kobos. See
  `docs/kobo.md`.

### Changed
- The "Page turn wave" switch is shown only on Kindles whose screen can sweep a page
  in (Paperwhite 5 and 12th gen).
- rinky and its SSH server are built for the Cortex-A9, the oldest processor of the
  supported Kindles. The build for the Paperwhite 12th gen's Cortex-A53 stopped with
  "Illegal instruction" on older models.

### Fixed
- Restarting rinky (after an in-app update, or `rinky-ctl.sh restart`) went back to the
  Kindle's Home screen on Kindles with a small temporary folder (32 MB on the
  Paperwhite 3 and 4 and the Oasis): there was no room for the new copy of rinky next
  to the old one. The launcher now removes the old copy first, or runs rinky from its
  install folder.

## [0.4.3] - 2026-11-04

### Added
- Tap a highlight to open its selection card again, with **Remove highlight** in place
  of **Highlight**. Note, Insightful, Search and the other actions work on the
  highlighted text. ([#16])

### Fixed
- When the Kindle went to sleep by itself (after the inactivity time) or by closing
  the cover, the Kindle's own screensaver could replace rinky's book cover. Its
  screensaver module kept running under rinky and reacted to the Kindle's own timer and
  the cover; the power button was not affected. rinky now unloads that module while it
  runs (as KOReader does) and tells the Kindle's power service not to start its
  screensaver; both come back when you leave rinky. ([#16])
- The brightness and warmth edge slides work only on the reading page. With the
  Insightful chat, a plugin dialog or another plugin window open, a slide along the
  screen edge no longer changes the light underneath. ([#16])

## [0.4.2] - 2026-11-04

### Added
- The reader menu has a "Go back to p. N" button after any jump: a search result,
  Contents, Go to, the page slider, a bookmark or highlight, a link or a footnote. It
  returns to where you were, one jump at a time. The swipe in from the left edge still
  goes back only after a link or footnote, so after a search it turns the page as
  before. ([#14])

### Changed
- Updating from *Settings > Update* installs and restarts rinky into the new version
  by itself, without a confirmation and without bringing up the Kindle's own screens.
  The button says so: *Install X and restart*. ([#15])
- Insightful shows exactly what was sent: a highlighted action's message shows
  "Selected passage — LOCATION", the passage, and the whole prompt, as
  insightful.koplugin does, instead of the action's name and "Preset prompt · tap to see
  what was sent". The prompts are the koplugin's v0.7.0 prompts, unchanged. ([#14])

### Fixed
- The reader no longer freezes after repeated taps on an Insightful message. Each tap
  opened another copy of a dialog; the copies kept appearing long after the chat closed,
  and while one was open every tap and swipe went to it. A plugin's repeated identical
  dialog now opens once. ([#14])
- Selecting text across a word the layout hyphenated at a line end ("con-" | "sist")
  gives the whole word ("consist"), not "con sist". A hyphen of the book at a line end
  stays ("well-known"). ([#14])

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

[0.7.0]: https://github.com/jyliuu/rinky/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/jyliuu/rinky/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/jyliuu/rinky/compare/v0.5.3...v0.6.0
[0.5.3]: https://github.com/jyliuu/rinky/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/jyliuu/rinky/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/jyliuu/rinky/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/jyliuu/rinky/compare/v0.4.3...v0.5.0
[0.4.3]: https://github.com/jyliuu/rinky/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/jyliuu/rinky/compare/v0.4.1...v0.4.2
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
[#14]: https://github.com/jyliuu/rinky/pull/14
[#15]: https://github.com/jyliuu/rinky/pull/15
[#16]: https://github.com/jyliuu/rinky/pull/16
