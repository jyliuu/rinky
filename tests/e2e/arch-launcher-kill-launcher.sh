#!/bin/bash
# e2e-cycles: 1
# Framework-stop launcher: kill -9 of the launcher shell. Only the watchdog is left; it
# must stop the orphaned rinky and start the framework.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

wait_home 240 home-before
start_reader
not_home rinky-running
lpid=$(guest 'cat /tmp/rinky/launcher.pid')
[ -n "$lpid" ] || fail "no launcher pid"
log "kill -9 launcher $lpid"
guest "kill -9 $lpid"
wait_until 30 "rinky stopped by the watchdog" reader_gone || fail "orphaned rinky still running"
wait_clean_home 300 home-after-kill-launcher
save_logs
orphans=$(guest 'ps -eo pid,args | grep "lipc-wait-event -m com.lab126.powerd" | grep -v grep | wc -l')
[ "$orphans" = 0 ] || fail "$orphans lipc-wait-event watcher(s) outlived rinky"
pass "no lipc-wait-event left behind"
grep -q "launcher $lpid is gone" "$OUT/watchdog.log" || fail "watchdog did not notice the launcher"
grep -q "start lab126_gui" "$OUT/watchdog.log" || fail "watchdog did not start the framework"
pass "kill -9 launcher restores Home through the watchdog"
