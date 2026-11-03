#!/bin/bash
# Phase 5 check 12 (hang): rinky stops processing input (heartbeat stops) in night
# mode. The watchdog counts 15 unchanged polls (RINKY_HANG_S=15), kills it; the
# launcher restores the settings and the framework.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
set_env RINKY_HANG_S=15
start_device device hang --after 8 --night
not_home hang-test-page
hb1=$(guest 'cat /tmp/rinky/heartbeat')
sleep 3
hb2=$(guest 'cat /tmp/rinky/heartbeat')
[ "$hb1" != "$hb2" ] && pass "heartbeat moves while rinky runs ($hb1 -> $hb2)"
wait_until 90 "hung rinky killed by the watchdog" reader_gone || fail "hung rinky not killed"
clear_env
wait_clean_home 300 home-after-hang
save_logs
grep -q "hung: heartbeat" "$OUT/watchdog.log" || fail "watchdog did not report the hang"
grep -q "rinky exited with 1[34][37]" "$OUT/rinky.log" || fail "no kill exit status"
gs=$(guest '/mnt/us/rinky/rinky device info 2>/dev/null | grep "^fb:"')
echo "$gs" | grep -q "grayscale 1" || fail "fb flag after hang: $gs"
pass "hang detected by the heartbeat, framework back, flag restored"
check_no_watchers
check_restored
