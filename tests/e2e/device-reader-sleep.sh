#!/bin/bash
# Phase 5 check 3 with the real reader (`rinky run`): power key -> screensaver
# shows the current page with the "asleep" badge, power key -> the page is back (the
# reader's own drawing while asleep goes to the shadow buffer), page turns still work.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_reader
tap 1000 900
sleep 4
p2=$(shot page2)
power_key
wait_log 20 "screensaver drawn (PowerKey)"
sleep 3
s=$(shot screensaver-current-page)
wait_log 20 "suspend refused"
power_key
wait_log 20 "awake, screen restored (PowerKey)"
sleep 3
a=$(shot page2-after-wake)
python3 "$E2E_ROOT/scripts/png-compare.py" "$p2" "$a" >"$OUT/compare-page2.txt" 2>&1 && pass "page restored pixel for pixel after the wake" ||
  log "page after wake differs from before (see compare-page2.txt)"
tap 1000 900
sleep 4
not_home page3
exit_device
wait_clean_home 300 home-after
save_logs
grep -E "GoingToScreenSaver|OutOfScreenSaver|gesture" "$OUT/rinky.log" | head -20 >&2
check_restored
