#!/bin/bash
# Verifier cycle 1 (checks 3, 9): the power key while the USB drive-mode lock screen is
# up. Plug in (QMP set_link usb on) with the drive-mode plan: lock screen. Power: the
# screensaver. Power again: the device is awake and still in drive mode, so the panel
# must show the lock screen again (pixel for pixel), not the screensaver.
# The emulator cannot export (USB networking), so drive mode ends by itself 20 s after
# the plug-in; the checks run before that.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"
H="$(dirname "$0")/device-helpers"

trap 'hmp "set_link usb on" >/dev/null 2>&1' EXIT
wait_home 240 home-before
wifi_join || fail "Wi-Fi needed for SSH while USB is off"
wguest true || fail "no SSH over the Wi-Fi port"
clear_env
start_device device usb --seconds 300 --plan drive
wait_log 20 "usb: started"
wlog() { wguest "grep -q -- '$1' $DEST/rinky.log"; }
wwait() { wait_until "$1" "log: $2" wlog "$2" || { wguest "cat $DEST/rinky.log" >"$OUT/rinky.log"; fail "not in log: $2"; }; }
wstop() { wguest "sh $DEST/rinky-ctl.sh stop" >/dev/null 2>&1 || true; }
wpower() { wguest "$DEST/rinky inject power" >/dev/null || fail "inject power"; }
reader_gone_wifi() { [ -z "$(wguest 'pidof rinky')" ]; }

hmp "set_link usb off" >/dev/null || fail "QMP set_link off"
wwait 30 "usb: cable Computer -> None"
hmp "set_link usb on" >/dev/null || fail "QMP set_link on"
wwait 30 "usb: lock screen shown"
sleep 2
lock=$(shot lock-screen)
wpower
wwait 15 "screensaver drawn (PowerKey)"
sleep 2
shot lock-then-screensaver >/dev/null
wpower
wwait 15 "awake, screen restored (PowerKey)"
sleep 2
after=$(shot awake-in-drive-mode)
wlog "usb: lock screen removed" && fail "drive mode ended before the check (export timeout came early)"
python3 "$E2E_ROOT/scripts/png-compare.py" "$lock" "$after" >"$OUT/compare-lock-after-wake.txt" 2>&1
same=$?
log "lock screen vs. panel after the wake: $(cat "$OUT/compare-lock-after-wake.txt")"
python3 "$H/region-mean.py" "$after" 1060 1600 1230 1655 >"$OUT/badge-corner-after-wake.txt"
wwait 40 "usb: lock screen removed, page restored"
sleep 3
shot after-drive-mode >/dev/null
wstop
wait_until 60 "rinky exited" reader_gone_wifi || fail "rinky did not exit"
if ! wait_until 120 "USB SSH answers again" guest true; then
  guest() { wguest "$@"; }
fi
wait_clean_home 300 home-after
wguest "cat $DEST/rinky.log" >"$OUT/rinky.log" 2>&1
[ $same = 0 ] || fail "awake in drive mode, but the panel does not show the lock screen (badge corner mean $(cat "$OUT/badge-corner-after-wake.txt"))"
pass "after sleep and wake in drive mode the lock screen is back"
check_no_watchers
check_restored
