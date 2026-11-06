# rinky on Kobo

rinky runs on several Kobo models with a 300 ppi screen; it is the same program as on
the Kindle. This page lists what is different on a Kobo. Which Kobos, and how to
install and start rinky: [install guide](install.md#3-kobo-and-tolino). Using rinky:
[user guide](user-guide.md).

Kobo support is new: it has run in an emulator, not yet on a real Kobo.

## What is different from the Kindle

- **Wi-Fi.** rinky uses the networks the Kobo already knows, and a network you add in
  rinky is known to the Kobo afterwards too. Turning Wi-Fi on joins a known network by
  itself.
- **USB.** rinky does not show the Kobo as a USB drive. Exit rinky first, then
  connect the cable.
- **Light.** The brightness slider goes from 0 to 100 and warmth from 0 to 10, the
  Kobo's own steps. There is no warmth schedule yet.
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
