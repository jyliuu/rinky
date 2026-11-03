#!/bin/bash
# Tests the slot logic of scripts/emu-slot.sh against a private lock directory
# (never the real one, never an emulator): concurrent acquires, ports, release,
# stale-lock cleanup and the refusal of the user's instance.
#   scripts/emu-slot-test.sh            prints PASS/FAIL lines; exit status = failures
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SLOTSH="$ROOT/scripts/emu-slot.sh"
export EMU_SLOTS
EMU_SLOTS=$(mktemp -d "${TMPDIR:-/tmp}/emu-slot-test.XXXXXX")
fails=0
ok() { echo "PASS $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }

# 1. Six concurrent acquires: exactly four succeed, each with its own slot.
for i in 1 2 3 4 5 6; do
  ( "$SLOTSH" acquire "t$i" >"$EMU_SLOTS.out$i" 2>&1; echo "rc=$?" >>"$EMU_SLOTS.out$i" ) &
done
wait
got=$(cat "$EMU_SLOTS".out* | grep -E '^[0-3]$' | sort | tr '\n' ' ')
nok=$(cat "$EMU_SLOTS".out* | grep -c 'rc=0')
nfail=$(cat "$EMU_SLOTS".out* | grep -c 'no free slot')
[ "$got" = "0 1 2 3 " ] && [ "$nok" = 4 ] && [ "$nfail" = 2 ] \
  && ok "6 concurrent acquires -> slots {$got}, 2 refused" \
  || bad "concurrent acquires: slots '$got', $nok ok, $nfail refused"

# 2. Two concurrent acquires of the last free slot: exactly one wins.
"$SLOTSH" release t1 >/dev/null 2>&1; "$SLOTSH" release t2 >/dev/null 2>&1
"$SLOTSH" release t3 >/dev/null 2>&1; "$SLOTSH" release t4 >/dev/null 2>&1
"$SLOTSH" release t5 >/dev/null 2>&1; "$SLOTSH" release t6 >/dev/null 2>&1
for n in a b c; do "$SLOTSH" acquire "fill-$n" >/dev/null; done
( "$SLOTSH" acquire race-x >"$EMU_SLOTS.rx" 2>&1 ) & ( "$SLOTSH" acquire race-y >"$EMU_SLOTS.ry" 2>&1 ) & wait
wins=$(cat "$EMU_SLOTS.rx" "$EMU_SLOTS.ry" | grep -cE '^[0-3]$')
[ "$wins" = 1 ] && ok "2 concurrent acquires of 1 free slot -> 1 winner" || bad "race for the last slot: $wins winners"

# 3. Re-acquire by the same name returns its slot; ports follow the slot.
slot=$("$SLOTSH" acquire fill-a)
again=$("$SLOTSH" acquire fill-a)
port=$("$SLOTSH" port fill-a)
[ "$slot" = "$again" ] && [ "$port" = $((4110 + 10 * slot)) ] \
  && ok "re-acquire keeps slot $slot, ssh port $port" || bad "re-acquire/port: $slot $again $port"
st() { "$SLOTSH" status 2>&1; }   # capture first: grep -q + pipefail would see SIGPIPE
st | grep -q "4140/4141" && ok "status lists ports 4110+10N/4111+10N" || bad "status ports"

# 4. Release frees the slot; releasing an unknown name is harmless.
"$SLOTSH" release fill-b >/dev/null 2>&1
[[ "$(st)" == *fill-b* ]] && bad "release left fill-b" || ok "release frees the slot"
"$SLOTSH" release nobody >/dev/null 2>&1 && ok "release of an unknown name is a no-op" || bad "release unknown"

# 5. cleanup: lock with a dead QEMU pid goes, fresh lock without QEMU stays, old one goes.
d=$(grep -l fill-c "$EMU_SLOTS"/slot*/owner | head -1); d=$(dirname "$d")
echo 999999 >"$d/qemu.pid"
"$SLOTSH" cleanup >/dev/null 2>&1
[ ! -d "$d" ] && ok "cleanup removes a lock whose QEMU is gone" || bad "stale qemu lock kept"
"$SLOTSH" cleanup >/dev/null 2>&1
[[ "$(st)" == *fill-a* ]] && ok "cleanup keeps a fresh lock without QEMU" || bad "fresh lock removed"
EMU_SLOT_GRACE_S=0 "$SLOTSH" cleanup >/dev/null 2>&1
[[ "$(st)" == *fill-a* ]] && bad "old lock kept" || ok "cleanup removes a lock older than the grace time"

# 6. The user's instance is refused everywhere.
for c in "acquire" "up" "down" "release" "ssh true" "deploy" "shot /tmp/x.png" "tap 1 1" "swipe 1 1 2 2" "hold 1 1 1" "power" "wifi" "log"; do
  set -- $c
  out=$("$SLOTSH" "$1" pw12-5195-jb "${@:2}" 2>&1); rc=$?
  { [ $rc -ne 0 ] && echo "$out" | grep -q "refusing to touch pw12-5195-jb"; } || { bad "$c pw12-5195-jb not refused: $(echo "$out" | head -2)"; continue; }
done
ok "every subcommand refuses pw12-5195-jb"

rm -rf "$EMU_SLOTS" "$EMU_SLOTS".out* "$EMU_SLOTS".r?
echo "emu-slot-test: $fails failed"
exit "$fails"
