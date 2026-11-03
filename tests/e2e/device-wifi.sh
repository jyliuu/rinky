#!/bin/bash
# Phase 5 checks 6 and 7 with the framework stopped: Wi-Fi through wifid/cmd lipc
# (status, scan, scan list, profiles, create a WPA profile with a password from stdin,
# delete it, radio off/on, connect a saved profile, current ESSID, signal, cmState,
# airplane mode via cmd wirelessEnable) and the clock (NTP sync when Wi-Fi connects,
# against an SNTP responder on the host at 127.0.0.1, reached from the guest as
# 10.0.2.2; never an outside server). `rinky device events` runs meanwhile and logs
# every wifid/cmd event and the automatic NTP sync.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

PORT=${SNTP_PORT:-$((12300 + $("$SLOT" port "$NAME") % 100))}
python3 "$(dirname "$0")/device-helpers/sntp-mock.py" --port "$PORT" --offset 75 >"$OUT/sntp-mock.log" 2>&1 &
MOCK=$!
trap 'kill $MOCK 2>/dev/null' EXIT
D=$DEST/rinky
wait_home 240 home-before
# A saved open profile for Kindle-QEMU (a fresh -snapshot boot has none).
wifi_join || fail "cannot join Kindle-QEMU"
# Start from the host's time (an earlier run may have left the +75 s).
guest "$D device clock set $(date +%s)" >/dev/null 2>&1
set_env "RINKY_NTP_SERVER=10.0.2.2:$PORT"
start_device device events 400
run() { log "\$ rinky device $*"; guest "$D device $*" 2>&1 | grep -v '^\[INFO' | tee -a "$OUT/wifi-commands.txt" >&2; }
run wifi status
run wifi scan
run wifi list
run wifi profiles
log "create a WPA profile, password on stdin"
echo 'fake-pass-1234' | guest "$D device wifi create rinky-wpa-test --password-stdin" 2>&1 | grep -v '^\[INFO' | tee -a "$OUT/wifi-commands.txt" >&2
run wifi profiles
grep -q 'essid: "rinky-wpa-test", netid: [0-9]*, secured: true' "$OUT/wifi-commands.txt" || fail "WPA profile not saved"
guest 'ps -eo args' | grep -q fake-pass && fail "the password is visible in a process list"
run wifi delete rinky-wpa-test
pass "WPA profile created from stdin and deleted"
run wifi enable 0
sleep 5
run wifi status
run wifi enable 1
sleep 5
run wifi connect Kindle-QEMU
grep -q 'connected to Kindle-QEMU' "$OUT/wifi-commands.txt" || fail "connect failed"
run wifi status
grep -q 'state: "CONNECTED", essid: Some("Kindle-QEMU")' "$OUT/wifi-commands.txt" || fail "state after connect"
sleep 5
run wifi status
pass "radio off/on and connect to the saved Kindle-QEMU profile"
wait_log 40 "ntp: 10.0.2.2:$PORT offset"
wait_log 10 "ntp: clock set"
log_lines "ntp:" | tee "$OUT/ntp-auto.txt" >&2
grep -q "RTC written" "$OUT/ntp-auto.txt" || fail "RTC not written after the NTP sync"
# The RTC follows the system clock (RTC_SET_TIME), so the time survives a reboot.
read -r sys_s rtc_s <<<"$(guest 'echo $(date +%s) $(cat /sys/class/rtc/rtc0/since_epoch)')"
echo "system $sys_s rtc $rtc_s host $(date +%s)" | tee "$OUT/rtc-after-ntp.txt" >&2
d=$((sys_s - rtc_s)); [ ${d#-} -le 2 ] || fail "RTC differs from the system clock by $d s"
d=$((sys_s - $(date +%s))); [ $d -ge 70 ] && [ $d -le 80 ] || fail "guest clock is $d s from the host, expected about +75"
pass "NTP sync when Wi-Fi connected (mock server, +75 s), RTC written"
run clock ntp "10.0.2.2:$PORT"
run clock now
run wifi airplane 1
[ "$(guest 'lipc-get-prop com.lab126.cmd wirelessEnable')" = 0 ] || fail "airplane mode: wirelessEnable not 0"
run wifi airplane 0
[ "$(guest 'lipc-get-prop com.lab126.cmd wirelessEnable')" = 1 ] || fail "airplane off: wirelessEnable not 1"
pass "airplane mode through cmd wirelessEnable"
log "wifid/cmd events seen by rinky:"
log_lines "WifiState\|com.lab126.cmd\|com.lab126.wifid" | head -40 | tee "$OUT/wifi-events.txt" >&2
grep -q 'WifiState("CONNECTED")' "$OUT/wifi-events.txt" || fail "no cmStateChange CONNECTED event"
# Put the guest clock back to the host's.
guest "$D device clock set $(date +%s)" >/dev/null 2>&1
guest "pid=\$(cat /tmp/rinky/rinky.pid); kill \$pid"
wait_until 60 "rinky stopped" reader_gone || fail "rinky did not stop"
clear_env
wait_clean_home 300 home-after
save_logs
check_no_watchers
check_restored
