#!/bin/bash
# Emulator slots for parallel agents: at most 4 eink-emulator instances at once.
#
#   scripts/emu-slot.sh acquire NAME           take a free slot for instance NAME (prints the slot number)
#   scripts/emu-slot.sh release NAME           give the slot of NAME back (refused while its QEMU runs)
#   scripts/emu-slot.sh status                 list slots, owners, QEMU pids and ports
#   scripts/emu-slot.sh cleanup                remove stale locks (QEMU gone); never kills a process
#   scripts/emu-slot.sh port NAME              print the SSH port of NAME's slot
#   scripts/emu-slot.sh slotdir NAME           print NAME's slot directory (boot_s, fw_cycles, ...)
#   scripts/emu-slot.sh up NAME                acquire, create NAME if missing, boot with -snapshot,
#                                              wait for SSH, jailbreak and the Home screen;
#                                              EMU_QEMU_EXTRA adds QEMU arguments (word-split),
#                                              e.g. a TCG plugin for diagnosis
#   scripts/emu-slot.sh restart NAME           quit NAME's QEMU and boot it again in the same slot
#                                              (the slot stays held; a fresh guest after -snapshot)
#   scripts/emu-slot.sh down NAME              QMP quit (then TERM/KILL of NAME's own QEMU if QMP
#                                              hangs), wait for QEMU to exit, release the slot
#   scripts/emu-slot.sh ssh NAME CMD...        run a command in the guest
#   scripts/emu-slot.sh push NAME SRC... DEST  copy host files into a guest directory
#   scripts/emu-slot.sh pull NAME SRC DEST     copy one guest file to the host
#   scripts/emu-slot.sh deploy NAME [--no-build]
#                                              zigbuild + copy binary, launcher, watchdog, testdata
#                                              to /mnt/us/rinky (--no-build: copy only)
#   scripts/emu-slot.sh shot NAME FILE.png     QMP screendump
#   scripts/emu-slot.sh tap NAME X Y [HOLD_S]  touch at panel pixels (0..1271 x 0..1695)
#   scripts/emu-slot.sh swipe NAME X1 Y1 X2 Y2 [STEPS]
#   scripts/emu-slot.sh hold NAME X Y SECONDS  long press
#   scripts/emu-slot.sh power NAME             press the power key: EV_KEY into bd71828-pwrkey (apps
#                                              reading evdev see it) + powerd powerButton 1 (powerd
#                                              ignores injected evdev events; it reacts 20-90 s later)
#   scripts/emu-slot.sh log NAME [LINES]       rinky, watchdog and serial logs
#   scripts/emu-slot.sh wifi NAME              join the emulated open AP Kindle-QEMU (wpa_cli);
#                                              the guest then reaches the host at 10.0.2.2
#
# Slots: lock directories $SLOTS/slot0..slot3 created with mkdir (atomic). Each holds
# owner (instance name), acquired (epoch seconds), acquirer (pid) and, after `up`,
# qemu.pid; `up`/`restart` also keep up.pid (the running emu-slot.sh) while they work,
# boot_s (seconds from QEMU start to Home) and fw_cycles (framework stop/start cycles
# counted by scripts/e2e/run.sh since that boot). Slot N uses SSH port 4110+10N and
# Wi-Fi SSH port 4111+10N. acquire, release and cleanup run under an flock on
# $SLOTS/.lock, so concurrent callers, one name acquired twice at once and cleanup
# during an acquire are serialized; the kernel drops the lock when a holder dies.
# `down` and a failed `up` remove a slot directory only under that lock and only while
# its owner is still NAME (another agent's cleanup + acquire may have reused the slot).
# `up` and `restart` of one name are serialized by a second flock,
# $SLOTS/.up-NAME.lock (fd 8): a second `up NAME` waits until the first has finished
# and then finds QEMU running. QEMU and `eink create` are started without fd 8.
# The user's own instance pw12-5195-jb (ports 4022/4023) is refused everywhere.
#
# Environment: EMU (emulator repo), EMU_SLOTS (lock directory), EMU_TIME_SCALE (factor
# for every deadline on a slow host, default 1), EMU_BOOT_DEADLINE_S (900),
# EMU_SLOT_GRACE_S (1200), EMU_QUIT_WAIT_S (30), EMU_UP_WAIT_S (how long an up/restart
# waits for another up/restart of the same name; default boot deadline + 1200),
# EMU_SLOT_PORT_BASE (4110; tests use another base).
set -euo pipefail

EMU=${EMU:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/eink-emulator}
SLOTS=${EMU_SLOTS:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/inkreader-wt/.emu-slots}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NSLOTS=4
PROTECTED=pw12-5195-jb
FIRMWARE=5.19.5
DEST=/mnt/us/rinky
PORT_BASE=${EMU_SLOT_PORT_BASE:-4110}
# Integer seconds times EMU_TIME_SCALE (a decimal factor such as 1.5).
scaled() { awk -v s="$1" -v f="${EMU_TIME_SCALE:-1}" 'BEGIN { if (f + 0 < 1) f = 1; printf "%d\n", s * f + 0.5 }'; }
# A lock without a QEMU pid older than this is stale (creation + start never finished).
GRACE_S=$(scaled "${EMU_SLOT_GRACE_S:-1200}")
BOOT_DEADLINE_S=$(scaled "${EMU_BOOT_DEADLINE_S:-900}")
# How long `down`/`restart` wait after QMP quit, and again after SIGTERM.
QUIT_WAIT_S=$(scaled "${EMU_QUIT_WAIT_S:-30}")
# How long `up`/`restart` wait for another up/restart of the same name to finish.
UP_WAIT_S=${EMU_UP_WAIT_S:-$(( BOOT_DEADLINE_S + $(scaled 1200) ))}
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
          -o LogLevel=ERROR -o ServerAliveInterval=5 -o ServerAliveCountMax=3 -i "$EMU/build/ssh/id_ed25519")

# Set by `up` while it holds a slot it took itself and has not started QEMU yet:
# `die` then gives that slot back (under the slot lock, only while UP_NAME still owns
# it and no QEMU runs there), so a failed `up` leaves no lock behind.
UP_FRESH_DIR=
UP_NAME=
die() {
  local dir=$UP_FRESH_DIR
  echo "emu-slot: $*" >&2
  # Cleared first: with_lock calls die itself when it cannot take the lock.
  UP_FRESH_DIR=
  if [ -n "$dir" ] && [ -n "$UP_NAME" ]; then
    with_lock release_dir_locked "$UP_NAME" "$dir" "again (no QEMU was started)" || true
  fi
  exit 1
}
log() { echo "emu-slot: $*" >&2; }
now() { date +%s; }

check_name() {
  local name=${1:-}
  [ -n "$name" ] || die "instance NAME required"
  [ "$name" != "$PROTECTED" ] || die "refusing to touch $PROTECTED"
  case "$name" in
    *[!a-z0-9_-]*) die "instance names allow only [a-z0-9_-]: $name" ;;
  esac
}

slot_dir() { echo "$SLOTS/slot$1"; }

# with_lock CMD...: runs CMD with the slot mutex (flock on $SLOTS/.lock, fd 9) held.
# BSD flock locks belong to the open file; the lock ends when fd 9 is closed or this
# process exits, so a killed holder never leaves it behind. CMD must not start
# long-lived children (they would inherit fd 9).
with_lock() {
  local rc=0
  mkdir -p "$SLOTS"
  exec 9>>"$SLOTS/.lock"
  perl -MFcntl=:flock -e '
    open(my $f, ">&=", 9) or die "emu-slot: fd 9: $!\n";
    my $deadline = time + 60;
    until (flock($f, LOCK_EX | LOCK_NB)) {
      die "emu-slot: slot lock $ARGV[0] busy for 60 s\n" if time > $deadline;
      select(undef, undef, undef, 0.05);
    }' "$SLOTS/.lock" || { exec 9>&-; die "cannot take the slot lock"; }
  "$@" || rc=$?
  exec 9>&-
  return $rc
}

# take_up_lock NAME: per-name flock ($SLOTS/.up-NAME.lock on fd 8), held until this
# process exits. Serializes `up` and `restart` of one name; a second caller waits (up to
# UP_WAIT_S) instead of starting a second QEMU. Long-lived children are started with
# 8>&-, so the lock ends with this process (also after kill -9).
take_up_lock() {
  local name=$1
  mkdir -p "$SLOTS"
  exec 8>>"$SLOTS/.up-$name.lock"
  perl -MFcntl=:flock -e '
    open(my $f, ">&=", 8) or die "emu-slot: fd 8: $!\n";
    my ($deadline, $said) = (time + $ARGV[1], 0);
    until (flock($f, LOCK_EX | LOCK_NB)) {
      die "emu-slot: another up/restart of $ARGV[0] still runs after $ARGV[1] s\n" if time > $deadline;
      print STDERR "emu-slot: another up/restart of $ARGV[0] is running; waiting for it\n" unless $said++;
      select(undef, undef, undef, 0.2);
    }' "$name" "$UP_WAIT_S" || { exec 8>&-; die "cannot take the up lock of $name"; }
}

# Print the slot number owned by NAME, or nothing.
find_slot() {
  local n
  for n in $(seq 0 $((NSLOTS - 1))); do
    if [ -f "$(slot_dir "$n")/owner" ] && [ "$(cat "$(slot_dir "$n")/owner" 2>/dev/null)" = "$1" ]; then
      echo "$n"
      return 0
    fi
  done
  return 0
}

ssh_port() { echo $((PORT_BASE + 10 * $1)); }
wifi_port() { echo $((PORT_BASE + 1 + 10 * $1)); }

# QEMU pid recorded in a slot, if that process is still a QEMU for the slot's owner.
alive_qemu() {
  local dir=$1 pid owner cmd
  [ -f "$dir/qemu.pid" ] || return 1
  pid=$(cat "$dir/qemu.pid" 2>/dev/null || true)
  [ -n "$pid" ] || return 1
  owner=$(cat "$dir/owner" 2>/dev/null || true)
  [ -n "$owner" ] || return 1
  cmd=$(ps -p "$pid" -o command= 2>/dev/null || true)
  case "$cmd" in
    *qemu-system*"-name $owner "*|*"eink run $owner "*) echo "$pid"; return 0 ;;
  esac
  return 1
}

# Any running QEMU process for NAME (found by its -name argument). awk reads all of ps's
# output: an early `exit` would let ps die of SIGPIPE, which pipefail + set -e turn into
# a silent exit 141 of this script.
qemu_pid_of() {
  ps -axo pid=,command= | awk -v n="$1" '!found && $0 ~ /qemu-system/ { for (i = 2; i < NF; i++) if ($i == "-name" && $(i + 1) == n) { print $1; found = 1; break } }'
}

# An emu-slot.sh `up`/`restart` that is still working on a slot (pid in up.pid).
alive_up() {
  local pid cmd
  pid=$(cat "$1/up.pid" 2>/dev/null || true)
  [ -n "$pid" ] || return 1
  cmd=$(ps -p "$pid" -o command= 2>/dev/null || true)
  case "$cmd" in *emu-slot.sh*) return 0 ;; esac
  return 1
}

# Seconds since the lock was taken: the acquired file, else the directory's mtime (an
# acquire that has made the directory but not yet written the file is not stale).
lock_age() {
  local dir=$1 t
  t=$(cat "$dir/acquired" 2>/dev/null || true)
  case "$t" in ''|*[!0-9]*) t=$(stat -f %m "$dir" 2>/dev/null || now) ;; esac
  echo $(( $(now) - t ))
}

# TCP ports with a listener on this host (lsof prints the listening pids).
port_busy() { lsof -nP -iTCP:"$1" -sTCP:LISTEN -t >/dev/null 2>&1; }

acquire_locked() {
  local name=$1 n dir existing
  existing=$(find_slot "$name")
  if [ -n "$existing" ]; then
    echo "$existing"
    return 0
  fi
  for n in $(seq 0 $((NSLOTS - 1))); do
    dir=$(slot_dir "$n")
    if mkdir "$dir" 2>/dev/null; then
      now >"$dir/acquired"
      echo "$PPID" >"$dir/acquirer"
      echo "$name" >"$dir/owner.tmp"
      mv "$dir/owner.tmp" "$dir/owner"
      echo "$n"
      return 0
    fi
  done
  echo "emu-slot: no free slot (max $NSLOTS); run '$0 status', or '$0 cleanup' to drop stale locks" >&2
  return 1
}

cmd_acquire() {
  check_name "$1"
  with_lock acquire_locked "$1" || exit 1
}

release_locked() {
  local name=$1 n pid
  n=$(find_slot "$name")
  [ -n "$n" ] || { log "$name holds no slot"; return 0; }
  if pid=$(alive_qemu "$(slot_dir "$n")"); then
    echo "emu-slot: QEMU $pid for $name is still running; use '$0 down $name'" >&2
    return 1
  fi
  rm -rf "$(slot_dir "$n")"
  log "released slot $n ($name)"
}

# release_dir_locked NAME DIR WHY: remove slot directory DIR if NAME still owns it and
# no QEMU runs there. Run under with_lock. When DIR was dropped (stale) and taken again
# by another instance meanwhile, it is left alone.
release_dir_locked() {
  local name=$1 dir=$2 why=$3 owner pid
  [ -d "$dir" ] || { log "$(basename "$dir") of $name is already gone (cleaned up meanwhile)"; return 0; }
  owner=$(cat "$dir/owner" 2>/dev/null || true)
  if [ "$owner" != "$name" ]; then
    log "$(basename "$dir") no longer belongs to $name (owner now: ${owner:-?}); left alone"
    return 0
  fi
  if pid=$(alive_qemu "$dir"); then
    log "QEMU $pid runs in $(basename "$dir") ($name); slot kept"
    return 1
  fi
  rm -rf "$dir"
  log "released $(basename "$dir") ($name) $why"
}

cmd_release() {
  check_name "$1"
  with_lock release_locked "$1" || exit 1
}

cmd_status() {
  local n dir owner age pid state
  printf '%-5s %-20s %-8s %-9s %-7s %s\n' SLOT OWNER QEMU STATE AGE_S PORTS
  for n in $(seq 0 $((NSLOTS - 1))); do
    dir=$(slot_dir "$n")
    if [ ! -d "$dir" ]; then
      printf '%-5s %-20s %-8s %-9s %-7s %s\n' "$n" - - free - "$(ssh_port "$n")/$(wifi_port "$n")"
      continue
    fi
    owner=$(cat "$dir/owner" 2>/dev/null || echo '?')
    age=$(lock_age "$dir")
    if pid=$(alive_qemu "$dir"); then state=running
    elif alive_up "$dir"; then pid=-; state=starting
    else pid=-; state=held; fi
    printf '%-5s %-20s %-8s %-9s %-7s %s\n' "$n" "$owner" "$pid" "$state" "$age" "$(ssh_port "$n")/$(wifi_port "$n")"
  done
}

cleanup_locked() {
  local n dir age owner pid
  for n in $(seq 0 $((NSLOTS - 1))); do
    dir=$(slot_dir "$n")
    [ -d "$dir" ] || continue
    owner=$(cat "$dir/owner" 2>/dev/null || echo '?')
    [ "$owner" != "$PROTECTED" ] || continue
    alive_qemu "$dir" >/dev/null && continue
    # An up/restart is still working here (QEMU not started yet, or between two boots).
    alive_up "$dir" && continue
    # The owner's QEMU runs but qemu.pid is missing or wrong: keep the lock, fix the pid.
    if [ "$owner" != '?' ] && pid=$(qemu_pid_of "$owner") && [ -n "$pid" ]; then
      log "slot $n ($owner): QEMU $pid runs; qemu.pid repaired"
      echo "$pid" >"$dir/qemu.pid"
      continue
    fi
    if [ -f "$dir/qemu.pid" ]; then
      log "slot $n ($owner): QEMU $(cat "$dir/qemu.pid" 2>/dev/null) is gone; removing the lock"
      rm -rf "$dir"
      continue
    fi
    age=$(lock_age "$dir")
    if [ "$age" -ge "$GRACE_S" ]; then
      log "slot $n ($owner): no QEMU after ${age}s; removing the lock"
      rm -rf "$dir"
    fi
  done
}

cmd_cleanup() { with_lock cleanup_locked; }

need_slot() {
  local n
  check_name "$1"
  n=$(find_slot "$1")
  [ -n "$n" ] || die "$1 holds no slot; run '$0 up $1' first"
  echo "$n"
}

guest() {
  local name=$1 n
  shift
  n=$(need_slot "$name")
  ssh "${SSH_OPTS[@]}" -p "$(ssh_port "$n")" root@127.0.0.1 "$@"
}

instance_exists() {
  [ -f "$EMU/machines/$1/machine.json" ]
}

qmp() { python3 "$EMU/scripts/eink-qmp.py" --machine "$@"; }

# stop_qemu NAME PID: QMP quit, then SIGTERM, then SIGKILL - only for a pid whose command
# line is NAME's QEMU (checked before each signal). Returns once the process is gone.
stop_qemu() {
  local name=$1 pid=$2 deadline cmd sig
  qmp "$name" quit >/dev/null 2>&1 || log "QMP quit for $name failed"
  deadline=$(( $(now) + QUIT_WAIT_S ))
  while kill -0 "$pid" 2>/dev/null && [ "$(now)" -lt "$deadline" ]; do sleep 1; done
  for sig in TERM KILL; do
    kill -0 "$pid" 2>/dev/null || break
    cmd=$(ps -p "$pid" -o command= 2>/dev/null || true)
    case "$cmd" in
      *qemu-system*"-name $name "*|*"eink run $name "*) ;;
      *) die "pid $pid is no longer $name's QEMU ($cmd); not signalling it" ;;
    esac
    log "QEMU $pid for $name did not exit after QMP quit; sending SIG$sig"
    kill -"$sig" "$pid" 2>/dev/null || true
    deadline=$(( $(now) + QUIT_WAIT_S ))
    while kill -0 "$pid" 2>/dev/null && [ "$(now)" -lt "$deadline" ]; do sleep 1; done
  done
  ! kill -0 "$pid" 2>/dev/null || die "QEMU $pid for $name is still running"
}

# boot_slot NAME N: starts NAME's QEMU in slot N unless it already runs there, then waits
# for SSH, the jailbreak and Home. Records qemu.pid, boot_s and fw_cycles=0.
boot_slot() {
  local name=$1 n=$2 dir port wport pid start shot deadline
  dir=$(slot_dir "$n")
  port=$(ssh_port "$n")
  wport=$(wifi_port "$n")
  if pid=$(alive_qemu "$dir"); then
    log "$name already running (QEMU $pid)"
  else
    pid=$(qemu_pid_of "$name")
    [ -z "$pid" ] || die "$name runs as QEMU $pid outside this slot; stop it first ('$0 down $name' stops it too)"
    for p in "$port" "$wport"; do
      if port_busy "$p"; then
        die "port $p of slot $n is already in use by pid(s) $(lsof -nP -iTCP:"$p" -sTCP:LISTEN -t 2>/dev/null | tr '\n' ' ')- not starting $name"
      fi
    done
    # The serial socket outlives QEMU; eink refuses to start while it exists.
    rm -f "$EMU/machines/$name/$name.serial.sock"
    log "starting $name on slot $n (ssh $port, wifi ssh $wport, -snapshot)"
    rm -f "$dir/boot_s"
    echo 0 >"$dir/fw_cycles"
    # `eink run` execs QEMU, so $! of the nohup job is the QEMU pid. The `&` must apply to
    # nohup alone (not to a `cd && nohup` list, whose $! would be a subshell).
    (cd "$EMU" || exit 1
     nohup ./eink run "$name" --headless --serial-socket --qmp-socket \
        --ssh-port "$port" --wifi-ssh-port "$wport" -- -snapshot ${EMU_QEMU_EXTRA:-} >"$dir/run.log" 2>&1 </dev/null 8>&- 9>&- &
     echo $! >"$dir/qemu.pid")
    UP_FRESH_DIR=
    sleep 2
    alive_qemu "$dir" >/dev/null || { cat "$dir/run.log" >&2; die "QEMU did not start (slot $n kept; '$0 down $name' releases it)"; }
  fi
  start=$(now)
  deadline=$(( start + BOOT_DEADLINE_S ))
  until ssh "${SSH_OPTS[@]}" -o ConnectTimeout=3 -p "$port" root@127.0.0.1 true 2>/dev/null; do
    [ "$(now)" -lt "$deadline" ] || die "SSH on $port did not answer within ${BOOT_DEADLINE_S}s (see $EMU/machines/$name/$name.serial.log); QEMU keeps running: '$0 restart $name' or '$0 down $name'"
    alive_qemu "$dir" >/dev/null || die "QEMU exited during boot (see $dir/run.log); '$0 down $name' releases the slot"
    sleep 5
  done
  log "SSH up after $(( $(now) - start ))s"
  until guest "$name" 'test -d /var/local/kmc/bin' 2>/dev/null; do
    [ "$(now)" -lt "$deadline" ] || die "/var/local/kmc/bin missing; read /var/local/eink-addons/jbsh.log in the guest"
    sleep 5
  done
  log "jailbroken after $(( $(now) - start ))s"
  # On 5.19.5 appmgrd reports com.lab126.KPPMainApp for Home and books alike, so Home is
  # detected on a screendump (tab bar at the bottom; see scripts/emu-screen.py).
  shot="$dir/home.png"
  until qmp "$name" screendump "$shot" >/dev/null 2>&1 && python3 "$ROOT/scripts/emu-screen.py" home "$shot" >/dev/null; do
    [ "$(now)" -lt "$deadline" ] || die "Home did not appear within ${BOOT_DEADLINE_S}s (last screen: $shot)"
    sleep 5
  done
  [ -f "$dir/boot_s" ] || echo $(( $(now) - start )) >"$dir/boot_s"
  log "Home after $(( $(now) - start ))s; screenshot $shot"
}

cmd_up() {
  local name=$1 n dir
  check_name "$name"
  take_up_lock "$name"
  n=$(find_slot "$name")
  if [ -z "$n" ]; then
    n=$(cmd_acquire "$name")
    UP_NAME=$name
    UP_FRESH_DIR=$(slot_dir "$n")
  fi
  dir=$(slot_dir "$n")
  echo $$ >"$dir/up.pid"
  if ! instance_exists "$name"; then
    log "creating $name from the $FIRMWARE base (--jbsh)"
    (cd "$EMU" && ./eink create "$name" --model kindle-paperwhite-6 --profile production --firmware "$FIRMWARE" --jbsh) \
      >"$dir/create.log" 2>&1 8>&- 9>&- || { cat "$dir/create.log" >&2; UP_NAME=$name; UP_FRESH_DIR=$dir; die "create $name failed"; }
  fi
  boot_slot "$name" "$n"
  rm -f "$dir/up.pid"
  echo "$n"
}

cmd_restart() {
  local name=$1 n dir pid
  check_name "$name"
  take_up_lock "$name"
  n=$(need_slot "$name")
  dir=$(slot_dir "$n")
  echo $$ >"$dir/up.pid"
  if pid=$(alive_qemu "$dir"); then
    log "stopping $name (QEMU $pid) for a fresh boot; slot $n stays held"
    stop_qemu "$name" "$pid"
  fi
  [ -n "$(qemu_pid_of "$name")" ] || rm -f "$EMU/machines/$name/$name.serial.sock"
  boot_slot "$name" "$n"
  rm -f "$dir/up.pid"
  echo "$n"
}

cmd_down() {
  local name=$1 n dir pid
  check_name "$name"
  n=$(find_slot "$name")
  if [ -n "$n" ]; then
    dir=$(slot_dir "$n")
    if pid=$(alive_qemu "$dir"); then
      stop_qemu "$name" "$pid"
    fi
  fi
  # A QEMU for NAME without (or outside) its slot lock is stopped too.
  pid=$(qemu_pid_of "$name")
  if [ -n "$pid" ]; then
    log "$name runs as QEMU $pid without a matching slot record; stopping it"
    stop_qemu "$name" "$pid"
  fi
  rm -f "$EMU/machines/$name/$name.serial.sock"
  if [ -z "$n" ]; then
    log "$name holds no slot"
    return 0
  fi
  # Between the stop above and this point another agent's cleanup may have dropped the
  # lock (QEMU gone) and its acquire may have taken slot $n: remove it only if it is
  # still NAME's (checked under the slot lock).
  with_lock release_dir_locked "$name" "$dir" "(stopped)" || exit 1
  log "$name stopped"
}

cmd_push() {
  local name=$1 n dest
  shift
  n=$(need_slot "$name")
  dest=${*: -1}
  set -- "${@:1:$(($# - 1))}"
  scp -O "${SSH_OPTS[@]}" -P "$(ssh_port "$n")" "$@" "root@127.0.0.1:$dest"
}

cmd_pull() {
  local n
  n=$(need_slot "$1")
  scp -O "${SSH_OPTS[@]}" -P "$(ssh_port "$n")" "root@127.0.0.1:$2" "$3"
}

cmd_deploy() {
  local name=$1 build=${2:-} bin
  need_slot "$name" >/dev/null
  bin=${EMU_DEPLOY_BIN:-$ROOT/target/armv7-unknown-linux-musleabihf/release/rinky}
  if [ "$build" = --no-build ]; then
    [ -f "$bin" ] || die "$bin missing; deploy once without --no-build"
  else
    (cd "$ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)} cargo zigbuild --release \
        --target armv7-unknown-linux-musleabihf -p ink-app)
  fi
  guest "$name" "mkdir -p $DEST"
  cmd_push "$name" "$bin" "$ROOT/scripts/rinky.sh" "$ROOT/scripts/rinky-watchdog.sh" "$ROOT/packaging/rinky-ctl.sh" \
    "$ROOT"/testdata/*.epub "$DEST/"
  # EMU_DEPLOY_BIN may have another file name (a copy of the ARM build): the guest wants "rinky".
  [ "$(basename "$bin")" = rinky ] || guest "$name" "mv -f $DEST/$(basename "$bin") $DEST/rinky"
  guest "$name" "chmod +x $DEST/rinky $DEST/rinky.sh $DEST/rinky-watchdog.sh $DEST/rinky-ctl.sh; ls -la $DEST"
}

cmd_shot() {
  check_name "$1"
  local file=$2
  mkdir -p "$(dirname "$file")"
  qmp "$1" screendump "$(cd "$(dirname "$file")" && pwd)/$(basename "$file")" >/dev/null
  echo "$file"
}

cmd_log() {
  local name=$1 lines=${2:-60}
  check_name "$name"
  echo "== $DEST/rinky.log"
  guest "$name" "tail -n $lines $DEST/rinky.log 2>/dev/null" || true
  echo "== $DEST/watchdog.log"
  guest "$name" "tail -n $lines $DEST/watchdog.log 2>/dev/null" || true
  echo "== serial"
  tail -n "$lines" "$EMU/machines/$name/$name.serial.log" 2>/dev/null || true
}

cmd_wifi() {
  local name=$1 deadline
  check_name "$name"
  local start step=0
  # Reuse a Kindle-QEMU network the supplicant already knows (a second `wifi` call).
  guest "$name" 'id=$(wpa_cli -i wlan0 list_networks | awk "/Kindle-QEMU/ {print \$1; exit}")
    [ -n "$id" ] || id=$(wpa_cli -i wlan0 add_network | tail -1)
    wpa_cli -i wlan0 set_network $id ssid Kindle-QEMU >/dev/null &&
    wpa_cli -i wlan0 set_network $id key_mgmt NONE >/dev/null &&
    wpa_cli -i wlan0 select_network $id >/dev/null'
  start=$(now)
  deadline=$(( start + $(scaled 60) ))
  until guest "$name" 'ifconfig wlan0 | grep -q "inet addr:10.0.2."' 2>/dev/null; do
    [ "$(now)" -lt "$deadline" ] || die "wlan0 got no 10.0.2.x address within $(scaled 60) s"
    # On a fresh 5.19.5 boot the supplicant can stay in SCANNING with ap_scan 2
    # (device, net and plugin api-requests): reassociate after 10 s, and after 20 s
    # switch to ap_scan 1 and select the network again.
    if [ $step -eq 0 ] && [ $(( $(now) - start )) -ge 10 ]; then
      step=1; log "wifi: no address after 10 s; wpa_cli reassociate"
      guest "$name" 'wpa_cli -i wlan0 reassociate >/dev/null' || true
    elif [ $step -eq 1 ] && [ $(( $(now) - start )) -ge 20 ]; then
      step=2; log "wifi: no address after 20 s; ap_scan 1, select_network, reassociate"
      guest "$name" 'wpa_cli -i wlan0 ap_scan 1 >/dev/null
        id=$(wpa_cli -i wlan0 list_networks | awk "/Kindle-QEMU/ {print \$1; exit}")
        [ -n "$id" ] && wpa_cli -i wlan0 select_network $id >/dev/null
        wpa_cli -i wlan0 reassociate >/dev/null' || true
    fi
    sleep 2
  done
  guest "$name" 'ifconfig wlan0 | grep "inet addr"; lipc-get-prop com.lab126.wifid cmState'
}

usage() { sed -n '2,53p' "$0"; exit 2; }

cmd=${1:-}
shift || true
case "$cmd" in
  acquire) cmd_acquire "${1:-}" ;;
  release) cmd_release "${1:-}" ;;
  status) cmd_status ;;
  cleanup) cmd_cleanup ;;
  port) n=$(need_slot "${1:-}"); ssh_port "$n" ;;
  slotdir) n=$(need_slot "${1:-}"); slot_dir "$n" ;;
  up) cmd_up "${1:-}" ;;
  restart) check_name "${1:-}"; cmd_restart "$1" ;;
  down) cmd_down "${1:-}" ;;
  ssh) name=${1:-}; shift || true; guest "$name" "$@" ;;
  push) check_name "${1:-}"; cmd_push "$@" ;;
  pull) check_name "${1:-}"; cmd_pull "$@" ;;
  deploy) check_name "${1:-}"; cmd_deploy "$1" "${2:-}" ;;
  shot) check_name "${1:-}"; [ $# -eq 2 ] || usage; cmd_shot "$1" "$2" ;;
  tap) check_name "${1:-}"; [ $# -ge 3 ] || usage
       python3 "$ROOT/scripts/emu-input.py" --machine "$1" tap "$2" "$3" --hold "${4:-0.08}" ;;
  swipe) check_name "${1:-}"; [ $# -ge 5 ] || usage
         python3 "$ROOT/scripts/emu-input.py" --machine "$1" swipe "$2" "$3" "$4" "$5" --steps "${6:-8}" ;;
  hold) check_name "${1:-}"; [ $# -eq 4 ] || usage
        python3 "$ROOT/scripts/emu-input.py" --machine "$1" hold "$2" "$3" "$4" ;;
  power) check_name "${1:-}"
         guest "$1" "$DEST/rinky inject power; (lipc-set-prop -i com.lab126.powerd powerButton 1 >/dev/null 2>&1 &); echo 'powerd powerButton 1 sent'" ;;
  log) cmd_log "${1:-}" "${2:-60}" ;;
  wifi) cmd_wifi "${1:-}" ;;
  *) usage ;;
esac
