#!/bin/bash
# e2e-cycles: 3
# Verifier (p0fix cycle 1): kill -9 at exact points of the launcher's start and exit
# paths on the guest (busybox sh, real upstart). The arch scenarios kill the launcher
# while it waits for the framework to stop and kill a running rinky; these rounds
# hit the other windows. A guest-side poller (20 ms) waits for a launcher log line or a
# process and kills at once:
#   R1 exit tap, then kill -9 the launcher when it logs "restart appmgrd" (inside
#      its cleanup, while appmgrd is being restarted);
#   R2 exit tap, then kill -9 the launcher when it logs "start lab126_gui";
#   R3 kill -9 rinky as soon as its process exists (startup, before it draws).
# After each round: clean Home (no Application Error dialog) within 300 s, no
# rinky, no lipc-wait-event watcher, launcher/watchdog logs saved.
# P0FIX_ROUNDS (default "R1 R2 R3") selects rounds.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
set -o pipefail

inks() { guest 'pidof rinky | wc -w' 2>/dev/null | tr -d ' '; }
watchers() { guest 'ps -eo pid,args | grep "lipc-wait-event -m com.lab126.powerd" | grep -v grep | wc -l' 2>/dev/null | tr -d ' '; }
LOGF=$DEST/rinky.log

# killer WHAT: starts a detached guest poller that kill -9s the launcher once LOGF holds
# the line WHAT (R1/R2), or rinky once it exists (WHAT=rinky). It writes
# /mnt/us/p0fix-killer.txt.
killer() {
  local body
  if [ "$1" = rinky ]; then
    body='i=0; while [ $i -lt 3000 ]; do p=$(pidof rinky); [ -n "$p" ] && break; usleep 20000; i=$((i+1)); done
          [ -n "$p" ] && kill -9 $p && echo "killed rinky $p after $i polls ($(date +%T))"'
  else
    body='i=0; while [ $i -lt 3000 ]; do grep -q "'"$1"'" '"$LOGF"' 2>/dev/null && break; usleep 20000; i=$((i+1)); done
          p=$(cat /tmp/rinky/launcher.pid 2>/dev/null)
          [ -n "$p" ] && kill -9 $p && echo "killed launcher $p at \"'"$1"'\" after $i polls ($(date +%T))"'
  fi
  guest "rm -f /mnt/us/p0fix-killer.txt; (setsid sh -c '$body' >/mnt/us/p0fix-killer.txt 2>&1 </dev/null &) ; true" \
    || fail "start the killer"
}

round_end() {   # LABEL
  wait_clean_home 300 "$1-home"
  guest 'cat /mnt/us/p0fix-killer.txt' >"$OUT/$1-killer.txt" 2>&1
  guest "cat $DEST/rinky.log" >"$OUT/$1-rinky.log" 2>&1
  guest "cat $DEST/watchdog.log" >"$OUT/$1-watchdog.log" 2>&1
  guest 'ls -la /tmp/rinky/ 2>&1; status lab126_gui; status appmgrd' >"$OUT/$1-state.txt" 2>&1
  grep -q "^killed " "$OUT/$1-killer.txt" || fail "$1: the poller killed nothing ($(cat "$OUT/$1-killer.txt"))"
  [ "$(inks)" = 0 ] || fail "$1: $(inks) rinky process(es) left after Home came back"
  [ "$(watchers)" = 0 ] || fail "$1: lipc-wait-event watcher(s) left"
  log "$1: $(cat "$OUT/$1-killer.txt")"
}

wait_home 240 home-before

# R1 / R2: kill the launcher inside its exit path. The exit taps use x/y values no
# other scenario uses (a repeated coordinate decodes as 0 in a new process, D1).
ROUNDS=${P0FIX_ROUNDS:-R1 R2 R3}
r=0
for line in "restart appmgrd" "start lab126_gui"; do
  r=$((r + 1))
  case " $ROUNDS " in *" R$r "*) ;; *) continue ;; esac
  log "R$r: exit tap, kill -9 the launcher at \"$line\""
  start_reader
  not_home "R$r-reader"
  killer "$line"
  tap $((1089 + 22 * r)) $((69 + 14 * r))
  wait_until 60 "rinky exited" reader_gone || fail "R$r: rinky did not exit after the exit tap"
  round_end "R$r"
  pass "R$r: launcher killed at \"$line\": clean Home, nothing left"
done

# R3: kill rinky at startup.
case " $ROUNDS " in *" R3 "*)
log "R3: kill -9 rinky as soon as it exists"
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; rm -rf $DEST/juliet.sdr"
killer rinky
guest "sh $DEST/rinky.sh $DEST/juliet.epub" || fail "launch"
# Home is still on the screen while the launcher stops the framework: wait for the kill
# and for the launcher to finish before checking Home.
killed() { guest 'grep -q "^killed " /mnt/us/p0fix-killer.txt' 2>/dev/null; }
launcher_done() { guest 'test -f /tmp/rinky/done' 2>/dev/null; }
wait_until 120 "rinky killed at startup" killed || fail "R3: rinky was not killed within 120 s"
wait_until 120 "launcher finished" launcher_done || log "R3: /tmp/rinky/done missing after 120 s"
round_end R3
grep -q "rinky exited with 137" "$OUT/R3-rinky.log" || log "R3: launcher log has no 'exited with 137' (see R3-rinky.log)"
pass "R3: rinky killed at startup: clean Home, nothing left"
;; esac
save_logs
pass "kill -9 in the launcher's exit path and at rinky startup"
