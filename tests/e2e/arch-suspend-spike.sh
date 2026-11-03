#!/bin/bash
# e2e-cycles: 1
# Suspend/resume spike with the framework stopped (docs/ARCHITECTURE.md, "Sleep and the Kindle framework").
#
# What it shows on the 5.19.5 emulator:
#   - powerd events are recorded (lipc-wait-event -m -t com.lab126.powerd '*');
#   - the power key reaches rinky (evdev KEY_POWER); powerd itself ignores the button
#     without the framework ("Splash screen is on. Ignoring power button");
#   - rinky draws its own screensaver and tries a kernel suspend with an RTC alarm
#     (`spike-suspend --suspend 60`); the result (resumed / refused) is recorded;
#   - the next power-key press wakes: the screen is redrawn and taps still work;
#   - the framework comes back on exit.
# The emulator image keeps preventScreenSaver=1; this run sets 0 and restores 1.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

EV=/tmp/powerd-events.txt
power_state() { guest 'lipc-get-prop -s com.lab126.powerd state' 2>/dev/null; }
record_state() { log "powerd state: $(power_state)"; }
in_log() { guest "grep -q '$1' $DEST/rinky.log"; }
events() { guest "cat $EV" >"$OUT/powerd-events.txt" 2>&1; }

wait_home 240 home-before
guest 'lipc-get-prop com.lab126.powerd status' >"$OUT/powerd-status-before.txt" 2>&1
guest 'lipc-set-prop -i com.lab126.powerd preventScreenSaver 0'
log "preventScreenSaver now $(guest 'lipc-get-prop com.lab126.powerd preventScreenSaver')"
guest 'cat /sys/power/suspend_stats/success /sys/power/suspend_stats/fail' >"$OUT/suspend-stats-before.txt" 2>&1

# Event recorder, independent of rinky (-t prints a timestamp per line).
guest "rm -f $EV; (setsid lipc-wait-event -m -t com.lab126.powerd '*' >$EV 2>&1 </dev/null &)"
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sh $DEST/rinky.sh -- spike-suspend --seconds 1200 --suspend 60"
wait_until 90 "spike running, framework stopped" reader_running || fail "spike did not start"
nap 5
not_home spike-awake
record_state

log "press power (emu-slot.sh power: evdev KEY_POWER + powerd powerButton 1)"
"$SLOT" power "$NAME" | tee -a "$OUT/scenario.log"
wait_until 30 "rinky saw the power key and drew its screensaver" in_log 'power key sleep, screensaver drawn' ||
  fail "rinky did not handle the power key"
nap 3
not_home screensaver
wait_until 30 "suspend attempt finished" in_log 'spike: \(suspend refused\|resumed\)' || fail "no suspend result"
guest "grep -E 'suspend refused|resumed after' $DEST/rinky.log" | tee "$OUT/suspend-result.txt"
guest 'dmesg | grep -iE "PM: |suspend entry|suspend exit|Freezing|fts_ts_(suspend|resume)" | tail -20' >"$OUT/dmesg-pm.txt" 2>&1
guest 'cat /sys/power/suspend_stats/success /sys/power/suspend_stats/fail /sys/power/suspend_stats/last_failed_errno' >"$OUT/suspend-stats-after.txt" 2>&1
guest 'for d in /sys/class/wakeup/*; do t=$(cat $d/active_time_ms); [ "$t" != 0 ] && echo "$(cat $d/name) active_time_ms=$t"; done' >"$OUT/active-wakeup-sources.txt" 2>&1

# Give powerd time to react (it answers 20-90 s late on the emulator).
log "waiting $(scale 90) s for powerd"
nap 90
events
record_state
if grep -q goingToScreenSaver "$OUT/powerd-events.txt"; then
  log "powerd sent goingToScreenSaver with the framework stopped"
else
  log "powerd sent no goingToScreenSaver with the framework stopped"
fi

log "press power to wake"
"$SLOT" power "$NAME" | tee -a "$OUT/scenario.log"
wait_until 30 "awake screen redrawn after the power key" in_log 'power key wake, awake screen drawn' ||
  fail "rinky did not wake on the power key"
nap 3
not_home awake-again
pass "screen redrawn after the wake"

log "taps after wake"
tap 400 1200
nap 3
tap 800 1300
nap 4
not_home taps-after-wake
guest "grep 'tap at' $DEST/rinky.log" | tee "$OUT/taps.txt"
# wave-a/device maps raw touch units to panel pixels exactly (TouchMapping::pixels); the
# phase 0 decoder logged 799,1299 for this tap (one-pixel offset).
grep -q "tap at 800,1300" "$OUT/taps.txt" || fail "tap after wake not received at the right place"
pass "touch works after the wake"

tap 1200 60
wait_until 30 "spike exited" reader_gone || fail "spike did not exit"
events
wait_clean_home 300 home-after
save_logs
# powerd logs the button handling late; collect it at the end.
guest "grep -E 'pbprop|pbisplash|propName=frameworkStarted' /var/log/messages | grep powerd | tail -8" | tee "$OUT/powerd-button-log.txt"
guest 'for p in $(ps -eo pid,args | grep "lipc-wait-event -m -t com.lab126.powerd" | grep -v grep | while read pid rest; do echo $pid; done); do kill $p; done; lipc-set-prop -i com.lab126.powerd preventScreenSaver 1'
pass "suspend spike finished"
