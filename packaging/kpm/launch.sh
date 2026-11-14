#!/bin/sh
# KPM launch hook: `kpm launch rinky` runs this, and the library entry
# documents/rinky.sh does the same through rinky-ctl.sh.
#
# The library entry is the usual way in; this hook exists for KPM's own launcher and
# for `kpm launch rinky` over SSH.
sh /mnt/us/rinky/rinky-ctl.sh start --library
