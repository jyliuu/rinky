#!/bin/bash
# e2e-cycles: 1
# Wave B verifier: the merged library + reader under abuse. Library mode, rapid taps on
# a library row, rapid menu open/close in the reader, then the power key (sleep + wake)
# while the reader menu, the Aa panel and the search keyboard are open. After each wake
# rinky must still run (same pid, no panic), the overlay must still be usable and
# leaving through menu Back + library Exit must bring the clean Kindle Home back.
# Coordinates: 1272 x 1696 portrait (wave-b-merge-smoke.sh, reader-menu-smoke.sh).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

B=/mnt/us/Books/inkverify-e2e
expect_log() { wait_until 30 "log: $1" in_log "$1" || fail "no log line '$1'"; }
count() { local c; c=$(guest "grep -c -- '$1' $DEST/rinky.log; true" 2>/dev/null | tr -dc 0-9); echo "${c:-0}"; }
more() { [ "$(count "$1")" -gt "$2" ]; }
alive() {
  local p; p=$(rinky_pid)
  [ -n "$p" ] || fail "$1: rinky is not running"
  [ "$p" = "$PID" ] || fail "$1: rinky pid changed $PID -> $p (restarted)"
  guest "grep -q panicked $DEST/rinky.log" && fail "$1: panic in rinky.log"
  log "$1: rinky $p alive"
}
# sleep_wake LABEL: power key, screensaver, power key, awake; N = count before.
sleep_wake() {
  local n1 n2; n1=$(count "screensaver drawn (PowerKey)"); n2=$(count "awake, screen restored (PowerKey)")
  power_key
  wait_until 30 "screensaver $1" more "screensaver drawn (PowerKey)" $n1 || fail "no screensaver after power ($1)"
  nap 3; shot "$1-asleep" >/dev/null
  power_key
  wait_until 30 "wake $1" more "awake, screen restored (PowerKey)" $n2 || fail "no wake after power ($1)"
  nap 3
  alive "$1"
  not_home "$1-awake"
}

wait_home 300 home-before
clear_env
guest "rm -rf $B $DEST/library.json $DEST/library-view.json; mkdir -p $B" || fail "prepare $B"
"$SLOT" push "$NAME" "$E2E_ROOT/testdata/juliet.epub" "$B/" >/dev/null || fail "push book"
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sh $DEST/rinky.sh --library" || fail "launch"
wait_until 120 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
wait_until 120 "library scan logged" in_log "library scan:" || fail "no 'library scan:' line"
nap 8
PID=$(rinky_pid)
not_home library-home
tap 477 1631; nap 5
tap 177 210; nap 3
tap 300 $((256 + 8 + 110 + 55)); nap 4
guest "test -e /mnt/us/documents/JAILBROKEN.txt" || fail "expected /mnt/us/documents/JAILBROKEN.txt (row order)"
# 1. Rapid taps on the book row: 4 taps with no wait.
for i in 1 2 3 4; do tap 600 $((270 + 150 + 75)); done
expect_log "saved position"
nap 6
alive rapid-row
not_home after-rapid-row
guest "grep -E 'screen:|saved position|cannot open' $DEST/rinky.log" >"$OUT/rapid-row-lines.txt" 2>&1
# 2. Rapid menu open/close: 9 center taps (odd: menu ends open).
for i in 1 2 3 4 5 6 7 8 9; do tap 636 848; done
nap 5
alive rapid-menu
not_home after-rapid-menu
# 3. Power with the menu open (open it if the rapid taps left it closed).
m=$(count "reader overlay Menu"); c=$(count "reader action: close")
log "menu opened $m times, closed $c times"
[ "$m" -gt "$c" ] || { tap 636 848; expect_log "reader overlay Menu"; nap 3; }
sleep_wake menu
# 4. Aa panel (from the menu that should still be open) + power.
tap 764 124; nap 4
in_log "reader overlay Aa" || { tap 636 848; nap 3; tap 764 124; expect_log "reader overlay Aa"; nap 3; }
sleep_wake aa
tap 1196 1076; expect_log "reader action: aa-size"; nap 5
alive aa-after-wake
tap 1158 724; nap 3
# 5. Search + keyboard + power.
tap 636 848; expect_log "reader overlay Menu"; nap 3
tap 1100 124; expect_log "reader overlay Search"; nap 3
tap 300 1300; nap 2
tap 400 1300; nap 2
not_home search-typed
sleep_wake search
tap 600 1300; nap 3
alive search-after-wake
not_home search-typed-after-wake
tap 1212 68; nap 3
# 6. Leave: menu Back -> library, Exit -> Kindle Home.
tap 636 848; expect_log "reader overlay Menu"; nap 3
tap 60 124; nap 6
alive back-to-library
not_home library-after-reader
tap 1212 75
wait_until 60 "rinky gone after Exit" reader_gone || fail "rinky still runs after Exit"
wait_clean_home 300 home-after
save_logs
grep -q "rinky exited with 0" "$OUT/rinky.log" || fail "exit status not 0 ($(grep 'exited with' "$OUT/rinky.log"))"
guest "rm -rf $B" || true
pass "rapid taps, rapid menu, power with menu / Aa / search keyboard open, exit to Kindle"
