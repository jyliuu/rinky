#!/bin/bash
# Verifier cycle 1 (check 12): the framework must come back when the userstore
# (/mnt/us, which holds rinky.log and watchdog.log) cannot be written. Seen on the
# emulator: after an eMMC write error ext4 remounted /mnt/us read-only during a run
# (device-verify1-suspend-leak, boot 2); then rinky aborted, the launcher's and
# watchdog's log lines were lost and lab126_gui stayed stop/waiting. On the device the
# same happens with an ext4 error or a USB mass-storage export (userstore unmounted).
#  1. `rinky device info` with stderr on /dev/full (ENOSPC) must not abort:
#     eprintln!/println! panic when the write fails, and the launcher sends rinky's
#     output to /mnt/us/rinky/rinky.log.
#  2. The launcher run from a copy in $R. Once rinky has taken over (framework
#     stopped), $R/rinky.log and $R/watchdog.log are replaced by directories, so
#     every later `>>"$LOG"` fails as it does on a read-only filesystem (rinky keeps
#     its already open output). busybox sh skips a command whose redirection fails, and
#     the launcher and watchdog run `rinky device restore`, the appmgrd restart and
#     `start lab126_gui` with `>>"$LOG"`. When the 30 s run ends, the stock Home must
#     come back within 180 s.
# If it does not, the scenario starts the framework by hand so the next one can run.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"
R=/var/tmp/inkro

wait_home 240 home-before
problems=""
guest "$DEST/rinky device info >/dev/null 2>/dev/full; echo \$?" >"$OUT/stderr-full.txt" 2>&1
rc=$(tail -1 "$OUT/stderr-full.txt")
log "rinky device info with stderr on /dev/full: exit $rc"
[ "$rc" = 0 ] || problems+="rinky device info aborts when stderr cannot be written (exit $rc); "

guest "rm -rf $R; mkdir -p $R && cp $DEST/rinky $DEST/rinky.sh $DEST/rinky-watchdog.sh $R/" ||
  fail "prepare $R"
log "launcher from $R: -- device sleep --seconds 30"
guest "sh $R/rinky.sh -- device sleep --seconds 30" || fail "launch"
wait_until 90 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
log "framework '$(framework_state)'; $R/rinky.log and watchdog.log made unwritable"
guest "rm -f $R/rinky.log $R/watchdog.log; mkdir $R/rinky.log $R/watchdog.log" || fail "replace the logs"
wait_until 90 "rinky exited (30 s run)" reader_gone || fail "rinky did not exit"
launcher_gone() { [ -z "$(guest 'cat /tmp/rinky/launcher.pid 2>/dev/null')" ]; }
wait_until 120 "launcher finished" launcher_gone || log "launcher still running after 120 s"
back=0
wait_until 180 "clean Home after the run" clean_home_now && back=1
shot after-run >/dev/null
st=$(framework_state)
guest "ls -la /tmp/rinky/; ps -eo pid,args | grep -E 'rinky|watchdog' | grep -v grep" >"$OUT/state.txt" 2>&1
log "framework after the run: $st"
if [ $back = 0 ]; then
  if probe && app_error_now; then
    cp "$PROBE" "$OUT/after-run-app-error.png"
    problems+="the framework came back ($st) but the stock 'Application Error' dialog stayed over Home for 180 s (after-run-app-error.png); "
  else
    problems+="no clean Home within 180 s with unwritable logs (framework: $st; after-run.png); "
  fi
  log "starting the framework by hand"
  guest "pkill -f '[i]nkreader-watchdog'; rm -rf /tmp/rinky/lock /tmp/rinky/fwstart;
    stop appmgrd; start appmgrd; sleep 3; start lab126_gui" >"$OUT/manual-restore.txt" 2>&1
  wait_home 300 home-after-manual-restore
fi
guest "rm -rf $R"
[ -z "$problems" ] || fail "$problems"
pass "unwritable logs: rinky did not abort and the framework came back"
