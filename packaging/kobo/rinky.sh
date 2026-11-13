#!/bin/sh
# rinky launcher for Kobo (and Tolino) e-readers, started from NickelMenu, KFMon or SSH.
# Replaces Nickel while rinky runs, as KOReader's koreader.sh does:
#   1. copy this script and the binary to $RUN/bin (tmpfs) and run from there, in a
#      session of its own, so nothing runs from /mnt/onboard (a USB connection after
#      exit, or an update, cannot hit a running file);
#   2. take Nickel's environment (PLATFORM, PRODUCT, WIFI_MODULE, INTERFACE, ...),
#      turn Bluetooth off, stop Nickel and its helpers;
#   3. run rinky, through the musl loader the package ships (lib/ld-musl-armhf.so.1,
#      copied to $RUN/bin too): rinky is a dynamically linked musl program and the Kobo
#      has only glibc; when it exits after writing $RUN/restart (an in-app update does),
#      the launcher runs the installed binary again without starting Nickel;
#   4. on every exit path: put back what rinky journaled (framebuffer bit depth and
#      rotation), turn Wi-Fi off, and start Nickel again (reboot if Nickel was not
#      running before, or KFMon asked for that).
#
#   sh /mnt/onboard/.adds/rinky/rinky.sh [BOOK | --library]
#   sh /mnt/onboard/.adds/rinky/rinky.sh -- SUBCOMMAND ARGS...   (any rinky subcommand)
#
# Settings: $DIR/rinky.env is sourced if present (lines like `export RINKY_SLEEP=0`).
# Log: $DIR/rinky.log (launcher and rinky).
DIR=${RINKY_DIR:-/mnt/onboard/.adds/rinky}
RUN=${RINKY_RUN:-/tmp/rinky}
LOG=$DIR/rinky.log
PATH="/sbin:/bin:/usr/sbin:/usr/bin:/usr/lib:$PATH"
[ -f "$DIR/rinky.env" ] && . "$DIR/rinky.env"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S') launcher] $*" >>"$LOG" 2>/dev/null
}

# 1. Run from tmpfs, detached from the caller (NickelMenu waits for nothing).
if [ "$RINKY_DETACHED" != 1 ]; then
    mkdir -p "$RUN/bin"
    if [ -e "$RUN/launcher.pid" ] && kill -0 "$(cat "$RUN/launcher.pid")" 2>/dev/null; then
        log "already running (pid $(cat "$RUN/launcher.pid"))"
        exit 0
    fi
    cp -f "$DIR/rinky.sh" "$RUN/bin/rinky.sh" || exit 1
    RINKY_DETACHED=1 RINKY_DIR="$DIR" RINKY_RUN="$RUN" setsid sh "$RUN/bin/rinky.sh" "$@" </dev/null >/dev/null 2>&1 &
    exit 0
fi
echo $$ >"$RUN/launcher.pid"
cd / || exit 1
# Rotated at 2 MiB, as rinky itself does (and the Kindle launcher).
if [ "$(wc -c <"$LOG" 2>/dev/null || echo 0)" -ge 2097152 ]; then
    mv -f "$LOG.1" "$LOG.2" 2>/dev/null
    mv -f "$LOG" "$LOG.1"
fi
# For every rinky run below.
export RINKY_RUN="$RUN" RINKY_HOME="$DIR" RINKY_PLATFORM=kobo
log "start: $*"

# copy_binary: the binary and its loader to $RUN/bin; LDSO is the loader that runs
# BIN (empty for an install without lib/: the binary then runs directly).
copy_binary() {
    rm -f "$RUN/bin/rinky" "$RUN/bin/ld-musl-armhf.so.1"
    LDSO=
    if [ -f "$DIR/lib/ld-musl-armhf.so.1" ]; then
        cp -f "$DIR/lib/ld-musl-armhf.so.1" "$RUN/bin/ld-musl-armhf.so.1" && chmod 755 "$RUN/bin/ld-musl-armhf.so.1" &&
            LDSO=$RUN/bin/ld-musl-armhf.so.1
    fi
    if [ -n "$LDSO" ] || [ ! -f "$DIR/lib/ld-musl-armhf.so.1" ]; then
        cp -f "$DIR/rinky" "$RUN/bin/rinky" && chmod 755 "$RUN/bin/rinky" && BIN=$RUN/bin/rinky && return 0
    fi
    # A full tmpfs: run from the userstore instead (USB storage must then wait).
    log "cannot copy the binary to $RUN/bin; running it from $DIR"
    rm -f "$RUN/bin/rinky" "$RUN/bin/ld-musl-armhf.so.1"
    BIN=$DIR/rinky
    LDSO=
    [ -f "$DIR/lib/ld-musl-armhf.so.1" ] && LDSO=$DIR/lib/ld-musl-armhf.so.1
}
# run_bin ARGS...: the binary through its loader.
run_bin() {
    if [ -n "$LDSO" ]; then "$LDSO" "$BIN" "$@"; else "$BIN" "$@"; fi
}
copy_binary

# 2. Nickel's environment, then stop it.
pid_of() {
    pidof "$1" 2>/dev/null | cut -d' ' -f1
}
import_env() {
    [ -n "$1" ] && [ -r "/proc/$1/environ" ] || return
    for kv in $(tr '\0' '\n' <"/proc/$1/environ" | grep -E '^(PLATFORM|PRODUCT|WIFI_MODULE|INTERFACE|LANG|NICKEL_HOME|DBUS_SESSION_BUS_ADDRESS)='); do
        export "${kv?}"
    done
}
NICKEL_PID=$(pid_of nickel)
FROM_NICKEL=false
[ -n "$NICKEL_PID" ] && FROM_NICKEL=true
FROM_KFMON=false
[ -n "$(pid_of kfmon)" ] && FROM_KFMON=true
import_env "$NICKEL_PID"
[ -z "$PLATFORM" ] || [ -z "$PRODUCT" ] && import_env "$(pid_of udevd)"
[ -z "$PRODUCT" ] && [ -x /bin/kobo_config.sh ] && PRODUCT=$(/bin/kobo_config.sh 2>/dev/null) && export PRODUCT
# Saved for rinky commands run later (Wi-Fi off below, `rinky device` over SSH).
for v in PLATFORM PRODUCT WIFI_MODULE INTERFACE; do
    eval "val=\${$v}"
    [ -n "$val" ] && echo "$v=$val"
done >"$RUN/nickel.env"
log "nickel ${NICKEL_PID:-not running}; platform ${PLATFORM:-?} product ${PRODUCT:-?} wifi ${WIFI_MODULE:-?}/${INTERFACE:-?}"

if [ "$FROM_NICKEL" = true ]; then
    # Bluetooth off: rinky has no use for it, and a live stack keeps the device awake.
    if [ -e /sys/devices/platform/bt/rfkill/rfkill0/state ]; then
        echo 0 >/sys/devices/platform/bt/rfkill/rfkill0/state 2>/dev/null
    fi
    sync
    killall -q -TERM nickel hindenburg sickel fickel strickel fontickel adobehost foxitpdf iink dhcpcd-dbus dhcpcd \
        bluealsa bluetoothd fmon nanoclock.lua memorylogger QtWebEngineProcess 2>/dev/null
    n=0
    while [ -n "$(pid_of nickel)" ] && [ $n -lt 20 ]; do
        usleep 250000 2>/dev/null || sleep 1
        n=$((n + 1))
    done
    [ -n "$(pid_of nickel)" ] && log "nickel still running after 5 s"
    rm -f /tmp/nickel-hardware-status
fi

# 3. Run rinky (again after a restart request).
touch "$RUN/restart-capable"
rm -f "$RUN/restart"
case "$1" in
    --) shift; set -- "$@" ;;
    --library | "") set -- library ;;
    *) set -- run "$@" ;;
esac
while :; do
    log "run: ${LDSO:+$LDSO }$BIN $*"
    # Exported, not a prefix: a prefix assignment on a shell function is not
    # reliably passed on to the commands it runs.
    export RINKY_LOG_FILE="$LOG"
    run_bin "$@" >>"$LOG" 2>&1
    status=$?
    log "rinky exited with status $status"
    if [ -e "$RUN/restart" ]; then
        rm -f "$RUN/restart"
        log "restart requested"
        copy_binary
        continue
    fi
    break
done

# 4. Put things back and start Nickel.
unset RINKY_LOG_FILE
run_bin device restore >>"$LOG" 2>&1
run_bin device wifi enable 0 >>"$LOG" 2>&1
rm -f "$RUN/restart-capable" "$RUN/launcher.pid" "$RUN/bin/rinky" "$RUN/bin/ld-musl-armhf.so.1" "$RUN/nickel.env"
sync

if [ "$FROM_KFMON" = true ] && ! grep -qs "reboot_on_exit=false" /mnt/onboard/.adds/kfmon/config/rinky.ini; then
    log "started from KFMon: reboot"
    /sbin/reboot
    exit 0
fi
if [ "$FROM_NICKEL" != true ]; then
    log "Nickel was not running when rinky started; leaving it stopped"
    exit 0
fi
log "starting Nickel"
[ -e /dev/mmcblk1p1 ] && umount /mnt/sd 2>/dev/null
if [ -e /etc/init.d/z-nickel-hardware-status ]; then
    # Firmware 5.x: the boot scripts start Nickel and what it needs. rc.local runs
    # under `sh -e` and starts with `killall animator.sh`, which fails when no boot
    # animation runs: run it with plain sh, so a failed step does not stop it before
    # Nickel. It starts pulseaudio and speech-dispatcher again (KOReader's nickel.sh).
    unset LD_LIBRARY_PATH
    grep -q pulseaudio /etc/rc.local && killall -q pulseaudio 2>/dev/null
    grep -q speech-dispatcher /etc/rc.local && killall -q speech-dispatcher 2>/dev/null
    /etc/init.d/z-nickel-hardware-status >/dev/null 2>&1
    sync
    sh /etc/rc.local >/dev/null 2>&1 &
else
    export LD_LIBRARY_PATH=/usr/local/Kobo
    rm -f /tmp/nickel-hardware-status
    mkfifo /tmp/nickel-hardware-status
    /usr/local/Kobo/hindenburg &
    LIBC_FATAL_STDERR_=1 /usr/local/Kobo/nickel -platform kobo -skipFontLoad &
    [ "$PLATFORM" != freescale ] && udevadm trigger &
fi
exit 0
