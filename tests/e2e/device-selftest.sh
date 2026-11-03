#!/bin/bash
# Phase 5 check 2: daemons and lipc properties after `stop lab126_gui`, plus every
# device subsystem check (`rinky selftest --lipc --device --write`), run through the
# launcher so the framework is stopped. Writes the full report to rinky.log.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_device selftest --lipc --device --write --book "$DEST/juliet.epub"
wait_until 120 "selftest finished" reader_gone || fail "selftest did not finish"
wait_clean_home 300 home-after
save_logs
grep -E "^(PASS|FAIL|SKIP) " "$OUT/rinky.log" >"$OUT/selftest-report.txt"
cat "$OUT/selftest-report.txt" >&2
grep -q "^PASS device.daemons.framework: lab126_gui: lab126_gui stop/waiting" "$OUT/rinky.log" ||
  fail "selftest did not run with the framework stopped"
grep "^PASS device.daemons.framework" "$OUT/rinky.log" | grep -q "com.lab126.kaf" &&
  fail "com.lab126.kaf still listed with the framework stopped"
pass "framework stopped during the checks; com.lab126.kaf gone"
for c in daemons.system lipc.powerd lipc.wifid lipc.cmd lipc.volumd lipc.hash_props lipc.subscriptions \
         lipc.timeout_kills_child input.touch input.power_key fb.night_flag light.get light.set battery \
         wifi.state usb clock power.suspend_ready power.rtc_alarm; do
  grep -q "^PASS device.$c:" "$OUT/rinky.log" || fail "device.$c did not pass"
done
pass "all device checks passed with the framework stopped"
grep -q "selftest: .* 0 failed" "$OUT/rinky.log" || fail "selftest reported failures"
check_restored
