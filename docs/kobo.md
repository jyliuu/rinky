# rinky on Kobo

rinky on a Kobo is the same program as on the Kindle, so the [user guide](user-guide.md)
applies. This page lists what is different. Which Kobos rinky runs on, and how to
install and start it, is in the [install guide](install.md#3-kobo-and-tolino).

Kobo support is new: it has run in an emulator, not yet on a real Kobo.

## What is different from the Kindle

- **Folders.** rinky's folder is `.adds/rinky` on the Kobo drive, so where the user
  guide says `/mnt/us/rinky`, read `/mnt/onboard/.adds/rinky`. Dictionaries go in
  `.adds/rinky/dict/`, and fonts in `fonts` at the top of the drive.
- **Books.** rinky lists books anywhere on the Kobo drive except in hidden folders.
- **Wi-Fi.** rinky uses the networks the Kobo already knows, and a network you add in
  rinky is known to the Kobo afterwards too. Turning Wi-Fi on joins a known network by
  itself.
- **USB.** rinky does not show the Kobo as a USB drive. Exit rinky first
  (*Settings > Exit to Kobo*), then connect the cable.
- **Light.** The brightness slider goes from 0 to 100 and warmth from 0 to 10, the
  Kobo's own steps. There is no warmth schedule yet.
- **SSH and USB network.** The Kobo package has no SSH server, and the USB network
  plugin is made for the Kindle.
- **Update.** *Settings > Update* works as on the Kindle and fetches the Kobo package.
- **Uninstall.** Delete `.adds/rinky` and `.adds/nm/rinky` over USB
  ([install guide](install.md#7-uninstall)).

## Troubleshooting

- **The NickelMenu entry is missing.** Check that `.adds/nm/rinky` is on the Kobo drive
  and that NickelMenu itself works (its own menu appears).
- **Kobo's home screen does not come back after rinky.** Hold the power button for
  about 15 seconds to restart the Kobo.
- **Taps land in the wrong place, or the picture is upside down.** rinky may have the
  wrong settings for your model. Please open an issue with `.adds/rinky/rinky.log`; its
  first lines name the model rinky found. Meanwhile, a line
  `export RINKY_TOUCH=swap` (or `swap,mirror_x`, or `swap,mirror_y`) in a file
  `.adds/rinky/rinky.env` changes how taps are read.
