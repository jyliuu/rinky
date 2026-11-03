rinky - e-book reader for the Kindle Paperwhite 12th gen (firmware 5.19.x)
=============================================================================

Needs a jailbroken Kindle. The full manual is user-guide.md in this folder.

Install (pick one)
------------------
A. Over USB, no shell needed:
   1. Unpack this archive on your computer.
   2. Connect the Kindle by USB and copy the folder "rinky" to the top of the
      Kindle drive. If you use KUAL, also copy "extensions/rinky" into the
      Kindle's "extensions" folder.
   3. Eject the Kindle.

B. Over SSH (installs both folders and the library entry, and records what it created,
   for a clean uninstall). Unpack on the userstore: the Kindle's /tmp is a small RAM
   disk that is often full.
      mkdir -p /mnt/us/.rinky-pkg
      tar -xzf /mnt/us/rinky-VERSION-kindlehf.tar.gz -C /mnt/us/.rinky-pkg
      sh /mnt/us/.rinky-pkg/rinky/install.sh
      rm -rf /mnt/us/.rinky-pkg /mnt/us/rinky-VERSION-kindlehf.tar.gz

Start
-----
Open "rinky" in the Kindle library (the entry documents/rinky.sh that the
installer creates), or KUAL > rinky > Start rinky, or over SSH:
      sh /mnt/us/rinky/rinky-ctl.sh start
"Starting rinky..." shows while the Kindle's screens stop (10-20 seconds).

The Kindle's own screens stop while rinky runs and come back when it exits.

Exit to Kindle
--------------
Inside rinky: tap the top-right corner of the screen (or press the power button).
If rinky crashes, the watchdog restores the Kindle within about 15 seconds.
If rinky hangs (KUAL is not reachable while rinky runs), over SSH:
      sh /mnt/us/rinky/rinky-ctl.sh stop
or restart the Kindle: hold the power button for about 40 seconds.

Logs and crashes
----------------
/mnt/us/rinky/rinky.log (and .log.1, .log.2) records every start, screen change
and exit with its reason ("exit: reason=..."). A crash writes
/mnt/us/rinky/crash-DATE-TIME.log. Settings > Diagnostics shows the log, lists the
crash logs and writes a debug report folder to send with a bug report.

Update
------
KUAL > rinky > Update rinky (Wi-Fi on), or over SSH:
      /mnt/us/rinky/rinky update install

Uninstall
---------
KUAL > rinky > Uninstall rinky, or over SSH:
      sh /mnt/us/rinky/uninstall.sh              (everything, including notes)
      sh /mnt/us/rinky/uninstall.sh --keep-data  (program only)

License: GNU AGPL 3.0 or later (LICENSE). Fonts: SIL Open Font License (LICENSE-Noto).
