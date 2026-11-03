#!/bin/bash
# Runs emulator E2E scenarios against a running instance and prints a summary.
#   scripts/e2e/run.sh NAME [OUTDIR] [PATTERN]
# NAME must be up (scripts/emu-slot.sh up NAME) with a current deploy
# (scripts/emu-slot.sh deploy NAME). Every tests/e2e/*.sh matching PATTERN (default *)
# runs as `SCENARIO NAME OUTDIR/<scenario>`; exit status 0 = pass, 77 = skipped (lib.sh
# `skip`: the scenario did not check anything, e.g. no internet; counted apart from PASS).
# OUTDIR defaults to out/e2e/<timestamp>. Exit status = failed + not run scenarios.
#
# Guest degradation and slow hosts:
#   - Each scenario declares how many framework stop/start cycles it causes with a line
#     `# e2e-cycles: N` (default 1). The count since the instance's last boot is kept in
#     its slot (fw_cycles). The 5.19.5 guest degrades after about 20 cycles in one boot,
#     so before a scenario that would pass E2E_MAX_CYCLES the instance is rebooted in
#     its slot (`emu-slot.sh restart`, the slot stays held) and redeployed without a
#     rebuild (`deploy --no-build`).
#   - Before each scenario SSH must answer; otherwise the instance is restarted.
#   - A scenario that exits 75 (lib.sh: the guest was not ready before the scenario
#     began, e.g. no clean Home in time) or hits its time limit is retried once after
#     a restart (E2E_RETRY=0 turns that off). Any other failure is a real FAIL.
#   - A restart that fails is tried once more; if that fails too, the run stops: the
#     remaining scenarios are listed as NOT RUN and counted in the exit status.
#   - E2E_TIME_SCALE (default: auto) multiplies every deadline in lib.sh, the scenario
#     time limit and emu-slot.sh's boot deadline. auto = max(1, boot_s / 300,
#     0.5 + load average / CPUs), at most 4, where boot_s is the instance's measured
#     QEMU start -> Home time.
# Environment: E2E_MAX_CYCLES (12), E2E_RESTART (auto | always = before every scenario
# | never), E2E_RETRY (1), E2E_TIME_SCALE (auto), E2E_SCENARIO_TIMEOUT_S (900, scaled),
# E2E_DIR (scenario directory, default tests/e2e; scripts/e2e/run-test.sh uses its own).
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SLOT="$ROOT/scripts/emu-slot.sh"
NAME=${1:?usage: scripts/e2e/run.sh NAME [OUTDIR] [PATTERN]}
OUTDIR=${2:-$ROOT/out/e2e/$(date +%Y%m%d-%H%M%S)}
PATTERN=${3:-*}
MAX_CYCLES=${E2E_MAX_CYCLES:-12}
RESTART_MODE=${E2E_RESTART:-auto}
RETRY=${E2E_RETRY:-1}
mkdir -p "$OUTDIR"
RUNLOG="$OUTDIR/run.log"

say() { echo "[$(date '+%H:%M:%S')] $*" | tee -a "$RUNLOG" >&2; }

SLOTDIR=$("$SLOT" slotdir "$NAME") || { echo "e2e: $NAME holds no slot" >&2; exit 100; }

# Time scale: explicit, or from the measured boot time and the host load.
time_scale() {
  local boot ncpu load
  boot=$(cat "$SLOTDIR/boot_s" 2>/dev/null || echo 0)
  ncpu=$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 1)
  load=$(sysctl -n vm.loadavg 2>/dev/null | awk '{print $2}')
  [ -n "$load" ] || load=$(awk '{print $1}' /proc/loadavg 2>/dev/null || echo 0)
  awk -v b="$boot" -v n="$ncpu" -v l="$load" 'BEGIN {
    s = 1; if (b / 300 > s) s = b / 300; if (0.5 + l / n > s) s = 0.5 + l / n; if (s > 4) s = 4
    printf "%.2f\n", s }'
}
if [ "${E2E_TIME_SCALE:-auto}" = auto ]; then E2E_TIME_SCALE=$(time_scale); fi
export E2E_TIME_SCALE EMU_TIME_SCALE=$E2E_TIME_SCALE
# At least 1x, as in lib.sh and emu-slot.sh: a scale below 1 must not shorten the
# scenario limit while the scenario's own deadlines stay at 1x.
scaled() { awk -v s="$1" -v f="$E2E_TIME_SCALE" 'BEGIN { if (f + 0 < 1) f = 1; printf "%d\n", s * f + 0.5 }'; }
SCENARIO_LIMIT=$(scaled "${E2E_SCENARIO_TIMEOUT_S:-900}")

cycles_now() { cat "$SLOTDIR/fw_cycles" 2>/dev/null || echo 0; }
add_cycles() { echo $(( $(cycles_now) + $1 )) >"$SLOTDIR/fw_cycles"; }
declared_cycles() {
  local c
  c=$(sed -n 's/^# e2e-cycles: *\([0-9][0-9]*\).*/\1/p' "$1" | head -1)
  echo "${c:-1}"
}

restarts=0
# restart_instance WHY: fresh boot in the same slot, then copy the last build again.
restart_instance() {
  say "restart $NAME: $1"
  restarts=$((restarts + 1))
  if ! "$SLOT" restart "$NAME" >>"$RUNLOG" 2>&1; then
    say "restart of $NAME failed (see $RUNLOG)"
    return 1
  fi
  "$SLOT" deploy "$NAME" --no-build >>"$RUNLOG" 2>&1 || { say "redeploy after restart failed"; return 1; }
  say "$NAME booted again (boot_s $(cat "$SLOTDIR/boot_s" 2>/dev/null), fw_cycles $(cycles_now))"
}

# restart_or_abort WHY: restart, a second try if that fails; after two failures the
# run is aborted (remaining scenarios are not run).
aborted=0
restart_or_abort() {
  restart_instance "$1" && return 0
  say "restart failed; trying once more"
  restart_instance "$1 (second try)" && return 0
  say "$NAME could not be restarted twice; the remaining scenarios are not run"
  aborted=1
  return 1
}

ssh_ok() {
  local i
  for i in 1 2 3; do
    "$SLOT" ssh "$NAME" true >/dev/null 2>&1 && return 0
    sleep 5
  done
  return 1
}

# Pids of all descendants of a process.
descendants() {
  local p
  for p in $(pgrep -P "$1" 2>/dev/null); do echo "$p"; descendants "$p"; done
}

# run_one SCENARIO OUT: runs it with the time limit; prints its exit status
# (124 = time limit reached).
run_one() {
  local scenario=$1 out=$2 spid wpid rc
  mkdir -p "$out"
  # Append mode: the time-limit note written by the watcher must not be overwritten.
  : >"$out/stdout.txt"
  bash "$scenario" "$NAME" "$out" >>"$out/stdout.txt" 2>&1 &
  spid=$!
  # The watcher must not hold this function's stdout (the caller's $(...) would wait
  # for it), and it stops its sleep when it is killed.
  ( trap 'kill "$sp" 2>/dev/null; exit 0' TERM
    sleep "$SCENARIO_LIMIT" & sp=$!
    wait "$sp"
    echo "e2e: time limit ${SCENARIO_LIMIT}s reached" >>"$out/stdout.txt"
    touch "$out/.timed-out"
    # The scenario and all its descendants (ssh, sleep, python), collected before the
    # first signal so that none is missed after being reparented.
    tree="$spid $(descendants "$spid")"
    kill -TERM $tree 2>/dev/null
    sleep 5; kill -KILL $tree 2>/dev/null ) >/dev/null 2>&1 &
  wpid=$!
  wait "$spid"; rc=$?
  kill "$wpid" 2>/dev/null; wait "$wpid" 2>/dev/null
  if [ -f "$out/.timed-out" ]; then rm -f "$out/.timed-out"; rc=124; fi
  echo "$rc"
}

ssh_ok || { echo "e2e: $NAME does not answer over SSH" >&2; exit 100; }
say "e2e on $NAME: time scale $E2E_TIME_SCALE, scenario limit ${SCENARIO_LIMIT}s, max $MAX_CYCLES framework cycles per boot (now $(cycles_now)), restart=$RESTART_MODE, retry=$RETRY"

pass=0
fail=0
notrun=0
skipped=0
summary=()
first=1
E2E_DIR=${E2E_DIR:-$ROOT/tests/e2e}
for scenario in "$E2E_DIR"/$PATTERN.sh; do
  [ -f "$scenario" ] || continue
  base=$(basename "$scenario" .sh)
  out="$OUTDIR/$base"
  need=$(declared_cycles "$scenario")
  echo "== $base"
  if [ "$aborted" = 0 ]; then
    if ! ssh_ok; then
      restart_or_abort "SSH does not answer before $base"
    elif [ "$RESTART_MODE" = always ] && [ $first = 0 ]; then
      restart_or_abort "E2E_RESTART=always"
    elif [ "$RESTART_MODE" = auto ] && [ $(( $(cycles_now) + need )) -gt "$MAX_CYCLES" ]; then
      restart_or_abort "$(cycles_now) framework cycles in this boot + $need for $base > $MAX_CYCLES"
    fi
  fi
  if [ "$aborted" = 1 ]; then
    notrun=$((notrun + 1))
    summary+=("$(printf '%-4s %-40s (the instance could not be restarted)' 'NOT RUN' "$base")")
    echo "   NOT RUN"
    continue
  fi
  first=0
  start=$(date +%s)
  rc=$(run_one "$scenario" "$out")
  add_cycles "$need"
  note=
  if [ "$rc" != 0 ] && [ "$RETRY" -gt 0 ] && { [ "$rc" = 75 ] || [ "$rc" = 124 ]; }; then
    say "$base: exit $rc ($([ "$rc" = 75 ] && echo 'guest not ready' || echo 'time limit')); restart and retry once"
    mv "$out" "$out.attempt1"
    if restart_or_abort "retry of $base"; then
      rc=$(run_one "$scenario" "$out")
      add_cycles "$need"
      note="(retried after restart; first attempt in $base.attempt1)"
    fi
  fi
  if [ "$rc" = 0 ]; then
    result=PASS
    pass=$((pass + 1))
  elif [ "$rc" = 77 ]; then
    result=SKIP
    skipped=$((skipped + 1))
    grep -h "SKIP" "$out/stdout.txt" 2>/dev/null | tail -1 | sed 's/^/   /'
  else
    result=FAIL
    fail=$((fail + 1))
    tail -5 "$out/stdout.txt" 2>/dev/null | sed 's/^/   /'
  fi
  secs=$(( $(date +%s) - start ))
  shots=$(ls "$out"/*.png 2>/dev/null | wc -l | tr -d ' ')
  summary+=("$(printf '%-4s %-40s %4ss %3s screenshots exit %-3s %s %s' "$result" "$base" "$secs" "$shots" "$rc" "$out" "$note")")
  echo "   $result in ${secs}s (exit $rc, fw_cycles now $(cycles_now))"
done

{
  echo "e2e summary for $NAME ($(date '+%Y-%m-%d %H:%M:%S')), time scale $E2E_TIME_SCALE, $restarts restart(s)"
  printf '%s\n' ${summary[@]+"${summary[@]}"}
  echo "$pass passed, $fail failed, $notrun not run, $skipped skipped"
} | tee "$OUTDIR/summary.txt"
exit $((fail + notrun))
