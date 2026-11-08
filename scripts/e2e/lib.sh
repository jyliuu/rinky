#!/bin/bash
# Helpers for emulator E2E scenarios (tests/e2e/*.sh). Source it:
#   . "$(dirname "$0")/../../scripts/e2e/lib.sh"
# A scenario is called as:  tests/e2e/<key>-<name>.sh INSTANCE OUTDIR
# It writes screenshots and logs into OUTDIR and exits non-zero on failure.
# Exit 75 (`not_ready`) means the guest was not ready before the scenario began (no
# clean Home in time); scripts/e2e/run.sh then restarts the instance and retries once.
# Deadlines in wait_until/wait_home/wait_clean_home and `nap` are multiplied by
# E2E_TIME_SCALE (set by run.sh from the host load and the measured boot time).
# A scenario declares its framework stop/start cycles with `# e2e-cycles: N`.

E2E_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
SLOT="$E2E_ROOT/scripts/emu-slot.sh"
SCREEN="$E2E_ROOT/scripts/emu-screen.py"
DEST=/mnt/us/rinky

: "${NAME:?set NAME (the instance) before sourcing lib.sh}"
: "${OUT:?set OUT (the output directory) before sourcing lib.sh}"
mkdir -p "$OUT"

log() { echo "[$(date '+%H:%M:%S')] $*" | tee -a "$OUT/scenario.log" >&2; }
E2E_TIME_SCALE=${E2E_TIME_SCALE:-1}
# scale SECONDS: SECONDS * E2E_TIME_SCALE, rounded, at least SECONDS.
scale() { awk -v s="$1" -v f="$E2E_TIME_SCALE" 'BEGIN { if (f + 0 < 1) f = 1; printf "%d\n", s * f + 0.5 }'; }
# nap SECONDS: sleep, scaled (waits that give the guest time to draw).
nap() { sleep "$(scale "$1")"; }
# On failure: save logs, then stop a running rinky so its launcher restores the
# framework (the next scenario starts from Home).
fail() {
  log "FAIL: $*"
  save_logs
  guest 'pid=$(cat /tmp/rinky/rinky.pid 2>/dev/null); [ -n "$pid" ] && kill "$pid" 2>/dev/null' || true
  exit 1
}
pass() { log "PASS: $*"; }
# skip WHY: nothing was checked; run.sh lists the scenario as SKIP (exit 77), not PASS.
skip() { log "SKIP: $*"; exit 77; }
# The guest was not ready before the scenario did anything (run.sh may restart + retry).
not_ready() {
  log "NOT READY: $*"
  save_logs
  exit 75
}

guest() { "$SLOT" ssh "$NAME" "$@"; }

# next_shot LABEL: prints OUT/NN-LABEL.png, NN = one more than the numbered files there.
# Numbered from the files already there: shot often runs inside $(...), where a counter
# variable would not survive.
next_shot() {
  local n
  n=$(ls "$OUT"/[0-9][0-9]-*.png 2>/dev/null | wc -l | tr -d ' ')
  printf '%s/%02d-%s.png\n' "$OUT" "$((n + 1))" "$1"
}

# shot LABEL -> OUT/NN-LABEL.png (a new screendump; prints the path)
shot() {
  local f
  f=$(next_shot "$1")
  "$SLOT" shot "$NAME" "$f" >/dev/null || fail "screendump $1"
  echo "$f"
}

# keep_probe LABEL -> OUT/NN-LABEL.png: the probe screendump that a check just decided
# on (not a new screendump, which could show a later frame); prints the path.
keep_probe() {
  local f
  f=$(next_shot "$1")
  cp "$PROBE" "$f" || return 1
  echo "$f"
}

tap() { "$SLOT" tap "$NAME" "$1" "$2" >/dev/null || fail "tap $1 $2"; }
swipe() { "$SLOT" swipe "$NAME" "$@" >/dev/null || fail "swipe $*"; }

is_home() { python3 "$SCREEN" home "$1" >/dev/null 2>&1; }

# wait_until DEADLINE_S DESCRIPTION COMMAND...: polls every 3 s; DEADLINE_S is scaled.
wait_until() {
  local deadline=$(( $(date +%s) + $(scale "$1") )) what=$2
  shift 2
  until "$@"; do
    [ "$(date +%s)" -lt "$deadline" ] || return 1
    sleep 3
  done
  log "ok: $what"
}

REF_HOME="$E2E_ROOT/tests/e2e/ref/home-5.19.5.png"
PROBE="$OUT/.probe.png"

probe() { "$SLOT" shot "$NAME" "$PROBE" >/dev/null 2>&1; }
home_now() { probe && is_home "$PROBE"; }
clean_home_now() { probe && python3 "$SCREEN" clean-home "$PROBE" "$REF_HOME" >/dev/null; }
app_error_now() { python3 "$SCREEN" app-error "$PROBE" "$REF_HOME" >/dev/null; }

# Before a scenario: wait for the clean stock Home (tests/e2e/ref/home-5.19.5.png).
# A leftover appmgrd "Application Error" dialog from an earlier run is closed and logged.
clean_home_or_dismiss() {
  clean_home_now && return 0
  if app_error_now; then
    cp "$PROBE" "$OUT/leftover-app-error.png"
    log "closing a leftover Application Error dialog (saved leftover-app-error.png)"
    tap 958 960
  fi
  return 1
}

# wait_home DEADLINE_S LABEL: clean Home before a run; saves LABEL.png (the screendump
# that passed the check). Without it the scenario has not started: exit 75
# (not_ready), not a FAIL.
wait_home() {
  local f
  wait_until "$1" "clean Home screen" clean_home_or_dismiss ||
    { keep_probe "$2-timeout" >/dev/null || true; not_ready "clean Home did not appear within $(scale "$1") s"; }
  f=$(keep_probe "$2") || exit 1
  log "screenshot $f (clean Home)"
}

# wait_clean_home DEADLINE_S LABEL: after a run the stock Home must come back with no
# dialog left (transient toasts may pass). Saves LABEL.png, the screendump that passed
# the check (or LABEL-timeout.png, the last one checked); fails otherwise.
wait_clean_home() {
  local f
  if ! wait_until "$1" "clean Home screen" clean_home_now; then
    f=$(keep_probe "$2-timeout") || true
    app_error_now && fail "Home came back with the appmgrd Application Error dialog ($f)"
    fail "clean Home did not come back within $(scale "$1") s ($f)"
  fi
  f=$(keep_probe "$2") || exit 1
  log "screenshot $f (clean Home)"
}

framework_state() { guest 'status lab126_gui' 2>/dev/null; }
rinky_pid() { guest 'pidof rinky' 2>/dev/null; }
reader_running() { [ -n "$(rinky_pid)" ] && [ -z "$(guest 'pidof KPPMainApp' 2>/dev/null)" ]; }
reader_gone() { [ -z "$(rinky_pid)" ]; }

# power_key: the power key as rinky sees it (evdev KEY_POWER injected by `rinky inject`).
# It puts rinky to sleep (screensaver); it does not exit.
power_key() { guest "$DEST/rinky inject power" >/dev/null || fail "inject power"; }

# exit_device: leave rinky the way "Exit to Kindle" does from a shell
# (rinky-ctl.sh stop: SIGTERM to rinky, the launcher restores the framework) and
# wait until rinky is gone. There is no hidden top-right exit corner any more, and
# the power key sleeps instead of exiting. rinky's exit status is then 143.
exit_device() {
  guest "sh $DEST/rinky-ctl.sh stop" >/dev/null 2>&1 || true
  wait_until 60 "rinky exited" reader_gone || fail "rinky did not exit"
}

# start_reader [BOOK]: launches rinky.sh and waits until rinky runs without the framework.
# READER_LOG=debug starts it with RINKY_LOG=debug (lines logged at debug only).
start_reader() {
  local book=${1:-$DEST/juliet.epub}
  local env=${READER_LOG:+RINKY_LOG=$READER_LOG }
  guest "rm -f $DEST/rinky.log $DEST/watchdog.log; rm -rf $DEST/juliet.sdr; ${env}sh $DEST/rinky.sh $book" || fail "launch"
  wait_until 90 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
  nap 4
  log "framework: $(framework_state)"
}

save_logs() {
  guest "cat $DEST/rinky.log" >"$OUT/rinky.log" 2>&1 || true
  guest "cat $DEST/watchdog.log" >"$OUT/watchdog.log" 2>&1 || true
  guest 'ps -eo pid,comm,args | grep -E "rinky|KPPMainApp|pillowd|lipc-wait" | grep -v grep' >"$OUT/ps.txt" 2>&1 || true
}

# not_home LABEL: screenshot that must not be Home (rinky is showing).
not_home() {
  local f
  f=$(shot "$1") || exit 1
  is_home "$f" && fail "$f still shows Home"
  log "screenshot $f (not Home)"
}

# Note: call fail-able helpers directly, not inside $(...): `fail` in a command
# substitution only leaves the subshell.
