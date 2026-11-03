#!/bin/bash
# e2e-cycles: 4
# Attacks on the framework-stop launcher and watchdog on the guest (busybox sh, real
# upstart):
#   A. second launcher 1 s after the first (first still waiting for the framework to
#      stop): refused, one rinky; exit restores a clean Home;
#   B. two launchers started at the same instant: at most one rinky may run (the
#      lock window is shown deterministically by scripts/e2e/launcher-race-test.sh 3b);
#      a clean Home afterwards;
#   C. two rounds of kill -9 at a random point 0-12 s after the launch, of the launcher
#      or of rinky (chosen at random; ATTACK_SEED fixes the choice): the framework
#      comes back to a clean Home and no rinky is left.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
set -o pipefail

RANDOM=${ATTACK_SEED:-$$}
inks() { guest 'pidof rinky | wc -w' 2>/dev/null | tr -d ' '; }
watchers() { guest 'ps -eo pid,args | grep "lipc-wait-event -m com.lab126.powerd" | grep -v grep | wc -l' 2>/dev/null | tr -d ' '; }
# Every exit tap uses another x: a new rinky process decodes a touch at exactly the
# point of the previous touch as (0,0), because the kernel drops ABS values that did not
# change and the decoder starts without the current values.
exits=0
exit_reader() {
  exits=$((exits + 1))
  tap $((1150 + 20 * exits)) 60
  wait_until 30 "rinky exited" reader_gone || fail "rinky did not exit after the exit tap ($(inks) left)"
}
after_round() {   # LABEL: clean Home, nothing left behind
  wait_clean_home 300 "$1-home"
  [ "$(inks)" = 0 ] || fail "$1: $(inks) rinky process(es) left after Home came back"
  [ "$(watchers)" = 0 ] || fail "$1: lipc-wait-event watcher(s) left"
  guest "cat $DEST/rinky.log" >"$OUT/$1-rinky.log" 2>&1
  guest "cat $DEST/watchdog.log" >"$OUT/$1-watchdog.log" 2>&1
  guest 'ls -la /tmp/rinky/ 2>&1' >"$OUT/$1-run-dir.txt"
}

wait_home 240 home-before

# A. second launcher 1 s later
log "A: launcher twice, 1 s apart"
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; rm -rf $DEST/juliet.sdr; sh $DEST/rinky.sh $DEST/juliet.epub; sleep 1; sh $DEST/rinky.sh $DEST/juliet.epub" || fail "launch"
wait_until 90 "rinky running, KPPMainApp gone" reader_running || fail "A: rinky did not take over"
nap 10
n=$(inks)
guest "grep -E 'already running|stale lock' $DEST/rinky.log" | tee "$OUT/A-lock-lines.txt"
[ "$n" = 1 ] || fail "A: $n rinkys run after two launches 1 s apart"
grep -q "already running" "$OUT/A-lock-lines.txt" || fail "A: second launcher did not log 'already running'"
not_home A-one-reader
pass "A: second launcher refused, one rinky"
exit_reader
after_round A

# B. two launchers at the same instant
log "B: two launchers at the same instant"
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sh $DEST/rinky.sh $DEST/juliet.epub & sh $DEST/rinky.sh $DEST/juliet.epub & wait" || fail "launch"
wait_until 90 "rinky running, KPPMainApp gone" reader_running || fail "B: rinky did not take over"
nap 10
n=$(inks)
guest "grep -E 'already running|stale lock|start:' $DEST/rinky.log" | tee "$OUT/B-lock-lines.txt"
guest 'ps -eo pid,args | grep -E "rinky|watchdog" | grep -v grep' >"$OUT/B-ps.txt" 2>&1
log "B: $n rinky process(es) after two simultaneous launches"
not_home B-reader
exit_reader
if [ "$n" != 1 ]; then
  # Both rinkys exit on the same tap; let both launchers finish before failing.
  wait_clean_home 300 B-home
  fail "B: $n rinkys ran after two simultaneous launches (lock race; B-lock-lines.txt)"
fi
pass "B: simultaneous launchers -> one rinky"
after_round B

# C. kill -9 at random points
for r in 1 2; do
  d=$(( RANDOM % 13 ))
  who=$([ $((RANDOM % 2)) = 0 ] && echo launcher || echo rinky)
  log "C$r: kill -9 $who ${d}s after the launch"
  guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sh $DEST/rinky.sh $DEST/juliet.epub" || fail "launch"
  sleep "$d"
  case $who in
    launcher) p=$(guest 'cat /tmp/rinky/launcher.pid 2>/dev/null') ;;
    rinky) p=$(guest 'pidof rinky' 2>/dev/null) ;;
  esac
  if [ -n "$p" ]; then
    guest "kill -9 $p" && log "C$r: killed $who $p"
    echo "kill -9 $who $p after ${d}s" >"$OUT/C$r-kill.txt"
  else
    # Nothing to kill yet: the run continues; end it with the exit tap.
    log "C$r: no $who process yet after ${d}s; normal exit instead"
    echo "no $who process after ${d}s; normal exit" >"$OUT/C$r-kill.txt"
    wait_until 90 "rinky running" reader_running || fail "C$r: rinky did not take over"
    nap 4
    exit_reader
  fi
  after_round "C$r"
  pass "C$r: framework back after kill -9 $who at ${d}s"
done
save_logs
pass "launcher attacks"
