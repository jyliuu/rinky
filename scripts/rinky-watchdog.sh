#!/bin/sh
# Watchdog for rinky.sh: makes sure the Kindle framework comes back.
# Started by the launcher in its own session. It polls once a second:
#   - launcher finished cleanly ($RUN/done exists)     -> make sure the framework runs, exit;
#     (every framework start here first starts the Amazon jobs the launcher stopped,
#     listed in $RUN/quieted;)
#   - launcher gone (crash, kill -9) while rinky runs -> stop rinky, undo its
#     settings, start the framework, exit;
#   - rinky gone but launcher still there          -> give the launcher 15 s to
#     restore the framework, then do it here, exit;
#   - rinky alive but its heartbeat ($RUN/heartbeat, a counter rinky rewrites
#     at least every 5 s while its event loop runs) unchanged for RINKY_HANG_S
#     (default 60) of this watchdog's own polls -> hung: SIGTERM, then SIGKILL; the
#     launcher's wait returns and it restores the framework.
# The hang count uses polls, not wall-clock time: across a kernel suspend the clock
# jumps but neither process runs, so a resume never looks like a hang.
# Never starts the framework while rinky is running normally.
DIR=${RINKY_DIR:-$(cd "$(dirname "$0")" && pwd)}
LOG=$DIR/watchdog.log
RUN=${RINKY_RUN:-/tmp/rinky}
HANG_S=${RINKY_HANG_S:-60}
[ -f "$DIR/rinky.env" ] && . "$DIR/rinky.env"
cd /

if [ -x "$RUN/bin/rinky" ]; then BIN=$RUN/bin/rinky; else BIN=$DIR/rinky; fi
# The musl loader that runs it (rinky.sh copies it beside the binary in $RUN/bin).
LDSO=$(dirname "$BIN")/ld-musl-armhf.so.1
[ -f "$LDSO" ] || LDSO=$DIR/lib/ld-musl-armhf.so.1
[ -f "$LDSO" ] || LDSO=

# The log is fd 3, opened once (see rinky.sh): a plain `>>` redirection to the log
# makes busybox sh skip the whole command when the log cannot be opened, and that
# skipped `start lab126_gui`. Fallback: a log on tmpfs, then /dev/null.
open_log() {
    mkdir -p "$RUN" 2>/dev/null
    for f in "$LOG" "$RUN/$(basename "$LOG")"; do
        if (exec 2>/dev/null; : >>"$f"); then
            exec 3>>"$f"
            [ "$f" = "$LOG" ] || log "cannot write $LOG; logging to $f"
            return 0
        fi
    done
    exec 3>/dev/null
}

log() { echo "[watchdog $(date '+%H:%M:%S')] $*" >&3; }
open_log

framework_running() {
    status lab126_gui 2>/dev/null | grep -q "start/"
}

# appmgrd keeps running while lab126_gui is stopped. On 5.19.5 (emulator) it records
# KPPMainApp's stop as `handle_app_failure ... abnormalExit=1` and shows "Application
# Error: The selected application could not be started" once lab126_gui is back - also
# for a plain stop/start without rinky (docs/ARCHITECTURE.md, "Sleep and the Kindle framework").
# Restarting appmgrd before the framework starts clears that. RINKY_RESTART_APPMGRD=0
# turns it off. Only done when the framework is not running.
restart_appmgrd() {
    [ "${RINKY_RESTART_APPMGRD:-1}" = 0 ] && return 0
    log "restart appmgrd"
    (stop appmgrd; start appmgrd) >&3 2>&3
    # Start the framework only once appmgrd answers on lipc, plus 2 s: with 1 s between
    # `start appmgrd` and `start lab126_gui` the emulator showed the alert again
    # (device-sleep e2e run), with 2 s it did not.
    n=0
    until lipc-get-prop -s com.lab126.appmgrd activeApp >/dev/null 2>&1; do
        [ $n -ge 20 ] && { log "appmgrd does not answer after 10 s"; break; }
        usleep 500000
        n=$((n + 1))
    done
    sleep 2
    log "appmgrd ready"
}

replay_journal() {
    J=$RUN/restore
    [ -f "$J" ] || return 0
    log "restoring settings: $(tr '\n' ';' <"$J")"
    if ! RINKY_RUN=$RUN $LDSO "$BIN" device restore >&3 2>&3; then
        log "rinky device restore failed; replaying lipc lines in the shell"
        while read -r kind jt pub prop val; do
            [ "$kind" = lipc ] && lipc-set-prop "-$jt" "$pub" "$prop" "$val" >&3 2>&3
        done <"$J"
    fi
    rm -f "$J"
}

# wake_amazon: starts the jobs the launcher stopped ($RUN/quieted), the last stopped
# first, once the launcher's background stop ($RUN/quiet.pid) has ended (the same
# function as in rinky.sh).
wake_amazon() {
    q=$(cat "$RUN/quiet.pid" 2>/dev/null)
    n=0
    while [ -n "$q" ] && kill -0 "$q" 2>/dev/null && [ $n -lt 30 ]; do sleep 1; n=$((n + 1)); done
    rm -f "$RUN/quiet.pid"
    # The Kindle's screensaver (blanket's module), unloaded while rinky ran.
    if [ -f "$RUN/blanket-unloaded" ]; then
        lipc-set-prop com.lab126.blanket load screensaver >/dev/null 2>&1
        rm -f "$RUN/blanket-unloaded"
        log "blanket: screensaver loaded again"
    fi
    [ -s "$RUN/quieted" ] || return 0
    log "starting again: $(tr '\n' ' ' <"$RUN/quieted")"
    for j in $(sed '1!G;h;$!d' "$RUN/quieted"); do
        (start "$j") >/dev/null 2>&1
    done
    rm -f "$RUN/quieted"
}

start_framework() {
    if ! mkdir "$RUN/fwstart" 2>/dev/null; then
        owner=$(cat "$RUN/fwstart/pid" 2>/dev/null)
        if [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
            log "framework start already in progress (pid $owner)"
            return 0
        fi
        rm -rf "$RUN/fwstart"
        mkdir "$RUN/fwstart" 2>/dev/null || return 0
    fi
    echo $$ >"$RUN/fwstart/pid"
    wake_amazon
    if framework_running; then
        log "framework already started"
    else
        restart_appmgrd
        log "start lab126_gui"
        (cd / && start lab126_gui) >&3 2>&3
    fi
    rm -rf "$RUN/fwstart"
}

alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }

stop_ink() {
    kill -TERM "$1" 2>/dev/null
    j=0
    while alive "$1" && [ $j -lt 30 ]; do usleep 100000; j=$((j + 1)); done
    kill -KILL "$1" 2>/dev/null
}

log "watching (pid $$, hang limit ${HANG_S}s)"
LAUNCHER=
i=0
# The launcher writes its pid before starting us; wait briefly for it anyway.
while [ -z "$LAUNCHER" ] && [ $i -lt 50 ]; do
    LAUNCHER=$(cat "$RUN/launcher.pid" 2>/dev/null)
    [ -n "$LAUNCHER" ] || usleep 100000
    i=$((i + 1))
done
log "launcher $LAUNCHER"
gone_since=
hb_last=
hb_same=0

while :; do
    if [ -f "$RUN/done" ]; then
        log "launcher finished"
        start_framework
        exit 0
    fi
    INK=$(cat "$RUN/rinky.pid" 2>/dev/null)
    if ! alive "$LAUNCHER"; then
        log "launcher $LAUNCHER is gone"
        if alive "$INK"; then
            log "stopping orphaned rinky $INK"
            echo "launcher gone" >"$RUN/watchdog-kill" 2>/dev/null
            stop_ink "$INK"
        fi
        replay_journal
        start_framework
        rm -f "$RUN/rinky.pid" "$RUN/launcher.pid" "$RUN/heartbeat"
        # The dead launcher's own lock only: a lock without a pid may be a new
        # launcher's, between its mkdir and its pid write.
        [ "$(cat "$RUN/lock/pid" 2>/dev/null)" = "$LAUNCHER" ] && rm -rf "$RUN/lock"
        exit 0
    fi
    if [ -n "$INK" ] && ! alive "$INK"; then
        now=$(date +%s)
        [ -n "$gone_since" ] || { gone_since=$now; log "rinky $INK is gone; launcher should restore the framework"; }
        if [ $((now - gone_since)) -ge 15 ]; then
            log "launcher did not finish in 15 s"
            replay_journal
            start_framework
            exit 0
        fi
    elif [ -z "$INK" ]; then
        # Between two rinkys of a restart ($RUN/restart; the launcher removes the pid
        # file and runs the new binary): nothing to watch, nothing to start. If the launcher
        # dies in this window the "launcher is gone" branch above still starts the framework.
        gone_since=
        hb_last=
        hb_same=0
    elif [ -n "$INK" ]; then
        gone_since=
        hb=$(cat "$RUN/heartbeat" 2>/dev/null)
        if [ -n "$hb" ]; then
            if [ "$hb" = "$hb_last" ]; then
                hb_same=$((hb_same + 1))
            else
                hb_same=0
                hb_last=$hb
            fi
            if [ $hb_same -ge "$HANG_S" ]; then
                log "rinky $INK hung: heartbeat $hb unchanged for $hb_same polls; killing it"
                # The launcher reports this as the exit reason (exit: reason=watchdog ...).
                echo "hang: heartbeat $hb unchanged for $hb_same s" >"$RUN/watchdog-kill" 2>/dev/null
                stop_ink "$INK"
                hb_same=0
            fi
        fi
    fi
    sleep 1
done
