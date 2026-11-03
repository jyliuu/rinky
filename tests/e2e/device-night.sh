#!/bin/bash
# Phase 5 check 10 (night mode, screenshot): `rinky device night test` sets the
# hardware grayscale flag (FBIOPUT_VSCREENINFO grayscale 2), refreshes with the MTK
# night waveforms (GCK16 = 8 for flashes, GLKW16 = 9 as FULL for partial updates),
# writes PNG screenshots (what the panel shows, and the raw buffer), and restores day.
# The QMP screendump shows whether the emulator models the inversion.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_device device night test --out /tmp/rinky/night --hold 20
wait_log 30 "night partial: waveform (9, 1)"
shot night-scanout >/dev/null
grep_ok() { in_log "$1" || fail "missing: $1"; }
grep_ok "night full: grayscale 2 hw true waveform (8, 1)"
grep_ok "night DU: waveform (1, 0)"
pass "grayscale flag 2 and night waveforms GCK16/GLKW16"
for f in day night-visible night-raw; do
  "$SLOT" pull "$NAME" "/tmp/rinky/night/$f.png" "$OUT/rinky-screenshot-$f.png" || fail "pull $f.png"
done
pass "rinky screenshots saved (rinky-screenshot-*.png)"
wait_until 60 "night test finished" reader_gone || fail "night test did not finish"
grep_ok "day again: grayscale 1"
wait_clean_home 300 home-after
gs=$(guest '/mnt/us/rinky/rinky device info 2>/dev/null | grep "^fb:"')
log "after: $gs"
echo "$gs" | grep -q "grayscale 1" || fail "grayscale flag not restored: $gs"
save_logs
check_restored
