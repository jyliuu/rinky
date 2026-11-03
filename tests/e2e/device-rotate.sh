#!/bin/bash
# Phase 5 check 10 (rotation): the frame buffer content is rotated in software and the
# touch is remapped: in each orientation the page text is upright, and a tap at panel
# 300,400 / 1000,1500 is logged at the logical point Rotation::from_panel gives and
# marked where the finger was.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_device device rotate all --seconds 25
# Expected logical coordinates on a 1272 x 1696 panel (geom.rs from_panel).
declare -A want=(
  [Portrait]="300,400 1000,1500" [Clockwise]="400,971 1500,271"
  [UpsideDown]="971,1295 271,195" [CounterClockwise]="1295,300 195,1000"
)
for rot in Portrait Clockwise UpsideDown CounterClockwise; do
  wait_log 40 "rotate: $rot logical"
  sleep 2
  tap 300 400
  sleep 2
  tap 1000 1500
  sleep 3
  shot "rotation-$rot" >/dev/null
  set -- ${want[$rot]}
  wait_log 15 "rotate $rot: tap 1 panel 300,400 -> logical $1"
  wait_log 15 "rotate $rot: tap 2 panel 1000,1500 -> logical $2"
  pass "$rot: taps remapped to $1 and $2"
done
wait_until 60 "rotate test finished" reader_gone || fail "rotate did not finish"
wait_clean_home 300 home-after
save_logs
check_restored
