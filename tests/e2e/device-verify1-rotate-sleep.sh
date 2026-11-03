#!/bin/bash
# Verifier cycle 1 (checks 3, 10): rotate clockwise, then sleep and wake with the power
# key. The rotated page must come back pixel for pixel and a tap must still be remapped
# (panel 300,400 -> logical 400,971).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_device device rotate cw --seconds 60
wait_log 30 "rotate: Clockwise logical"
sleep 2
a=$(shot rotated-awake)
power_key
wait_log 20 "screensaver drawn (PowerKey)"
sleep 3
shot rotated-screensaver >/dev/null
power_key
wait_log 20 "awake, screen restored (PowerKey)"
sleep 3
b=$(shot rotated-after-wake)
python3 "$E2E_ROOT/scripts/png-compare.py" "$a" "$b" >"$OUT/compare-rotated.txt" 2>&1 ||
  fail "rotated page differs after the wake (compare-rotated.txt)"
pass "rotated page restored pixel for pixel after sleep and wake"
tap 300 400
wait_log 15 "rotate Clockwise: tap 1 panel 300,400 -> logical 400,971"
pass "tap after the wake remapped to logical 400,971"
wait_until 90 "rotate test finished" reader_gone || fail "rotate did not finish"
wait_clean_home 300 home-after
save_logs
check_no_watchers
check_restored
