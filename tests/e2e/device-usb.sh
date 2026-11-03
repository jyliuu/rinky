#!/bin/bash
# Phase 5 check 9: USB cable detection, drive mode and charge only, with QMP
# `set_link usb off/on` as the cable (USB VBUS follows the USB NIC link on this emulator:
# /sys/class/udc/11211000.usb/state changes; SSH then uses the Wi-Fi port).
#  1. plug-in 1 -> the test page asks for drive mode: lock screen, volumd
#     disableMounting 0; the emulator cannot export (USB is its network,
#     useUsbForNetwork 1), so after 20 s the export times out: lock screen removed, page
#     back, library rescan signal;
#  2. plug-in 2 -> charge only: no lock screen, disableMounting stays 1;
#  3. hal usbPlugOut/usbPlugIn events (as udev's notifyusb sends them) are seen.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

trap 'hmp "set_link usb on" >/dev/null 2>&1' EXIT
wait_home 240 home-before
wifi_join || fail "Wi-Fi needed for SSH while USB is off"
wguest true || fail "no SSH over the Wi-Fi port"
clear_env
start_device device usb --seconds 400 --plan drive,charge
wait_log 20 "usb: started, cable Ok(Computer)"
not_home usb-test-page
guest 'cat /sys/class/udc/11211000.usb/state; lipc-get-prop com.lab126.volumd disableMounting' >"$OUT/udc-state-plugged.txt"
wlog() { wguest "grep -q -- '$1' $DEST/rinky.log"; }
wwait() { wait_until "$1" "log: $2" wlog "$2" || { wguest "cat $DEST/rinky.log" >"$OUT/rinky.log"; fail "not in log: $2"; }; }
wcount() { wguest "grep -c -- '$1' $DEST/rinky.log"; }
unplugged_twice() { [ "$(wcount 'usb: cable Computer -> None')" -ge 2 ]; }
wstop() { wguest "sh $DEST/rinky-ctl.sh stop" >/dev/null 2>&1 || true; }
reader_gone_wifi() { [ -z "$(wguest 'pidof rinky')" ]; }

log "set_link usb off (cable out)"
hmp "set_link usb off" >/dev/null || fail "QMP set_link off"
wwait 30 "usb: cable Computer -> None"
wwait 10 "event: Usb(false)"
wguest 'cat /sys/class/udc/11211000.usb/state' >"$OUT/udc-state-unplugged.txt"
log "set_link usb on (cable in, plug-in 1: drive mode)"
hmp "set_link usb on" >/dev/null || fail "QMP set_link on"
wwait 30 "usb: cable None -> Computer"
wwait 10 "usb: drive mode chosen"
wwait 10 "usb: lock screen shown"
sleep 3
shot usb-lock-screen >/dev/null
[ "$(wguest 'lipc-get-prop com.lab126.volumd disableMounting')" = 0 ] || fail "drive mode: disableMounting not 0"
wwait 40 "export did not start within"
wwait 10 "usb: lock screen removed, page restored"
wwait 10 "usb: library rescan signal"
sleep 3
shot usb-after-drive-mode >/dev/null
[ "$(wguest 'lipc-get-prop com.lab126.volumd disableMounting')" = 1 ] || fail "after drive mode: disableMounting not 1"
pass "plug-in 1: cable detection, drive-mode request, lock screen, export timeout, rescan"

log "set_link usb off/on (plug-in 2: charge only)"
locks=$(wcount "usb: lock screen shown")
hmp "set_link usb off" >/dev/null || fail "QMP set_link off"
wait_until 30 "cable out again" unplugged_twice || fail "second unplug not seen"
hmp "set_link usb on" >/dev/null || fail "QMP set_link on"
wwait 30 "usb: charge only chosen"
wwait 10 "usb: charge only (disableMounting 1, no export)"
sleep 5
shot usb-charge-only >/dev/null
[ "$(wcount 'usb: lock screen shown')" = "$locks" ] || fail "charge only showed the lock screen"
[ "$(wguest 'lipc-get-prop com.lab126.volumd disableMounting')" = 1 ] || fail "charge only: disableMounting not 1"
pass "plug-in 2: charge only (no lock screen, disableMounting 1)"

log "hal usbPlugOut / usbPlugIn (udev's notifyusb)"
wguest "lipc-send-event com.lab126.hal usbPlugOut; sleep 2; lipc-send-event com.lab126.hal usbPlugIn" || fail "hal events"
sleep 3
wwait 10 "com.lab126.hal usbPlugOut"
wwait 10 "com.lab126.hal usbPlugIn"
wguest "grep -E 'hal|usb' $DEST/rinky.log | tail -30" >"$OUT/hal-usb.txt"
pass "hal usbPlugOut/usbPlugIn events received"

# Exit over the Wi-Fi port (USB networking may still be coming back).
wstop
wait_until 60 "rinky exited" reader_gone_wifi || fail "rinky did not exit"
if ! wait_until 120 "USB SSH answers again" guest true; then
  log "USB SSH did not come back; the rest of the checks use the Wi-Fi port"
  guest() { wguest "$@"; }
fi
wait_clean_home 300 home-after
wguest "cat $DEST/rinky.log" >"$OUT/rinky.log" 2>&1
wguest "cat $DEST/watchdog.log" >"$OUT/watchdog.log" 2>&1
wguest 'ps -eo pid,comm,args | grep -E "rinky|KPPMainApp|pillowd|lipc-wait" | grep -v grep' >"$OUT/ps.txt" 2>&1
check_no_watchers
check_restored
