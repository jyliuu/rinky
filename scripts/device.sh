#!/bin/bash
# Smoke-test helper for the user's real Kindle (PW12, jailbroken, SSH from KOReader or
# the jailbreak's own SSH server). HOST and PORT are required; there is no default
# address. Only the user runs this: agents never contact a real device.
#
#   scripts/device.sh HOST PORT deploy      build, copy binary + launcher + watchdog + test book
#                                           to /mnt/us/rinky (never touches /mnt/us/koreader)
#   scripts/device.sh HOST PORT start [BOOK]  start rinky through the framework-stop launcher
#   scripts/device.sh HOST PORT stop        stop rinky (the launcher restarts the framework)
#   scripts/device.sh HOST PORT status      processes, framework state, last log lines
#   scripts/device.sh HOST PORT log [DIR]   fetch rinky.log and watchdog.log (default out/device/)
#   scripts/device.sh HOST PORT selftest    run `rinky selftest --lipc --device` (no screen drawing,
#                                           no settings changed); add --write to let it change and
#                                           restore brightness, the night flag and the RTC alarm
#   scripts/device.sh HOST PORT device ARGS run `rinky device ARGS` (info, events 60, battery, ...)
#   scripts/device.sh HOST PORT shot [FILE] screenshot of the panel to FILE (default out/device/shot.png)
#   scripts/device.sh HOST PORT run -- ARGS start `rinky ARGS` through the launcher
#                                           (framework stopped), e.g. `run -- device sleep --seconds 600`
#
# Environment: SSH_KEY=path (default: ssh's own defaults), SSH_USER (default root).
# The device has no SFTP; files are copied with `cat` over ssh.
set -euo pipefail

HOST=${1:?usage: scripts/device.sh HOST PORT COMMAND (no default address)}
PORT=${2:?usage: scripts/device.sh HOST PORT COMMAND}
CMD=${3:-}
shift 3 || true
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEST=/mnt/us/rinky
SSH_USER=${SSH_USER:-root}
OPTS=(-o ConnectTimeout=10 -o ServerAliveInterval=5 -p "$PORT")
[ -n "${SSH_KEY:-}" ] && OPTS+=(-i "$SSH_KEY")

dev() { ssh "${OPTS[@]}" "$SSH_USER@$HOST" "$@"; }

# push LOCAL REMOTE: copy one file with cat (no scp/sftp needed).
push() {
  dev "cat > '$2.tmp' && mv '$2.tmp' '$2'" <"$1"
}

case "$CMD" in
  deploy)
    (cd "$ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)} cargo zigbuild --release \
        --target armv7-unknown-linux-musleabihf -p ink-app)
    dev "mkdir -p $DEST"
    push "$ROOT/target/armv7-unknown-linux-musleabihf/release/rinky" "$DEST/rinky"
    push "$ROOT/scripts/rinky.sh" "$DEST/rinky.sh"
    push "$ROOT/scripts/rinky-watchdog.sh" "$DEST/rinky-watchdog.sh"
    push "$ROOT/testdata/juliet.epub" "$DEST/juliet.epub"
    dev "chmod +x $DEST/rinky $DEST/rinky.sh $DEST/rinky-watchdog.sh; ls -la $DEST; $DEST/rinky version"
    ;;
  start)
    BOOK=${1:-$DEST/juliet.epub}
    dev "sh $DEST/rinky.sh '$BOOK'"
    echo "started; the framework stops and rinky shows the book. Exit: top-right corner tap or power key."
    ;;
  stop)
    dev 'pid=$(cat /tmp/rinky/rinky.pid 2>/dev/null); if [ -n "$pid" ]; then kill -TERM $pid; echo "sent TERM to $pid"; else echo "not running"; fi'
    ;;
  status)
    dev "status lab126_gui; ps -eo pid,comm,args | grep -E 'rinky|KPPMainApp|pillowd' | grep -v grep; tail -n 15 $DEST/rinky.log 2>/dev/null; tail -n 5 $DEST/watchdog.log 2>/dev/null"
    ;;
  log)
    OUT=${1:-$ROOT/out/device}
    mkdir -p "$OUT"
    dev "cat $DEST/rinky.log" >"$OUT/rinky.log" || true
    dev "cat $DEST/watchdog.log" >"$OUT/watchdog.log" || true
    ls -la "$OUT"
    ;;
  selftest)
    dev "cd $DEST && ./rinky selftest --lipc --device $* --book $DEST/juliet.epub"
    ;;
  device)
    dev "$DEST/rinky device $*"
    ;;
  shot)
    OUT=${1:-$ROOT/out/device/shot.png}
    mkdir -p "$(dirname "$OUT")"
    dev "$DEST/rinky device screenshot /tmp/rinky-shot.png >/dev/null && cat /tmp/rinky-shot.png && rm -f /tmp/rinky-shot.png" >"$OUT"
    ls -la "$OUT"
    ;;
  run)
    [ "${1:-}" = "--" ] && shift
    dev "sh $DEST/rinky.sh -- $*"
    echo "started; the framework stops. Logs: scripts/device.sh HOST PORT log"
    ;;
  *)
    sed -n 2,24p "$0"
    exit 2
    ;;
esac
