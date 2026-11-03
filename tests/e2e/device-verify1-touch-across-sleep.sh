#!/bin/bash
# Verifier cycle 1 (checks 3, 11): a finger on the glass when the device goes to sleep
# (a hand resting on the screen while pressing power, a cover closing on a finger).
# `rinky device events` logs every event. The Down is delivered before the
# screensaver; the lift happens while asleep. After the wake the app must not be left
# with a contact that went down and never came up.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_device device events 90
log "hold 600,800 for 6 s; power after 2 s (sleep), power again after the lift (wake)"
"$SLOT" hold "$NAME" 600 800 6 >/dev/null 2>&1 &
holder=$!
sleep 2
power_key
wait "$holder"
sleep 2
power_key
wait_log 20 "awake, screen restored (PowerKey)"
sleep 2
tap 300 300
sleep 3
guest "grep -E 'event: touch|event: Power|sleep:' $DEST/rinky.log" >"$OUT/events.txt" 2>&1
downs=$(grep -c 'Down 600,800' "$OUT/events.txt")
ups=$(grep -cE '(Up|DownUp) 600,800' "$OUT/events.txt")
log "contact at 600,800: $downs down, $ups up"
wait_until 120 "events tool finished" reader_gone || fail "events did not finish"
wait_clean_home 300 home-after
save_logs
check_restored
[ "$downs" -ge 1 ] || fail "the hold never arrived as a Down"
[ "$ups" -ge 1 ] || fail "the app got Down at 600,800 and never an Up (the lift came while asleep and was dropped)"
pass "the contact held across sleep was closed with an Up"
