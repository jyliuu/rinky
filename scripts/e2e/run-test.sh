#!/bin/bash
# Host test of scripts/e2e/run.sh with the fake emulator (scripts/e2e/fake-emu.sh): no
# real emulator, no network. Five stub scenarios check the recovery rules:
#   s1  5 framework cycles, passes;
#   s2  5 cycles: 5 + 5 > E2E_MAX_CYCLES=8, so the instance restarts before it; passes;
#   s3  exits 75 (guest not ready) the first time: restart + retry, then passes;
#   s4  runs past the time limit the first time: killed, restart + retry, then passes;
#   s5  fails with exit 1: a real FAIL, no retry;
#   s6  skips (exit 77): listed as SKIP, counted apart from PASS and FAIL.
# Expected: 4 passed, 1 failed, 1 skipped, 3 restarts, slot kept, fw_cycles reset by each restart.
# Second run (t1-t3) with boots that fail: t1 passes; t2 exits 75, its restart fails
# twice, so the run stops: t2 FAIL, t3 NOT RUN, exit status 2.
#   scripts/e2e/run-test.sh [OUTDIR]     prints PASS/FAIL lines; exit status = failures
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
TMP=$(mktemp -d "${TMPDIR:-/tmp}/e2e-run-test.XXXXXX")
OUT=${1:-$TMP/out}
. "$ROOT/scripts/e2e/fake-emu.sh"
fake_emu_setup "$TMP"
SLOT="$ROOT/scripts/emu-slot.sh"
fails=0
ok() { echo "PASS $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }

# deploy --no-build copies a built binary; with the scp stub a placeholder file will do.
export EMU_DEPLOY_BIN="$TMP/rinky-placeholder"
: >"$EMU_DEPLOY_BIN"

S="$TMP/scenarios"
mkdir -p "$S"
cat >"$S/s1.sh" <<'EOF'
#!/bin/bash
# e2e-cycles: 5
echo "s1 on $1"
EOF
cat >"$S/s2.sh" <<'EOF'
#!/bin/bash
# e2e-cycles: 5
echo "s2 on $1"
EOF
cat >"$S/s3.sh" <<'EOF'
#!/bin/bash
# e2e-cycles: 1
m="$FAKE_DIR/s3-tried"
if [ ! -f "$m" ]; then touch "$m"; echo "NOT READY: no clean Home"; exit 75; fi
echo "s3 second attempt"
EOF
cat >"$S/s4.sh" <<'EOF'
#!/bin/bash
# e2e-cycles: 1
m="$FAKE_DIR/s4-tried"
if [ ! -f "$m" ]; then touch "$m"; sleep 60; fi
echo "s4 done"
EOF
cat >"$S/s5.sh" <<'EOF'
#!/bin/bash
# e2e-cycles: 1
echo "FAIL: a real failure"
exit 1
EOF
cat >"$S/s6.sh" <<'EOF'
#!/bin/bash
# e2e-cycles: 0
echo "SKIP: nothing to check here"
exit 77
EOF

"$SLOT" up rt-x >"$TMP/up.txt" 2>&1 || { cat "$TMP/up.txt"; bad "fake up failed"; }
dir=$("$SLOT" slotdir rt-x)
acq=$(cat "$dir/acquired")
q0=$(cat "$dir/qemu.pid")

E2E_DIR="$S" E2E_MAX_CYCLES=8 E2E_TIME_SCALE=1 E2E_SCENARIO_TIMEOUT_S=8 \
  "$ROOT/scripts/e2e/run.sh" rt-x "$OUT" 's*' >"$TMP/run.txt" 2>&1
rc=$?
cp "$TMP/run.txt" "$OUT/run-stdout.txt" 2>/dev/null
sum="$OUT/summary.txt"
line() { grep " $1 " "$sum"; }

[ "$rc" = 1 ] && grep -q "4 passed, 1 failed, 0 not run, 1 skipped" "$sum" && ok "run.sh exit 1: 4 passed, 1 failed, 1 skipped" \
  || bad "run.sh exit $rc: $(tail -1 "$sum" 2>/dev/null)"
grep -q "3 restart(s)" "$sum" && ok "3 restarts (cycle budget, not-ready retry, time-limit retry)" \
  || bad "restarts: $(head -1 "$sum")"
grep -q "restart rt-x: 5 framework cycles in this boot + 5 for s2 > 8" "$OUT/run.log" \
  && ok "restart before s2 because 5 + 5 cycles > E2E_MAX_CYCLES 8" || bad "no cycle-budget restart before s2"
line s3 | grep -q "^PASS.*retried after restart" && [ -d "$OUT/s3.attempt1" ] \
  && ok "s3 (exit 75 first) passed on the retry after a restart" || bad "s3: $(line s3)"
line s4 | grep -q "^PASS.*retried after restart" && grep -q "time limit 8s reached" "$OUT/s4.attempt1/stdout.txt" \
  && ! grep -q "s4 done" "$OUT/s4.attempt1/stdout.txt" \
  && ok "s4 (time limit first) was killed, then passed on the retry" || bad "s4: $(line s4)"
line s6 | grep -q "^SKIP.*exit 77 " && ok "s6 (exit 77) is listed as SKIP" || bad "s6: $(line s6)"
line s5 | grep -q "^FAIL.*exit 1 " && ! [ -d "$OUT/s5.attempt1" ] \
  && ok "s5 (exit 1) is a FAIL without a retry" || bad "s5: $(line s5)"
[ "$(cat "$dir/acquired" 2>/dev/null)" = "$acq" ] && [ "$(cat "$dir/qemu.pid")" != "$q0" ] \
  && ok "the slot stayed held across restarts (QEMU $q0 -> $(cat "$dir/qemu.pid"))" || bad "slot lost or QEMU unchanged"
# After the last restart (s4 retry): s4 (1) + s5 (1) = 2 cycles in the current boot.
[ "$(cat "$dir/fw_cycles")" = 2 ] && ok "fw_cycles counts only the current boot (2)" \
  || bad "fw_cycles $(cat "$dir/fw_cycles")"
left=$(ps -axo command= | grep -c "[s]leep 60$")
[ "$left" = 0 ] && ok "no scenario process left after the time limit" || bad "$left leftover sleep(s)"

# Second run: the instance cannot be restarted.
T="$TMP/scenarios2"
mkdir -p "$T"
printf '#!/bin/bash\n# e2e-cycles: 1\necho t1\n' >"$T/t1.sh"
printf '#!/bin/bash\n# e2e-cycles: 1\ntouch "$FAKE_DIR/fail-run"; echo "NOT READY"; exit 75\n' >"$T/t2.sh"
printf '#!/bin/bash\n# e2e-cycles: 1\necho t3 must not run; exit 0\n' >"$T/t3.sh"
E2E_DIR="$T" E2E_MAX_CYCLES=8 E2E_TIME_SCALE=1 E2E_SCENARIO_TIMEOUT_S=8 \
  "$ROOT/scripts/e2e/run.sh" rt-x "$OUT/abort" 't*' >"$TMP/run2.txt" 2>&1
rc=$?
sum2="$OUT/abort/summary.txt"
[ "$rc" = 2 ] && grep -q "1 passed, 1 failed, 1 not run" "$sum2" && grep -q "^NOT RUN t3 " "$sum2" \
  && grep -q "could not be restarted twice" "$OUT/abort/run.log" && [ ! -d "$OUT/abort/t3" ] \
  && ok "failed restarts (twice) stop the run: t2 FAIL, t3 NOT RUN, exit 2" || bad "abort run: rc=$rc $(tail -3 "$sum2" 2>/dev/null)"
[ "$(grep -c 'restart rt-x: retry of t2' "$OUT/abort/run.log")" = 2 ] && ok "the failed restart was tried twice" \
  || bad "restart attempts: $(grep -c 'restart rt-x' "$OUT/abort/run.log")"
rm -f "$FAKE_DIR/fail-run"

"$SLOT" down rt-x >/dev/null 2>&1
fake_emu_teardown "$TMP"
[ -n "${1:-}" ] || rm -rf "$TMP"
echo "e2e-run-test: $fails failed"
exit "$fails"
