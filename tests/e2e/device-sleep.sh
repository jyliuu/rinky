#!/bin/bash
# Phase 5 checks 3, 4, 11: the device layer owns the power key, the cover and
# inactivity with the framework stopped (`rinky device sleep`).
#  - first touch after start lands where tapped (kernel slot cache seeded; phase 0 saw 0,0);
#  - power key: frontlight off, screensaver (current page + badge), suspend requested
#    (RTC alarm + /sys/power/state; the emulator refuses with EBUSY: USB wakeup source);
#  - power key again: page restored with GC16, input re-grabbed, taps at the right place;
#  - cover (com.lab126.hal magSensorClosed/Opened): sleep and wake, with the chosen-image
#    screensaver (a self-made PNG, compared pixel for pixel by panel level with the screendump);
#  - low-battery event (RINKY_LOW_BATTERY=99 makes the emulator's ~59 % low);
#  - auto-sleep after 1 minute without input (RINKY_AUTOSLEEP=1), with the cover
#    screensaver (a cover canvas the app hands over: Power::set_screensaver(Cover)).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
python3 "$(dirname "$0")/device-helpers/make-screensaver-png.py" "$OUT/screensaver-image.png" || fail "make the test PNG"
"$SLOT" push "$NAME" "$OUT/screensaver-image.png" "$DEST/ss-test.png" >/dev/null || fail "push the test PNG"
# Put 636,700 into the kernel's slot cache (an inert spot on Home), so the first tap
# after the start arrives without ABS_MT_POSITION_X/Y.
tap 636 700
sleep 2
set_env RINKY_AUTOSLEEP=1 RINKY_SUSPEND_RETRY=30 RINKY_LOW_BATTERY=99
start_device device sleep --seconds 900 --screensaver "page,image:$DEST/ss-test.png,cover"
not_home awake
wait_log 10 "kernel slot cache at open slot 0 idle at 636,700"
tap 636 700
wait_log 20 "tap at 636,700"
log_lines "kernel slot cache\|tap at" | head -3 | tee "$OUT/first-touch.txt" >&2
pass "first touch after start at 636,700 (not 0,0); the kernel cache held 636,700, so the event had no position"
# Check 8: with the warning level at 99 % the emulator's battery (about 59 %) is "low".
wait_log 10 "battery: low ("
wait_log 10 "event: LowBattery("
log_lines "battery: low\\|LowBattery" | head -2 | tee "$OUT/low-battery.txt" >&2
pass "low-battery event (RINKY_LOW_BATTERY=99)"

power_key
wait_log 20 "screensaver drawn (PowerKey)"
sleep 3
f=$(shot screensaver-power-key)
is_home "$f" && fail "screensaver missing"
bl=$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')
[ "$bl" = 0 ] || fail "frontlight still on while asleep ($bl)"
pass "frontlight off while asleep"
wait_log 20 "suspend requested (rtc alarm"
wait_log 20 "suspend refused"
log_lines "suspend" | tee "$OUT/suspend-attempts.txt" >&2
guest 'cat /sys/power/suspend_stats/fail /sys/power/suspend_stats/last_failed_errno; for d in /sys/class/wakeup/*; do t=$(cat $d/active_time_ms); [ "$t" != 0 ] && echo "$(cat $d/name) active_time_ms=$t"; done' >"$OUT/wakeup-sources.txt" 2>&1
tap 400 1200
sleep 2
in_log "tap at 400,1200" && fail "a tap went through while asleep"
pass "touches ignored while asleep"

power_key
wait_log 20 "awake, screen restored (PowerKey)"
wait_log 10 "touch: re-grabbed"
wait_log 10 "next screensaver image"
sleep 3
not_home awake-after-power-key
bl=$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')
[ "$bl" != 0 ] || fail "frontlight not restored"
tap 400 1200
wait_log 20 "tap at 400,1200"
tap 800 1300
wait_log 20 "tap at 800,1300"
sleep 2
shot taps-after-wake >/dev/null
pass "screen restored and taps exact after the wake"

hal_event magSensorClosed
wait_log 20 "screensaver drawn (Cover)"
sleep 3
img=$(shot cover-closed-image-screensaver)
python3 "$(dirname "$0")/device-helpers/compare-levels.py" "$OUT/screensaver-image.png" "$img" >"$OUT/compare-image-levels.txt" 2>&1 ||
  fail "chosen-image screensaver differs from the PNG (see compare-image-levels.txt)"
pass "chosen-image screensaver shown pixel for pixel (16 panel levels)"
hal_event magSensorOpened
wait_log 20 "awake, screen restored (Cover)"
wait_log 10 "next screensaver cover"
sleep 2
pass "cover closes and opens"

log "waiting for auto-sleep (1 minute without input)"
wait_log 120 "screensaver drawn (Inactivity)"
sleep 3
shot auto-sleep-cover-screensaver >/dev/null
power_key
wait_log 20 "awake, screen restored (PowerKey)"
pass "auto-sleep after inactivity"

exit_device
clear_env
wait_clean_home 300 home-after
save_logs
check_restored
