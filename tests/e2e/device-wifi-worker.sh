#!/bin/bash
# The network worker (crates/ink-device/src/netman.rs) in the full app on the Kindle:
# with Kindle-QEMU joined before the start, the worker sees the connection from the
# link (wlan0 up with an address), shows the toast and keeps "Wi-Fi was on" in the
# settings; after wpa_supplicant drops the association it notices the loss and the
# connection coming back (wifid rejoins by itself in the emulator, within seconds; a
# quick rejoin of the same network gets no second toast); a sleep and wake reach it.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
wifi_join || fail "cannot join Kindle-QEMU"
guest "cat /sys/class/net/wlan0/operstate; ifconfig wlan0 | grep 'inet addr'" >"$OUT/link-before.txt" 2>&1
start_reader
wait_log 60 'net: status Connected { essid: "Kindle-QEMU"'
wait_log 20 'wifi: toast "Connected to Kindle-QEMU"'
pass "the worker saw the connection from the link and said so"
wait_until 20 "settings keep wifi_was_on" guest "grep -q 'wifi_was_on = true' $DEST/settings.toml" ||
  fail "wifi_was_on not saved in settings.toml"
pass "Wi-Fi was on, kept in the settings"
# Drop the association behind wifid's back: the address goes, a probe sees it.
guest 'wpa_cli -i wlan0 disconnect' >"$OUT/disconnect.txt" 2>&1
wait_log 90 'net: connection lost'
pass "lost connection noticed"
wait_until 120 "connected again" guest "sed -n '/net: connection lost/,\$p' $DEST/rinky.log | grep -q 'net: status Connected { essid: \"Kindle-QEMU\"'" ||
  fail "not connected again after the loss"
[ "$(guest "grep -c 'wifi: toast \"Connected to Kindle-QEMU\"' $DEST/rinky.log")" = 1 ] || fail "a toast for the quick rejoin"
pass "connected again, without a second toast"
power_key
wait_log 30 "net: Awake(false)"
power_key
wait_log 30 "net: Awake(true)"
pass "sleep and wake reach the worker"
exit_device
wait_clean_home 300 home-after
save_logs
grep -E "net: |wifi: " "$OUT/rinky.log" | head -60 >"$OUT/net-lines.txt"
check_no_watchers
check_restored
