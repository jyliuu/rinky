#!/bin/bash
# e2e-cycles: 1
# Framework-stop launcher: kill -9 of rinky. The launcher's wait returns and it
# starts the framework; the watchdog sees the clean finish.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

wait_home 240 home-before
start_reader
not_home rinky-running
pid=$(rinky_pid)
log "kill -9 rinky $pid"
guest "kill -9 $pid"
wait_until 30 "rinky gone" reader_gone || fail "rinky still running"
wait_clean_home 300 home-after-kill
save_logs
orphans=$(guest 'ps -eo pid,args | grep "lipc-wait-event -m com.lab126.powerd" | grep -v grep | wc -l')
[ "$orphans" = 0 ] || fail "$orphans lipc-wait-event watcher(s) outlived rinky"
pass "no lipc-wait-event left behind"
grep -q "rinky exited with 137" "$OUT/rinky.log" || fail "launcher did not record status 137"
pass "kill -9 rinky restores Home"
