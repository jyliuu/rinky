#!/bin/bash
# e2e-cycles: 1
# Phase 3 reader on the emulator (framework stopped): menu (center tap), Go to panel,
# Aa panel (font size +), bookmark, search page, long-press selection. Checks the
# reader's log lines and keeps a screenshot per step.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

logged() { guest "grep -q '$1' $DEST/rinky.log" 2>/dev/null; }
expect_log() { wait_until 30 "log: $1" logged "$1" || fail "no log line '$1'"; }

wait_home 240 home-before
start_reader
expect_log "saved position"
nap 3
not_home page1
tap 1000 900; nap 3
tap 636 848;  expect_log "reader overlay Menu"; nap 3; not_home menu
tap 652 68;   expect_log "reader overlay Goto"; nap 3; not_home goto
tap 1212 68;  expect_log "reader action: close"; nap 3; not_home goto-closed
tap 636 848;  nap 3
tap 764 68;   expect_log "reader overlay Aa"; nap 3; not_home aa
# Font size + (the first slider's plus button).
guest "grep 'reader button' $DEST/rinky.log | tail -3" >"$OUT/aa-buttons.txt" 2>&1 || true
tap 1196 890; expect_log "reader action: aa-size:42"; nap 5; not_home aa-bigger
tap 1158 314; nap 3   # Done
tap 636 848;  nap 3
tap 988 68;   expect_log "reader bookmark page"; nap 3; not_home bookmarked
tap 636 848;  nap 3
tap 1100 68;  expect_log "reader overlay Search"; nap 3; not_home search
tap 1212 68;  nap 3
"$SLOT" hold "$NAME" 300 600 1.5 >/dev/null || fail "hold"
nap 3
not_home hold
save_logs
grep -E "reader (overlay|action|bookmark|selection)" "$OUT/rinky.log" | tee "$OUT/reader-lines.txt"
pass "menu, Go to, Aa, bookmark, search on the emulator"
# The first tap closes the selection popup (if the hold selected text), then exit.
exit_device
