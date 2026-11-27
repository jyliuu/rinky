#!/bin/sh
# KPM uninstall hook, run by `kpm uninstall rinky`.
#
# Prefer rinky's own uninstaller: the installer recorded in /mnt/us/rinky/.install-state
# what it created, so it removes exactly that and leaves settings, notes and book .sdr
# folders alone unless asked otherwise (--keep-data keeps even those).
if [ -f /mnt/us/rinky/uninstall.sh ]; then
    sh /mnt/us/rinky/uninstall.sh
    exit $?
fi

# rinky's folder is gone but files may remain (a partly removed install): take out the
# pieces the package creates.
echo "Removing rinky..."
rm -f /mnt/us/documents/rinky.sh
rm -rf /mnt/us/rinky
rm -rf /mnt/us/extensions/rinky
