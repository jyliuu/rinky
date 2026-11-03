#!/bin/bash
# e2e-cycles: 3
# The appmgrd "Application Error" alert after a framework restart (docs/ARCHITECTURE.md,
# "Sleep and the Kindle framework"). Three framework cycles without rinky:
#   plain    stop lab126_gui; start lab126_gui 5 s after KPPMainApp/pillowd are gone
#   slow     the same with 90 s between the stop and the start
#   restart  stop lab126_gui; stop appmgrd; start appmgrd; wait until appmgrd answers
#            on lipc; start lab126_gui  (what the launcher and the watchdog do)
# libappmgr.so (5.19.5) handles the `backgroundAppUnLoaded` property, which KPPMainApp's
# post-stop hook sets, as remove_app(uri, abnormalExit=1): it logs handle_app_failure
# and queues a relaunch; the relaunch fails with framework_not_initialized while
# lab126_gui is down. The `slow` cycle shows whether the alert depends on pillow being
# up when appmgrd shows it.
# Each cycle writes a syslog marker, then saves the appmgrd/KPPMainApp/lab126_gui lines
# of /var/log/messages after the marker, appmgrd's lipc properties before/after, a
# screenshot of the result and whether the alert appeared. A leftover alert is closed.
# Pass: the `restart` cycle ends on a clean Home. The `plain` result is recorded (the
# alert there depends on timing).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
set -o pipefail

fw_stopped() { [ -z "$(guest 'pidof KPPMainApp; pidof pillowd' 2>/dev/null)" ]; }
appmgrd_answers() { guest 'lipc-get-prop com.lab126.appmgrd activeApp >/dev/null 2>&1'; }
props() { guest 'echo "appmgrd activeApp=$(lipc-get-prop com.lab126.appmgrd activeApp 2>&1)"; echo "kaf frameworkStarted=$(lipc-get-prop com.lab126.kaf frameworkStarted 2>&1)"; status appmgrd; status lab126_gui; status kppmainapp' 2>&1; }
home_or_alert() { probe && { is_home "$PROBE" || app_error_now; }; }

cycle() {   # LABEL RESTART_APPMGRD [GAP_S]
  local label=$1 restart=$2 gap=${3:-5} t0 result
  log "cycle $label: appmgrd restart=$restart"
  props >"$OUT/$label-props-before.txt"
  guest "logger -t p0fix-mark 'cycle $label start'"
  t0=$(date +%s)
  guest 'stop lab126_gui' >>"$OUT/scenario.log" 2>&1
  wait_until 60 "framework stopped" fw_stopped || fail "$label: framework did not stop"
  props >"$OUT/$label-props-stopped.txt"
  guest "logger -t p0fix-mark 'cycle $label stopped'"
  sleep "$gap"
  if [ "$restart" = 1 ]; then
    guest 'stop appmgrd; start appmgrd' >>"$OUT/scenario.log" 2>&1
    wait_until 60 "appmgrd answers on lipc" appmgrd_answers || log "$label: appmgrd did not answer within 60 s"
    sleep 2
  fi
  guest "logger -t p0fix-mark 'cycle $label start lab126_gui'"
  guest 'cd / && start lab126_gui' >>"$OUT/scenario.log" 2>&1
  wait_until 300 "Home (or the alert) is shown" home_or_alert || fail "$label: neither Home nor the alert within $(scale 300) s"
  # The alert appears a few seconds after Home is drawn; watch for 40 s.
  result=clean
  local end=$(( $(date +%s) + $(scale 40) ))
  while [ "$(date +%s)" -lt "$end" ]; do
    probe && app_error_now && { result=alert; break; }
    sleep 3
  done
  # The alert evidence is the screendump that was checked (lib.sh keep_probe).
  if [ "$result" = alert ]; then keep_probe "$label-result" >/dev/null; else shot "$label-result" >/dev/null; fi
  props >"$OUT/$label-props-after.txt"
  echo "$label: $result after $(( $(date +%s) - t0 )) s" | tee -a "$OUT/results.txt"
  guest "awk '/p0fix-mark.*cycle $label start/ { f = 1 } f' /var/log/messages" >"$OUT/$label-messages.txt" 2>&1
  grep -E "p0fix-mark|appmgrd|KPPMainApp|lab126_gui|framework|blanket|pillow" "$OUT/$label-messages.txt" >"$OUT/$label-appmgrd.txt"
  grep -E "p0fix-mark|background_app_unloaded|handle_app_failure|framework_not_initialized|alert|set_pillow_prop_failed|SIGCHILD" "$OUT/$label-messages.txt" >"$OUT/$label-key-lines.txt"
  if [ "$result" = alert ]; then
    log "$label: alert shown; closing it"
    tap 958 960
  fi
  wait_clean_home 120 "$label-home"
  eval "result_$label=$result"
}

wait_home 240 home-before
guest 'cat /etc/upstart/appmgrd.conf; echo ----; cat /etc/upstart/lab126_gui.conf' >"$OUT/upstart-jobs.txt" 2>&1
cycle plain 0 5
cycle slow 0 90
cycle restart 1 5
{ echo "plain: $result_plain"; echo "slow: $result_slow"; echo "restart: $result_restart"; } >"$OUT/summary.txt"
[ "$result_restart" = clean ] || fail "the appmgrd restart did not prevent the alert"
pass "appmgrd restart -> clean Home (plain stop/start: $result_plain, 90 s gap: $result_slow)"
