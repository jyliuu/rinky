# rinky on Kobo

rinky runs on several Kobo models with a 300 ppi screen. It is the same program as on
the Kindle; only the package around it differs. This page covers what is different on
a Kobo. The [install guide](install.md) covers installing, the
[user guide](user-guide.md) rinky itself.

Kobo support is new. It has been tested in an emulator only, not on a real Kobo. If
something goes wrong, see [Troubleshooting](#troubleshooting).

Which Kobos rinky runs on and how to install it: [install guide](install.md#3-kobo-and-tolino).

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
