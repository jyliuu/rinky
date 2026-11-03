#!/bin/bash
# Tests scripts/emu-slot.sh against a private lock directory and a fake emulator
# (never the real lock directory, never a real emulator, never a network host):
#   - concurrent acquires (8 callers + 4 cleanups at once; 4 callers for one name);
#   - ports, release, refusal of the user's instance;
#   - stale locks: dead QEMU pid, half-written lock, old lock, lock of a running `up`,
#     lock whose qemu.pid is wrong while the owner's QEMU runs;
#   - up / restart / down with a fake `eink` (starts a perl process named
#     qemu-system-fake -name NAME), a fake eink-qmp.py and a fake `ssh` on PATH:
#     instance already running, QEMU killed with -9, QEMU running outside its slot,
#     slot port taken by another program, QMP quit ignored, SIGTERM ignored.
#   scripts/e2e/slot-test.sh            prints PASS/FAIL lines; exit status = failures
# (scripts/emu-slot-test.sh is main's original 6-check test of the lock logic; it still
# passes against this emu-slot.sh. scripts/e2e/verify-slot-races.sh adds the race cases.)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SLOTSH="$ROOT/scripts/emu-slot.sh"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/slot-test.XXXXXX")
export EMU_SLOTS="$TMP/slots"
mkdir -p "$EMU_SLOTS"
fails=0
ok() { echo "PASS $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }
st() { "$SLOTSH" status 2>&1; }   # capture first: grep -q + pipefail would see SIGPIPE
slots_of() { grep -l "^$1\$" "$EMU_SLOTS"/slot*/owner 2>/dev/null | wc -l | tr -d ' '; }
release_all() { for d in "$EMU_SLOTS"/slot*; do [ -d "$d" ] && rm -rf "$d"; done; return 0; }

# ---------------------------------------------------------------- lock logic only
# 1. Eight concurrent acquires while four cleanups run: exactly four win, one slot each,
#    and no cleanup removes a lock that an acquire is still writing.
for i in 1 2 3 4 5 6 7 8; do
  ( "$SLOTSH" acquire "t$i" >"$TMP/acq$i" 2>&1; echo "rc=$?" >>"$TMP/acq$i" ) &
done
for i in 1 2 3 4; do ( "$SLOTSH" cleanup >"$TMP/cln$i" 2>&1 ) & done
wait
got=$(cat "$TMP"/acq* | grep -E '^[0-3]$' | sort | tr '\n' ' ')
nok=$(cat "$TMP"/acq* | grep -c 'rc=0')
nfail=$(cat "$TMP"/acq* | grep -c 'no free slot')
partial=$(for d in "$EMU_SLOTS"/slot*; do [ -d "$d" ] && [ ! -f "$d/owner" ] && echo "$d"; done | wc -l | tr -d ' ')
[ "$got" = "0 1 2 3 " ] && [ "$nok" = 4 ] && [ "$nfail" = 4 ] && [ "$partial" = 0 ] \
  && ok "8 concurrent acquires + 4 cleanups -> slots {$got}, 4 refused, no half-written lock" \
  || bad "concurrent acquires: slots '$got', $nok ok, $nfail refused, $partial half-written"
release_all

# 2. Four concurrent acquires of ONE name: one slot, every caller gets the same number.
for i in 1 2 3 4; do ( "$SLOTSH" acquire same-x >"$TMP/same$i" 2>&1 ) & done
wait
nums=$(cat "$TMP"/same* | grep -E '^[0-3]$' | sort -u | wc -l | tr -d ' ')
[ "$(slots_of same-x)" = 1 ] && [ "$nums" = 1 ] \
  && ok "4 concurrent acquires of one name -> 1 slot, same number for all" \
  || bad "same-name race: $(slots_of same-x) slots, $nums distinct numbers"
release_all

# 3. Two concurrent acquires of the last free slot: exactly one wins.
for n in a b c; do "$SLOTSH" acquire "fill-$n" >/dev/null; done
( "$SLOTSH" acquire race-x >"$TMP/rx" 2>&1 ) & ( "$SLOTSH" acquire race-y >"$TMP/ry" 2>&1 ) & wait
wins=$(cat "$TMP/rx" "$TMP/ry" | grep -cE '^[0-3]$')
[ "$wins" = 1 ] && ok "2 concurrent acquires of 1 free slot -> 1 winner" || bad "race for the last slot: $wins winners"
"$SLOTSH" release race-x >/dev/null 2>&1; "$SLOTSH" release race-y >/dev/null 2>&1

# 4. Re-acquire by the same name returns its slot; ports follow the slot.
slot=$("$SLOTSH" acquire fill-a)
again=$("$SLOTSH" acquire fill-a)
port=$("$SLOTSH" port fill-a)
[ "$slot" = "$again" ] && [ "$port" = $((4110 + 10 * slot)) ] \
  && ok "re-acquire keeps slot $slot, ssh port $port" || bad "re-acquire/port: $slot $again $port"
"$SLOTSH" acquire fill-d >/dev/null
st | grep -q "4140/4141" && ok "status lists ports 4110+10N/4111+10N" || bad "status ports"
"$SLOTSH" release fill-d >/dev/null 2>&1

# 5. Release frees the slot; releasing an unknown name is harmless.
"$SLOTSH" release fill-b >/dev/null 2>&1
[[ "$(st)" == *fill-b* ]] && bad "release left fill-b" || ok "release frees the slot"
"$SLOTSH" release nobody >/dev/null 2>&1 && ok "release of an unknown name is a no-op" || bad "release unknown"

# 6. cleanup: dead QEMU pid goes; fresh lock stays; old lock goes; a lock directory
#    whose acquire has not written `acquired` yet stays; a lock with a live up.pid stays.
d=$(dirname "$(grep -l fill-c "$EMU_SLOTS"/slot*/owner | head -1)")
echo 999999 >"$d/qemu.pid"
"$SLOTSH" cleanup >/dev/null 2>&1
[ ! -d "$d" ] && ok "cleanup removes a lock whose QEMU is gone" || bad "stale qemu lock kept"
"$SLOTSH" cleanup >/dev/null 2>&1
[[ "$(st)" == *fill-a* ]] && ok "cleanup keeps a fresh lock without QEMU" || bad "fresh lock removed"
EMU_SLOT_GRACE_S=0 "$SLOTSH" cleanup >/dev/null 2>&1
[[ "$(st)" == *fill-a* ]] && bad "old lock kept" || ok "cleanup removes a lock older than the grace time"
mkdir "$EMU_SLOTS/slot2"            # an acquire between its mkdir and its first write
EMU_SLOT_GRACE_S=60 "$SLOTSH" cleanup >/dev/null 2>&1
[ -d "$EMU_SLOTS/slot2" ] && ok "cleanup keeps a just-created lock without files (mtime age)" \
  || bad "cleanup removed a lock that an acquire was still writing"
rmdir "$EMU_SLOTS/slot2"
"$SLOTSH" acquire busy-up >/dev/null
d=$(dirname "$(grep -l busy-up "$EMU_SLOTS"/slot*/owner)")
bash -c 'exec -a "/bin/bash scripts/emu-slot.sh up busy-up" sleep 30' & fakeup=$!
sleep 0.3
echo "$fakeup" >"$d/up.pid"
EMU_SLOT_GRACE_S=0 "$SLOTSH" cleanup >/dev/null 2>&1
[ -d "$d" ] && ok "cleanup keeps a lock while its up/restart runs (up.pid)" || bad "lock of a running up removed"
st | grep busy-up | grep -q starting && ok "status shows 'starting' for that slot" || bad "status: $(st | grep busy-up)"
kill "$fakeup" 2>/dev/null; wait "$fakeup" 2>/dev/null
EMU_SLOT_GRACE_S=0 "$SLOTSH" cleanup >/dev/null 2>&1
[ ! -d "$d" ] && ok "cleanup removes it once that up is gone (killed up)" || bad "lock of a killed up kept"
release_all

# 7. The user's instance is refused everywhere.
refused=1
for c in "acquire" "up" "down" "restart" "release" "ssh true" "deploy" "push x /tmp" "pull /x /tmp/x" \
         "shot /tmp/x.png" "tap 1 1" "swipe 1 1 2 2" "hold 1 1 1" "power" "wifi" "log" "port"; do
  set -- $c
  out=$("$SLOTSH" "$1" pw12-5195-jb "${@:2}" 2>&1); rc=$?
  { [ $rc -ne 0 ] && echo "$out" | grep -q "refusing to touch pw12-5195-jb"; } || { bad "$c pw12-5195-jb not refused: $(echo "$out" | head -2)"; refused=0; }
done
[ $refused = 1 ] && ok "every subcommand refuses pw12-5195-jb"

# ---------------------------------------------------------------- fake emulator
# Ports 47110+ (nothing should listen there); `ssh` on PATH is a stub, so no connection
# is made anywhere (scripts/e2e/fake-emu.sh).
. "$ROOT/scripts/e2e/fake-emu.sh"
fake_emu_setup "$TMP"
FAKE=$EMU
qpid() { cat "$(dirname "$(grep -l "^$1\$" "$EMU_SLOTS"/slot*/owner)")/qemu.pid" 2>/dev/null; }
alive() { kill -0 "$1" 2>/dev/null; }
slotfile() { cat "$(dirname "$(grep -l "^$1\$" "$EMU_SLOTS"/slot*/owner)")/$2" 2>/dev/null; }

# 8. up from nothing (creates the instance), records qemu.pid, boot_s and fw_cycles.
"$SLOTSH" up fake-a >"$TMP/up-a" 2>&1; rc=$?
p1=$(qpid fake-a)
[ $rc = 0 ] && [ -n "$p1" ] && alive "$p1" && [ -f "$FAKE/machines/fake-a/machine.json" ] \
  && [ -n "$(slotfile fake-a boot_s)" ] && [ "$(slotfile fake-a fw_cycles)" = 0 ] && [ -z "$(slotfile fake-a up.pid)" ] \
  && ok "up creates and boots fake-a (QEMU $p1, boot_s $(slotfile fake-a boot_s), fw_cycles 0)" \
  || bad "up fake-a: rc=$rc pid=$p1 $(tail -2 "$TMP/up-a")"

# 9. up of an instance that is already running: same slot, same QEMU.
s1=$(tail -1 "$TMP/up-a")
"$SLOTSH" up fake-a >"$TMP/up-a2" 2>&1; rc=$?
[ $rc = 0 ] && [ "$(tail -1 "$TMP/up-a2")" = "$s1" ] && [ "$(qpid fake-a)" = "$p1" ] && grep -q "already running" "$TMP/up-a2" \
  && ok "up of a running instance reuses slot $s1 and QEMU $p1" || bad "second up: rc=$rc $(cat "$TMP/up-a2")"

# 10. release while QEMU runs is refused.
"$SLOTSH" release fake-a >"$TMP/rel" 2>&1 && bad "release with running QEMU succeeded" \
  || { grep -q "still running" "$TMP/rel" && ok "release refuses while QEMU runs"; }

# 11. QEMU killed with -9: status says held, up boots it again in the same slot.
kill -9 "$p1"; sleep 0.5
st | grep fake-a | grep -q held && ok "status shows 'held' after kill -9 of QEMU" || bad "status after kill: $(st | grep fake-a)"
"$SLOTSH" up fake-a >"$TMP/up-a3" 2>&1; rc=$?
p2=$(qpid fake-a)
[ $rc = 0 ] && [ "$(tail -1 "$TMP/up-a3")" = "$s1" ] && [ "$p2" != "$p1" ] && alive "$p2" \
  && ok "up after kill -9 restarts QEMU in slot $s1 ($p1 -> $p2)" || bad "up after kill: rc=$rc $(tail -2 "$TMP/up-a3")"

# 12. restart keeps the lock (same slot, same acquired time) and boots a new QEMU.
acq=$(slotfile fake-a acquired)
"$SLOTSH" restart fake-a >"$TMP/rs" 2>&1; rc=$?
p3=$(qpid fake-a)
[ $rc = 0 ] && [ "$(slotfile fake-a acquired)" = "$acq" ] && [ "$p3" != "$p2" ] && alive "$p3" && ! alive "$p2" \
  && [ "$(slotfile fake-a fw_cycles)" = 0 ] \
  && ok "restart: slot kept, QEMU $p2 -> $p3, fw_cycles reset" || bad "restart: rc=$rc $(tail -3 "$TMP/rs")"

# 13. Killed QEMU + cleanup: the stale lock goes, nothing is killed.
kill -9 "$p3"; sleep 0.5
"$SLOTSH" cleanup >/dev/null 2>&1
[ "$(slots_of fake-a)" = 0 ] && ok "cleanup drops the lock of a killed QEMU" || bad "lock kept after QEMU kill"

# 14. QEMU of NAME running outside any slot: up refuses and keeps no slot; down stops it.
(cd "$FAKE" && nohup ./eink run fake-b --headless >/dev/null 2>&1 </dev/null &)
alive_b() { ps -axo command= | grep -q "[q]emu-system-fake -name fake-b "; }
sleep 1
"$SLOTSH" up fake-b >"$TMP/up-b" 2>&1; rc=$?
[ $rc != 0 ] && grep -q "outside this slot" "$TMP/up-b" && [ "$(slots_of fake-b)" = 0 ] \
  && ok "up refuses a QEMU running outside its slot and gives the slot back" || bad "outside QEMU: rc=$rc slots=$(slots_of fake-b) $(tail -2 "$TMP/up-b")"
ob=$(cat "$FAKE/machines/fake-b/fake.pid")
"$SLOTSH" down fake-b >"$TMP/down-b" 2>&1
[ -n "$ob" ] && ! alive "$ob" && ! alive_b && grep -q "without a matching slot record" "$TMP/down-b" && ok "down stops a QEMU that has no slot lock" || bad "down left QEMU $ob running: $(cat "$TMP/down-b")"

# 15. A slot port already in use by another program: up refuses before starting QEMU.
python3 -c 'import socket, sys, time
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(1); time.sleep(30)' 47110 & lst=$!
sleep 1
"$SLOTSH" up fake-c >"$TMP/up-c" 2>&1; rc=$?
[ $rc != 0 ] && grep -q "already in use" "$TMP/up-c" && [ "$(slots_of fake-c)" = 0 ] && [ ! -f "$FAKE/machines/fake-c/fake.pid" ] \
  && ok "up refuses a slot whose port is taken, starts no QEMU, keeps no slot" || bad "busy port: rc=$rc $(tail -2 "$TMP/up-c")"
kill "$lst" 2>/dev/null; wait "$lst" 2>/dev/null

# 16. down when QMP quit does nothing: SIGTERM ends the QEMU; slot released.
"$SLOTSH" up fake-d >/dev/null 2>&1
pd=$(qpid fake-d)
touch "$TMP/hang-qmp"
"$SLOTSH" down fake-d >"$TMP/down-d" 2>&1; rc=$?
[ $rc = 0 ] && ! alive "$pd" && [ "$(slots_of fake-d)" = 0 ] && grep -q SIGTERM "$TMP/down-d" \
  && ok "down escalates to SIGTERM when QMP quit hangs" || bad "hung QMP: rc=$rc $(cat "$TMP/down-d")"

# 17. ...and to SIGKILL when the QEMU also ignores SIGTERM.
"$SLOTSH" up fake-e >/dev/null 2>&1
pe=$(qpid fake-e)
touch "$TMP/ignore-term"
"$SLOTSH" restart fake-e >/dev/null 2>&1   # the restarted fake QEMU ignores SIGTERM
pe=$(qpid fake-e)
"$SLOTSH" down fake-e >"$TMP/down-e" 2>&1; rc=$?
[ $rc = 0 ] && ! alive "$pe" && [ "$(slots_of fake-e)" = 0 ] && grep -q SIGKILL "$TMP/down-e" \
  && ok "down escalates to SIGKILL when SIGTERM is ignored" || bad "TERM ignored: rc=$rc $(cat "$TMP/down-e")"
rm -f "$TMP/hang-qmp" "$TMP/ignore-term"

# 18. up killed while it creates the instance: the lock stays only while that up lives.
FAKE_CREATE_S=30 "$SLOTSH" up fake-f >/dev/null 2>&1 & upf=$!
sleep 2
[ "$(slots_of fake-f)" = 1 ] && EMU_SLOT_GRACE_S=0 "$SLOTSH" cleanup >/dev/null 2>&1 && [ "$(slots_of fake-f)" = 1 ] \
  && ok "cleanup keeps the slot of an up that is creating its instance" || bad "slot of a working up removed"
kill -9 "$upf" 2>/dev/null; wait "$upf" 2>/dev/null
pkill -f "eink create fake-f" 2>/dev/null
EMU_SLOT_GRACE_S=0 "$SLOTSH" cleanup >/dev/null 2>&1
[ "$(slots_of fake-f)" = 0 ] && ok "cleanup drops the slot once that up was killed" || bad "slot of a killed up kept"

# 19. Four `up` at once for four names + one more: four boot, the fifth is refused and
#     leaves no lock behind.
for n in g h i j k; do ( "$SLOTSH" up "fake-$n" >"$TMP/up-$n" 2>&1; echo "rc=$?" >>"$TMP/up-$n" ) & done
wait
nup=$(cat "$TMP"/up-[g-k] | grep -c 'rc=0'); nref=$(cat "$TMP"/up-[g-k] | grep -c 'no free slot')
nslots=$(ls -d "$EMU_SLOTS"/slot* 2>/dev/null | wc -l | tr -d ' ')
[ "$nup" = 4 ] && [ "$nref" = 1 ] && [ "$nslots" = 4 ] \
  && ok "5 concurrent ups -> 4 booted, 1 refused, 4 locks" || bad "concurrent ups: $nup up, $nref refused, $nslots locks"
for n in g h i j k; do "$SLOTSH" down "fake-$n" >/dev/null 2>&1; done
left=$(ps -axo command= | grep -c "[q]emu-system-fake -name fake-")
[ "$left" = 0 ] && [ -z "$(ls -d "$EMU_SLOTS"/slot* 2>/dev/null)" ] && ok "down of all leaves no fake QEMU and no lock" \
  || bad "$left fake QEMU(s) / locks left: $(ls "$EMU_SLOTS")"

fake_emu_teardown "$TMP"
rm -rf "$TMP"
echo "slot-test: $fails failed"
exit "$fails"
