#!/bin/bash
# Verifier (wave A cycle 1): rapid taps on the gallery with the framework stopped.
# - 20 taps per second (50 ms contacts, no gap) on the Toast button: every tap is logged
#   as an action (at least 18 of 20: the emulator's touch model may merge contacts).
# - 20 taps per second cycling the tab buttons, then a normal tap: the gallery answers.
# - Four rotate taps 50 ms apart at the rotate button's portrait panel position: no
#   crash; the gallery keeps answering taps afterwards (positions from the log).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
HERE=$(cd "$(dirname "$0")" && pwd)

LOG=$DEST/rinky.log
pos() { guest "grep 'demo $1 $2 panel' $LOG | tail -1" 2>/dev/null | awk '{print $(NF-1), $NF}'; }
tapb() { local p; p=$(pos button "$1"); [ -n "$p" ] || fail "no button $1 in the log"; tap $p; }
# grep -c prints 0 and exits 1 when nothing matches: keep only its number.
count() { local n; n=$(guest "grep -c -- '$1' $LOG" 2>/dev/null | tail -1); echo "${n:-0}"; }
at_least() { [ "$(count "$1")" -ge "$2" ]; }
expect() { wait_until 30 "'$1' x$2" at_least "$1" "$2" || fail "expected $2 log lines with '$1', got $(count "$1")"; }
qmpx() { python3 "$HERE/ui-qmp.py" --machine "$NAME" "$@" >/dev/null || fail "ui-qmp $*"; }

wait_home 240 home-before
guest "rm -f $LOG $DEST/watchdog.log; sh $DEST/rinky.sh -- demo --kindle" || fail "launch"
wait_until 90 "gallery running, KPPMainApp gone" reader_running || fail "rinky did not take over"
expect "demo button rotate" 1
sleep 5

# 20 taps per second on Toast.
set -- $(pos button toast)
coords=""
for i in $(seq 1 20); do coords="$coords $1 $2"; done
qmpx taps $coords --gap 0
sleep 8
n=$(count "demo action: toast")
log "toast taps logged: $n of 20"
[ "$n" -ge 18 ] || fail "only $n of 20 rapid taps reached the Toast button"
not_home after-rapid-toasts
pass "20 taps per second on a button: $n actions"

# 20 taps per second across the tabs, ending on Lists (tab:1).
coords=""
for t in 2 3 4 0 1 2 3 4 0 1; do coords="$coords $(pos button tab:$t)"; done
qmpx taps $coords --gap 0
sleep 6
guest "grep 'demo action: tab' $LOG" >"$OUT/tabs.log"
last=$(tail -1 "$OUT/tabs.log" | sed 's/.*demo action: //')
log "last tab action: $last ($(wc -l <"$OUT/tabs.log" | tr -d ' ') tab actions)"
not_home after-rapid-tabs
# Back to Controls (tab:2 first, so tab:0 is never the selected tab when tapped).
before=$(count "demo action: tab:0")
tapb tab:2; sleep 3
tapb tab:0; expect "demo action: tab:0" $(( before + 1 ))
sleep 3

# Four rapid taps at the portrait rotate position (each rotation moves the button).
set -- $(pos button rotate)
qmpx taps $1 $2 $1 $2 $1 $2 $1 $2 --gap 0
sleep 10
r=$(count "demo action: rotate")
log "rotate actions after 4 rapid taps: $r"
[ -n "$(rinky_pid)" ] || fail "rinky died during rapid rotation"
not_home after-rapid-rotate
# Whatever the orientation now, the logged rotate position is current: rotate until
# portrait again, then the toast button must still answer.
for i in 1 2 3 4; do
  guest "grep 'demo layout' $LOG | tail -1" | grep -q "Portrait" && break
  tapb rotate; sleep 4
done
guest "grep 'demo layout' $LOG | tail -1" | grep -q "Portrait" || fail "could not get back to portrait"
before=$(count "demo action: toast")
tapb toast
expect "demo action: toast" $(( before + 1 ))
not_home portrait-again
pass "rapid rotate taps: no crash, taps still map"

tapb exit
wait_until 30 "gallery exited" reader_gone || fail "gallery did not exit"
save_logs
grep -c "gesture: " "$OUT/rinky.log" >"$OUT/gesture-count.txt" || true
wait_clean_home 300 home-after
pass "rapid taps"
