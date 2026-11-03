#!/bin/bash
# e2e-cycles: 1
# Framework-stop launcher, normal exit: Home -> rinky (framework stopped) -> page
# turn -> exit by the top-right corner -> Home again.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

wait_home 240 home-before
start_reader
case "$(framework_state)" in *stop/*) pass "lab126_gui stopped while rinky runs" ;; *) fail "framework not stopped: $(framework_state)" ;; esac
not_home rinky-page1
tap 1000 900
nap 4
not_home rinky-page2
exit_device
wait_clean_home 300 home-after
save_logs
orphans=$(guest 'ps -eo pid,args | grep "lipc-wait-event -m com.lab126.powerd" | grep -v grep | wc -l')
[ "$orphans" = 0 ] || fail "$orphans lipc-wait-event watcher(s) outlived rinky"
pass "no lipc-wait-event left behind"
grep -q "start lab126_gui" "$OUT/rinky.log" || fail "launcher did not start the framework"
grep -q "launcher finished" "$OUT/watchdog.log" || fail "watchdog did not see the clean exit"
pass "normal exit restores Home"
