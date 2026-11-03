#!/bin/bash
# e2e-cycles: 1
# Settings screen with the framework stopped, reached from the gallery
# (`rinky demo --kindle`, Settings button): the one-page grouped list (Reading,
# Gestures, Control center, Home & library, ... up to "Exit to Kindle"), control center
# by a swipe down from the top edge (brightness minus changes powerd flIntensity), the
# Rotate tile (landscape and back, settings.toml, canvas resize), then "Exit to Kindle"
# with its confirmation (Cancel keeps rinky, Exit brings Home back).
# Coordinates: portrait 1272 x 1696. The settings list is one scrolling page: Back is the
# app bar chevron (60, 68); the control center tiles are at (177, 850) Night, (483, 850)
# Auto warmth, (789, 850) Wi-Fi, (1095, 850) Rotate (the layout in docs/ui-settings or
# the 2026-11-02 screenshots); in landscape the same screen is turned, the Rotate tile is
# at panel (420, 1470).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

LOG=$DEST/rinky.log
# `rinky demo` keeps its settings in /tmp/rinky-sim (cli.rs), not in $DEST.
SETTINGS=/tmp/rinky-sim/settings.toml
count() { local n; n=$(guest "grep -c -- '$1' $LOG" 2>/dev/null | tr -dc '0-9'); echo "${n:-0}"; }
at_least() { [ "$(count "$1")" -ge "$2" ]; }
expect() { wait_until 30 "'$1' x$2" at_least "$1" "$2" || fail "expected $2 log lines with '$1', got $(count "$1")"; }
fl() { guest 'lipc-get-prop com.lab126.powerd flIntensity' 2>/dev/null | tr -dc '0-9'; }

wait_home 240 home-before
guest "rm -f $LOG $DEST/watchdog.log $SETTINGS; sh $DEST/rinky.sh -- demo --kindle" || fail "launch"
wait_until 90 "gallery running, KPPMainApp gone" reader_running || fail "rinky did not take over"
wait_until 30 "demo layout" at_least "demo button settings" 1 || fail "no settings button in the log"
p=$(guest "grep 'demo button settings panel' $LOG | tail -1" | awk '{print $(NF-1), $NF}')
tap $p
expect "screen: settings$" 1
expect "settings: page Main built" 1
sleep 4
not_home settings-main

# Scroll the list down and up again.
swipe 600 1400 600 300; expect "settings list: scrolled to" 1; sleep 3
not_home settings-scrolled
swipe 600 300 600 1400; sleep 3

# Control center: swipe down from the top edge, brightness minus, tap below the panel.
before=$(fl)
swipe 600 20 600 700
expect "control center open" 1; sleep 3
not_home quick-light
tap 192 264; expect "control center: Brightness" 1; sleep 3
after=$(fl)
log "flIntensity $before -> $after"
[ -n "$after" ] && [ "$after" != "$before" ] || fail "flIntensity did not change ($before -> $after)"
pass "brightness from the control center ($before -> $after)"

# Rotate tile: landscape, then back to portrait (control center stays open).
tap 1095 850; expect "rotation Clockwise" 1; expect "canvas 1272x1696 -> 1696x1272" 1; sleep 4
not_home cc-landscape
guest "cat $SETTINGS" >"$OUT/settings-landscape.toml"
grep -q "rotation = 1" "$OUT/settings-landscape.toml" || fail "rotation 1 not saved"
tap 420 1470; expect "rotation Portrait" 1; expect "canvas 1696x1272 -> 1272x1696" 1; sleep 4
not_home cc-portrait-again
pass "rotate tile: rotation saved and applied, canvas resized"
tap 600 1400; expect "control center closed" 1; sleep 3
not_home settings-after-cc

# Exit to Kindle (last row of the list): Cancel first, then Exit.
for i in 1 2 3 4; do swipe 600 1500 600 200; sleep 2; done
sleep 2
tap 400 1510; sleep 3
not_home exit-dialog
tap 390 1050; sleep 3
reader_running || fail "Cancel ended rinky"
tap 400 1510; sleep 3
tap 870 1050; expect "settings: exit to Kindle" 1
wait_until 30 "rinky exited" reader_gone || fail "rinky did not exit"
save_logs
guest "rm -f $SETTINGS"
wait_clean_home 300 home-after
pass "exit to Kindle"
