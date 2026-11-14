# Installing rinky

rinky runs on Kindle and Kobo e-readers. Each kind gets its own package and needs a
different preparation: a jailbreak and KUAL on a Kindle, NickelMenu on a Kobo. Once
rinky runs, the [user guide](user-guide.md) explains how to use it.

1. [Which device, which package](#1-which-device-which-package)
2. [Kindle](#2-kindle)
3. [Kobo and Tolino](#3-kobo-and-tolino)
4. [Copying files to the e-reader](#4-copying-files-to-the-e-reader)
5. [Check the download](#5-check-the-download)
6. [Update](#6-update)
7. [Uninstall](#7-uninstall)

## 1. Which device, which package

Look up your model, then download the package in the last column from the
project's releases page. The model is under *Settings > Device Options > Device Info*
on a Kindle and *More > Settings > Device information* on a Kobo.

| Device | Status | Before you start | Package |
|---|---|---|---|
| Kindle Paperwhite 12th gen (2024) | tested on the device | jailbreak + KUAL | `rinky-<version>-kindle.zip` |
| Kindle Paperwhite 3 and 4, Oasis 3, Kindle 12th gen (2024) | runs in an emulator with Amazon's firmware | jailbreak + KUAL | `rinky-<version>-kindle.zip` |
| Kindle Paperwhite 5 and Signature Edition, Kindle 11th gen, Colorsoft, Scribe (all three), Oasis 1 and 2, Voyage | untested; same display interface as a model above | jailbreak + KUAL | `rinky-<version>-kindle.zip` |
| Kobo Libra 2 | runs in an emulator with Kobo's firmware 4.38 | NickelMenu | `rinky-<version>-kobo.zip` |
| Kobo Clara HD, Clara 2E, Forma, Libra H2O | untested; same display interface as the Libra 2 | NickelMenu | `rinky-<version>-kobo.zip` |
| Kobo Clara BW, Clara Colour, Libra Colour | untested; same hardware family as the Tolino Shine 5 | NickelMenu | `rinky-<version>-kobo.zip` |
| Tolino Shine 5 | runs in an emulator with its firmware 5.12 | SSH only, see [Tolino](#tolino) | `rinky-<version>-kobo.tar.gz` |
| Tolino Shine Color, Vision Color | untested | SSH only | `rinky-<version>-kobo.tar.gz` |
| Kindle Paperwhite 1 and 2, Kindle 7th, 8th and 10th gen; Kobo Glo HD, Aura ONE, Aura H2O², older Kobos; Kobo Elipsa, Sage | not supported | | |

A model is not supported when its screen has too few pixels for rinky's layout, which
is made for 300 ppi, or when it drives its display in a way rinky does not know yet.

What differs between models once rinky runs:

- **Page keys** turn pages on the Kindle Voyage and Oasis and the Kobo Libra 2, Libra
  Colour, Forma and Libra H2O. On the Voyage they are the pressure-sensitive bezel
  keys, which rinky switches on while it runs.
- **Warmth:** models without a warm light (Kindle Paperwhite 3 and 4, Voyage, Oasis 1
  and 2, Kindle 11th and 12th gen) have no Warmth slider.
- **Colour** screens (Kindle Colorsoft, Kobo Clara Colour and Libra Colour) show
  grayscale.
- **Scribe:** the pen does nothing in rinky; use your finger.
- **Night mode** is done by the display controller on the Kindle Paperwhite 5 and
  newer and on the Kobo i.MX models (Libra 2, Clara HD and 2E, Forma, Libra H2O); on
  the others rinky inverts the picture itself, which may ghost a little more.

The `.tar.gz` packages hold the same files as the zips; they are for installing over
SSH and for the in-app update.

## 2. Kindle

### Get the Kindle ready

rinky only runs on a **jailbroken** Kindle, started from **KUAL** (the Kindle Unified
Application Launcher) or, on devices where KUAL is unavailable, from **KPM** (the Kindle
Package Manager).

1. **Find the firmware version** under *Settings > Device Options > Device Info* (on
   older firmware: *Menu > Settings > Menu > Device Info*), for example *5.18.1*.
2. **Jailbreak.** Which jailbreak works depends on the firmware version, not on the
   model. [kindlemodding.org](https://kindlemodding.org/) lists them and walks you
   through the one for your version. Recent firmware uses WinterBreak or AdBreak
   followed by the jb.sh hotfix. Turn on airplane mode first and leave it on until you
   are done: a firmware update can close the hole the jailbreak needs.
3. **Install KUAL** (and MRPI, which installs it) from the same site, in the build it
   gives for your firmware. Firmware 5.16.3 and newer needs the *hard-float* KUAL; the
   Paperwhite 3, Voyage and Oasis 1 stay on older firmware and use the older KUAL.
   rinky is the same package for both. A device whose jailbreak provides KPM instead of
   KUAL skips this step and uses [Install with KPM](#install-with-kpm-no-kual).

### Install over USB

1. Unpack `rinky-<version>-kindle.zip` on your computer. You get two folders, `rinky`
   and `extensions`.
2. Connect the Kindle by USB (see [Copying files](#4-copying-files-to-the-e-reader)
   for how it shows up on your computer).
3. Copy the folder `rinky` to the top level of the Kindle's storage.
4. Open the Kindle's `extensions` folder (KUAL made it) and copy `extensions/rinky`
   from the zip into it.
5. Eject the Kindle.

### Install with KPM (no KUAL)

Some jailbreaks ship [KPM](https://kindlemodding.org/) instead of KUAL, and current
devices — the Scribe, for one — have no working KUAL booklet at all. KPM installs
packages without it, so every release also carries a KPM package:

```
rinky-<version>-kindlehf.kpkg
```

It holds the same `rinky/` and `extensions/` folders as the USB zip, plus the manifest
and the three hooks KPM calls. Add the repository, then install:

```sh
kpm add-repo https://jyliuu.github.io/rinky/manifest.v2.json
kpm install rinky
```

That address never changes: every release rewrites `manifest.v2.json` on the `gh-pages`
branch, and the manifest points at the `.kpkg` of the newest release, so the same two
commands install and update rinky. (The repository owner has to enable Pages once —
*Settings > Pages > Deploy from a branch > `gh-pages`* — for the address to answer.)

Two alternatives, for a repository that does not publish the manifest:

- `rinky-<version>-kpm-repo.json`, next to the `.kpkg` in the release, is the same
  manifest with a relative artifact URL. Upload both to a directory of your own (or use
  the release URL) and pass that path instead.
- With the `.kpkg` already on the Kindle and no repository at all, the SSH install below
  works unchanged — the package has the same layout.

`kpm install rinky` runs the same installer as the other packages, so the library entry
`documents/rinky.sh` is created too: start rinky from there, or with `kpm launch rinky`.
`kpm uninstall rinky` removes rinky and keeps your settings, notes and positions.

### Install over SSH

If you have SSH access to the Kindle, copy `rinky-<version>-kindlehf.tar.gz` to it and
run:

```sh
mkdir -p /mnt/us/.rinky-pkg
tar -xzf /mnt/us/rinky-<version>-kindlehf.tar.gz -C /mnt/us/.rinky-pkg
sh /mnt/us/.rinky-pkg/rinky/install.sh
rm -rf /mnt/us/.rinky-pkg /mnt/us/rinky-<version>-kindlehf.tar.gz
```

This also adds a **rinky** entry to the Kindle library (when the drive has a
`documents` folder), so you can start rinky without KUAL.

The installer (`rinky/install.sh`, in both packages) checks the package files against
their checksums first. It refuses to run while rinky runs, and one install or update
runs at a time (*"another install or update is running"*). It installs all files or
none: each file is first copied next to the old one, and the old files are replaced
only when every copy worked. If the e-reader is full it stops with *"cannot write ...
(no space left?); nothing was changed"* and the installed version stays as it was, so
it needs room for one more copy of the package. Installing over an older version keeps
your settings, notes and plugins.

### Start

Open KUAL and choose *rinky > Start rinky*, or over SSH
`sh /mnt/us/rinky/rinky-ctl.sh start`. The Kindle's own screens stop while rinky runs
and come back when you choose *Settings > Exit to Kindle*. rinky lists the books in
`Books` and `documents` on the Kindle drive.

rinky keeps its files in the `rinky` folder on the Kindle drive: settings, the
library index and its log `rinky.log`.

## 3. Kobo and Tolino

### Get the Kobo ready

A Kobo needs no jailbreak. rinky is started from
[NickelMenu](https://pgaskin.net/NickelMenu/), which adds a menu to Kobo's own
software.

1. Download `KoboRoot.tgz` from the NickelMenu page.
2. Connect the Kobo by USB. It shows up as a drive named *KOBOeReader*.
3. Copy `KoboRoot.tgz` into the folder `.kobo` on that drive. The folder is hidden:
   see [Copying files](#4-copying-files-to-the-e-reader) for how to see hidden folders.
4. Eject the Kobo. It installs NickelMenu and restarts.

NickelMenu needs Kobo firmware 4.x, which every supported Kobo runs.

### Install over USB

1. Connect the Kobo by USB.
2. Unpack `rinky-<version>-kobo.zip` **onto the Kobo drive**, not into a folder of its
   own. The zip holds one folder, `.adds`, with `.adds/rinky` (the program) and
   `.adds/nm/rinky` (its NickelMenu entry). If the drive already has an `.adds`
   folder (NickelMenu makes one), the two are merged: say yes when your computer asks.
   On a Mac in Terminal: `unzip rinky-<version>-kobo.zip -d /Volumes/KOBOeReader`.
3. Eject the Kobo.

### Install over SSH

If you have set up SSH or telnet on the Kobo, copy `rinky-<version>-kobo.tar.gz` to it
and run:

```sh
mkdir -p /tmp/rinky-pkg && cd /tmp/rinky-pkg
tar -xzf /path/to/rinky-<version>-kobo.tar.gz
sh rinky/install.sh
```

### Start

Tap the NickelMenu button (bottom right of the home screen, or in the main menu) and
choose **rinky**. Kobo's own software stops while rinky runs; *Settings > Exit to Kobo*
starts it again, which takes a few seconds. rinky lists the books anywhere on the Kobo
drive (hidden folders excepted).

rinky keeps its files in `.adds/rinky` on the Kobo drive: settings, the library index
and its log `rinky.log`. More on what differs on a Kobo: [rinky on Kobo](kobo.md).

### Tolino

The Tolinos made by Kobo (Shine 5, Shine Color, Vision Color) run Kobo's software with
firmware 5.x, which NickelMenu does not support yet. rinky runs on them, but there is
no menu entry to start it: only with SSH, after installing over SSH as above, by
running `sh /mnt/onboard/.adds/rinky/rinky.sh`.

## 4. Copying files to the e-reader

**How the e-reader shows up** when you connect it by USB:

| Device | Shows up as |
|---|---|
| Kindle Paperwhite 3 and 4, Voyage, Oasis 1 to 3 | a USB drive named *Kindle* |
| Kindle Paperwhite 5 and newer, Kindle 11th gen and newer, Scribe, Colorsoft | an MTP device, like a phone (some firmware updates switched older Kindles to MTP too) |
| Kobo | a USB drive named *KOBOeReader* (tap *Connect* on the Kobo when it asks) |

**Windows:** a USB drive appears in File Explorer under *This PC*, and so does an MTP
device. Unpack a zip with right-click > *Extract All*. To see hidden folders (Kobo:
`.kobo`, `.adds`): File Explorer > *View* > *Show* > *Hidden items*.

**macOS:** a USB drive appears in Finder. Finder does not show MTP devices: use an MTP
app such as [OpenMTP](https://openmtp.ganeshrvel.com/). Double-click a zip to unpack it.
Folders whose name starts with a dot are hidden: press
<kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>.</kbd> in Finder to show them, or copy in
Terminal (`cp -R`, `unzip -d`).

**Linux:** a USB drive is mounted by your desktop; for MTP use your file manager's MTP
support or `jmtpfs`. `unzip` unpacks, `ls -a` shows hidden folders.

Always eject the e-reader before you unplug it.

## 5. Check the download

Every release has a `SHA256SUMS` file. To check a download, compare its SHA-256 with
the line for it there: `shasum -a 256 <file>` on a Mac, `sha256sum <file>` on Linux,
`certutil -hashfile <file> SHA256` on Windows. The in-app update and the SSH installer
check the package themselves.

## 6. Update

In rinky, *Settings > Update* installs a newer version over Wi-Fi and restarts into
it, keeping your settings, notes and reading positions. It needs about 30 MB of free
space. Installing a newer package by hand as above works too; the
[user guide](user-guide.md#9-update) has the other ways.

## 7. Uninstall

Exit rinky first.

- **Kindle:** in KUAL choose *rinky > Uninstall rinky*, or over SSH
  `sh /mnt/us/rinky/uninstall.sh` (add `--keep-data` to keep settings and notes). The
  [user guide](user-guide.md#10-uninstall) lists exactly what it removes.
- **Kobo:** connect it by USB and delete the folder `.adds/rinky` and the file
  `.adds/nm/rinky`. Leave the rest of `.adds` if you use NickelMenu for anything else.

Your books are never touched. rinky's reading positions and notes are in small files
next to each book (`<book>.sdr/rinky.*.json`); delete them too if you want nothing
left.
