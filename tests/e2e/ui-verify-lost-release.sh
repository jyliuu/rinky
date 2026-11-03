#!/bin/bash
# Verifier finding (wave A cycle 1), fixed. Test: crates/ink-ui/tests/verify_ui_attacks.rs
# lost_release_of_a_second_finger_does_not_disable_taps.
#
# Two fingers go down (raw protocol-B records written into the fts_ts node); only slot 0
# lifts: slot 1's release is lost (as with SYN_DROPPED on a busy reader). Then three
# ordinary QMP taps on the Toast button must each give "demo action: toast". Before the
# fix the recogniser paired every new finger with the stale slot-1 contact, so no tap was
# recognised again; now a contact left over from a finished pair never pairs again.
#   bash tests/e2e/ui-verify-lost-release.sh INSTANCE OUTDIR
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
HERE=$(cd "$(dirname "$0")" && pwd)

LOG=$DEST/rinky.log
pos() { guest "grep 'demo $1 $2 panel' $LOG | tail -1" 2>/dev/null | awk '{print $(NF-1), $NF}'; }
tapb() { local p; p=$(pos button "$1"); [ -n "$p" ] || fail "no button $1 in the log"; tap $p; }
# grep -c prints 0 and exits 1 when nothing matches: keep only its number.
count() { local n; n=$(guest "grep -c -- '$1' $LOG" 2>/dev/null | tail -1); echo "${n:-0}"; }
at_least() { [ "$(count "$1")" -ge "$2" ]; }
mt() {
  local dir
  dir=$(mktemp -d "$OUT/.mt.XXXXXX") || fail "mktemp"
  python3 "$HERE/ui-verify-mt.py" "$dir" "$@" || fail "ui-verify-mt $*"
  guest 'rm -rf /tmp/ui-mt; mkdir -p /tmp/ui-mt' || fail "mkdir"
  "$SLOT" push "$NAME" "$dir"/*.bin /tmp/ui-mt/ >/dev/null || fail "push mt"
  guest 'ev=$(grep -A4 "Name=\"fts_ts\"" /proc/bus/input/devices | sed -n "s/.*\(event[0-9]*\).*/\1/p" | head -1)
         for f in /tmp/ui-mt/*.bin; do cat $f > /dev/input/$ev; usleep 40000; done' || fail "write mt"
}

wait_home 240 home-before
guest "rm -f $LOG $DEST/watchdog.log; sh $DEST/rinky.sh -- demo --kindle" || fail "launch"
wait_until 90 "gallery running, KPPMainApp gone" reader_running || fail "rinky did not take over"
wait_until 30 "layout logged" at_least "demo button toast" 1 || fail "no layout"
sleep 5
# Control: a plain tap works.
tapb toast
wait_until 20 "control tap" at_least "demo action: toast" 1 || fail "control tap not seen"
# Two fingers on an empty area of the Controls tab; slot 1's release is lost.
mt ghost 300 1500 900 1500
sleep 3
for i in 1 2 3; do tapb toast; sleep 3; done
n=$(count "demo action: toast")
log "toast actions after the lost release: $(( n - 1 )) of 3"
not_home after-lost-release
# Clean up: the missing release, then exit.
mt lift1
sleep 2
ok=0
[ "$n" -ge 4 ] && ok=1
tapb exit
wait_until 30 "gallery exited" reader_gone || guest 'pid=$(cat /tmp/rinky/rinky.pid 2>/dev/null); [ -n "$pid" ] && kill "$pid"'
save_logs
wait_clean_home 300 home-after
[ "$ok" = 1 ] || fail "taps after a lost release of slot 1: $(( n - 1 )) of 3 recognised"
pass "taps keep working after a lost release"
