#!/bin/bash
# Verifier finding (wave A cycle 1), fixed. Test: crates/ink-text/tests/verify_text_attacks.rs
# corrupt_collection_header_is_skipped.
#
# A 12-byte /mnt/us/fonts/broken.ttc whose header claims 0xFFFFFFFF faces (a corrupted or
# truncated collection a user copied). The gallery scans /mnt/us/fonts at start
# (ink_text::registry::scan_fonts): it must skip the file and run. Before the fix (ARM
# release build, ui-jb 2026-10-25) scan_fonts tried every one of the 4294967295 claimed
# faces, logging one warning each: after 6 minutes it had reached face 36161, the gallery
# never drew and Home did not come back. Now face::collection_faces gives 0 for such a
# header and the file is skipped with one "corrupt collection header" warning.
#   bash tests/e2e/ui-verify-bad-ttc.sh INSTANCE OUTDIR
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

LOG=$DEST/rinky.log
# grep -c prints 0 and exits 1 when nothing matches: keep only its number.
count() { local n; n=$(guest "grep -c -- '$1' $LOG" 2>/dev/null | tail -1); echo "${n:-0}"; }
at_least() { [ "$(count "$1")" -ge "$2" ]; }

wait_home 240 home-before
printf 'ttcf\000\001\000\000\377\377\377\377' >"$OUT/broken.ttc"
guest 'mkdir -p /mnt/us/fonts' || fail "mkdir"
"$SLOT" push "$NAME" "$OUT/broken.ttc" /mnt/us/fonts/ >/dev/null || fail "push broken.ttc"
guest "ls -l /mnt/us/fonts" >"$OUT/fonts-dir.txt" 2>&1
guest "rm -f $LOG $DEST/watchdog.log; sh $DEST/rinky.sh -- demo --kindle" || fail "launch"
ok=0
if wait_until 90 "gallery layout logged" at_least "demo button toast" 1; then
  sleep 5
  [ -n "$(rinky_pid)" ] && ok=1
fi
shot gallery-or-home >/dev/null || true
save_logs
grep -E "panicked|fonts" "$OUT/rinky.log" >"$OUT/panic.txt" 2>/dev/null || true
faces=$(grep -c 'broken.ttc face' "$OUT/rinky.log" 2>/dev/null)
log "faces of broken.ttc tried: ${faces:-0}; rinky pid now: $(rinky_pid)"
guest 'rm -f /mnt/us/fonts/broken.ttc' || true
# Stop the reader in both cases (a hung scan never exits) so the launcher restores the framework.
guest 'pid=$(pidof rinky); [ -n "$pid" ] && kill $pid' || true
wait_clean_home 300 home-after
[ "$ok" = 1 ] || fail "the gallery did not start with a corrupt .ttc in /mnt/us/fonts (${faces:-0} faces tried; see rinky.log)"
pass "corrupt .ttc skipped"
