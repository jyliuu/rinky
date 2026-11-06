# rinky on Kobo

rinky runs on several Kobo models with a 300 ppi screen. It is the same program as on
the Kindle; only the package around it differs. This page covers what is different on
a Kobo. The [user guide](user-guide.md) describes rinky itself.

Kobo support is new. It has been tested in an emulator only, not on a real Kobo. If
something goes wrong, see [Troubleshooting](#troubleshooting).

## Models

| Kobo | rinky |
|---|---|
| Libra 2 | ran in the emulator with Kobo's firmware 4.38: books, taps, page keys, light, warmth, night mode, exit to the Kobo home screen |
| Tolino Shine (5th gen), the Kobo Clara BW inside | ran in the emulator with the official firmware 5.12, Wi-Fi and the in-app update too |
| Clara BW, Clara Colour, Libra Colour, Tolino Shine Color, Tolino Vision Color | untested; same display interface as the Shine 5 |
| Clara HD, Clara 2E, Forma, Libra H2O | untested; same display interface as the Libra 2 |
| Elipsa, Sage | not supported yet: another display interface (sunxi) |
| Glo HD, Aura ONE, Aura H2O², older Kobos | not supported: older display interfaces or a screen with too few pixels |

On the Colour models rinky shows grayscale. On the Libra 2, Libra Colour, Forma and
Libra H2O the page keys turn pages (tried on the Libra 2 in the emulator). Closing a
sleep cover should put rinky to sleep; that could not be tried in the emulator.

## Install

You do not need to jailbreak a Kobo. rinky is started from
[NickelMenu](https://pgaskin.net/NickelMenu/), a menu entry added to Kobo's own
software.

1. **Install NickelMenu** if you have not already. Download `KoboRoot.tgz` from its
   page, connect the Kobo by USB, copy the file into the hidden `.kobo` folder on the
   Kobo drive, eject the Kobo and wait while it restarts.
2. **Download** `rinky-<version>-kobo.zip` from the project's releases page.
3. **Unpack it onto the Kobo drive.** The zip holds one folder, `.adds`, with rinky and
   its menu entry inside. Copy that folder to the top level of the Kobo drive; if the
   drive already has an `.adds` folder (NickelMenu makes one), merge the two. On a Mac
   folders whose name starts with a dot are hidden: in Finder, press
   <kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>.</kbd> to show them, or unpack in Terminal
   with `unzip rinky-<version>-kobo.zip -d /Volumes/KOBOeReader`.
4. **Eject the Kobo.** The NickelMenu button (bottom right of the home screen, or in
   the main menu) now has an entry **rinky**.

Over SSH or telnet (if you have set that up): copy `rinky-<version>-kobo.tar.gz` to the
Kobo, unpack it in `/tmp` and run `sh rinky/install.sh`.

rinky keeps its files in `.adds/rinky` on the Kobo drive: settings, the library index,
reading positions and its log `rinky.log`. Books can be anywhere on the drive; the
library looks through all of it (hidden folders excepted).

## What is different from the Kindle

- **Starting and exiting.** Tap NickelMenu > rinky. rinky stops Kobo's own software
  while it runs. Settings > Exit to Kobo closes rinky and starts Kobo's home screen
  again; it takes a few seconds.
- **Wi-Fi.** rinky uses the networks the Kobo already knows, and a network you add in
  rinky is known to the Kobo afterwards too. Turning Wi-Fi on joins a known network by
  itself.
- **USB.** rinky does not show the Kobo as a USB drive. Exit rinky first, then
  connect the cable.
- **Light.** The brightness slider goes from 0 to 100 and warmth from 0 to 10, the
  Kobo's own steps. There is no warmth schedule yet.
- **Night mode** is done by the display controller on the i.MX models (Libra 2, Clara
  HD and 2E, Forma, Libra H2O) and by inverting the picture on the others.
- **Update.** Settings > Update works as on the Kindle and fetches the Kobo package.

## Uninstall

Exit rinky, connect the Kobo by USB and delete `.adds/rinky` and the file
`.adds/nm/rinky`. Leave the rest of `.adds` alone if you use NickelMenu for anything
else.

## Troubleshooting

- **The NickelMenu entry is missing.** Check that `.adds/nm/rinky` is on the Kobo drive
  and that NickelMenu itself works (its own menu appears).
- **Kobo's home screen does not come back after rinky.** Hold the power button for
  about 15 seconds to restart the Kobo.
- **Taps land in the wrong place, or the picture is upside down.** The model table may
  be wrong for your Kobo. Please open an issue with `.adds/rinky/rinky.log`; its first
  lines name the model rinky found. Meanwhile `RINKY_TOUCH=swap`, `swap,mirror_x` or
  `swap,mirror_y` in a file `.adds/rinky/rinky.env` (as `export RINKY_TOUCH=...`) changes
  how taps are read.
- **Tolinos** run Kobo firmware 5.x, which NickelMenu does not support yet. rinky runs on
  them, but there is no menu entry to start it from.
