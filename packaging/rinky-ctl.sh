#!/bin/sh
# rinky control script. Installed as /mnt/us/rinky/rinky-ctl.sh; used by the
# KUAL menu and by uninstall.sh, and usable over SSH.
#
#   sh /mnt/us/rinky/rinky-ctl.sh start [BOOK | --library]
#        start rinky through the framework-stop launcher (default: the library)
#   sh /mnt/us/rinky/rinky-ctl.sh stop
#        "Exit to Kindle": stop rinky if it runs and make sure the Kindle framework
#        (lab126_gui) runs. Safe to call when rinky is not running.
#   sh /mnt/us/rinky/rinky-ctl.sh restart [BOOK | --library]
#        swap the running rinky for the installed binary without starting the Kindle
#        framework in between: the launcher runs the new binary again with the same
#        arguments (the arguments here are only used when rinky is not running yet:
#        then this is the same as start). With an older launcher, or when the new
#        binary cannot start, the framework comes back as after stop.
#   sh /mnt/us/rinky/rinky-ctl.sh status
#        print whether rinky and the framework run
#
# Environment (tests): RINKY_PREFIX=DIR puts /mnt/us under DIR;
#                      RINKY_RUN=DIR is the runtime directory (default /tmp/rinky).
set -u

PREFIX=${RINKY_PREFIX:-}
DEST=$PREFIX/mnt/us/rinky
RUN=${RINKY_RUN:-/tmp/rinky}
LOG=$DEST/rinky.log

say() { echo "rinky-ctl: $*"; }
log() { echo "[ctl $(date '+%H:%M:%S')] $*" >>"$LOG" 2>/dev/null; }
alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }
nap() { usleep 200000 2>/dev/null || sleep 0.2 2>/dev/null || sleep 1; }

# wait_gone PID SECONDS: polls every 0.2 s; returns 1 when PID is still alive.
wait_gone() {
    n=0
    while alive "$1"; do
        [ "$n" -ge $(($2 * 5)) ] && return 1
        nap
        n=$((n + 1))
    done
    return 0
}

on_kindle() { command -v status >/dev/null 2>&1 && command -v start >/dev/null 2>&1; }
framework_running() { status lab126_gui 2>/dev/null | grep -q "start/"; }

# pgrep is not part of every Kindle busybox; ps is.
# shellcheck disable=SC2009
watchdog_pids() {
    ps -eo pid,args 2>/dev/null | grep "[r]inky-watchdog.sh" | while read -r pid _; do echo "$pid"; done
}

# An install copied over USB never ran install.sh, which lists the book .sdr folders
# that exist before rinky first runs (uninstall.sh keeps those). Make that list
# before the first start instead. Folders that already hold an rinky file are left
# out, as in install.sh.
record_sdr() {
    [ -f "$DEST/.install-sdr" ] && return 0
    US=$PREFIX/mnt/us
    (cd "$US" && find . \( -path ./koreader -o -path ./system -o -path ./rinky -o -path ./extensions \) -prune -o \
        -type d -name '*.sdr' -print -prune 2>/dev/null) | sed 's|^\./||' | while IFS= read -r d; do
        ls -d "$US/$d"/rinky* >/dev/null 2>&1 || echo "$d"
    done >"$DEST/.install-sdr.new$$" &&
        mv -f "$DEST/.install-sdr.new$$" "$DEST/.install-sdr" && log "start: recorded $(wc -l <"$DEST/.install-sdr") existing .sdr folder(s)"
    rm -f "$DEST/.install-sdr.new$$"
}

cmd_start() {
    [ -f "$DEST/rinky" ] || {
        say "rinky is not installed in $DEST"
        return 1
    }
    record_sdr
    # A copy over USB (MTP) can drop the exec bit; on a vfat userstore this is a no-op.
    [ -x "$DEST/rinky" ] || chmod 755 "$DEST/rinky" 2>/dev/null
    if alive "$(cat "$RUN/launcher.pid" 2>/dev/null)"; then
        say "rinky is already running"
        return 0
    fi
    [ $# -gt 0 ] || set -- --library
    log "start: $*"
    sh "$DEST/rinky.sh" "$@"
}

cmd_stop() {
    launcher=$(cat "$RUN/launcher.pid" 2>/dev/null)
    ink=$(cat "$RUN/rinky.pid" 2>/dev/null)
    if alive "$launcher"; then
        say "stopping the launcher $launcher (it starts the Kindle framework again)"
        log "stop: TERM launcher $launcher"
        kill -TERM "$launcher" 2>/dev/null
        wait_gone "$launcher" 60 || say "launcher $launcher still running after 60 s"
    fi
    if alive "$ink"; then
        say "stopping rinky $ink"
        log "stop: TERM rinky $ink"
        kill -TERM "$ink" 2>/dev/null
        wait_gone "$ink" 3 || kill -KILL "$ink" 2>/dev/null
    fi
    # The watchdog exits by itself once the launcher is done; give it a moment.
    for pid in $(watchdog_pids); do
        wait_gone "$pid" 5 || kill -TERM "$pid" 2>/dev/null
    done
    if on_kindle && ! framework_running; then
        say "starting the Kindle framework"
        log "stop: start lab126_gui"
        # Same order as the launcher: appmgrd first, or 5.19.5 shows "Application Error".
        (stop appmgrd; start appmgrd) >/dev/null 2>&1
        (cd / && start lab126_gui) >/dev/null 2>&1
    fi
    say "rinky is not running"
}

cmd_restart() {
    launcher=$(cat "$RUN/launcher.pid" 2>/dev/null)
    ink=$(cat "$RUN/rinky.pid" 2>/dev/null)
    if ! alive "$launcher" || ! alive "$ink" || [ ! -f "$RUN/restart-capable" ]; then
        # Not running, or a launcher from before this command existed: it would end and
        # start the framework when rinky exits, so do the full stop and start.
        say "no restart-capable launcher with a running rinky; stop and start"
        log "restart: launcher '$launcher' rinky '$ink' capable $([ -f "$RUN/restart-capable" ] && echo yes || echo no): stop, start"
        cmd_stop
        sleep 2
        cmd_start "$@"
        return $?
    fi
    say "restarting rinky $ink (the framework stays stopped)"
    log "restart: TERM rinky $ink, launcher $launcher"
    : >"$RUN/restart"
    kill -TERM "$ink" 2>/dev/null
    # The launcher writes the new pid after it has copied the binary and started it.
    n=0
    new=
    while alive "$launcher" && [ "$n" -lt 300 ]; do
        new=$(cat "$RUN/rinky.pid" 2>/dev/null)
        [ -n "$new" ] && [ "$new" != "$ink" ] && break
        new=
        nap
        n=$((n + 1))
    done
    if [ -n "$new" ] && alive "$new"; then
        say "rinky restarted (pid $new)"
        return 0
    fi
    say "restart did not leave rinky running; see $LOG"
    return 1
}

cmd_status() {
    if alive "$(cat "$RUN/rinky.pid" 2>/dev/null)"; then
        say "rinky running (pid $(cat "$RUN/rinky.pid"))"
    else
        say "rinky not running"
    fi
    if on_kindle; then
        say "framework: $(status lab126_gui 2>/dev/null)"
    fi
    [ -f "$DEST/VERSION" ] && say "installed: $(sed -n 's/^version=//p' "$DEST/VERSION")"
    return 0
}

cmd=${1:-}
[ $# -gt 0 ] && shift
case "$cmd" in
    start) cmd_start "$@" ;;
    stop | exit) cmd_stop ;;
    restart) cmd_restart "$@" ;;
    status) cmd_status ;;
    *)
        sed -n '2,21p' "$0"
        exit 2
        ;;
esac
