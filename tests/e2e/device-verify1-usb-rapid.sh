#!/bin/bash
# Verifier cycle 1 (check 9): five quick unplug/plug cycles (QMP set_link usb off/on,
# 2 s apart) with the plan drive,charge (alternating per plug-in). After the cable
# settles plugged in and the emulator's export timeout passes: rinky still runs,
# every lock screen shown was removed, the panel shows no lock screen, volumd
# disableMounting is 1 while rinky runs and 0 after it exits.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"
H="$(dirname "$0")/device-helpers"

trap 'hmp "set_link usb on" >/dev/null 2>&1' EXIT
wait_home 240 home-before
wifi_join || fail "Wi-Fi needed for SSH while USB is off"
wguest true || fail "no SSH over the Wi-Fi port"
clear_env
start_device device usb --seconds 400 --plan drive,charge
wait_log 20 "usb: started"
pid=$(rinky_pid)
wcount() { wguest "grep -c -- '$1' $DEST/rinky.log"; }
wstop() { wguest "sh $DEST/rinky-ctl.sh stop" >/dev/null 2>&1 || true; }
reader_gone_wifi() { [ -z "$(wguest 'pidof rinky')" ]; }
for i in 1 2 3 4 5; do
  hmp "set_link usb off" >/dev/null || fail "QMP set_link off"
  sleep 2
  hmp "set_link usb on" >/dev/null || fail "QMP set_link on"
  sleep 2
done
log "settling 35 s (export timeout 20 s)"
sleep 35
[ "$(wguest 'pidof rinky')" = "$pid" ] || fail "rinky $pid did not survive the plug cycles"
shown=$(wcount "usb: lock screen shown")
removed=$(wcount "usb: lock screen removed")
plugs=$(wcount "event: Usb(true)")
unplugs=$(wcount "event: Usb(false)")
dm=$(wguest 'lipc-get-prop com.lab126.volumd disableMounting')
f=$(shot settled)
frame=$(python3 "$H/region-mean.py" "$f" 2 600 25 1400)
log "plug-ins $plugs, unplugs $unplugs, lock screens shown $shown / removed $removed, disableMounting $dm, left frame mean $frame"
wguest "grep -E 'usb:|event: Usb' $DEST/rinky.log" >"$OUT/usb-lines.txt" 2>&1
[ "$shown" = "$removed" ] || fail "lock screen shown $shown times but removed $removed times"
[ "$frame" -gt 150 ] || fail "the panel still shows the lock-screen frame (mean $frame)"
[ "$dm" = 1 ] || fail "disableMounting is $dm while rinky runs (charge only / ask expect 1)"
pass "5 rapid plug cycles: no stuck lock screen, disableMounting 1, rinky alive"
wstop
wait_until 60 "rinky exited" reader_gone_wifi || fail "rinky did not exit"
if ! wait_until 120 "USB SSH answers again" guest true; then
  guest() { wguest "$@"; }
fi
wait_clean_home 300 home-after
wguest "cat $DEST/rinky.log" >"$OUT/rinky.log" 2>&1
check_no_watchers
check_restored
