rinky - e-book reader for Kindle and Kobo e-readers
===================================================

This folder is rinky: the program, its installer and its manuals. The same program
runs on Kindles (jailbroken, firmware 5.x, started from KUAL) and Kobos (started from
NickelMenu); the package you downloaded is for one of them.

  install.md     which device, which package, and every install step
  user-guide.md  using rinky, updating, uninstalling, troubleshooting

Install, in short
-----------------
Kindle: copy the folders "rinky" and "extensions" from the zip to the top of the
        Kindle drive, eject, then KUAL > rinky > Start rinky.
Kobo:   unpack the zip onto the Kobo drive (it holds the hidden folder ".adds"),
        eject, then NickelMenu > rinky.
Over SSH (either):
        tar -xzf rinky-VERSION-<kindlehf|kobo>.tar.gz && sh rinky/install.sh

Exit, logs, update, uninstall
-----------------------------
Settings > Exit to Kindle (or Kobo) brings the e-reader's own screens back.
rinky.log in this folder records every start and exit with its reason; a crash writes
crash-DATE-TIME.log. Settings > Update installs a newer version over Wi-Fi.
Uninstall: see install.md, section 7.

If rinky hangs, hold the power button (about 40 seconds on a Kindle, 15 on a Kobo) to
restart the e-reader.

License: GNU AGPL 3.0 or later (LICENSE). Fonts: SIL Open Font License (LICENSE-Noto).
