#!/bin/bash
# Verifier cycle 1 (checks 3, 4): RINKY_SUSPEND_RETRY=1 (the smallest value the
# setting accepts) while suspend is refused (the emulator's USB wakeup source; on the
# device any active wakeup source, e.g. a USB cable). Each refused attempt blocks the
# input loop for 2 s (wakeup_count) and the next attempt is already due when it
# returns, so the loop goes from attempt to attempt and never polls the power key or
# touch. The power key must still wake the device.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
set_env RINKY_SUSPEND_RETRY=1
start_device device sleep --seconds 600
power_key
wait_log 20 "suspend refused"
sleep 10
log "power key to wake (attempts so far: $(guest "grep -c '[0-9] sleep: suspend refused' $DEST/rinky.log"))"
power_key
woke=0
wait_until 30 "awake again" in_log "awake, screen restored (PowerKey)" && woke=1
presses=$(guest "grep -c '^power key pressed' $DEST/rinky.log")
log "power presses read by rinky: $presses of 2"
shot after-second-press >/dev/null
if [ $woke = 0 ]; then
  save_logs
  # Leave the screensaver loop: stop rinky; the launcher restores the framework.
  guest 'kill $(cat /tmp/rinky/rinky.pid)'
  clear_env
  wait_clean_home 300 home-after
  fail "the power key never woke the device with a 1 s retry ($presses of 2 presses read)"
fi
pass "power key wakes the device with RINKY_SUSPEND_RETRY=1"
exit_device
clear_env
wait_clean_home 300 home-after
save_logs
check_restored
