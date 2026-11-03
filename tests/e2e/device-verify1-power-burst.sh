#!/bin/bash
# Verifier cycle 1 (check 3): ten power-key presses in a burst, 0-300 ms apart, while
# `rinky device sleep` runs. Afterwards the sleep state in the log, the frontlight
# and the panel must agree, one more press must toggle the state, and rinky must
# still be the same process.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"
H="$(dirname "$0")/device-helpers"

# Badge corner (asleep: black box) vs the page's light gray steps (awake).
corner() { python3 "$H/region-mean.py" "$1" 1060 1600 1230 1655; }
nlines() { guest "grep -c -- '$1' $DEST/rinky.log"; }
state_check() { # LABEL: prints asleep|awake after checking log, light and panel agree
  local draws restores st bl f m
  draws=$(nlines '[0-9] sleep: screensaver drawn')
  restores=$(nlines '[0-9] sleep: awake, screen restored')
  if [ "$draws" -gt "$restores" ]; then st=asleep; else st=awake; fi
  bl=$(guest 'cat /sys/class/backlight/fp9967-bl1/brightness')
  f=$(shot "$1")
  m=$(corner "$f")
  log "$1: log $draws screensavers / $restores restores -> $st; backlight $bl; badge corner mean $m"
  if [ $st = asleep ]; then
    [ "$bl" = 0 ] || fail "$1: log says asleep but the frontlight is $bl"
    [ "$m" -lt 100 ] || fail "$1: log says asleep but no screensaver badge (corner mean $m)"
  else
    [ "$bl" != 0 ] || fail "$1: log says awake but the frontlight is off"
    [ "$m" -gt 150 ] || fail "$1: log says awake but the panel shows the badge (corner mean $m)"
  fi
  STATE=$st
}

wait_home 240 home-before
clear_env
start_device device sleep --seconds 600
pid=$(rinky_pid)
state_check awake-before
[ "$STATE" = awake ] || fail "not awake at the start"
log "10 power presses in a burst (rinky inject power, 0-300 ms apart)"
guest "for d in 20000 300000 50000 250000 20000 100000 300000 30000 200000 0; do $DEST/rinky inject power >/dev/null 2>&1; usleep \$d; done"
sleep 6
[ "$(rinky_pid)" = "$pid" ] || fail "rinky $pid did not survive the burst"
presses=$(guest "grep -c '^power key pressed' $DEST/rinky.log")
ignored=$(nlines 'power key ignored (debounce)')
log "presses seen $presses, ignored by debounce $ignored"
[ "$presses" = 10 ] || fail "only $presses of 10 presses reached rinky"
state_check after-burst
s1=$STATE
power_key
sleep 5
state_check after-one-more-press
[ "$STATE" != "$s1" ] || fail "one more press did not toggle the state ($s1)"
pass "power burst: 10 presses seen, log, frontlight and panel agree ($s1), the next press toggles"
if [ "$STATE" = asleep ]; then power_key; sleep 5; fi
exit_device
wait_clean_home 300 home-after
save_logs
check_no_watchers
check_restored
