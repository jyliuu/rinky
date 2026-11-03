#!/bin/bash
# Verifier attacks on scripts/emu-slot.sh (p0fix verify cycle 1). Private lock directory
# and the fake emulator from scripts/e2e/fake-emu.sh: no real emulator, no network.
#
#   V1 `down A` removes the slot directory it found at its start, after QEMU is gone,
#      without checking under the lock that the directory still belongs to A. If A's
#      lock was cleaned up and another instance B acquired the same slot meanwhile,
#      `down A` deletes B's lock. The window (QEMU exit -> rm) is held open here with
#      SIGSTOP on the `down` process; on a real host it is about 1-2 s (the 1 s
#      kill -0 poll plus ps), so it needs a `cleanup` and an `acquire` by other agents
#      inside it.
#   V2 three `up` of one name at the same instant (instance exists): one QEMU, one
#      slot, every caller exits 0, qemu.pid names the live QEMU.
#   V3 `restart` killed with -9 after its old QEMU is gone and before it boots the new
#      one (held there with a QMP quit that hangs): cleanup drops the lock (dead
#      qemu.pid, dead up.pid) and no QEMU is left.
#   V4 `down` killed with -9 after QEMU is gone: cleanup drops the lock; a new `up`
#      of the same name then works.
#   V5 an `acquire` frozen (SIGSTOP) while it holds the flock: other callers wait, give
#      up after 60 s with an error, and leave no lock behind.
#
#   scripts/e2e/verify-slot-races.sh      PASS/FAIL lines; exit status = failures
# V1 and V2 failed at fb10e9a (verifier findings). Fixed in emu-slot.sh: `down` and a
# failed `up` remove a slot only under the slot lock while NAME still owns it, and
# up/restart of one name are serialized by $SLOTS/.up-NAME.lock. All five run always.
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SLOTSH="$ROOT/scripts/emu-slot.sh"
TMP=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/verify-slot.XXXXXX")" && pwd -P)
. "$ROOT/scripts/e2e/fake-emu.sh"
fake_emu_setup "$TMP"
export EMU_SLOT_PORT_BASE=47310      # away from emu-slot-test.sh (47110) and run-test.sh
FAKE=$EMU
fails=0
ok() { echo "PASS $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }
slots_of() { grep -l "^$1\$" "$EMU_SLOTS"/slot*/owner 2>/dev/null | wc -l | tr -d ' '; }
slot_of() { local f; f=$(grep -l "^$1\$" "$EMU_SLOTS"/slot*/owner 2>/dev/null | head -1); [ -n "$f" ] && basename "$(dirname "$f")"; }
qpid() { local s; s=$(slot_of "$1") && cat "$EMU_SLOTS/$s/qemu.pid" 2>/dev/null; }
alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }
fakes_of() { ps -axo pid=,command= | awk -v n="$1" '$0 ~ /qemu-system-fake/ { for (i = 2; i < NF; i++) if ($i == "-name" && $(i + 1) == n) print $1 }'; }
waitfor() { local t=$(( $(date +%s) + $1 )); shift; until "$@"; do [ "$(date +%s)" -lt "$t" ] || return 1; sleep 0.2; done; }
release_all() { local d; for d in "$EMU_SLOTS"/slot*; do [ -d "$d" ] && rm -rf "$d"; done; pkill -f "$TMP/qemu-system-fake" 2>/dev/null; sleep 0.3; return 0; }

# ------------------------------------------------------------------ V1
"$SLOTSH" up v1-a >"$TMP/v1-up" 2>&1 || bad "V1 setup: up v1-a failed: $(tail -2 "$TMP/v1-up")"
s_a=$(slot_of v1-a); pa=$(qpid v1-a)
touch "$FAKE_DIR/hang-qmp"                       # QMP quit does nothing: down waits on kill -0
EMU_QUIT_WAIT_S=30 "$SLOTSH" down v1-a >"$TMP/v1-down" 2>&1 & dpid=$!
sleep 1.5
kill -STOP "$dpid"                               # down is inside its kill -0 / sleep 1 loop
kill -9 "$pa"; sleep 0.3                         # QEMU goes away (as after a QMP quit)
rm -f "$FAKE_DIR/hang-qmp"
"$SLOTSH" cleanup >"$TMP/v1-cleanup" 2>&1        # another agent: v1-a's QEMU is gone -> lock removed
s_b=$("$SLOTSH" acquire v1-b 2>"$TMP/v1-acq")    # another agent takes the free slot
kill -CONT "$dpid"; wait "$dpid"; drc=$?
if [ "$s_b" != "${s_a#slot}" ]; then
  bad "V1 setup: v1-b got slot '$s_b', not v1-a's ${s_a#slot} ($(cat "$TMP/v1-cleanup"))"
elif [ "$(slots_of v1-b)" = 1 ]; then
  ok "V1 down v1-a (exit $drc) left v1-b's lock in slot $s_b alone"
else
  bad "V1 down v1-a (exit $drc) removed slot $s_b, which v1-b had acquired meanwhile ($(tail -1 "$TMP/v1-down"))"
fi
grep -h 'emu-slot:' "$TMP/v1-down" | sed 's/^/     down v1-a: /'
release_all

# ------------------------------------------------------------------ V2
mkdir -p "$FAKE/machines/v2-x"; echo '{}' >"$FAKE/machines/v2-x/machine.json"
for i in 1 2 3; do ( "$SLOTSH" up v2-x >"$TMP/v2-up$i" 2>&1; echo "rc=$?" >>"$TMP/v2-up$i" ) & done
wait
nok=$(cat "$TMP"/v2-up* | grep -c '^rc=0$')
live=$(fakes_of v2-x | wc -l | tr -d ' ')
q=$(qpid v2-x)
if [ "$nok" = 3 ] && [ "$live" = 1 ] && [ "$(slots_of v2-x)" = 1 ] && alive "$q" && [ "$(fakes_of v2-x)" = "$q" ]; then
  ok "V2 3 simultaneous up of one name: all exit 0, one QEMU ($q), one slot"
else
  bad "V2 3 simultaneous up of one name: $nok exit 0, $live fake QEMU(s) [$(fakes_of v2-x | tr '\n' ' ')], $(slots_of v2-x) slot(s), qemu.pid '$q' alive=$(alive "$q" && echo yes || echo no); $(grep -h 'emu-slot:' "$TMP"/v2-up* | sort | uniq -c | tr '\n' ';')"
fi
grep -h 'emu-slot:' "$TMP"/v2-up* | sort | uniq -c | sed 's/^/     up v2-x: /'
"$SLOTSH" down v2-x >/dev/null 2>&1
release_all

# ------------------------------------------------------------------ V3
"$SLOTSH" up v3-r >/dev/null 2>&1
p0=$(qpid v3-r)
touch "$FAKE_DIR/hang-qmp"                       # restart waits in stop_qemu's kill -0 loop
EMU_QUIT_WAIT_S=30 "$SLOTSH" restart v3-r >"$TMP/v3-rs" 2>&1 & rpid=$!
sleep 1.5
kill -9 "$p0"; sleep 0.3                         # the old QEMU is gone ...
kill -9 "$rpid" 2>/dev/null; wait "$rpid" 2>/dev/null   # ... and restart dies before booting
rm -f "$FAKE_DIR/hang-qmp"
left=$(fakes_of v3-r | tr '\n' ' ')
st3=$("$SLOTSH" status 2>&1 | grep v3-r)
"$SLOTSH" cleanup >"$TMP/v3-cl" 2>&1
[ -z "$left" ] && [ "$(slots_of v3-r)" = 0 ] \
  && ok "V3 restart killed between stop and boot: no QEMU left, cleanup drops the lock (status before: $st3)" \
  || bad "V3 after a killed restart: QEMU(s) '$left', $(slots_of v3-r) lock(s) after cleanup ($(cat "$TMP/v3-cl"))"
release_all

# ------------------------------------------------------------------ V4
"$SLOTSH" up v4-d >/dev/null 2>&1
p4=$(qpid v4-d)
touch "$FAKE_DIR/hang-qmp"
EMU_QUIT_WAIT_S=30 "$SLOTSH" down v4-d >/dev/null 2>&1 & dpid=$!
sleep 1.5
kill -9 "$p4"; kill -9 "$dpid" 2>/dev/null; wait "$dpid" 2>/dev/null
rm -f "$FAKE_DIR/hang-qmp"
"$SLOTSH" cleanup >/dev/null 2>&1
c1=$(slots_of v4-d)
"$SLOTSH" up v4-d >"$TMP/v4-up" 2>&1; rc=$?
p5=$(qpid v4-d)
[ "$c1" = 0 ] && [ $rc = 0 ] && alive "$p5" && ok "V4 down killed after QEMU exit: cleanup drops the lock, up works again (QEMU $p5)" \
  || bad "V4 after a killed down: locks after cleanup $c1, up rc=$rc $(tail -1 "$TMP/v4-up")"
"$SLOTSH" down v4-d >/dev/null 2>&1
release_all

# ------------------------------------------------------------------ V5
# A holder of the flock that never finishes: hold fd 9 with an flock from perl, frozen.
mkdir -p "$EMU_SLOTS"
perl -MFcntl=:flock -e 'open(my $f, ">>", $ARGV[0]) or die; flock($f, LOCK_EX) or die; kill "STOP", $$; sleep 600' "$EMU_SLOTS/.lock" & holder=$!
sleep 0.5
t0=$(date +%s)
"$SLOTSH" acquire v5-w >"$TMP/v5-acq" 2>&1; rc=$?
dt=$(( $(date +%s) - t0 ))
kill -9 "$holder" 2>/dev/null; wait "$holder" 2>/dev/null
[ $rc != 0 ] && [ "$dt" -ge 59 ] && [ "$dt" -le 75 ] && [ "$(slots_of v5-w)" = 0 ] && grep -q "busy for 60 s" "$TMP/v5-acq" \
  && ok "V5 acquire behind a frozen flock holder gives up after ${dt}s with an error, no lock" \
  || bad "V5 frozen holder: rc=$rc after ${dt}s, $(slots_of v5-w) lock(s): $(tail -2 "$TMP/v5-acq")"
"$SLOTSH" acquire v5-w >/dev/null 2>&1 && [ "$(slots_of v5-w)" = 1 ] && ok "V5 acquire works again once the holder is gone" \
  || bad "V5 acquire after the holder died"
release_all

fake_emu_teardown "$TMP"
rm -rf "$TMP"
echo "verify-slot-races: $fails failed"
exit "$fails"
