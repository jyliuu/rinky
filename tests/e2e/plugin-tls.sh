#!/bin/bash
# HTTPS from a Lua plugin in the real reader: the plugin `tlscheck` (hosts =
# gutenberg.org) streams one public-domain text (Project Gutenberg #1513) over TLS
# (ureq + rustls + webpki roots, built for ARM) through the emulated Wi-Fi.
# Needs internet access from the emulator: runs only with INK_E2E_INTERNET=1; otherwise
# it exits 77 (`skip`), which run.sh lists as SKIP, not PASS.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
PLUG=$DEST/plugins
SRC=$(dirname "$0")/plugin-fixtures

if [ "${INK_E2E_INTERNET:-0}" != 1 ]; then
  skip "set INK_E2E_INTERNET=1 to fetch one text from www.gutenberg.org"
fi
trap 'guest "rm -rf $PLUG" >/dev/null 2>&1' EXIT

wait_home 240 home-before
wlan_up() { guest 'ifconfig wlan0 | grep -q "inet addr:10.0.2."' 2>/dev/null; }
if ! wlan_up; then
  "$SLOT" wifi "$NAME" >"$OUT/wifi.txt" 2>&1 || {
    guest 'wpa_cli -i wlan0 reassociate' >>"$OUT/wifi.txt" 2>&1
    wait_until 60 "wlan0 has a 10.0.2.x address" wlan_up || fail "Wi-Fi join failed: $(cat "$OUT/wifi.txt")"
  }
fi
guest "rm -rf $PLUG; mkdir -p $PLUG/tlscheck" || fail "mkdir"
"$SLOT" push "$NAME" "$SRC/tlscheck/plugin.toml" "$SRC/tlscheck/main.lua" "$PLUG/tlscheck/" >/dev/null || fail "push tlscheck"
finished() { guest "grep -q 'tls done\|tls error' $DEST/rinky.log" 2>/dev/null; }
start_reader
not_home page1
# The phase 0 reader delivers plugin network events on page turns.
for i in $(seq 1 15); do
  finished && break
  tap 1000 900
  sleep 4
done
exit_device
save_logs
grep "plugin" "$OUT/rinky.log" >"$OUT/plugin-lines.txt"
grep -E "plugin tlscheck: tls (response|done|error)" "$OUT/rinky.log" | tee "$OUT/tls.txt"
grep -qE "plugin tlscheck: tls response 200 text/plain" "$OUT/rinky.log" || fail "no HTTPS response"
grep -qE "plugin tlscheck: tls done status 200, [0-9]{5,} bytes in [0-9]+ chunks, has ROMEO true" "$OUT/rinky.log" || fail "HTTPS body not streamed"
wait_clean_home 300 home-after
pass "HTTPS streaming from a plugin over Wi-Fi"
