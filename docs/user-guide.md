# rinky user guide

rinky is an e-book reader for jailbroken Kindles and for Kobos. While it runs it takes
over the screen from the Kindle's own software; when you exit, the Kindle's Home screen
comes back. It was developed on a Kindle Paperwhite 12th generation (2024) with
firmware 5.19.

This guide is written for the Kindle, with Kindle paths such as `/mnt/us/rinky`. On a
Kobo rinky works the same once it runs; its folder is `.adds/rinky` on the Kobo drive,
and [rinky on Kobo](kobo.md) lists what else differs.

1. [Install and start](#1-install-and-start)
2. [Add books](#2-add-books)
3. [The library](#3-the-library)
4. [Reading](#4-reading)
5. [Light, sleep and settings](#5-light-sleep-and-settings)
6. [Built-in plugins](#6-built-in-plugins)
7. [Export your highlights](#7-export-your-highlights)
8. [Exit to Kindle](#8-exit-to-kindle)
9. [Update](#9-update)
10. [Uninstall](#10-uninstall)
11. [Troubleshooting](#11-troubleshooting)
12. [Files rinky writes](#12-files-rinky-writes)
13. [License](#13-license)

## 1. Install and start

The [install guide](install.md) lists the supported models and walks through the
install: a jailbreak and KUAL on a Kindle, NickelMenu on a Kobo.

To start rinky:

- **Kindle:** open KUAL and choose *rinky > Start rinky*. If you installed over SSH
  there is also a **rinky** item in the Kindle library that does the same.
- **Kobo:** open NickelMenu and choose *rinky*.
- **Over SSH:** `sh /mnt/us/rinky/rinky-ctl.sh start` opens the library;
  `sh /mnt/us/rinky/rinky-ctl.sh start /mnt/us/Books/book.epub` opens that book.

The Kindle's own screens stop, which takes a few seconds and may flash the screen, and
rinky opens on its Home screen. A small background program, the *watchdog*, brings the
Kindle's screens back if rinky crashes.

rinky changes little on your e-reader. It keeps its own folder (`rinky`, or
`.adds/rinky` on a Kobo) and a menu entry for KUAL or NickelMenu. Next to each book
you open it writes a small file `<book>.sdr/rinky.<ext>.json` (for example
`rinky.epub.json`) with your position, highlights and notes. The `.sdr` folder is the
one KOReader uses; rinky never changes KOReader's files in it, nor the e-reader's own
library. [Uninstall](#10-uninstall) removes everything rinky added.

## 2. Add books

On a Kindle rinky lists the books in the `Books` and `documents` folders; on a Kobo,
books anywhere on the drive except hidden folders. *Settings > Library folders*
chooses other folders.

rinky does not show up as a USB drive while it runs. To copy books over USB, exit to
the Kindle (*Settings > Exit to Kindle*), connect the cable, copy the books, eject,
and start rinky again. The library picks up new files each time rinky starts; *Settings >
KOReader import > Rescan the library* reads the folders again.

Without a cable you can send books from calibre ([Calibre wireless](#calibre-wireless)),
download them from an online catalog ([OPDS catalogs](#opds-catalogs)), or copy them
over [SSH](#usb-network-and-ssh).

| Format | |
| --- | --- |
| EPUB, MOBI and AZW3 without DRM, FB2, FB3, TXT, HTML | reflowable: you choose font, size and margins |
| PDF, CBZ | fixed pages; pinch to zoom |
| KFX and books with DRM (the Amazon store) | listed, but rinky cannot open them. Read them in the Kindle's own reader after [Exit to Kindle](#8-exit-to-kindle). |

## 3. The library

The bar at the bottom switches between Home, Library, Collections and Settings. You
can choose two to five items for it, in any order, under *Settings > Navigation bar*:
Files, Search, Statistics and Catalogs (OPDS) are also available.

- **Home** shows the book you are reading under *Continue reading*, with its chapter,
  progress and an estimate of the time left. Below it are the *Recently opened* books
  and *Want to read*, the books you have not opened yet.
- **Library** lists every book as a list or a grid. Sort by Recent, Title, Author or
  Date added, filter by reading status or format, and search by title or author.
  Swipe up or down, or drag the scroll bar on the right, to move through the list.
- **Collections** are your own groups of books.
- **Files** opens a book from any folder, including ones outside the library.

Tap a book to read it. Hold it for its menu: open, *Book details*, *Mark as finished*
or *Mark as unread*, *Add to collection…*, *Export highlights* and *Delete…*. Each book
has one of four statuses: Unread, Reading, Finished or On hold.

If you used KOReader before, *Settings > KOReader import > Import from KOReader* copies
your positions, highlights, notes, reading status and statistics. KOReader's files are
only read, never changed.

## 4. Reading

### Turning pages and other gestures

| Gesture | Default action |
| --- | --- |
| Tap the right two thirds, swipe left | next page |
| Tap the left third, swipe right | previous page |
| Tap the middle, or near the top | reader menu |
| Tap the top-left corner | bookmark this page |
| Tap the bottom-left corner | light off, tap again for the old level |
| Tap the bottom-right corner | warmth off, tap again for the old level |
| Slide up or down the left edge | brightness |
| Slide up or down the right edge | warmth |
| Swipe up from the bottom edge | reading settings (Aa) |
| Swipe down from the top edge | control center |
| Swipe in from the left edge | back, after a link or footnote |
| Long press on a word | select it and look it up in the dictionary |
| Pinch in or out | smaller or larger text (zoom on PDF and CBZ) |
| Two-finger tap | show or hide the status bars |
| Two-finger swipe up or down | brighter or dimmer |
| Two-finger swipe right or left | warmer or cooler |

*Settings > Reader gestures* changes any of these. The top-right corner, the bottom
middle, double tap and plain vertical swipes do nothing by default; a corner left
unused turns the page like the zone around it. Page keys on the Kindle Voyage and Oasis
and on Kobos that have them turn pages too.

rinky saves your position after every page turn, and the book opens there next time.

### The reader menu

A tap in the middle of the page opens the menu. The top bar has, from the left:
**Contents**, **Aa**, **Light**, **Bookmark**, **Search** and **More**. The bottom bar
shows the page and the pages left in the chapter, and has a slider for moving through
the book. Below the slider are buttons from plugins, such as *Dictionary*,
*Wikipedia* or *Insightful*; when there are more than four, the last one is *More*.

After any jump (a search result, Contents, the slider, a bookmark, a link or a
footnote) the bottom bar shows **Go back to p. N**, which returns where you were, one
jump at a time.

**More** holds *Go to page or chapter*, *Settings*, *Reading statistics*, *Vocabulary*
and *Close book*.

### Contents, bookmarks and search

**Contents** opens the *Go to* page with three tabs. *Contents* lists the chapters with
how much of each you have read. *Bookmarks* and *Highlights* list yours; tap one to go
there. The percent slider at the bottom jumps anywhere in the book.

Tap the top-left corner of a page, or **Bookmark** in the menu, to bookmark the page or
remove its bookmark.

**Search** finds text in the book. After you pick a result, a small bar at the bottom
steps to the previous and next hit; taps on the page still turn pages.

Tapping a link follows it. A footnote opens in a popup with *Go to note*; after a jump
to a note, a **Back to p. N** pill takes you back.

### Text appearance (Aa)

Tap **Aa** in the menu, or swipe up from the bottom edge, to open *Reading settings*.

- **Text:** presets (Compact, Standard, Large, Largest), font size, font, Justify,
  Hyphenate, Book's styles (off ignores the book's own styling) and Weight (Normal,
  Darker, Darkest; thicker strokes, the same line breaks).
- **Layout:** line spacing, side, top and bottom margins, space between paragraphs and
  first-line indent.
- **Page:** page turn wave, status bars, how often the page flashes to clear ghosting,
  and night mode.

Text and Layout changes apply to the open book only. **Use for all books** makes them
the default for every book, which you can also set under *Settings > Default layout*.
Page settings apply everywhere.

rinky can use any TrueType or OpenType font you copy to the `fonts` folder on the
Kindle drive, as well as the Kindle's own fonts such as Bookerly. *Settings > Font*
chooses the default reading font and the font of rinky's own screens.

### Status bars

The top and bottom bars around the text can show the clock, battery, Wi-Fi, light
level, page, percent, chapter or book title, author, pages or time left in the chapter,
time left in the book, and a progress bar. Set them up under *Settings > Status bars*,
or *Aa > Page > Status bars…*. A two-finger tap hides and shows them.

### Selecting text, highlights and notes

Hold a finger on a word to select it; with the default settings rinky also looks it up
in the dictionary. Drag to extend the selection. To select past the end of the page,
drag below the last line and hold still: the view moves on half a page and the
selection continues.

When you lift your finger a card shows the text with **Highlight**, **Note**,
**Insightful**, **Search** and **More** (Wikipedia, dictionary look-up and other plugin
actions). Tap a highlight later to get the card again, with **Remove highlight** in
place of Highlight.

### PDF and comics

PDF and CBZ pages are shown whole. Pinch out to zoom in and pinch in to zoom out; the
zoom lasts until you close the book. Fonts and margins do not apply.

## 5. Light, sleep and settings

### Control center

Swipe down from the top edge on any screen, or tap **Light** in the reader menu. The
panel has brightness and warmth sliders, *Auto warmth* (off, from sunset, or your own
hours) and up to two rows of buttons. Choose the buttons under *Settings > Control
center buttons*: Night mode, Auto warmth, Wi-Fi, Rotate, Refresh screen, Status bars,
Screenshot, Sleep, Battery, Settings and plugin buttons such as Sync progress, Calibre,
USB network and SSH server. A button with three dots in its corner has more options
when you hold it. Screenshots are saved in `/mnt/us/rinky/screenshots`.

Models without a warm light have no warmth slider.

### Sleep and the cover

The power button, or closing a magnetic cover, puts the Kindle to sleep: the light
goes off and the screensaver is drawn. The power button or opening the cover wakes it.
After 10 minutes without a touch the Kindle sleeps by itself; *Settings > Sleep*
changes the time.

*Settings > Screensaver* shows the current page, the book cover, a blank screen, or a
PNG picture you put in `/mnt/us/rinky/screensavers`.

A cover can move a little in a bag, so rinky ignores a cover that opens for less than
half a second. If the cover wakes the Kindle and nobody touches it, it goes back to
sleep after 3 minutes.

### Battery

*Settings > Battery* shows how much charge the Kindle uses per hour awake and asleep,
how much it has used since the last charge, and how long the rest may last. A rate
appears once the charge has dropped a point over at least a quarter of an hour; until
then, after an hour without a drop, the page shows an upper bound such as "under 0.2 %
per hour". Tap the battery card in *Settings > Device info* to get there, or add a
*Battery* button to the control center.

While rinky runs it stops the Amazon background services it does not use (cloud sync,
usage reporting, audio, Bluetooth) and starts them again when you exit. Power
management, Wi-Fi, USB and battery protection keep running.

### Settings

Open *Settings* from the navigation bar, or *More > Settings* in the reader. It is one
scrolling list:

- **Reading:** font, page turn wave, status bars, default layout, full refresh.
- **Gestures:** reader gestures.
- **Control center:** which buttons it shows.
- **Home & library:** navigation bar, library folders, KOReader import.
- **Keyboard:** which layouts the globe key switches between.
- **Plugins:** each plugin's switch and settings pages.
- **Device:** light, Wi-Fi, airplane mode, sleep, screensaver, orientation, night mode,
  language, device info, battery.
- **Diagnostics:** log level, view the log, crash logs, debug report.
- **About:** update.

*Exit to Kindle* is the last row.

## 6. Built-in plugins

Each plugin can be switched off under *Settings > Plugins*. Plugins you write yourself
in Lua go in `/mnt/us/rinky/plugins/<name>/`; [the plugin guide](plugins.md) explains
how to write one.

### Dictionary

A long press on a word looks it up. The card shows the headword, pronunciation, part of
speech and first sense; swipe it to see other dictionaries, tap **Full view** for the
whole entry, or **Add to vocabulary** to keep the word for review. You can also type a
word: *Dictionary* in the reader menu.

rinky reads StarDict dictionaries. Put each one in its own folder under
`/mnt/us/rinky/dict/`, for example `/mnt/us/rinky/dict/wordnet/` with the `.ifo`,
`.idx` and `.dict` (or `.dict.dz`) files. *Settings > Plugins > Dictionaries* sets the
order and turns dictionaries on and off.

### Statistics and vocabulary

rinky records how long and how much you read. *More > Reading statistics* in the reader,
or the *Statistics* item of the navigation bar, shows today, this week, the last 30
days, your streak, pages per hour, a calendar and one card per book.

*Words* on the statistics screen, or *More > Vocabulary* in the reader, lists the words
you looked up and quizzes you on them with flashcards.

### Insightful

Insightful answers questions about the book you are reading, using an AI provider
with your own API key: OpenAI, DeepSeek, OpenRouter or Anthropic. Put the key in
`/mnt/us/rinky/plugins/insightful/data/insightful.toml`:

```toml
provider = "openai"

[providers.openai]
base_url = "https://api.openai.com/v1/chat/completions"
model = "gpt-4.1-mini"
api_key = "sk-..."
```

If you used Insightful in KOReader, rinky reads its `configuration.lua` once instead.

- **Ask about a passage.** Select text and tap **Insightful**. *Explain*, *Give
  examples*, *Context / history* and *People / characters* ask at once. *Ask AI…*
  opens the chat; tap *Message Insightful…* to type, and the passage goes with your
  message.
- **One chat or many.** Questions about passages continue the book's current chat.
  Turn on *New chat for highlighted actions* in the reader menu's *Insightful* page to
  start a new chat each time, or tick the checkbox under the actions to do it once.
- **Chats.** The reader menu's *Insightful* page has *Continue current chat*, *Chats*
  (tap one to open it, hold it to delete it) and *Start new chat*.
- **While it answers** you see each look-up it makes in the book. Tap *Stop* to end
  the answer. If you close the chat the answer keeps coming, and a notice tells you
  when it is ready. Each answer ends with follow-up questions you can tap.
- **Statistics** on the same page shows requests, tokens and, when the provider
  reports it, cost.

### KOReader sync

Keeps your position in step with KOReader on other devices through a KOReader sync
server. Log in or register under *Settings > Plugins > Progress sync (KOSync)*. When
you open a book that another device has read further, rinky asks whether to jump
there. Your position is sent while you read, when you close the book and before the
Kindle sleeps; *Sync progress* in the reader menu or the control center syncs at once.

### OPDS catalogs

Browses online book catalogs such as Project Gutenberg or your own calibre server, and
downloads books into `/mnt/us/Books`. Add a catalog under *Settings > Plugins > OPDS
catalogs* (its address, then a name). Open catalogs from that page, or add *Catalogs*
to the navigation bar.

### Calibre wireless

Sends books from calibre on your computer to the Kindle over Wi-Fi, like KOReader's
calibre plugin.

1. In calibre, choose *Connect/share > Start wireless device connection*.
2. In rinky, with Wi-Fi on, tap *Connect to calibre* in the reader menu or the library
   menu, or the *Calibre* button in the control center.
3. In calibre, send books to the device. Each one shows a short message when it
   arrives, and the library updates after each batch.

rinky finds calibre on the network by itself. If it does not (some routers block
this), set *Server* under *Settings > Plugins > Calibre wireless* to the address
calibre shows, for example `192.168.1.20:9090`. The same page has the password and the
books folder (default `/mnt/us/Books`; books go in calibre's `Author/Title` folders).
Deleting a book from the device in calibre deletes the file on the Kindle. The
connection ends when calibre ejects the device, when you disconnect, when the Kindle
sleeps, or after 5 minutes with nothing sent.

### Wikipedia

Select a word or phrase and tap *Wikipedia* on the card (under *More* if it does not
fit), or type a term with *Wikipedia* in the reader menu. You need Wi-Fi, except for
pages you looked up before.
A card shows the start of the article; *Full view* opens all of it, and *Save* stores
it as a small book in `/mnt/us/Books`. If there is no page with that exact title,
rinky lists the closest matches.

*Settings > Plugins > Wikipedia* sets the language (default `en`) and extra languages
to swipe between on the card.

### USB network and SSH

These two plugins let you reach a Kindle from a computer; they are made for the Kindle.
Add their buttons to the control center (*Settings > Control center buttons*,
*USB network* and *SSH server*); a tap turns each on or off, and the line under the
name shows the address.

**USB network** turns the USB cable into a network link. The Kindle is
`192.168.15.244`; give the computer `192.168.15.201` with netmask `255.255.255.0`:

- macOS: `networksetup -setmanual "Kindle USB network" 192.168.15.201 255.255.255.0`
  (the port appears in System Settings > Network).
- Linux: `sudo ip addr add 192.168.15.201/24 dev <interface>`, then
  `sudo ip link set <interface> up`.
- Windows is not supported yet.

While USB network is on, the Kindle cannot be used as a USB drive. That comes back the
next time you plug in the cable after exiting rinky.

**SSH server** lets you log in to the Kindle as root over USB network or Wi-Fi, on
port 22.

1. Put your computer's public key (the line in `~/.ssh/id_ed25519.pub`, for example)
   in `/mnt/us/rinky/ssh/authorized_keys`. The file is created with instructions the
   first time the server starts. A changed key counts from the next start.
2. Turn the server on and run `ssh root@192.168.15.244` (USB network) or
   `ssh root@<address shown on the button>` (Wi-Fi).
3. Copy files with `scp -O`, for example
   `scp -O book.epub root@192.168.15.244:/mnt/us/Books/`. The `-O` is needed because
   there is no SFTP.

*Settings > Plugins > SSH server* changes the port and can start the server every time
rinky starts. *Login without password* lets anyone who can reach the Kindle log in as
root; use it only on a network you trust, and turn it off again.

## 7. Export your highlights

rinky writes a book's highlights, notes and bookmarks to a Markdown file,
`/mnt/us/rinky/exports/<title>.md`: title and author, then each entry in book order
with its chapter, the quoted text, your note, the page and the date. Copy it off over
USB or SSH.

- While reading: *Contents*, the *Highlights* tab, *Export*.
- In the library: hold a book, *Export highlights*.
- Every book at once: *Settings > KOReader import > Export all highlights*.
- Over SSH: `/mnt/us/rinky/rinky export-highlights BOOK` (a path or part of a title),
  or `--all`.

Exporting again replaces the file. Page numbers are filled in when you export from the
reader; from the library only PDFs get them.

## 8. Exit to Kindle

*Settings > Exit to Kindle*, the last row of the settings list, closes rinky and brings
back the Kindle's own Home screen. Use it to shop, to read Amazon books, to copy files
over USB, or for settings rinky does not have. rinky asks first unless you turn off
*Ask before exit*. Your position is already saved.

The Home screen can take a few minutes to appear; the Kindle's software is slow to
start.

Two other ways, for when something went wrong:

- KUAL: *rinky > Exit to Kindle (stop rinky)*. KUAL is not available while rinky runs,
  so this is for when rinky is stuck in the background or the Home screen did not come
  back. It stops rinky if it runs and starts the Kindle's screens if they are not
  running.
- SSH: `sh /mnt/us/rinky/rinky-ctl.sh stop` does the same.

If rinky crashes, the watchdog starts the Kindle's screens within about 15 seconds.

## 9. Update

Turn on Wi-Fi, then *Settings > Update*, *Check for update*, and *Install X and
restart*. rinky downloads the new version from the project's GitHub releases, checks
it, installs it and restarts into it. Your settings, notes and plugins are kept.

The update needs about 30 MB free. It changes nothing unless the download matches the
checksums published with the release, and it installs all files or none: if anything
fails, the old version stays installed.

Other ways to update:

- KUAL: *rinky > Update rinky*. The new version runs the next time you start rinky.
- SSH: `/mnt/us/rinky/rinky update check`, then `/mnt/us/rinky/rinky update install`.
- Without Wi-Fi: install the new package over the old one as in the
  [install guide](install.md). Your data is kept.

To update from a fork or a mirror, add to `/mnt/us/rinky/settings.toml`:

```toml
[plugins.update]
url = "https://api.github.com/repos/OWNER/REPO/releases/latest"
# allow_http = true     # only for a mirror on your own network
```

## 10. Uninstall

On a Kindle, in KUAL choose *rinky > Uninstall rinky*, then:

- *Uninstall, keep notes and settings* removes the program and its KUAL entry. Your
  settings, positions, notes and plugin data stay, ready for a later install.
- *Uninstall completely* removes everything rinky added.

Close and reopen KUAL afterwards so the entry disappears. Over SSH the same is
`sh /mnt/us/rinky/uninstall.sh`, with `--keep-data` to keep your data.

A complete uninstall stops rinky, deletes the `rinky*` files in each book's `.sdr`
folder (and the folder itself if it is then empty and rinky created it), the KUAL
entry, the library item `documents/rinky.sh`, and the `rinky` folder. KOReader's files
and your books are not touched.

If you cannot use KUAL or SSH, delete these over USB: the folder `rinky`, the folder
`extensions/rinky`, and optionally the files starting with `rinky` in the `<book>.sdr`
folders next to your books.

On a Kobo, see the [install guide](install.md#7-uninstall).

## 11. Troubleshooting

When you report a problem, please include `/mnt/us/rinky/rinky.log` and
`/mnt/us/rinky/VERSION`. *Settings > Diagnostics > Write debug report* gathers the logs,
your settings without passwords, and any crash logs in one folder.

### The Kindle's Home screen does not come back

1. Wait a few minutes; the Kindle's software is slow to start.
2. Over SSH: `sh /mnt/us/rinky/rinky-ctl.sh stop`. If that does not help,
   `start lab126_gui`.
3. As a last resort, hold the power button for about 40 seconds until the Kindle
   restarts. Your books and notes are safe.

### rinky does not respond

KUAL cannot help here, because it is stopped while rinky runs. Over SSH run
`sh /mnt/us/rinky/rinky-ctl.sh stop`. Without SSH, hold the power button for about 40
seconds to restart the Kindle; rinky does not start by itself afterwards.

### "Application Error: The selected application could not be started"

This can appear on Home after you exit rinky. Tap *Close*. rinky tries to prevent it,
but the Kindle sometimes shows it anyway.

### KUAL has no rinky entry

Check that the Kindle drive has `extensions/rinky` with `config.xml` and `menu.json`
in it, then close and reopen KUAL.

### "Start rinky" does nothing

- Look at `rinky/rinky.log` on the Kindle drive.
- rinky may still be running in the background: choose *Exit to Kindle (stop rinky)*
  in KUAL, then start again.
- Over SSH, run `/mnt/us/rinky/rinky version`. If it says *Permission denied*, the
  jailbreak is incomplete; run the jailbreak's hotfix again.

### A book does not open

KFX and DRM books cannot be opened. For any other book, the reason is in `rinky.log`;
please report it with the book's format.

### The screen stays blank, or taps land in the wrong place

rinky works out the model from the serial number and writes it near the top of
`rinky.log` (`model: ...`). If it is wrong, name the model in
`/mnt/us/rinky/rinky.env` (on a Kobo `.adds/rinky/rinky.env`):

```sh
export RINKY_MODEL=PaperWhite4
```

The names are `PaperWhite3`, `PaperWhite4`, `PaperWhite5`, `PaperWhite5SE`,
`PaperWhite6`, `Voyage`, `Oasis`, `Oasis2`, `Oasis3`, `Basic4`, `Basic5`, `Scribe`,
`Scribe3`, `ColorSoft`, `ScribeColorSoft`; on a Kobo `Libra2`, `ClaraHD`, `Clara2E`,
`Forma`, `LibraH2O`, `ClaraBW`, `ClaraColour`, `LibraColour`, `TolinoShine5`,
`TolinoShineColor`, `TolinoVisionColor`.

If only the taps are wrong, `export RINKY_TOUCH=swap`, `swap,mirror_x` or
`swap,mirror_y` in the same file changes how they are read. Please open an issue with
the log either way.

### Sleep and the cover

Other lines for `rinky.env`:

| Line | Effect |
| --- | --- |
| `export RINKY_COVER=0` | ignore the cover |
| `export RINKY_COVER_SETTLE_MS=0` | wake as soon as the cover opens |
| `export RINKY_COVER_UNUSED_S=0` | no early sleep after the cover wakes the Kindle |
| `export RINKY_QUIET=0` | leave the Amazon background services running |

If the Kindle does not wake, hold the power button for about 10 seconds to restart it.

### The update fails

| Message | What to do |
| --- | --- |
| `up to date` | Nothing: you have the latest version. |
| `timed out`, `http: ...` | No network. Turn on Wi-Fi and try again. |
| `checksum mismatch ... update refused` | The download was damaged or is the wrong file. Nothing was changed; try again later. |
| `has no SHA-256 ... update refused` | The release has no checksums. Nothing was changed. |
| `No space left on device (os error 28)` | The download did not fit. Nothing was changed; free about 30 MB. |
| `installer failed: ... cannot write ... (no space left?); nothing was changed` | The new files did not fit next to the old ones. The old version is still installed; free some space. |
| `another update is running`, `another install or update is running` | Wait for the other update or install to finish. |
| `installer failed: ...` (other text) | The installer stopped; after a KUAL update the full output is in `/mnt/us/rinky/update.log`. |
| `HTTP status 404` | No release at the configured address. |
| `plain http is refused` | The update address starts with `http://`. Use `https://`, or set `allow_http`. |

### Log files

| File | Contents |
| --- | --- |
| `/mnt/us/rinky/rinky.log` | rinky and its launcher |
| `/mnt/us/rinky/watchdog.log` | the watchdog |
| `/mnt/us/rinky/kual.log` | KUAL menu actions |
| `/mnt/us/rinky/update.log` | *Update rinky* from KUAL |
| `/mnt/us/rinky/VERSION` | installed version and build |

## 12. Files rinky writes

Everything rinky writes is inside `/mnt/us/rinky` or the books' `.sdr` folders, plus
the two menu entries, so uninstall can remove it all.

| Path | What | Complete uninstall | `--keep-data` |
| --- | --- | --- | --- |
| `/mnt/us/rinky/` | program, settings (`settings.toml`), library index, logs, plugins and their data, statistics, vocabulary, `exports/`, `screenshots/`, `ssh/`, dictionaries and fonts you added | removed | only the program files removed |
| `/mnt/us/extensions/rinky/` | KUAL entry | removed | removed |
| `/mnt/us/extensions/` | only if the installer created it | removed if empty | removed if empty |
| `/mnt/us/documents/rinky.sh` | library item (SSH install only) | removed | removed |
| `<book>.sdr/rinky.<ext>.json` | position, highlights, notes, bookmarks | removed | kept |
| `/tmp/rinky/` | lock and process ids while running (in memory) | removed | removed |

## 13. License

rinky is free software under the GNU Affero General Public License, version 3 or
later (`/mnt/us/rinky/LICENSE`), the same license as MuPDF, which it uses for PDFs.
The bundled Noto fonts are under the SIL Open Font License
(`/mnt/us/rinky/LICENSE-Noto`).
