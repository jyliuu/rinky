#!/bin/bash
# The sample Lua plugin in the real reader (framework stopped by the launcher).
# Installs plugins/sample-lua (plugin.toml + main.lua) into /mnt/us/rinky/plugins,
# starts the reader on juliet.epub, turns pages (next, next, previous) and exits.
# Checks rinky.log for: the plugin loaded at start with its registrations,
# on_book_open, on_page_change per page shown, the status widget text, on_close and the
# session toast; the sidecar for the plugin's per-book data; data/sessions.csv.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
PLUG=$DEST/plugins

page_changes() { guest "grep -c 'plugins: on_page_change([0-9]*) -> sample-lua' $DEST/rinky.log" 2>/dev/null || echo 0; }
at_least() { [ "$(page_changes)" -ge "$1" ]; }

# Leave no plugins behind for later scenarios (also after a failure).
trap 'guest "rm -rf $PLUG" >/dev/null 2>&1' EXIT

wait_home 240 home-before
guest "rm -rf $PLUG; mkdir -p $PLUG/sample-lua" || fail "mkdir $PLUG"
"$SLOT" push "$NAME" "$E2E_ROOT/plugins/sample-lua/plugin.toml" "$E2E_ROOT/plugins/sample-lua/main.lua" "$PLUG/sample-lua/" >/dev/null || fail "push plugin"
# Fast on_page_change calls are logged at debug.
READER_LOG=debug start_reader
wait_until 30 "first page shown to the plugin" at_least 1 || fail "no on_page_change in the log"
not_home page1
tap 1000 900; wait_until 20 "page 2" at_least 2 || fail "page 2 not reported to the plugin"; sleep 2; not_home page2
tap 1000 900; wait_until 20 "page 3" at_least 3 || fail "page 3 not reported to the plugin"; sleep 2; not_home page3
tap 150 900;  wait_until 20 "back to page 2" at_least 4 || fail "page 2 again not reported"; sleep 2; not_home page2-again
exit_device
save_logs
grep "plugin" "$OUT/rinky.log" >"$OUT/plugin-lines.txt"
check() { grep -qF "$1" "$OUT/rinky.log" || fail "rinky.log lacks: $1"; log "log has: $1"; }
grep -qE "plugins: [0-9]+ compiled-in, 1 Lua plugin\(s\) in $PLUG" "$OUT/rinky.log" || fail "the Lua plugin was not found at start"
check "plugins: loaded sample-lua 0.1.0 (lua, enabled) permissions [document, ui, storage]"
check "plugins: sample-lua registered 2 menu items, 1 selection actions, 1 gestures, 1 settings pages, 1 library actions, 1 status widgets"
check "plugins: on_book_open -> sample-lua"
check "plugin sample-lua: book opened: Romeo and Juliet, read so far 0 min"
check "plugins: sample-lua: status timer = \"Read 0 min\""
check "plugins: on_page_change(1) -> sample-lua"
check "plugins: on_page_change(3) -> sample-lua"
check "plugins: on_close -> sample-lua"
check "plugins: sample-lua: toast \"Session: 0 min, 3 pages\""
if grep -E "WARN.*plugins: sample-lua: |-> sample-lua .*failed" "$OUT/rinky.log"; then fail "sample-lua errors in the log"; fi
guest "cat $DEST/juliet.sdr/rinky.epub.json" >"$OUT/sidecar.json"
python3 - "$OUT/sidecar.json" <<'PY' || fail "sidecar lacks the plugin's per-book data"
import json, sys
d = json.load(open(sys.argv[1]))["plugins"]["sample-lua"]
assert d["pages"] == 3 and "seconds" in d and "words" in d, d
print("sidecar plugins.sample-lua =", d)
PY
guest "ls -la $PLUG/sample-lua $PLUG/sample-lua/data; cat $PLUG/sample-lua/data/sessions.csv" >"$OUT/plugin-files.txt" 2>&1
grep -q ',"Romeo and Juliet",[0-9]*,3$' "$OUT/plugin-files.txt" || fail "sessions.csv lacks the session line"
log "sessions.csv: $(grep 'Romeo' "$OUT/plugin-files.txt")"
wait_clean_home 300 home-after
pass "sample plugin loaded at start, hooks, status widget, per-book data, data file"
