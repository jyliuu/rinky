#!/bin/bash
# Fix cycle 1 (check 4): Power::defer_suspend in the running input loop. `device sleep
# --defer 30` defers suspend for 30 s from the start. The power key 5 s in draws the
# screensaver at once; the first suspend attempt (normally 2 s after the screensaver)
# must wait until the deferral ends, about 30 s after the start.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

secs() { echo "$1" | awk -F'[:.]' '{print $1*3600+$2*60+$3}'; }
wait_home 240 home-before
clear_env
start_device device sleep --seconds 120 --defer 30
power_key
wait_log 20 "[0-9] sleep: screensaver drawn (PowerKey)"
sleep 8
in_log "suspend requested" && fail "suspend requested while deferred"
log "no suspend attempt 8 s after the screensaver (deferred)"
wait_log 40 "[0-9] sleep: suspend requested"
guest "grep -E 'defer|sleep: screensaver drawn|sleep: suspend' $DEST/rinky.log" >"$OUT/defer.txt" 2>&1
t_defer=$(secs "$(grep -m1 'sleep: defer_suspend' "$OUT/defer.txt" | awk '{print $1}')")
t_saver=$(secs "$(grep -m1 '[0-9] sleep: screensaver drawn' "$OUT/defer.txt" | awk '{print $1}')")
t_susp=$(secs "$(grep -m1 '[0-9] sleep: suspend requested' "$OUT/defer.txt" | awk '{print $1}')")
log "defer at $t_defer, screensaver at $t_saver, first suspend attempt at $t_susp (s of day)"
echo "defer->suspend $((t_susp - t_defer)) s; screensaver->suspend $((t_susp - t_saver)) s" >>"$OUT/defer.txt"
[ $((t_susp - t_defer)) -ge 29 ] || fail "first suspend attempt $((t_susp - t_defer)) s after the defer (expected >= 29)"
pass "suspend deferred: first attempt $((t_susp - t_defer)) s after defer_suspend(30 s), $((t_susp - t_saver)) s after the screensaver"
power_key
wait_log 20 "[0-9] sleep: awake, screen restored (PowerKey)"
sleep 2
exit_device
wait_clean_home 300 home-after
save_logs
check_restored
