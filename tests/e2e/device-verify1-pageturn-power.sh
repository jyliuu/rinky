#!/bin/bash
# Verifier cycle 1 (check 3): the power key during a page turn in the real reader
# (`rinky run`). A tap turns the page and the power key follows at once. The
# screensaver must show the new page (with the badge), and after the wake the panel
# must show that same page, not the one before the turn.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"
H="$(dirname "$0")/device-helpers"

wait_home 240 home-before
clear_env
start_reader
before=$(shot page-before)
log "tap (next page) and the power key right after it"
tap 1000 900
guest "$DEST/rinky inject power" >/dev/null || fail "inject power"
wait_log 20 "screensaver drawn (PowerKey)"
sleep 3
s=$(shot screensaver)
power_key
wait_log 20 "awake, screen restored (PowerKey)"
sleep 3
a=$(shot after-wake)
# Badge box (screensaver.rs: bottom-right, about 1040..1244 x 1590..1668).
n1=$(python3 "$H/diff-outside.py" "$s" "$a" 1030 1580 1250 1675)
n2=$(python3 "$H/diff-outside.py" "$before" "$a" 0 0 0 0)
log "screensaver vs after wake, outside the badge: $n1 px differ; page before vs after wake: $n2 px differ"
echo "screensaver-vs-after-wake (outside badge): $n1; before-turn-vs-after-wake: $n2" >"$OUT/compare.txt"
[ "$n1" = 0 ] || fail "the page after the wake is not the page under the screensaver ($n1 px)"
[ "$n2" != 0 ] || fail "the page did not turn (or the turn was lost by the sleep)"
pass "power key during a page turn: the turned page is under the screensaver and back after the wake"
exit_device
wait_clean_home 300 home-after
save_logs
check_restored
