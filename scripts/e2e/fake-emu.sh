#!/bin/bash
# Fake eink-emulator for host tests of scripts/emu-slot.sh and scripts/e2e/run.sh.
# Source it, then call `fake_emu_setup DIR`. It exports:
#   EMU=DIR/emu            fake repo: `eink create|run` and scripts/eink-qmp.py
#   EMU_SLOTS=DIR/slots    private lock directory
#   EMU_SLOT_PORT_BASE=47110, EMU_BOOT_DEADLINE_S=20, EMU_QUIT_WAIT_S=2
#   FAKE_DIR=DIR           marker files: hang-qmp (QMP quit does nothing),
#                          ignore-term (new fake QEMUs ignore SIGTERM),
#                          fail-run (`eink run` exits 1 at once: a boot that fails)
#   PATH=DIR/bin:$PATH     `ssh` and `scp` stubs that succeed without connecting
# `eink run NAME` execs a perl process whose command line is
# `perl DIR/qemu-system-fake -name NAME -machine fake ...`, which emu-slot.sh's pid
# checks accept as NAME's QEMU. QMP screendump copies tests/e2e/ref/home-5.19.5.png, so
# Home is detected at once. Nothing here opens a network connection.
fake_emu_setup() {
  local dir=$1 root
  root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
  export EMU="$dir/emu" EMU_SLOTS="$dir/slots" FAKE_DIR="$dir"
  export EMU_SLOT_PORT_BASE=47110 EMU_BOOT_DEADLINE_S=20 EMU_QUIT_WAIT_S=2
  export FAKE_HOME_PNG="$root/tests/e2e/ref/home-5.19.5.png"
  mkdir -p "$EMU/scripts" "$EMU/machines" "$EMU/build/ssh" "$EMU_SLOTS" "$dir/bin"
  touch "$EMU/build/ssh/id_ed25519"
  cat >"$dir/qemu-system-fake" <<'EOF'
#!/usr/bin/perl
# Fake QEMU: sleeps; ignores SIGTERM when $FAKE_DIR/ignore-term exists.
$SIG{TERM} = 'IGNORE' if -e "$ENV{FAKE_DIR}/ignore-term";
sleep 600;
EOF
  cat >"$EMU/eink" <<'EOF'
#!/bin/bash
# Fake eink: `create NAME ...` makes machine.json; `run NAME ...` execs the fake QEMU.
cmd=$1 name=$2
shift 2
case "$cmd" in
  create) mkdir -p "machines/$name"; sleep "${FAKE_CREATE_S:-0}"; echo '{}' >"machines/$name/machine.json" ;;
  run) [ -e "$FAKE_DIR/fail-run" ] && { echo "fake eink: run refused (fail-run)" >&2; exit 1; }
       mkdir -p "machines/$name"; echo $$ >"machines/$name/fake.pid"
       exec perl "$FAKE_DIR/qemu-system-fake" -name "$name" -machine fake "$@" ;;
esac
EOF
  cat >"$EMU/scripts/eink-qmp.py" <<'EOF'
# Fake QMP client: quit (SIGTERM to the fake QEMU, or nothing when $FAKE_DIR/hang-qmp
# exists), screendump (copies $FAKE_HOME_PNG).
import os, shutil, signal, sys
name, cmd = sys.argv[2], sys.argv[3]
emu = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if cmd == "quit":
    if os.path.exists(os.path.join(os.environ["FAKE_DIR"], "hang-qmp")):
        sys.exit(1)
    os.kill(int(open(os.path.join(emu, "machines", name, "fake.pid")).read()), signal.SIGTERM)
elif cmd == "screendump":
    shutil.copy(os.environ["FAKE_HOME_PNG"], sys.argv[4])
EOF
  printf '#!/bin/sh\n# ssh stub for host tests: no connection\nexit 0\n' >"$dir/bin/ssh"
  printf '#!/bin/sh\n# scp stub for host tests: no connection\nexit 0\n' >"$dir/bin/scp"
  chmod +x "$EMU/eink" "$dir/qemu-system-fake" "$dir/bin/ssh" "$dir/bin/scp"
  export PATH="$dir/bin:$PATH"
}

# Stops every fake QEMU started from DIR.
fake_emu_teardown() {
  pkill -f "$1/qemu-system-fake" 2>/dev/null
  return 0
}
