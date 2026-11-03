#!/bin/bash
# Phase 5 check 12 (crash): rinky aborts (exit 134) while the hardware night flag is
# set; the launcher replays the restore journal (grayscale back to 1, fast mode off,
# volumd disableMounting back) and starts the framework: clean, not inverted Home.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_device device crash --after 8 --night
not_home crash-test-page-night
guest 'cat /tmp/rinky/restore' | tee "$OUT/journal-before-crash.txt" >&2
grep -q "fb grayscale 1" "$OUT/journal-before-crash.txt" || fail "night flag change not journaled"
wait_until 60 "rinky crashed" reader_gone || fail "rinky still running"
wait_clean_home 300 home-after-crash
save_logs
grep -q "rinky exited with 134" "$OUT/rinky.log" || fail "no abort exit status"
grep -q "restore fb: grayscale -> 1" "$OUT/rinky.log" || fail "grayscale not restored"
gs=$(guest '/mnt/us/rinky/rinky device info 2>/dev/null | grep "^fb:"')
echo "$gs" | grep -q "grayscale 1" || fail "fb flag after crash: $gs"
pass "crash in night mode: framework back, flag restored ($gs)"
check_no_watchers
check_restored
