#!/bin/sh
# KPM install hook, run by `kpm install rinky` from the unpacked .kpkg.
#
# The package root holds rinky/ and extensions/rinky/, so rinky's own installer (which
# expects its package one level above itself) can run unmodified: it copies rinky/ to
# /mnt/us/rinky, extensions/rinky/ to /mnt/us/extensions/rinky and creates the library
# entry documents/rinky.sh. Everything it creates is recorded in /mnt/us/rinky/
# .install-state, so KPM's uninstall hook can put the device back as it was.
set -e

echo "Installing rinky..."
sh ./rinky/install.sh
