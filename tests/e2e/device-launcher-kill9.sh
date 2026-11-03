#!/bin/bash
# Phase 5 check 12 (kill -9 while asleep): the frontlight is off and volumd
# disableMounting is 1 when rinky is killed; the launcher replays the journal
# (powerd flIntensity re-set, disableMounting 0) and starts the framework.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
bl0=$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')
start_device device sleep --seconds 600
power_key
wait_log 20 "screensaver drawn (PowerKey)"
[ "$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')" = 0 ] || fail "light not off"
guest 'cat /tmp/rinky/restore' | tee "$OUT/journal-asleep.txt" >&2
grep -q "lipc i com.lab126.powerd flIntensity" "$OUT/journal-asleep.txt" || fail "flIntensity not journaled"
pid=$(rinky_pid)
log "kill -9 rinky $pid (asleep)"
guest "kill -9 $pid"
wait_until 30 "rinky gone" reader_gone || fail "rinky still running"
wait_clean_home 300 home-after-kill
save_logs
grep -q "rinky exited with 137" "$OUT/rinky.log" || fail "launcher did not record 137"
bl=$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')
log "frontlight sysfs before $bl0, after $bl"
[ "$bl" != 0 ] || fail "frontlight still off after the restore"
pass "kill -9 while asleep: frontlight back ($bl), framework back"
check_no_watchers
check_restored
