#!/bin/bash
# Plugin network access in the real reader over the emulated Wi-Fi: a mock server on the
# host (127.0.0.1, seen by the guest as 10.0.2.2) streams SSE events and chunks and
# answers a JSON POST; the Lua plugin `netcheck` (permission `network`, hosts = 10.0.2.2)
# logs every event as it arrives while pages turn. A request to a host outside `hosts`
# and a request from a plugin without the permission are refused before connecting.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
PLUG=$DEST/plugins
SRC=$(dirname "$0")/plugin-fixtures

wait_home 240 home-before
wlan_up() { guest 'ifconfig wlan0 | grep -q "inet addr:10.0.2."' 2>/dev/null; }
if ! wlan_up; then
  # `emu-slot.sh wifi` selects the network; on 5.19.5 the supplicant sometimes stays in
  # SCANNING (ap_scan=2) until it is told to reassociate.
  "$SLOT" wifi "$NAME" >"$OUT/wifi.txt" 2>&1 || {
    log "wifi: no address yet ($(tail -1 "$OUT/wifi.txt")); reassociating"
    guest 'wpa_cli -i wlan0 reassociate' >>"$OUT/wifi.txt" 2>&1
    wait_until 60 "wlan0 has a 10.0.2.x address" wlan_up || fail "Wi-Fi join failed: $(cat "$OUT/wifi.txt")"
  }
fi
guest 'ifconfig wlan0 | grep "inet addr"; wpa_cli -i wlan0 status | grep -E "^(ssid|wpa_state)"' >>"$OUT/wifi.txt" 2>&1
log "wifi: $(grep 'inet addr' "$OUT/wifi.txt" | tail -1)"
rm -f "$OUT/port"
python3 -I "$SRC/mock_server.py" "$OUT/port" 2>"$OUT/mock-server.log" &
MOCK=$!
# Stop the server and leave no plugins behind (also after a failure).
trap 'kill $MOCK 2>/dev/null; guest "rm -rf $PLUG" >/dev/null 2>&1' EXIT
for _ in 1 2 3 4 5 6 7 8 9 10; do [ -s "$OUT/port" ] && break; sleep 0.5; done
PORT=$(cat "$OUT/port") || fail "mock server did not start"
log "mock server on 127.0.0.1:$PORT (guest: 10.0.2.2:$PORT)"
guest "rm -rf $PLUG; mkdir -p $PLUG/netcheck $PLUG/nonet" || fail "mkdir"
for p in netcheck nonet; do
  "$SLOT" push "$NAME" "$SRC/$p/plugin.toml" "$SRC/$p/main.lua" "$PLUG/$p/" >/dev/null || fail "push $p"
done
guest "echo '{\"base\": \"http://10.0.2.2:$PORT\"}' > $PLUG/netcheck/settings.json" || fail "settings.json"

done_all() { guest "grep -c 'sse stream done\|chunk stream done\|json status 200' $DEST/rinky.log" 2>/dev/null | grep -qx 3; }
start_reader
not_home page1
# The phase 0 reader delivers plugin network events on page turns; turn pages until all
# three requests finished.
for i in 1 2 3 4 5 6 7 8; do
  done_all && break
  tap 1000 900
  sleep 3
done
not_home pages-turned
exit_device
save_logs
cp "$OUT/mock-server.log" "$OUT/mock-server.txt" 2>/dev/null
grep "plugin" "$OUT/rinky.log" >"$OUT/plugin-lines.txt"
check() { grep -qE "$1" "$OUT/rinky.log" || fail "rinky.log lacks: $1"; log "log has: $1"; }
check "plugin netcheck: starting requests to http://10.0.2.2:$PORT"
check "plugin netcheck: sse delta: one"
check "plugin netcheck: sse delta: two"
check "plugin netcheck: sse delta: three"
check "plugin netcheck: sse done: \[DONE\]"
check "plugin netcheck: sse stream done 200"
check "plugin netcheck: chunk of [0-9]+ bytes"
check "plugin netcheck: chunk stream done 200"
check "plugin netcheck: json status 200, answer 42, echo life"
check "plugin netcheck: other host refused: true \(.*not in hosts of plugin.toml"
check "plugin nonet: without permission refused: true \(.*permission denied: network"
grep -q '"GET /sse' "$OUT/mock-server.log" || fail "mock server saw no /sse request"
grep -q '"POST /json' "$OUT/mock-server.log" || fail "mock server saw no /json request"
wait_clean_home 300 home-after
pass "streaming plugin requests over Wi-Fi, permission and host checks"
