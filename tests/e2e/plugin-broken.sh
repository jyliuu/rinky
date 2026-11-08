#!/bin/bash
# Broken and hostile Lua plugins next to the sample plugin: the reader keeps working.
#   syntax   main.lua does not compile          -> not loaded, reported
#   misnamed plugin.toml name differs from its folder -> not loaded, reported
#   crasher  on_book_open raises an error        -> error logged, plugin stays enabled
#   looper   on_page_change loops forever        -> time limit (500 ms), disabled
#   escaper  tries os.execute, io.open('/etc/passwd'), '../' paths, require of a path,
#            string.rep of 1 GB; logs what happened (all refused)
# The reader must still turn pages, the sample plugin must still get its hooks, and the
# stock Home must come back.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
PLUG=$DEST/plugins
SRC=$(dirname "$0")/plugin-fixtures

page_changes() { guest "grep -c 'plugins: on_page_change([0-9]*) -> sample-lua' $DEST/rinky.log" 2>/dev/null || echo 0; }
at_least() { [ "$(page_changes)" -ge "$1" ]; }

# Leave no plugins behind for later scenarios (also after a failure).
trap 'guest "rm -rf $PLUG" >/dev/null 2>&1' EXIT

wait_home 240 home-before
guest "rm -rf $PLUG; mkdir -p $PLUG/sample-lua $PLUG/syntax $PLUG/misnamed $PLUG/crasher $PLUG/looper $PLUG/escaper" || fail "mkdir"
"$SLOT" push "$NAME" "$E2E_ROOT/plugins/sample-lua/plugin.toml" "$E2E_ROOT/plugins/sample-lua/main.lua" "$PLUG/sample-lua/" >/dev/null || fail "push sample"
for p in syntax misnamed crasher looper escaper; do
  "$SLOT" push "$NAME" "$SRC/$p/plugin.toml" "$SRC/$p/main.lua" "$PLUG/$p/" >/dev/null || fail "push $p"
done
# Fast on_page_change calls are logged at debug.
READER_LOG=debug start_reader
wait_until 30 "first page" at_least 1 || fail "no on_page_change for sample-lua"
not_home page1
tap 1000 900; wait_until 20 "page 2" at_least 2 || fail "page 2 not shown"; sleep 2; not_home page2
tap 1000 900; wait_until 20 "page 3" at_least 3 || fail "page 3 not shown"; sleep 2; not_home page3
exit_device
save_logs
grep "plugin" "$OUT/rinky.log" >"$OUT/plugin-lines.txt"
check() { grep -qE "$1" "$OUT/rinky.log" || fail "rinky.log lacks: $1"; log "log has: $1"; }
check "plugins: [0-9]+ compiled-in, 4 Lua plugin\(s\)"
check "plugins: syntax: not loaded: lua: main.lua:[0-9]+:"
check "plugins: misnamed: not loaded: manifest: plugin name \"other\" differs from its folder name \"misnamed\""
check "plugins: crasher: on_book_open: lua: main.lua:[0-9]+: crasher fails on purpose"
check "plugins: looper: on_page_change: limit: time limit exceeded"
check "plugins: looper: disabled: too slow on a page turn"
check "plugin escaper: os.execute = nil"
check "plugin escaper: os.exit = nil"
check "plugin escaper: io.popen = nil"
check "plugin escaper: debug = nil"
check "plugin escaper: io.open\(/etc/passwd\) refused: .*outside the plugin's data folder"
check "plugin escaper: files.write\(../escape.txt\) refused: .*outside the plugin's data folder"
check "plugin escaper: require\(../x\) refused: .*module name"
check "plugin escaper: string.rep 1 GB refused: .*memory"
check "plugin escaper: own data file ok"
check "plugins: on_page_change\(3\) -> sample-lua"
check "plugins: on_close -> sample-lua"
# The looper ran once (about 500 ms), then never again.
grep -E "on_page_change\([0-9]+\) -> looper" "$OUT/rinky.log" | tee "$OUT/looper-calls.txt"
[ "$(wc -l <"$OUT/looper-calls.txt")" -eq 1 ] || fail "looper was called again after it was disabled"
guest "ls $PLUG $PLUG/escaper/escape.txt $PLUG/escaper/data 2>&1" >"$OUT/escape-check.txt" 2>&1
grep -q "escape.txt: No such file" "$OUT/escape-check.txt" || fail "a file escaped the data folder: $(cat "$OUT/escape-check.txt")"
wait_clean_home 300 home-after
pass "broken plugins reported, limits enforced, reader and sample plugin unaffected"
