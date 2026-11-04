# rinky user guide

rinky is an e-book reader for the Kindle Paperwhite 12th generation (2024, also
called PW6 or PW12) on firmware 5.19.x. While it runs it replaces the Kindle's own
screens; when you exit, the Kindle's Home screen comes back.

> **How to read this guide.** Lines marked **To be completed at integration** are
> placeholders for features that are still being built. Everything else describes the
> install package as it is now.

Contents:

1. [Before you start](#1-before-you-start)
2. [Install](#2-install)
3. [Start rinky](#3-start-rinky)
4. [Using rinky](#4-using-rinky)
5. [Plugins](#5-plugins)
6. [Exit to Kindle](#6-exit-to-kindle)
7. [Update](#7-update)
8. [Uninstall](#8-uninstall)
9. [Troubleshooting](#9-troubleshooting)
10. [Files rinky writes](#10-files-rinky-writes)
11. [License](#11-license)

## 1. Before you start

You need:

- A Kindle Paperwhite 12th gen on firmware 5.19.x. Check it under
  *Settings > Device Options > Device Info*.
- A **jailbroken** Kindle (for example WinterBreak or AdBreak, followed by the jb.sh
  hotfix). rinky cannot run on a Kindle that is not jailbroken.
- One way to start it:
  - **KUAL** (Kindle Unified Application Launcher), recommended. rinky adds an
    *rinky* entry to KUAL.
  - or **SSH** access to the Kindle (for example from the jailbreak's tools, or the SSH
    server in KOReader).
- Wi-Fi, only for updates.

What rinky changes on your Kindle:

- It adds the folder `rinky` (program and its data) and the KUAL entry
  `extensions/rinky` on the Kindle drive.
- Next to each book you open it keeps a small file `<book>.sdr/rinky.<ext>.json`
  (for example `rinky.epub.json` or `rinky.pdf.json`; reading position, notes). The `.sdr` folder is the one KOReader uses; rinky never changes
  KOReader's files in it.
- Nothing else. The Kindle's system files, the Kindle library and KOReader
  (`/mnt/us/koreader`) are not touched. [Uninstall](#8-uninstall) removes all of the
  above.

Download the install package `rinky-<version>-kindlehf.tar.gz` from the project's
releases page. If you want to check the download, compare its SHA-256 with the
`SHA256SUMS` file of the same release. Example:
`shasum -a 256 rinky-0.4.3-kindlehf.tar.gz` on a Mac, or `sha256sum ...` on Linux.

## 2. Install

There are two ways to install. Both end with the same files on the Kindle.

### A. Over USB (no shell needed)

1. Unpack the archive on your computer. On macOS, double-click it. On Linux, run
   `tar -xzf rinky-<version>-kindlehf.tar.gz`. On Windows, use 7-Zip. You get two
   folders: `rinky` and `extensions`.
2. Connect the Kindle by USB. Newer Kindles, the Paperwhite 12th generation among
   them, connect as an MTP device (like a phone), not as a drive. On Windows the Kindle
   appears in File Explorer under *This PC*. macOS Finder does not show MTP devices, so
   use an MTP app (for example OpenMTP). Older Kindles show up as a drive named *Kindle*.
3. Copy the folder `rinky` to the top level of the Kindle's storage.
4. If you use KUAL: open `extensions` from the archive and copy the folder
   `extensions/rinky` into the Kindle's `extensions` folder. If the Kindle has no
   `extensions` folder, KUAL is not installed and you can skip this step.
5. Eject the Kindle.

When you install over USB, nothing records whether you created the `extensions` folder
yourself. If you created it only for rinky, delete it by hand after an uninstall.
The first start from KUAL records which book `.sdr` folders already exist, as the
installer does (see [Uninstall](#8-uninstall)).

### B. Over SSH

Copy the package to the Kindle (over USB into the top level of the drive, or with
`scp`), then on the Kindle:

```sh
mkdir -p /mnt/us/.rinky-pkg
tar -xzf /mnt/us/rinky-<version>-kindlehf.tar.gz -C /mnt/us/.rinky-pkg
sh /mnt/us/.rinky-pkg/rinky/install.sh
rm -rf /mnt/us/.rinky-pkg /mnt/us/rinky-<version>-kindlehf.tar.gz
```

The installer:

- checks the package files against their checksums, then copies them to
  `/mnt/us/rinky` and `/mnt/us/extensions/rinky`;
- installs all files or none: it first copies every file next to the old one under a
  temporary name (`<name>.new<number>`), and replaces the old files only when every
  copy worked. If the Kindle is full, it stops with *"cannot write ... (no space
  left?); nothing was changed"*, deletes its temporary copies and leaves the installed
  version as it was. It needs free space for one full copy of the package (about
  5 MB) on top of the installed version (the in-app update needs about 10 MB, see
  [Update](#7-update));
- runs one at a time: a second install or update started meanwhile stops with
  *"another install or update is running"*;
- notes which book `.sdr` folders exist before rinky first runs (in
  `/mnt/us/rinky/.install-sdr`), so the uninstaller keeps them;
- creates `/mnt/us/extensions` if it does not exist, and records that in
  `/mnt/us/rinky/.install-state` so the uninstaller can remove it again;
- keeps your settings, notes and plugins when you install over an older version;
- refuses to run while rinky is running. Exit rinky first.

### Check the install

```sh
sh /mnt/us/rinky/rinky-ctl.sh status    # prints the installed version
/mnt/us/rinky/rinky version
```

## 3. Start rinky

- **KUAL:** open KUAL, then choose *rinky > Start rinky*.
- **SSH:** `sh /mnt/us/rinky/rinky-ctl.sh start` opens the library.
  `sh /mnt/us/rinky/rinky-ctl.sh start /mnt/us/Books/book.epub` opens that
  book.

What happens:

1. The Kindle's own screens stop. This takes a few seconds; the screen may flash.
2. rinky shows the library. Books are listed from `/mnt/us/Books` and
   `/mnt/us/documents`.
3. When you [exit](#6-exit-to-kindle), the Kindle's Home screen comes back. This can
   take up to about three minutes (measured in the emulator; not yet measured on a
   real Kindle).

A small background program (the *watchdog*) makes sure the Kindle's screens come back
even if rinky crashes.

## 4. Using rinky

> **To be completed at integration:** screenshots and the final gestures for each
> section below. The current development build behaves as described in the
> "Now" notes.

### Library and home

> **To be completed at integration (phase 4):** continue reading, recent books,
> collections, grid and list views, sorting, filters, search, book details, deleting
> books, the file browser, and the one-time import of KOReader progress and notes.

The library uses one word for a book you have not opened: **Unread**. It is the filter
chip, the cover badge, the status in the book menu and on the details page, and the
first word of the line under a book on the home screen.

Now: the library lists up to ten books from `/mnt/us/Books` and `/mnt/us/documents`.
Tap a title to open it.

### Reading

> **To be completed at integration (phase 3):** tap zones and how to change them,
> top and bottom bars, progress display (page, location, percent, time left), the
> *Aa* panel (font, size, spacing, margins, alignment, hyphenation, orientation, dark
> mode, refresh), Go to (contents, page, location, percent), page flip, search,
> selecting text, highlights and notes, bookmarks, footnotes and links, dictionary
> look-up, and the end-of-book screen.

Now:

- Tap the right two thirds of the screen for the next page, and the left third for
  the previous page. Horizontal swipes also turn pages.
- Light, as in KOReader: slide a finger up or down the left edge for brightness, or
  the right edge for warmth (on the page itself; not while a menu, the Insightful chat
  or a dialog is open). Tap the bottom-left corner to turn the light off and tap
  it again to get the old level back; the bottom-right corner does the same for
  warmth. *Settings > Gestures* turns these off or moves them, and the bottom middle
  of the page can be given an action there too.
- rinky saves your position after every page turn. The book opens at that page
  the next time.
- Tap a highlight to get its card again: **Remove highlight**, **Note**,
  **Insightful** and the other selection actions work on the highlighted text.
- After a jump (a search result, Contents, Go to, the page slider, a bookmark, a link
  or a footnote), the reader menu shows **Go back to p. N**: it returns to where you
  were, one jump at a time.

#### Text appearance

The *Aa* panel (swipe up from the bottom, or the reader menu) changes the open book.
*Use for all books* saves the values as the defaults, which are also on
*Settings > Default layout*. Every change lays the book out again once and keeps your
place.

- *Text tab:* **Hyphenate**, **Book's styles** (off: the book's own CSS is ignored and
  only rinky's plain style and your settings apply) and **Weight** (Normal,
  Darker, Darkest: every stroke of the text is drawn 1 or 2 pixels wider; the line
  breaks do not change).
- *Layout tab:* **Space between paragraphs** (None, 0.5 em, 1 em, added to the book's
  own spacing) and **First-line indent** (Book, None, 1, 1.5, 2 em; "Book" keeps the
  book's own indent; centred and right-aligned paragraphs are not indented).
- In `settings.toml`, `[reader]` takes `paragraph_spacing` (em), `paragraph_indent`
  (em; a negative number means the book's own), `font_weight` (0, 1, 2), `hyphenate`
  and `publisher_styles`. Missing keys keep the old behaviour.
- Weight is not applied to the fallback fonts used for scripts the reading font lacks
  (for example CJK).

### Formats

| Format | Status |
| --- | --- |
| EPUB | readable |
| FB2, TXT, HTML, MOBI/AZW3 without DRM | **To be completed at integration (phase 6)** |
| PDF, CBZ | **To be completed at integration (phase 6)** |
| KFX and DRM-protected books (the Amazon store) | not readable. They are listed but refused with a message. Read them in the Kindle's own reader: [Exit to Kindle](#6-exit-to-kindle). |

### Light, Wi-Fi, sleep, USB and settings

> **To be completed at integration (phase 5):** frontlight and warmth, Wi-Fi networks,
> the power button and sleep screen, automatic sleep, battery, USB drive mode, the
> settings screens, and screenshots.

Now: pressing the power button in the development build exits rinky. Sleep and
wake inside rinky are not finished and have not been tested on a real Kindle yet.

### Getting your highlights off the Kindle

rinky writes a book's highlights, notes and bookmarks to a Markdown file,
`/mnt/us/rinky/exports/<title>.md`: title and author, then every entry in book
order with the chapter, the highlighted text as a quote, your note, and the page and
date. Copy the file over USB or SSH. A book with nothing to export gets no file.

- While reading: menu, *Go to*, *Highlights* tab, *Export*.
- In the library: hold a book, *Export highlights*.
- Settings, *KOReader import* page, *Highlights*: *Export all highlights* (one file per book that
  has highlights, notes or bookmarks; books with the same title get the format or an
  id added to the name).
- Over SSH: `rinky export-highlights BOOK` (a file path or part of a title) or
  `rinky export-highlights --all`. It prints each file it wrote.

A toast shows the file name. The full path is in the log. Page numbers and percents are
filled in when you export from the reader; from the library, only fixed-layout books
(PDF) have page numbers. Exporting again replaces the file.

## 5. Plugins

rinky has two kinds of plugins. Both use the same plugin interface.

- **Built-in plugins** come with rinky.
  > **To be completed at integration (phases 8 to 10):** the list of built-in plugins
  > and how to use them: Insightful (AI explanations), dictionary, reading statistics,
  > vocabulary builder, OPDS catalogs, KOReader sync, Wikipedia (Calibre wireless is
  > below). Also how to turn each plugin on and off.
- **Your own Lua plugins** live in `/mnt/us/rinky/plugins/<name>/`, one folder per
  plugin, and are loaded when rinky starts.

### Insightful

Insightful answers questions about the book you are reading. It works with OpenAI,
DeepSeek, OpenRouter and Anthropic: put your API key in
`/mnt/us/rinky/plugins/insightful/data/insightful.toml` (or keep KOReader's
`insightful.koplugin/configuration.lua`, which rinky reads once).

- **Ask about a passage.** Select text and tap **Insightful**. **Explain**, **Give
  examples**, **Context / history** and **People / characters** send their question
  at once; the chat shows the passage and the whole prompt that was sent. **Ask AI…** opens the chat with the keyboard closed: tap **Message
  Insightful…** to type; the passage goes with your message.
- **One chat or many.** Highlighted actions continue the book's current chat, so
  related questions stay together. Turn on **New chat for highlighted actions** in the
  reader menu's **Insightful** page to start a new chat each time. To change it for one
  highlight only, tick the checkbox at the bottom of the actions (**Start a new chat**,
  or **Start in existing chat** when the setting is on). Hold the setting to use it as
  the default for other books.
- **Chats.** Reader menu, **Insightful**: **Continue current chat**, **Chats** (tap a
  chat to open it, hold it to delete it) and **Start new chat**. Gestures can open the
  current chat or the chat list (Settings, Gestures: *Insightful: show current chat*,
  *Insightful: show chats*).
- **While it answers** you see each book lookup as an *agent action*. Tap **Stop** to
  end the answer, or close the chat: the answer keeps coming and is saved, and a
  notice tells you when it is ready. Each answer ends with follow-up questions; tap one
  to ask it.
- **Statistics** (reader menu, **Insightful**): requests and tokens for the current
  book and all books, and the cost when the provider reports it.

### Calibre wireless

Sends books from calibre on your computer to the Kindle over Wi-Fi, like KOReader's
calibre plugin (calibre's "wireless device" connection).

1. In calibre: **Connect/share > Start wireless device connection** (set a password
   there if you want one).
2. On the Kindle, with Wi-Fi on: **Settings > Plugins > Calibre wireless > Connect to
   calibre**, or **Connect to calibre** in the reader menu (behind the **More…** pill
   when the menu has more than four plugin entries).
3. rinky finds calibre on the network by itself. If it does not (some routers block
   broadcasts), set **Server** to the address calibre shows, for example
   `192.168.1.20:9090`.
4. In calibre, send books to the device. Each book shows a short message when it
   arrives; the library is refreshed after each batch. Deleting a book on the device in
   calibre deletes the file on the Kindle (its highlights in the `.sdr` folder stay).

The settings page shows the state (`Connected to <computer> · receiving 2/5`) and has
Connect/Disconnect, **Server**, **Password** and **Books folder** (default: the first
library folder, `/mnt/us/Books`). Books go into calibre's folders, for example
`/mnt/us/Books/Author Name/Title - Author Name.epub`. The connection ends when calibre
ejects the device, when you tap Disconnect, when the Kindle goes to sleep, or after 5
minutes without activity (`idle_minutes` under `[plugins.calibre]` in `settings.toml`).
Wrong password: rinky asks for it again and reconnects. The book list calibre sees
is kept in `/mnt/us/rinky/plugins/calibre/data/metadata.calibre.json`. To turn the
plugin off: **Settings > Plugins**.

### Wikipedia

Looks up the selected word or phrase on Wikipedia. Needs Wi-Fi (a lookup you made
before also works without it).

1. Select a word or phrase in a book and tap **More**, then **Wikipedia**, in the
   selection popup. You
   can also type a search term: **Wikipedia** in the reader menu.
2. A card shows the page title, a short description, the first paragraph and the
   language (`Wikipedia - en`). A message "Looking up ..." shows while it loads.
3. **Full view** opens the whole article as plain text; it pages like other long texts.
   **Save** stores the article as a small `.html` book in `/mnt/us/Books` (the library
   refreshes; a file with the same name is never overwritten).
4. If Wikipedia has no page with that exact title, a list of the best search hits
   opens; tap one to see its card. A misspelling may offer the corrected word.
5. With extra languages set, the card has one page per language (`2/3 - Wikipedia - de`).
   Swipe left or right on the card; a language not looked at yet loads then. A language
   without an article shows a "No article found" page.

Settings: **Settings > Plugins > Wikipedia**. **Language** is the language code of the
main Wikipedia (default `en`; `da`, `de`, `fr`, `simple`, ...). **Extra languages** is a
list of codes to swipe between, separated by spaces or commas. The same can be set in
`settings.toml`:

```toml
[plugins.wikipedia]
language = "da"
languages = ["en", "de"]
save_dir = "/mnt/us/Books"
```

If Wi-Fi is off the card says "Turn on Wi-Fi to look this up on Wikipedia"; turn Wi-Fi on
and tap **Retry**. The last 50 summaries are kept in
`/mnt/us/rinky/plugins/wikipedia/data/cache.json`; delete it to start over.

### USB network and SSH

Two plugins for reaching the Kindle from a computer: **USB network** makes the USB cable
a network link, **SSH server** lets you log in to a shell. Each has a control-center
button: **Settings > Control center buttons**, add **USB network** and **SSH server**.
A tap turns it on or off; the line under the name shows the address while it is on.
Without a USB cable the USB network button says **Not connected**: plug the Kindle into
a computer first.
Hold a button (three dots in its corner) for its settings, or open **Settings > Plugins >
USB network** and **SSH server**.

**USB network.** While it is on, the Kindle shows up on the computer as a USB Ethernet
adapter instead of a file-transfer (MTP) device. The Kindle is `192.168.15.244`; give the
computer `192.168.15.201` (netmask `255.255.255.0`) once:

- macOS: a network port **Kindle USB network** appears in System Settings > Network.
  `networksetup -setmanual "Kindle USB network" 192.168.15.201 255.255.255.0`
- Linux: `sudo ip addr add 192.168.15.201/24 dev <interface>` and
  `sudo ip link set <interface> up`.
- Windows: not supported yet (it needs a driver for this kind of adapter).

USB file transfer is off while USB network is on, and turning USB network off does not
bring it back while rinky runs: file transfer comes from the Kindle home screen,
which is stopped while rinky runs. USB network is turned off when rinky exits;
file transfer then comes back the next time you plug the cable in at the Kindle home
screen. A restart of rinky (an update or a deploy) keeps USB network on.
**Turn on when rinky starts** switches it on at every start.

**SSH server.** Runs an SSH server (dropbear, included in rinky) while rinky
runs, on port 22, over USB network or Wi-Fi. It stops when rinky exits, and starts
again by itself after a restart (an update or a deploy).

1. Put your computer's public key (for example the one line in `~/.ssh/id_ed25519.pub`)
   in `/mnt/us/rinky/ssh/authorized_keys`, one key per line. The file is created with
   instructions the first time the server starts.
2. Turn the server on, then on the computer: `ssh root@192.168.15.244` (USB network) or
   `ssh root@<Wi-Fi address>`; the button shows the address. The first login asks you to
   trust the Kindle's key.
3. Copy files with `scp -O`, for example `scp -O book.epub root@192.168.15.244:/mnt/us/Books/`
   (there is no SFTP, so newer OpenSSH needs `-O`).

**Login without password** (off by default) accepts any password, for when no key is set
up yet. Anyone who can reach the Kindle over Wi-Fi or the cable then gets a root shell,
so use it on networks you trust and turn it off again. **Port** changes the port (KOReader's
own SSH server uses 2222). **Start when rinky starts** starts it at every start.
Changed keys are read when the server starts: turn it off and on again.

A Lua plugin folder contains:

```text
/mnt/us/rinky/plugins/wordcount/
  plugin.toml     name, version, permissions
  main.lua        the code (the file named by `entry`)
  data/           the plugin's own files (created by rinky)
```

`plugin.toml`:

```toml
name = "wordcount"              # lowercase letters, digits, - and _
version = "0.1.0"
description = "Counts words on the page"
permissions = ["document", "ui"]
entry = "main.lua"
```

Permissions. A plugin can only use what its `plugin.toml` asks for:

| Permission | Allows |
| --- | --- |
| `document` | reading the open book: text, contents, search, selection |
| `ui` | dialogs, menus, text viewers, keyboard input, messages |
| `storage` | the plugin's own settings and files |
| `network` | internet requests |

Rules:

- Lua plugins run in a sandbox. They cannot run programs and have no direct file
  access (no `io`, `os` or `require`); they keep data through the `storage`
  permission.
- Each call has a time limit. A plugin that fails is switched off for the session and
  its error is shown. The book stays open.
- To remove a plugin, delete its folder.

> **To be completed at integration (phase 7):** the Lua API reference (events, menu
> items, selection actions, gestures, settings pages, dialogs, storage, HTTP), the
> sample plugin, and where plugin errors are shown.

## 6. Exit to Kindle

*Exit to Kindle* closes rinky and brings back the Kindle's own Home screen. Use it
to shop, to read Amazon (KFX) books, for Audible, or for any setting rinky does not
have.

- **Inside rinky:**
  > **To be completed at integration:** the final menu entry (for example
  > *Menu > Exit to Kindle*).

  Now: tap the top-right corner of the screen, or press the power button.
- After the exit, the Kindle Home screen returns within about three minutes. Your
  reading position is already saved.
- **From KUAL:** *rinky > Exit to Kindle (stop rinky)*. KUAL is part of the
  Kindle's own screens, which are stopped while rinky runs. So use this entry when
  something went wrong: for example, rinky is still running in the background, or
  the Home screen did not come back. It stops rinky if it runs and starts the
  Kindle's screens if they are not running. If rinky is not running it does
  nothing.
- **Over SSH:** `sh /mnt/us/rinky/rinky-ctl.sh stop` does the same.

If rinky crashes or hangs and is killed, the watchdog brings the Kindle's screens
back on its own within about 15 seconds.

## 7. Update

Updates come from the project's GitHub releases. Each release contains the package
`rinky-<version>-kindlehf.tar.gz` and a `SHA256SUMS` file.

- **In rinky:** *Settings > Update*, *Check for update*, then *Install X and restart*.
  Turn Wi-Fi on first. rinky installs the new version and restarts into it by itself;
  the Kindle's own screens do not come up in between.
- **KUAL:** *rinky > Update rinky*. Turn Wi-Fi on first. The result appears at
  the bottom of the screen.
- **Over SSH:**

  ```sh
  /mnt/us/rinky/rinky update check      # show whether a newer version exists
  /mnt/us/rinky/rinky update install    # download and install it
  ```

What the update does:

1. It reads the latest release from
   `https://api.github.com/repos/jyliuu/rinky/releases/latest`.
2. It continues only if that version is newer than the installed one. Add `--force` to
   reinstall the same version.
3. It downloads the package and checks its SHA-256 against the release's `SHA256SUMS`
   file and against the checksum GitHub publishes for the file. If no checksum is
   available, or a checksum does not match, it stops: *"checksum mismatch ... update
   refused"*. Nothing is changed and the download is deleted.
4. It checks that the package contains only rinky's own folders and checks the
   SHA-256 again while it unpacks the file, then runs the package's installer. The
   installer installs all files or none (see [Install](#2-install)). Your settings,
   notes and plugins are kept.
5. From *Settings > Update* rinky then restarts into the new version. From KUAL or
   SSH the new version runs the next time you start rinky.

The update needs about 10 MB free on the Kindle: the unpacked package in
`/mnt/us/rinky/.update` (about 5 MB) and the installer's copies next to the old
files (about 5 MB) exist at the same time. Both are deleted when the update ends.

Only one update runs at a time. If you tap *Update rinky* again while an update
runs, the second one stops at once with *"another update is running"*.

To use a different release address (a fork or a local mirror), add this to
`/mnt/us/rinky/settings.toml`:

```toml
[plugins.update]
url = "https://api.github.com/repos/OWNER/REPO/releases/latest"
# allow_http = true     # only for a mirror on your own network
```

You can also give `--url URL` on the command line, or set the environment variable
`RINKY_UPDATE_URL`. Plain `http://` addresses are refused unless `allow_http` is
set or `--allow-http` is given.

You can also update without Wi-Fi: install the new package over the old one as in
[Install](#2-install). Your data is kept.

## 8. Uninstall

Uninstalling puts the Kindle back the way it was before rinky was installed.

- **KUAL:** *rinky > Uninstall rinky*, then choose one:
  - *Uninstall, keep notes and settings*: removes the program and the KUAL entry. Your
    settings, reading positions, notes and plugin data stay in `/mnt/us/rinky` and
    next to your books, ready for a later install.
  - *Uninstall completely*: removes everything rinky added.

  Afterwards, close and reopen KUAL so the entry disappears from its menu.
- **Over SSH:**

  ```sh
  sh /mnt/us/rinky/uninstall.sh              # everything
  sh /mnt/us/rinky/uninstall.sh --keep-data  # program only
  ```

A complete uninstall:

1. stops rinky if it is running, and makes sure the Kindle's screens run;
2. deletes `rinky.<ext>.json` (and any other `rinky*` file) in each book's `.sdr`
   folder, and the `.sdr` folder itself if it is then empty and did not exist before
   rinky was installed. KOReader's files in those folders are kept;
3. deletes `/mnt/us/extensions/rinky`, and `/mnt/us/extensions` if the installer
   created it and it is now empty;
4. deletes `/mnt/us/rinky` and the temporary folder `/tmp/rinky`.

The uninstaller does not run while an install or an update runs. It then stops with
*"an install or update is running (pid N)"* or *"an update is running (pid N)"*, and
changes nothing; KUAL shows *"Uninstall failed; log in /tmp"*. Wait until the update has
shown its result, then uninstall again.

**By hand over USB**, if you cannot use KUAL or SSH:

1. Delete the folder `rinky`.
2. Delete `extensions/rinky`.
3. Optional: in the folders next to your books named `<book>.sdr`, delete the files
   whose names start with `rinky` (`rinky.epub.json`, `rinky.pdf.json` and
   so on; older versions wrote `rinky.json`).

## 9. Troubleshooting

### The Kindle screens do not come back after rinky

1. Wait three minutes. Restarting the Kindle's screens is slow.
2. If rinky crashed, the Kindle's screens are started again on their own: by the
   launcher at once, or by the watchdog within 15 seconds. They then take the same few
   minutes to appear.
3. Over SSH: `sh /mnt/us/rinky/rinky-ctl.sh stop`. If that does not help,
   run `start lab126_gui`.
4. As a last resort, restart the Kindle: hold the power button for about 40 seconds,
   until the screen goes blank and the Kindle restarts. Your books and notes are safe.

### rinky does not respond

While rinky runs, the Kindle's own screens and KUAL are stopped, so KUAL cannot
help here.

1. Over SSH: `sh /mnt/us/rinky/rinky-ctl.sh stop`. It stops rinky and
   starts the Kindle's screens.
2. Without SSH: restart the Kindle by holding the power button for about 40 seconds.
   rinky does not start by itself after a restart.
3. Please report the problem with `/mnt/us/rinky/rinky.log`.

### "Application Error: The selected application could not be started" on Home

This can appear after the Kindle screens restart. Tap *Close*. rinky restarts the
Kindle's application manager before it brings the screens back, which normally avoids
the message.

### KUAL has no rinky entry

- Check that the folder `extensions/rinky` exists on the Kindle drive and contains
  `config.xml` and `menu.json`.
- Close and reopen KUAL.

### "Start rinky" does nothing

- Look at `/mnt/us/rinky/rinky.log` (over USB or SSH).
- rinky may already be running in the background. Use *Exit to Kindle* in KUAL,
  then start again.
- The jailbreak must allow programs to run from the Kindle drive. Run
  `/mnt/us/rinky/rinky version` over SSH. If it prints *Permission denied*,
  the jailbreak is incomplete. Run the jailbreak's hotfix again.

### Taps land in the wrong place

Touch mapping can be adjusted with the environment variable `RINKY_TOUCH`
(`swap`, `mirror_x`, `mirror_y`, comma-separated). Now this works only when you start
rinky over SSH:

```sh
RINKY_TOUCH=swap,mirror_x sh /mnt/us/rinky/rinky-ctl.sh start
```

> **To be completed at integration (phase 5):** the touch setting in the settings
> screen and in `settings.toml` (`[device] touch = "swap,mirror_x"`), so that it also
> applies when you start from KUAL. The development build does not read that key yet.

### A book does not open

- KFX and DRM-protected books are refused. Read them with the Kindle's own reader.
- Other books: the error is in `/mnt/us/rinky/rinky.log`. Please report it
  together with the book's format.

### The update fails

| Message | Meaning |
| --- | --- |
| `checksum mismatch ... update refused` | The download differs from what the release says (damaged download or wrong file). Nothing was changed. Try again later. |
| `another update is running` | An update started earlier has not finished. Wait for its message, then try again if needed. |
| `No space left on device (os error 28)` | The Kindle is full: the download did not fit in `/mnt/us/rinky/.update`. Nothing was changed. Delete some books and try again. |
| `installer failed: ... cannot write ... (no space left?); nothing was changed` | The download worked, but the new files did not fit next to the old ones. The installer deleted its copies; the old version is still installed. Free about 10 MB and try again. |
| `installer failed: ... another install or update is running` | An install from SSH or another update runs at the same time. Try again when it has finished. |
| `installer failed: ...` (other text) | The package's installer stopped. The text after `installer failed:` is its last lines; the full output is in `/mnt/us/rinky/update.log` when you used KUAL. |
| `has no SHA-256 ... update refused` | The release has no `SHA256SUMS` and no checksum for the package. Nothing was changed. |
| `HTTP status 404` | No release was found at the configured address. |
| `plain http is refused` | The configured address starts with `http://`. Use `https://`, or set `allow_http`. |
| `timed out`, `http: ...` | No network. Turn Wi-Fi on and try again. |
| `up to date` | You already have the latest version. |

### Sleep and battery

The power button or closing a magnetic cover puts the Kindle to sleep: the frontlight
goes off, the sleep screen is drawn, and the Kindle sleeps fully after a moment. The
power button or opening the cover wakes it. After 10 minutes without a touch the
Kindle goes to sleep by itself.

A cover can move a little in a bag. rinky ignores a cover that opens for less than
half a second. If the Kindle wakes from the cover and nobody touches it, it goes back
to sleep after 3 minutes.

While rinky runs it stops the Amazon background services it does not use (cloud sync,
usage reporting, audio, Bluetooth) and starts them again when you leave rinky. Power
management, Wi-Fi, USB and battery protection stay on.

To change these, put lines like the ones below in `/mnt/us/rinky/rinky.env`:

| Line | Effect |
| --- | --- |
| `export RINKY_COVER=0` | ignore the cover |
| `export RINKY_COVER_SETTLE_MS=0` | wake as soon as the cover opens |
| `export RINKY_COVER_UNUSED_S=0` | no early sleep after a cover wake |
| `export RINKY_QUIET=0` | leave the Amazon services running |

If the Kindle does not wake, hold the power button for about 10 seconds to restart it.

### Logs and reporting a problem

| File | Contents |
| --- | --- |
| `/mnt/us/rinky/rinky.log` | launcher and reader messages |
| `/mnt/us/rinky/watchdog.log` | watchdog messages (framework restarts) |
| `/mnt/us/rinky/kual.log` | KUAL menu actions |
| `/mnt/us/rinky/update.log` | output of *Update rinky* from KUAL |
| `/mnt/us/rinky/VERSION` | installed version and build |

`/mnt/us/rinky/rinky selftest --lipc` checks the device connections and
prints one PASS/FAIL line per check. Please include the logs and `VERSION` when you
report a problem.

## 10. Files rinky writes

| Path | What | Removed by uninstall |
| --- | --- | --- |
| `/mnt/us/rinky/` | program, scripts, `settings.toml`, `library.json`, `cache/`, `plugins/`, logs | yes (`--keep-data`: only the program files) |
| `/mnt/us/rinky/.install-state` | what the installer created | yes |
| `/mnt/us/rinky/.install-sdr` | the book `.sdr` folders that existed before rinky first ran | yes (`--keep-data`: kept) |
| `/mnt/us/rinky/.install.lock/` | while an install runs | yes (also deleted when the install ends) |
| `/mnt/us/rinky/.update/`, `.update.lock` | temporary download, and the lock, while an update runs | yes (also deleted after each update) |
| `/mnt/us/rinky/kual.log`, `update.log` | messages of the KUAL actions and the full output of KUAL updates (each kept under 256 KiB) | yes (`--keep-data`: kept) |
| `/mnt/us/extensions/rinky/` | KUAL entry | yes |
| `/mnt/us/extensions/` | only if the installer created it | yes, if empty |
| `/mnt/us/rinky/exports/<title>.md` | highlight exports (only when you export) | yes (the folder is inside `/mnt/us/rinky`) |
| `<book folder>/<book>.sdr/rinky.<ext>.json` (older versions: `rinky.json`) | position, highlights, notes, bookmarks | yes (not with `--keep-data`) |
| `/tmp/rinky/` | lock and process ids while running (in memory) | yes; also gone after a restart |
| `/mnt/us/rinky/ssh/` | SSH server: `authorized_keys`, the Kindle's host keys, `dropbear.log` | yes (the folder is inside `/mnt/us/rinky`) |
| `/dev/rinky-ssh/` (in memory; `/var/run` and `/var/local` are not root-only on the PW12) | a root-only copy of `authorized_keys` and the pid file while the SSH server runs | removed when the server stops |

> **To be completed at integration:** any further files the finished features write
> (for example screenshots or statistics). They must be listed here and stay inside
> `/mnt/us/rinky` or the `.sdr` folders, so uninstall can remove them.

## 11. License

rinky is free software under the GNU Affero General Public License, version 3 or
later (`/mnt/us/rinky/LICENSE`). The PDF engine links MuPDF, which uses the same
license. The bundled Noto Serif fonts are under the SIL Open Font License
(`/mnt/us/rinky/LICENSE-Noto`).
