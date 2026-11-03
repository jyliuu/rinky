#!/bin/bash
# e2e-cycles: 2
# Verifier (p0fix cycle 1): a direct test of the documented root cause of the appmgrd
# "Application Error" dialog (docs/ARCHITECTURE.md, "Sleep and the Kindle framework"):
# the dialog appears when appmgrd handles KPPMainApp's `backgroundAppUnLoaded` (and
# sends its appmgrAppFailedFatal alert) only after pillow is back; restarting appmgrd
# before `start lab126_gui` drops that pending work.
# appmgrd is frozen (SIGSTOP) before `stop lab126_gui`, so the hook's lipc set waits in
# appmgrd's queue and its whole handling is delayed past the framework start:
#   frozen   freeze appmgrd; stop lab126_gui; start lab126_gui; wait until pillowd runs
#            (Home itself cannot appear: KPPMainApp's launch goes through appmgrd);
#            SIGCONT appmgrd; wait for Home; watch 60 s.          Expected: dialog.
#   restart  freeze appmgrd; stop lab126_gui; stop appmgrd; start appmgrd (the
#            launcher's restart; upstart kills the frozen process); start lab126_gui;
#            watch 60 s.                                         Expected: clean Home.
# Each cycle records /var/log/messages continuously (tail -F; the guest rotates the log
# within minutes) and keeps the appmgrd key lines. appmgrd is always continued on exit.
# Result lines in results.txt. P0FIX_CYCLES (default "frozen restart") selects cycles.
# First run (verify-1/e2e-6): with the old wait for Home, Home never appeared while
# appmgrd was frozen; after SIGCONT the dialog appeared ("inconclusive(alert)").
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
set -o pipefail

fw_stopped() { [ -z "$(guest 'pidof KPPMainApp; pidof pillowd' 2>/dev/null)" ]; }
appmgrd_answers() { guest 'lipc-get-prop com.lab126.appmgrd activeApp >/dev/null 2>&1'; }
pillow_up() { [ -n "$(guest 'pidof pillowd' 2>/dev/null)" ]; }
thaw() { guest 'p=$(pidof appmgrd); [ -n "$p" ] && kill -CONT $p; true' >/dev/null 2>&1; }
trap thaw EXIT
appmgrd_state() { guest 'p=$(pidof appmgrd); echo "appmgrd pid=$p state=$(cut -d" " -f3 /proc/$p/stat 2>/dev/null)"' 2>&1; }

watch_alert() {   # SECONDS -> prints alert|clean
  local end=$(( $(date +%s) + $(scale "$1") ))
  while [ "$(date +%s)" -lt "$end" ]; do
    probe && app_error_now && { echo alert; return; }
    sleep 3
  done
  echo clean
}

syslog_start() {   # LABEL: tail -F the syslog into /mnt/us (rotation-proof)
  guest "rm -f /mnt/us/p0fix-syslog-$1.txt; (setsid tail -n 0 -F /var/log/messages >/mnt/us/p0fix-syslog-$1.txt 2>/dev/null </dev/null & echo \$! >/mnt/us/p0fix-tail-$1.pid); true"
}
syslog_after() {   # LABEL
  # Kill by pid: `pkill -f PATTERN` would also match the guest shell running this line.
  guest "kill \$(cat /mnt/us/p0fix-tail-$1.pid) 2>/dev/null; cat /mnt/us/p0fix-syslog-$1.txt" >"$OUT/$1-messages.txt" 2>&1
  grep -E "p0fix-verify|background_app_unloaded|handle_app_failure|framework_not_initialized|alert|set_pillow_prop_failed|FM Metrics" \
    "$OUT/$1-messages.txt" >"$OUT/$1-key-lines.txt"
}

cycle() {   # LABEL RESTART(0|1)
  local label=$1 restart=$2 result note=""
  log "cycle $label"
  syslog_start "$label"
  sleep 1
  guest "logger -t p0fix-verify 'cycle $label start'"
  guest 'kill -STOP $(pidof appmgrd)' || fail "$label: could not stop appmgrd"
  appmgrd_state | tee "$OUT/$label-appmgrd-frozen.txt"
  guest '(stop lab126_gui >/mnt/us/p0fix-stop.log 2>&1 &); true'
  wait_until 120 "framework stopped (appmgrd frozen)" fw_stopped || { thaw; fail "$label: framework did not stop within 120 s"; }
  guest "logger -t p0fix-verify 'cycle $label stopped'"
  if [ "$restart" = 1 ]; then
    guest "logger -t p0fix-verify 'cycle $label restart appmgrd'"
    guest 'timeout 90 sh -c "stop appmgrd; start appmgrd"' >>"$OUT/scenario.log" 2>&1
    wait_until 60 "new appmgrd answers on lipc" appmgrd_answers || log "$label: appmgrd did not answer within 60 s"
    appmgrd_state | tee "$OUT/$label-appmgrd-new.txt"
  fi
  guest "logger -t p0fix-verify 'cycle $label start lab126_gui'"
  guest '(cd / && start lab126_gui >/mnt/us/p0fix-start.log 2>&1 &); true'
  if [ "$restart" = 0 ]; then
    wait_until 120 "pillowd runs (appmgrd still frozen)" pillow_up || note="pillowd did not start within $(scale 120) s; "
    nap 20
    shot "$label-before-thaw" >/dev/null
    guest "logger -t p0fix-verify 'cycle $label SIGCONT appmgrd'"
    thaw
  fi
  if wait_until 300 "Home drawn" home_now; then
    nap 5
    result=$(watch_alert 60)
  else
    shot "$label-no-home" >/dev/null
    thaw
    note="${note}Home did not appear within $(scale 300) s; "
    result="inconclusive($(watch_alert 60))"
  fi
  # watch_alert decided on the probe screendump: keep that frame as the evidence.
  case "$result" in *alert*) keep_probe "$label-result" >/dev/null ;; *) shot "$label-result" >/dev/null ;; esac
  syslog_after "$label"
  echo "$label: $result ${note}" | tee -a "$OUT/results.txt"
  if probe && app_error_now; then
    log "$label: closing the dialog"
    tap 958 960
  fi
  wait_clean_home 180 "$label-home"
  eval "result_$label=\$result"
}

wait_home 240 home-before
: >"$OUT/results.txt"
CYCLES=${P0FIX_CYCLES:-frozen restart}
result_frozen=not-run result_restart=not-run
case " $CYCLES " in *" frozen "*) cycle frozen 0 ;; esac
case " $CYCLES " in *" restart "*) cycle restart 1 ;; esac
{ echo "frozen: $result_frozen"; echo "restart: $result_restart"; } >"$OUT/summary.txt"
case "$result_frozen" in alert|not-run) ;; *) fail "frozen cycle: $result_frozen (expected the dialog; see frozen-key-lines.txt)" ;; esac
case "$result_restart" in clean|not-run) ;; *) fail "restart cycle: $result_restart (expected clean Home)" ;; esac
pass "appmgrd delayed past the framework start -> dialog; appmgrd restart -> clean Home"
