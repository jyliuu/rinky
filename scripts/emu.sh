#!/bin/bash
# Drive one eink-emulator instance from the host (low-level; agents use emu-slot.sh,
# which picks ports from a slot). MACHINE and PORT are required: there is no default
# instance, so nothing here can touch an instance by accident.
#   MACHINE=NAME PORT=SSHPORT scripts/emu.sh ssh 'cmd'      run a command in the guest
#   MACHINE=NAME PORT=SSHPORT scripts/emu.sh deploy         zigbuild + copy binary, scripts, test books
#   MACHINE=NAME PORT=SSHPORT scripts/emu.sh shot FILE.png  QMP screendump
#   MACHINE=NAME PORT=SSHPORT scripts/emu.sh tap X Y        QMP tap (0..1271 x 0..1695)
set -euo pipefail

EMU=${EMU:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/eink-emulator}
MACHINE=${MACHINE:?set MACHINE=<instance name> (no default)}
PORT=${PORT:?set PORT=<host SSH port of that instance> (no default)}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
          -o LogLevel=ERROR -i "$EMU/build/ssh/id_ed25519")
BIN="$ROOT/target/armv7-unknown-linux-musleabihf/release/rinky"
DEST=/mnt/us/rinky

if [ "$MACHINE" = pw12-5195-jb ] && [ "${EMU_ALLOW_PW12:-}" != yes ]; then
  echo "emu.sh: pw12-5195-jb is the user's instance; set EMU_ALLOW_PW12=yes only if the user asked" >&2
  exit 1
fi

cmd=${1:-}
shift || true
case "$cmd" in
  ssh)
    ssh "${SSH_OPTS[@]}" -p "$PORT" root@127.0.0.1 "$@"
    ;;
  deploy)
    (cd "$ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)} cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p ink-app)
    ssh "${SSH_OPTS[@]}" -p "$PORT" root@127.0.0.1 "mkdir -p $DEST"
    scp -O "${SSH_OPTS[@]}" -P "$PORT" "$BIN" "$ROOT/scripts/rinky.sh" "$ROOT/scripts/rinky-watchdog.sh" \
        "$ROOT"/testdata/*.epub root@127.0.0.1:$DEST/
    ssh "${SSH_OPTS[@]}" -p "$PORT" root@127.0.0.1 "chmod +x $DEST/rinky $DEST/*.sh; ls -la $DEST"
    ;;
  shot)
    python3 "$EMU/scripts/eink-qmp.py" --machine "$MACHINE" screendump "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
    ;;
  tap)
    python3 "$EMU/scripts/eink-qmp.py" --machine "$MACHINE" tap "$1" "$2"
    ;;
  *)
    sed -n 2,9p "$0"
    exit 2
    ;;
esac
