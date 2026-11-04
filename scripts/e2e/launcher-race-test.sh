#!/bin/bash
# Host test of the framework-stop launcher and watchdog (scripts/rinky.sh,
# scripts/rinky-watchdog.sh; owned by the device agent) with stub upstart commands:
# no emulator, no Kindle, no network. Each case copies both scripts into a fresh sandbox
# and puts stubs for `stop`, `start`, `status`, `pidof`, `setsid`, `usleep` and the lipc
# tools (`lipc-get-prop`, `lipc-set-prop`, `lipc-probe`: answer at once, print 1) first on
# PATH. rinky is replaced by a script that records its pid in $SB/live/PID and runs
# until it is killed or $SB/quit appears (`rinky device ...` returns at once). Every stub call goes to $SB/calls.log.
#
# Processes are found through pid files and the logs, never through path regexes.
# Paths are physical (macOS TMPDIR ends in '/', and /var is a symlink).
#
# The host /bin/sh is bash 3.2 in POSIX mode, not the Kindle's busybox ash: the logic
# and the races are the same, the timing is not.
#
# Cases (PASS/FAIL per check; known launcher bugs show as FAIL with the observed state,
# so the device agent can use this as a regression test; INFO = expected limitation):
#    1 normal run (positive control: the matchers see rinky and the watchdog)
#    2 second launcher while the first runs: refused
#    3 two launchers at the same instant (repeated): exactly one rinky
#    4 kill -9 rinky: framework started once
#    5 kill -9 launcher while it waits for the framework to stop: watchdog restores,
#      no rinky appears later
#    6 kill -9 launcher while rinky runs: watchdog stops rinky, starts once
#    7 kill -9 watchdog, then normal exit: launcher restores
#    8 launcher + watchdog killed together: framework stays stopped (INFO); the stale
#      lock does not block the next start
#    9 kill -9 launcher inside `start lab126_gui`: framework ends up running
#   10 kill -9 launcher inside the appmgrd restart: framework ends up running
#   11 SIGTERM to the launcher while rinky runs: rinky stopped, started once
#   15 Amazon services: stopped while rinky runs, running again after a normal exit
#      and after kill -9 of the launcher (the watchdog starts them)
#   12 rinky exits at once with an error: framework started once
#   13 framework does not stop within the deadline: no rinky, framework back
#   14 random kill -9 points (launcher or rinky, 0-4 s after launch), repeated:
#      the framework always comes back and no rinky is left
#
#   scripts/e2e/launcher-race-test.sh [OUTDIR] [RANDOM_ROUNDS]   exit status = failures
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
TMP=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/launcher-race.XXXXXX")" && pwd -P)
OUT=${1:-$TMP/out}
ROUNDS=${2:-10}
mkdir -p "$OUT"
fails=0
ok() { echo "PASS $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }
info() { echo "INFO $*"; }

setup() {   # fresh sandbox $SB with the framework running
  SB="$TMP/sb$1"
  rm -rf "$SB"
  mkdir -p "$SB/bin" "$SB/app" "$SB/run" "$SB/state" "$SB/live"
  cp "$ROOT/scripts/rinky.sh" "$ROOT/scripts/rinky-watchdog.sh" "$SB/app/"
  echo "start/running" >"$SB/state/lab126_gui"
  cat >"$SB/bin/stub" <<EOF
#!/bin/bash
# Upstart stubs. The framework "stops" STOP_DELAY s after 'stop lab126_gui' (KPPMainApp
# and pillowd gone); 'start lab126_gui' takes START_DELAY s; 'stop appmgrd' APPMGRD_DELAY s.
SB=$SB
echo "\$(perl -MTime::HiRes=time -e 'printf "%.3f", time') \$\$ \$(basename "\$0") \$*" >>"\$SB/calls.log"
case "\$(basename "\$0") \$1" in
  "status lab126_gui") echo "lab126_gui \$(cat "\$SB/state/lab126_gui")" ;;
  "stop lab126_gui") echo stop/waiting >"\$SB/state/lab126_gui"; date +%s >"\$SB/state/stopped_at" ;;
  "start lab126_gui") echo start/starting >"\$SB/state/lab126_gui"; sleep "\${START_DELAY:-0}"; echo start/running >"\$SB/state/lab126_gui" ;;
  "stop appmgrd") sleep "\${APPMGRD_DELAY:-0}" ;;
  # Other jobs exist when \$SB/state/JOB does (case 15: the Amazon services).
  "status "*) [ -f "\$SB/state/\$1" ] && echo "\$1 \$(cat "\$SB/state/\$1")" ;;
  "stop "*) [ -f "\$SB/state/\$1" ] && echo stop/waiting >"\$SB/state/\$1" ;;
  "start "*) [ -f "\$SB/state/\$1" ] && echo start/running >"\$SB/state/\$1" ;;
  "pidof KPPMainApp"|"pidof pillowd")
     t=\$(cat "\$SB/state/stopped_at" 2>/dev/null || echo 0)
     if grep -q start/ "\$SB/state/lab126_gui" || [ \$(( \$(date +%s) - t )) -lt \${STOP_DELAY:-1} ]; then echo 4242; else exit 1; fi ;;
  "pidof "*) exit 1 ;;
  "lipc-get-prop "*) echo 1 ;;
esac
exit 0
EOF
  chmod +x "$SB/bin/stub"
  for c in stop start status pidof lipc-get-prop lipc-set-prop lipc-probe; do ln -s stub "$SB/bin/$c"; done
  printf '#!/bin/bash\nexec "$@"\n' >"$SB/bin/setsid"
  printf '#!/bin/bash\nperl -e "select(undef,undef,undef,$1/1000000)"\n' >"$SB/bin/usleep"
  cat >"$SB/app/rinky" <<EOF
#!/bin/bash
# 'rinky device restore' (launcher and watchdog, on every exit path) ends at once.
[ "\${1:-}" = device ] && { echo "\$(date +%s) rinky \$\$ device \$*" >>"$SB/ink.log"; exit 0; }
# 'rinky splash TEXT' (launcher screens while the framework stops/starts) ends at once.
[ "\${1:-}" = splash ] && { echo "\$(date +%s) rinky \$\$ splash \$*" >>"$SB/ink.log"; exit 0; }
echo \$\$ >"$SB/live/\$\$"
echo "\$(date +%s) rinky \$\$ start \$*" >>"$SB/ink.log"
trap 'echo "\$(date +%s) rinky \$\$ TERM" >>"$SB/ink.log"; rm -f "$SB/live/\$\$"; exit 143' TERM
[ "\${INK_MODE:-}" = fail ] && { echo "rinky: cannot open book" >&2; rm -f "$SB/live/\$\$"; exit 3; }
while [ ! -f "$SB/quit" ]; do sleep 0.2; done
echo "\$(date +%s) rinky \$\$ quit" >>"$SB/ink.log"
rm -f "$SB/live/\$\$"
EOF
  chmod +x "$SB/bin/setsid" "$SB/bin/usleep" "$SB/app/rinky"
}
launch() { env PATH="$SB/bin:$PATH" RINKY_RUN="$SB/run" RINKY_STOP_DEADLINE="${DEADLINE:-10}" "$@" sh "$SB/app/rinky.sh" book.epub; }
# Live fake rinkys (pid files whose process exists).
inks() { local n=0 f; for f in "$SB"/live/*; do [ -f "$f" ] && kill -0 "$(basename "$f")" 2>/dev/null && n=$((n + 1)); done; echo $n; }
ink_pids() { local f; for f in "$SB"/live/*; do [ -f "$f" ] && basename "$f"; done; }
starts() { cat "$SB/calls.log" 2>/dev/null | awk '$3 == "start" && $4 == "lab126_gui" { n++ } END { print n + 0 }'; }
appmgrd_restarts() { cat "$SB/calls.log" 2>/dev/null | awk '$3 == "stop" && $4 == "appmgrd" { n++ } END { print n + 0 }'; }
fw() { cat "$SB/state/lab126_gui"; }
lpid() { cat "$SB/run/launcher.pid" 2>/dev/null; }
wdpid() { sed -n 's/.*watching (pid \([0-9]*\)[,)].*/\1/p' "$SB/app/watchdog.log" 2>/dev/null | tail -1; }
# A process that exists and is not a zombie (a killed child stays a zombie until reaped).
alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null && case "$(ps -o stat= -p "$1" 2>/dev/null)" in Z*|'') return 1 ;; esac; }
waitfor() { local t=$(( $(date +%s) + $1 )); shift; until "$@"; do [ "$(date +%s)" -lt "$t" ] || return 1; sleep 0.2; done; }
running1() { [ "$(inks)" -ge 1 ]; }
fw_up() { grep -q start/running "$SB/state/lab126_gui"; }
in_calls() { grep -q "$1" "$SB/calls.log" 2>/dev/null; }
quiet() { sleep 2.5; }   # let the watchdog finish its last poll
# Stop everything of this sandbox, keep a copy of it.
finish() {
  local p
  touch "$SB/quit"; waitfor 20 fw_up; quiet
  for p in $(lpid) $(wdpid) $(ink_pids); do kill -9 "$p" 2>/dev/null; done
  pkill -9 -f "$SB/app/" 2>/dev/null
  cp -R "$SB" "$OUT/" 2>/dev/null
}

# 1. normal run, with positive controls for the process matchers
setup 1; launch
if waitfor 15 running1 && [ "$(fw)" = stop/waiting ]; then ok "1 launcher stops the framework and runs rinky ($(inks) found by pid file)"
else bad "1 no rinky ($(fw))"; fi
alive "$(wdpid)" && alive "$(lpid)" && ok "1 control: watchdog $(wdpid) and launcher $(lpid) alive" || bad "1 control: watchdog '$(wdpid)' launcher '$(lpid)' not found"
touch "$SB/quit"; waitfor 20 fw_up; quiet
[ "$(starts)" = 1 ] && [ "$(inks)" = 0 ] && ! alive "$(wdpid)" && ok "1 normal exit: framework started once, watchdog finished" \
  || bad "1 start lab126_gui called $(starts) times, $(inks) rinky left, watchdog alive: $(alive "$(wdpid)" && echo yes || echo no)"
finish

# 2. second launcher while the first runs
setup 2; launch; waitfor 15 running1
launch; sleep 3
[ "$(inks)" = 1 ] && grep -q "already running" "$SB/app/rinky.log" && ok "2 second launcher refused while the first runs" \
  || bad "2 $(inks) rinkys after a second launch"
finish

# 3. two launchers at the same instant, repeated
dup=0
for r in 1 2 3 4 5; do
  setup "3-$r"; launch & launch & wait
  sleep 5
  n=$(inks)
  stale=$(grep -c "removing stale lock" "$SB/app/rinky.log" 2>/dev/null)
  if [ "$n" != 1 ]; then
    dup=$((dup + 1))
    echo "     round $r: $n rinkys, 'removing stale lock' lines: ${stale:-0}"
  fi
  finish
done
[ "$dup" = 0 ] && ok "3 simultaneous launchers -> 1 rinky in 5 of 5 rounds" \
  || bad "3 simultaneous launchers -> more or fewer than 1 rinky in $dup of 5 rounds (lock race: see sandboxes sb3-*)"

# 3b. The same window made deterministic: a lock directory without launcher.pid is what a
#     second launcher sees while the first is between `mkdir lock` and writing its pid.
setup 3b; mkdir -p "$SB/run/lock"
launch; sleep 4
if grep -q "removing stale lock" "$SB/app/rinky.log" || [ "$(inks)" != 0 ] || ! grep -q "another launcher is starting" "$SB/app/rinky.log"; then
  bad "3b a lock without a pid (first launcher between mkdir and pid write) is taken as stale: second launcher proceeds (inks=$(inks))"
else
  ok "3b a lock without a pid is not taken over"
fi
finish

# 3c. A lock without a pid that is older than a minute (its launcher was killed between
#     mkdir and the pid write) is stale: the next start takes it over.
setup 3c; mkdir -p "$SB/run/lock"; touch -t "$(date -v-5M '+%Y%m%d%H%M.%S' 2>/dev/null || date -d '-5 min' '+%Y%m%d%H%M.%S')" "$SB/run/lock"
rm -f "$SB/quit"; launch; waitfor 15 running1
[ "$(inks)" = 1 ] && grep -q "removing stale lock (launcher without pid gone)" "$SB/app/rinky.log" \
  && ok "3c a lock without a pid older than a minute is taken over" || bad "3c inks=$(inks) $(tail -2 "$SB/app/rinky.log")"
finish

# 4. kill -9 rinky
setup 4; launch; waitfor 15 running1
kill -9 $(ink_pids); waitfor 20 fw_up; quiet
[ "$(starts)" = 1 ] && grep -q "rinky exited with 137" "$SB/app/rinky.log" && ok "4 kill -9 rinky: framework started once" \
  || bad "4 starts=$(starts) $(tail -2 "$SB/app/rinky.log")"
finish

# 5. kill -9 launcher during the framework-stop wait
setup 5; STOP_DELAY=6 launch
waitfor 5 test -n "$(lpid)"; sleep 1.5
kill -9 "$(lpid)" 2>/dev/null
waitfor 20 fw_up; sleep 8
[ "$(inks)" = 0 ] && fw_up && grep -q "launcher .* is gone" "$SB/app/watchdog.log" && ok "5 kill -9 launcher during the stop wait: watchdog restores, no rinky" \
  || bad "5 inks=$(inks) fw=$(fw) $(tail -2 "$SB/app/watchdog.log" 2>/dev/null)"
finish

# 6. kill -9 launcher while rinky runs
setup 6; launch; waitfor 15 running1
kill -9 "$(lpid)"; waitfor 20 fw_up; quiet
[ "$(inks)" = 0 ] && [ "$(starts)" = 1 ] && ok "6 kill -9 launcher: watchdog stops rinky and starts the framework once" \
  || bad "6 inks=$(inks) starts=$(starts)"
finish

# 7. kill -9 watchdog, then normal exit
setup 7; launch; waitfor 15 running1; sleep 1
w=$(wdpid); kill -9 "$w"
alive "$w" && bad "7 watchdog $w survived kill -9"
touch "$SB/quit"; waitfor 20 fw_up; quiet
[ "$(starts)" = 1 ] && ok "7 kill -9 watchdog: launcher still restores on exit" || bad "7 starts=$(starts) fw=$(fw)"
finish

# 8. launcher and watchdog both killed: nothing is left to restore the framework
setup 8; launch; waitfor 15 running1
kill -9 "$(wdpid)" "$(lpid)"; kill -9 $(ink_pids); sleep 3
[ "$(fw)" = stop/waiting ] && info "8 launcher + watchdog killed together: framework stays stopped (expected; nothing left to restore it)"
rm -f "$SB/quit"; launch; waitfor 15 running1
[ "$(inks)" = 1 ] && grep -q "removing stale lock" "$SB/app/rinky.log" && ok "8 stale lock from a killed launcher does not block the next start" \
  || bad "8 next start after stale lock: inks=$(inks)"
finish

# 9. kill -9 launcher while `start lab126_gui` runs (START_DELAY=4)
setup 9; START_DELAY=4 launch; waitfor 15 running1
touch "$SB/quit"
waitfor 10 in_calls " start lab126_gui"; sleep 1
kill -9 "$(lpid)" 2>/dev/null
waitfor 20 fw_up; sleep 8
fw_up && [ "$(inks)" = 0 ] && ok "9 kill -9 launcher inside 'start lab126_gui': framework running (start called $(starts)x, appmgrd restarted $(appmgrd_restarts)x)" \
  || bad "9 fw=$(fw) inks=$(inks) starts=$(starts)"
finish

# 10. kill -9 launcher during the appmgrd restart (APPMGRD_DELAY=4)
setup 10; APPMGRD_DELAY=4 launch; waitfor 15 running1
touch "$SB/quit"
waitfor 10 in_calls " stop appmgrd"; sleep 1
kill -9 "$(lpid)" 2>/dev/null
waitfor 25 fw_up; sleep 3
fw_up && ok "10 kill -9 launcher inside the appmgrd restart: framework running (start called $(starts)x, appmgrd restarted $(appmgrd_restarts)x)" \
  || bad "10 fw=$(fw) starts=$(starts) $(tail -3 "$SB/app/watchdog.log")"
finish

# 11. SIGTERM to the launcher while rinky runs
setup 11; launch; waitfor 15 running1
kill -TERM "$(lpid)"; waitfor 20 fw_up; quiet
[ "$(inks)" = 0 ] && [ "$(starts)" = 1 ] && grep -q "stopping rinky" "$SB/app/rinky.log" \
  && ok "11 SIGTERM to the launcher: rinky stopped, framework started once" || bad "11 inks=$(inks) starts=$(starts)"
finish

# 12. rinky fails at once
setup 12; INK_MODE=fail launch
waitfor 20 in_calls " start lab126_gui"; quiet
fw_up && [ "$(starts)" = 1 ] && grep -q "rinky exited with 3" "$SB/app/rinky.log" \
  && ok "12 rinky exits 3 at once: framework started once" || bad "12 fw=$(fw) starts=$(starts)"
finish

# 13. framework does not stop within the deadline (2 s, framework needs 30 s)
setup 13; DEADLINE=2 STOP_DELAY=30 launch
waitfor 15 in_calls " start lab126_gui"; quiet
[ "$(inks)" = 0 ] && fw_up && grep -q "giving up" "$SB/app/rinky.log" \
  && ok "13 framework stop timeout: no rinky, framework started again" || bad "13 inks=$(inks) fw=$(fw)"
finish

# 15. Amazon services stopped while rinky runs and started again
quieted() { [ "$(jobs_state)" = "stop/waiting stop/waiting " ]; }
jobs_state() { local j; for j in todo minerva_service; do printf '%s ' "$(cat "$SB/state/$j")"; done; }
for how in exit kill; do
  setup "15-$how"
  echo start/running >"$SB/state/todo"; echo start/running >"$SB/state/minerva_service"
  echo stop/waiting >"$SB/state/btmanagerd"   # not running: left alone
  # The stops run in the background while rinky starts.
  launch; waitfor 15 running1; waitfor 10 quieted
  during=$(jobs_state)
  if [ $how = exit ]; then touch "$SB/quit"; else kill -9 "$(lpid)"; fi
  waitfor 25 fw_up; quiet
  after=$(jobs_state)
  [ "$during" = "stop/waiting stop/waiting " ] && [ "$after" = "start/running start/running " ] \
    && [ "$(cat "$SB/state/btmanagerd")" = stop/waiting ] && [ ! -e "$SB/run/quieted" ] \
    && ok "15 Amazon services ($how): stopped while rinky runs, started again" \
    || bad "15 ($how) during=[$during] after=[$after] btmanagerd=$(cat "$SB/state/btmanagerd")"
  finish
done

# 14. random kill -9 points
left=0
for r in $(seq 1 "$ROUNDS"); do
  setup "14-$r"
  d=$(awk -v s="$RANDOM" 'BEGIN { srand(s); printf "%.1f", rand() * 4 }')
  who=$([ $((RANDOM % 2)) = 0 ] && echo launcher || echo rinky)
  STOP_DELAY=1 launch; sleep "$d"
  case $who in
    launcher) p=$(lpid) ;;
    rinky) p=$(ink_pids | head -1) ;;
  esac
  # Nothing to kill yet (rinky not started): the run continues; end it normally.
  # Otherwise the framework must come back by itself and no rinky may survive
  # (an orphan would draw over the Kindle UI).
  if [ -n "$p" ]; then kill -9 "$p" 2>/dev/null; else who="$who (not started yet; normal exit)"; touch "$SB/quit"; fi
  if waitfor 25 fw_up && sleep 3 && [ "$(inks)" = 0 ]; then
    echo "     round $r: kill -9 $who ${p:-(none yet)} after ${d}s -> framework back, starts=$(starts)"
  else
    left=$((left + 1))
    echo "     round $r: kill -9 $who ${p:-(none yet)} after ${d}s -> fw=$(fw) inks=$(inks)"
  fi
  finish
done
[ "$left" = 0 ] && ok "14 $ROUNDS random kill -9 points: framework always back, no rinky left" \
  || bad "14 $left of $ROUNDS random kill points left the framework stopped or an rinky running"

rm -rf "$TMP"/sb*
echo "launcher-race-test: $fails failed (sandboxes copied to $OUT)"
exit "$fails"
