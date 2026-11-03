#!/bin/bash
# Fix cycle 1 (check 12): the userstore logs cannot be written from the start (both are
# directories here, as with a read-only, full or missing /mnt/us). The launcher used to
# skip `stop lab126_gui` (its `>>"$LOG"` redirection failed) and give up after 40 s.
# Now it opens its log once as fd 3 and falls back to /tmp/rinky/rinky.log
# (tmpfs): the framework must stop, rinky must run (its output in the fallback log)
# and the framework must come back when the 25 s run ends.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"
R=/var/tmp/inkro-start

wait_home 240 home-before
clear_env
guest "rm -rf $R /tmp/rinky/rinky.log /tmp/rinky/watchdog.log
  mkdir -p $R && cp $DEST/rinky $DEST/rinky.sh $DEST/rinky-watchdog.sh $R/ &&
  mkdir $R/rinky.log $R/watchdog.log && ls -la $R" >"$OUT/prepare.txt" 2>&1 || fail "prepare $R"
log "launcher from $R with $R/rinky.log and $R/watchdog.log as directories"
guest "sh $R/rinky.sh -- device sleep --seconds 25" || fail "launch"
wait_until 90 "rinky running, KPPMainApp gone" reader_running || fail "framework not stopped / rinky did not take over"
log "framework '$(framework_state)' while rinky runs"
pass "framework stopped and rinky running although the logs cannot be written"
shot rinky-running >/dev/null
wait_until 90 "rinky exited (25 s run)" reader_gone || fail "rinky did not exit"
wait_clean_home 300 home-after
guest "cat /tmp/rinky/rinky.log" >"$OUT/fallback-rinky.log" 2>&1
guest "cat /tmp/rinky/watchdog.log" >"$OUT/fallback-watchdog.log" 2>&1
log "framework after the run: $(framework_state)"
F=$OUT/fallback-rinky.log
grep -q "cannot write $R/rinky.log; logging to /tmp/rinky/rinky.log" "$F" || fail "no fallback note in the tmpfs log"
grep -q "stop lab126_gui" "$F" || fail "launcher did not log stop lab126_gui"
grep -q "sleep: started" "$F" || fail "rinky output is not in the fallback log"
grep -q "rinky exited with 0" "$F" || fail "rinky exit status not 0"
grep -q "start lab126_gui" "$F" || fail "launcher did not start the framework"
grep -q "cannot write $R/watchdog.log" "$OUT/fallback-watchdog.log" || fail "watchdog did not fall back"
grep -q "launcher finished" "$OUT/fallback-watchdog.log" || fail "watchdog did not see the clean finish"
guest "rm -rf $R"
check_restored
pass "unwritable logs from the start: framework stopped, rinky ran, framework back; logs on tmpfs"
