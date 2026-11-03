#!/bin/bash
# Verifier cycle 1 (checks 3, 12): kill -9 of the launcher (not rinky) while the
# screensaver shows and the frontlight is off. The watchdog must see the launcher gone,
# stop the orphaned rinky, undo the journal (frontlight back on), start the
# framework (clean Home), and leave no lipc watcher of rinky behind.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
bl0=$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')
start_device device sleep --seconds 600
power_key
wait_log 20 "screensaver drawn (PowerKey)"
[ "$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')" = 0 ] || fail "light not off while asleep"
shot screensaver >/dev/null
guest 'cat /tmp/rinky/restore' >"$OUT/journal-asleep.txt" 2>&1
lpid=$(guest 'cat /tmp/rinky/launcher.pid')
ipid=$(rinky_pid)
[ -n "$lpid" ] && [ -n "$ipid" ] || fail "no launcher ($lpid) or rinky ($ipid) pid"
log "kill -9 launcher $lpid (rinky $ipid asleep)"
guest "kill -9 $lpid"
wait_until 60 "orphaned rinky stopped" reader_gone || fail "the watchdog did not stop the orphaned rinky $ipid"
wait_clean_home 300 home-after-kill
save_logs
grep -q "launcher $lpid is gone" "$OUT/watchdog.log" || fail "watchdog.log does not record the launcher's death"
bl=$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')
log "frontlight sysfs before $bl0, after $bl"
[ "$bl" != 0 ] || fail "frontlight still off after the watchdog's restore"
pass "kill -9 of the launcher while asleep: watchdog stopped rinky, frontlight back ($bl), framework back"
check_no_watchers
check_restored
