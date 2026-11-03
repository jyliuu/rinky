#!/bin/bash
# e2e-cycles: 1
# rinky-ctl.sh restart: swaps the rinky process while lab126_gui stays stopped
# (no framework start in between), then exit returns to Home as usual.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

wait_home 240 home-before
start_reader
not_home reader-before
old=$(rinky_pid)
[ -n "$old" ] || fail "no rinky pid before the restart"
guest "echo restart-marker >>$DEST/rinky.log"
guest "sh $DEST/rinky-ctl.sh restart" || fail "rinky-ctl.sh restart failed"
new=$(rinky_pid)
[ -n "$new" ] && [ "$new" != "$old" ] || fail "rinky pid did not change ($old -> $new)"
pass "new rinky process $new (was $old)"
case "$(framework_state)" in *stop/*) pass "lab126_gui still stopped after the restart" ;; *) fail "framework state after restart: $(framework_state)" ;; esac
nap 6
not_home reader-after
save_logs
after=$(awk '/restart-marker/ { f = 1; next } f' "$OUT/rinky.log")
grep -q 'start lab126_gui' <<<"$after" && fail "the framework was started during the restart"
grep -q 'restart: running the installed rinky again' <<<"$after" || fail "launcher did not log the restart"
pass "launcher log: restart without a framework start"
exit_device
wait_clean_home 300 home-after
pass "exit after a restart restores Home"
