#!/bin/bash
# Widget gallery (`rinky demo --kindle`) with the framework stopped: every widget,
# overlays, night mode, fonts found, keyboard typing (key preview, alternates, repeat,
# Danish layout, password), every gesture (two-finger tap and pinch as raw multi-touch
# records written into the fts_ts node), and taps in all four rotations. Positions come
# from the gallery's own "demo button/key NAME panel X Y" log lines. Screenshots and the
# log go to OUT.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
HERE=$(cd "$(dirname "$0")" && pwd)

LOG=$DEST/rinky.log
# pos KIND NAME: "X Y" from the newest layout line.
pos() { guest "grep 'demo $1 $2 panel' $LOG | tail -1" 2>/dev/null | awk '{print $(NF-1), $NF}'; }
tapb() { local p; p=$(pos button "$1"); [ -n "$p" ] || fail "no button $1 in the log"; tap $p; }
tapk() { local p; p=$(pos key "$1"); [ -n "$p" ] || fail "no key $1 in the log"; tap $p; }
# grep -c prints 0 and exits 1 when nothing matches: keep its output, add nothing.
count() { local n; n=$(guest "grep -c -- '$1' $LOG" 2>/dev/null | tr -dc '0-9'); echo "${n:-0}"; }
at_least() { [ "$(count "$1")" -ge "$2" ]; }
expect() { wait_until 20 "'$1' x$2" at_least "$1" "$2" || fail "expected $2 log lines with '$1', got $(count "$1")"; }
qmpx() { python3 "$HERE/ui-qmp.py" --machine "$NAME" "$@" >/dev/null || fail "ui-qmp $*"; }
# mt GESTURE ARGS...: raw two-finger records into the touch node. Not fatal: the
# emulator's touch model may set up a single MT slot (two-finger gestures are also
# covered by the recorded pt_mt stream in crates/ink-ui/tests).
mt() {
  local dir
  dir=$(mktemp -d "$OUT/.mt.XXXXXX") || return 0
  python3 "$HERE/ui-mt.py" "$dir" "$@" || { log "ui-mt $* failed"; return 0; }
  guest 'rm -rf /tmp/ui-mt; mkdir -p /tmp/ui-mt' || return 0
  "$SLOT" push "$NAME" "$dir"/*.bin /tmp/ui-mt/ >/dev/null || { log "push mt failed"; return 0; }
  guest 'ev=$(grep -A4 "Name=\"fts_ts\"" /proc/bus/input/devices | sed -n "s/.*\(event[0-9]*\).*/\1/p" | head -1)
         for f in /tmp/ui-mt/*.bin; do cat $f > /dev/input/$ev; usleep 40000; done' ||
    log "writing mt records failed"
}

wait_home 240 home-before
# A user font next to the system fonts. An italic face is found by the scan but never
# used as a fallback, so CJK and symbols come from the system's faces (STSong, ...).
"$SLOT" push "$NAME" "$E2E_ROOT/fonts/NotoSerif-Italic.ttf" /mnt/us/fonts/ >/dev/null || fail "push font"
guest "rm -f $LOG $DEST/watchdog.log; sh $DEST/rinky.sh -- demo --kindle" || fail "launch"
wait_until 90 "gallery running, KPPMainApp gone" reader_running || fail "rinky did not take over"
expect "demo button rotate" 1
sleep 5
not_home controls
guest "grep -E 'fonts' $LOG" >"$OUT/fonts.log"
grep -q "/usr/java/lib/fonts: [1-9][0-9]* files" "$OUT/fonts.log" || fail "no system fonts found"
grep -q "/mnt/us/fonts: 1 files, 1 faces, 1 families: Noto Serif" "$OUT/fonts.log" || fail "user font not found"

# Overlays.
tapb toast; expect "demo action: toast" 1; sleep 2; not_home toast
tapb dialog; expect "demo action: dialog" 1; sleep 2; not_home dialog
tap 300 1050; expect "demo action: dialog:cancel" 1
tapb popup; expect "demo action: popup" 1; sleep 2; not_home popup
tap 100 1500; expect "demo action: dismiss" 1
tapb sheet; expect "demo action: sheet" 1; sleep 2; not_home sheet
swipe 636 1000 636 1500; expect "demo action: close" 1
# Night mode through the theme and the device night flag.
tapb night; expect "demo night mode on" 1; sleep 3; not_home night
tapb night; expect "demo night mode off" 1; sleep 3

# Tabs.
tapb tab:1; expect "demo action: tab:1" 1; sleep 3; not_home lists
swipe 900 1300 900 600; sleep 3; not_home lists-page2
tapb tab:2; expect "demo action: tab:2" 1; sleep 4; not_home text

# Keyboard.
tapb tab:3; expect "demo action: tab:3" 1; sleep 3; not_home input
tapk shift; tapk h; tapk e; tapk j; tapk space
expect "demo text input: Hej " 1
# Key preview: hold "d" (no alternates), the bubble stays while the finger is down.
( "$SLOT" hold "$NAME" $(pos key d) 3.0 >/dev/null ) &
sleep 1.0; shot key-preview >/dev/null
wait
expect "demo text input: Hej d" 1
# Alternates: hold "a"; the row opens after the long-press time (a wall-clock timer).
a=$(pos key a)
( "$SLOT" hold "$NAME" $a 3.0 >/dev/null ) &
sleep 1.2; shot alternates >/dev/null
wait
expect "demo text input: Hej da" 1
# Hold, then slide to the first alternate (one key width right, one row up).
set -- $a
qmpx drag "$1" "$2" $(( $1 + 126 )) $(( $2 - 135 )) --hold 1.0
expect "demo text input: Hej daà" 1
# Repeat on hold: backspace held 0.95 s repeats at 600, 750 and 900 ms: "Hej daà" -> "Hej ".
"$SLOT" hold "$NAME" $(pos key del) 0.95 >/dev/null
sleep 2
last=$(guest "grep 'demo text input' $LOG | tail -1" | sed 's/.*demo text input: //')
[ "$last" = "Hej " ] || fail "after holding backspace the text is '$last', expected 'Hej '"
pass "repeat on hold: three deletes"
# Danish layout from the globe key.
tapk globe; expect "demo keyboard language da" 1; sleep 2
tapk space; tapk æ; tapk ø; tapk å
expect "demo text input: .*æøå" 1
not_home typed-danish
tapb focus:password; tapk s; tapk k
expect "demo text password: ••" 1
not_home password
guest "grep 'demo text' $LOG" >"$OUT/typed.log"

# Gestures.
tapb tab:4; expect "demo action: tab:4" 1; sleep 3
tap 600 800
qmpx taps 300 600 305 604 --gap 0.05
"$SLOT" hold "$NAME" 300 900 1.0 >/dev/null
swipe 900 1200 300 1200
"$SLOT" hold "$NAME" 30 1670 1.2 >/dev/null
mt twotap 400 700 800 700
sleep 1
mt pinch 600 1000 200 600
for g in TwoFingerTap Pinch; do
  # The guest is slow: give the injected frames time to be handled.
  if wait_until 30 "$g" at_least "gesture: $g" 1; then
    pass "$g from raw multi-touch records"
  else
    log "NOTE: no $g from the raw multi-touch records (single-slot touch model?)"
  fi
done
sleep 3
not_home gestures
guest "grep 'gesture:' $LOG" >"$OUT/gestures.txt"
for g in "Tap {" DoubleTap LongPress "Swipe {" CornerHold; do
  grep -q "$g" "$OUT/gestures.txt" || fail "no $g in gestures.txt"
done
pass "single-finger gestures recognised (QMP input)"

# Rotation: rotate four times; in each orientation tap the toast button at its rotated
# panel position (from the log) and check the action.
tapb tab:0; expect "demo action: tab:0" 1; sleep 3
for i in 1 2 3 4; do
  tapb rotate
  expect "demo action: rotate" "$i"
  sleep 4
  not_home "rotation-$i"
  tapb toast
  expect "demo action: toast" $(( i + 1 ))
  sleep 3
done
guest "grep -E 'demo (layout|button (rotate|toast))' $LOG" >"$OUT/rotation.log"

tapb exit
wait_until 30 "gallery exited" reader_gone || fail "gallery did not exit"
save_logs
wait_clean_home 300 home-after
pass "gallery"
