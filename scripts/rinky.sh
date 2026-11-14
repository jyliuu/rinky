#!/bin/sh
# rinky launcher for jailbroken Kindles with upstart (firmware 5.x).
# Replaces the Kindle framework while rinky runs:
#   1. copy the binary and both scripts to $RUN/bin (tmpfs) and detach from the caller
#      (KUAL, ssh) into its own session, so nothing runs from /mnt/us (USB drive mode
#      can take the userstore, and a redeploy cannot hit a running file);
#   2. start rinky-watchdog.sh (own session too);
#   3. stop lab126_gui and wait until KPPMainApp and pillowd are gone;
#   3b. unload the Kindle's screensaver (blanket), stop the Amazon services rinky has
#      no use for ($QUIET_JOBS: cloud sync,
#      metrics, log uploaders, audio, Bluetooth; listed in $RUN/quieted) and kill what
#      a stopped job left running (its shell died, its daemon did not: fastmetrics,
#      wifilocker; a stuck orphaned fastmetrics made powerd take 32 s per event);
#   4. run rinky, through the musl loader the package ships (lib/ld-musl-armhf.so.1,
#      copied next to the binary in $RUN/bin): rinky is a dynamically linked musl
#      program and the Kindle has only glibc;
#      (`rinky-ctl.sh restart` writes $RUN/restart and sends SIGTERM to rinky: the
#      launcher then copies the installed binary again and runs it again, without
#      starting the framework in between; if that fails it takes the exit path below;)
#      (a crash inside a native plugin leaves $RUN/plugin-crashed: the launcher runs
#      rinky again, which turns that plugin off, without starting the framework; at
#      most twice within 60 s, then it takes the exit path below;)
#   5. on every exit path (normal exit, error, crash, signal, watchdog kill):
#      undo the settings rinky journaled ($RUN/restore: volumd disableMounting,
#      powerd flIntensity, the framebuffer night flag, ...), start the jobs in
#      $RUN/quieted again, then start lab126_gui.
# If this launcher itself is killed (even with kill -9), the watchdog stops rinky,
# undoes the settings and starts the framework. If rinky hangs (its heartbeat file
# stops changing for RINKY_HANG_S seconds), the watchdog kills it and this
# launcher restores the framework.
#
#   sh /mnt/us/rinky/rinky.sh [BOOK [extra run arguments]]
#   sh /mnt/us/rinky/rinky.sh --library
#   sh /mnt/us/rinky/rinky.sh -- SUBCOMMAND ARGS...   (any rinky subcommand)
#
# Settings: $DIR/rinky.env is sourced if present (lines like
# `export RINKY_AUTOSLEEP=10`); RINKY_RESTART_APPMGRD=0 skips the appmgrd
# restart; RINKY_STOP_DEADLINE (s) bounds the framework stop; RINKY_QUIET=0 leaves
# the Amazon services running; RINKY_QUIET_JOBS replaces their list.
# Screen: "Starting rinky..." is drawn (by `rinky splash`, or FBInk) right away
# and again once the framework is stopped; "Closing rinky..." (or the crash note)
# before the framework starts again, so the screen is never left blank.
# Log: every phase logs its duration ("phase <name>: N ms", from /proc/uptime); the
# end is one "exit: reason=<...> status=<n>" line (reason from rinky's
# $RUN/exit-reason, the watchdog's $RUN/watchdog-kill or the exit status). The log is
# rotated at 2 MB (rinky.log.1, .2) before the launcher opens it; rinky writes
# the same file itself (RINKY_LOG_FILE) and rotates it while it runs.
# Files: $DIR/rinky.log (launcher + rinky), $DIR/watchdog.log,
#        $RUN/{lock/pid,launcher.pid,rinky.pid,done,heartbeat,restore,bin/} with
#        RUN=/tmp/rinky (bin/rinky is deleted when the launcher exits). When $DIR/rinky.log cannot be written (read-only,
#        full or missing userstore), the log is $RUN/rinky.log (tmpfs) instead.
DIR=${RINKY_DIR:-$(cd "$(dirname "$0")" && pwd)}
LOG=$DIR/rinky.log
RUN=${RINKY_RUN:-/tmp/rinky}
STOP_DEADLINE=${RINKY_STOP_DEADLINE:-40}
[ -f "$DIR/rinky.env" ] && . "$DIR/rinky.env"
# Upstart jobs stopped while rinky runs (5.19.5 names; a job that is not running is
# skipped). Dependents come before what they depend on. Kept: powerd, lipcd, deviced,
# cmd/wifid/wifis (network), volumd and mtp (USB), tmd (thermal), battstressd (battery
# protection), x, dbus and the system jobs.
QUIET_JOBS=${RINKY_QUIET_JOBS-"todo adm odot archive dpmd dynconfig rcm acedropboxd logmgr
    ace_messaging_service ace_eventmgr integrity_listener phd demd platformmetricsd
    minerva_service minervad perfd stackdumpd ttsorchestrator playermgr_limit playermgr
    audiomgrd acsbtfd btmanagerd appmgrd"}

# The log is opened once, as fd 3, and every command writes to it with >&3 2>&3.
# Duplicating an open descriptor cannot fail. A plain `>>` redirection to the log can:
# busybox sh then skips the command itself, so a read-only or full userstore (ext4
# error, USB export, the log replaced by a directory) used to skip `stop lab126_gui`,
# `rinky device restore` and `start lab126_gui`. Fallback: a log on tmpfs, then
# /dev/null. The probe runs in a subshell because a failed `exec 3>>FILE` ends the
# shell (`exec` is a special builtin; checked on the 5.19.5 busybox 1.34.1 sh).
open_log() {
    mkdir -p "$RUN" 2>/dev/null
    for f in "$LOG" "$RUN/$(basename "$LOG")"; do
        if (exec 2>/dev/null; : >>"$f"); then
            exec 3>>"$f"
            LOGFILE=$f
            [ "$f" = "$LOG" ] || log "cannot write $LOG; logging to $f"
            return 0
        fi
    done
    LOGFILE=
    exec 3>/dev/null
}

# rotate_log: rinky.log over 2 MB becomes rinky.log.1 (.1 becomes .2), the
# same rule rinky uses while it runs.
rotate_log() {
    size=$(wc -c <"$LOG" 2>/dev/null) || return 0
    [ "${size:-0}" -ge 2097152 ] || return 0
    mv -f "$LOG.1" "$LOG.2" 2>/dev/null
    mv -f "$LOG" "$LOG.1" 2>/dev/null
}

log() { echo "[launcher $(date '+%H:%M:%S')] $*" >&3; }

# copy_pair TAG: the installed loader and binary to $RUN/bin (loader first, so a
# binary there always has its loader beside it).
copy_pair() {
    copy_loader "$1" && cp "$DIR/rinky" "$RUN/bin/.rinky$1" 2>/dev/null &&
        chmod 755 "$RUN/bin/.rinky$1" && mv -f "$RUN/bin/.rinky$1" "$RUN/bin/rinky"
}

# copy_loader TAG: copies the installed musl loader to $RUN/bin (cp to a name with TAG,
# then mv). No loader installed: nothing to copy, and the binary runs directly.
copy_loader() {
    [ -f "$DIR/lib/ld-musl-armhf.so.1" ] || { rm -f "$RUN/bin/ld-musl-armhf.so.1"; return 0; }
    cp "$DIR/lib/ld-musl-armhf.so.1" "$RUN/bin/.ld$1" 2>/dev/null &&
        chmod 755 "$RUN/bin/.ld$1" && mv -f "$RUN/bin/.ld$1" "$RUN/bin/ld-musl-armhf.so.1"
}

# now_ms: milliseconds since boot (/proc/uptime has 1/100 s); 0 without /proc.
now_ms() {
    read -r up _ </proc/uptime 2>/dev/null || { echo 0; return; }
    s=${up%.*}
    c=${up#*.}
    echo $((s * 1000 + ${c#0} * 10))
}
# phase NAME START_MS: logs how long NAME took.
phase() { log "phase $1: $(($(now_ms) - $2)) ms"; }

# loader_for BINARY: the musl loader that runs BINARY (beside it in $RUN/bin, in lib/
# next to it in $DIR); nothing when there is none (an install without lib/).
loader_for() {
    for f in "$(dirname "$1")/ld-musl-armhf.so.1" "$(dirname "$1")/lib/ld-musl-armhf.so.1"; do
        [ -f "$f" ] && { echo "$f"; return 0; }
    done
}

# run_bin BINARY ARGS...: BINARY through its loader (or directly without one), with
# RINKY_RUN set.
run_bin() {
    rb=$1
    shift
    rl=$(loader_for "$rb")
    if [ -n "$rl" ]; then RINKY_RUN=$RUN "$rl" "$rb" "$@"; else RINKY_RUN=$RUN "$rb" "$@"; fi
}

# splash TEXT...: a white screen with one centred line per TEXT. rinky draws it
# itself; FBInk (KMC or libkh) is the fallback.
splash() {
    sb=${BIN:-$DIR/rinky}
    if [ -x "$sb" ] && run_bin "$sb" splash "$@" >&3 2>&3; then return 0; fi
    for f in /var/local/kmc/bin/fbink /mnt/us/libkh/bin/fbink; do
        if [ -x "$f" ]; then
            "$f" -q -c -m -M "$*" >&3 2>&3 && return 0
        fi
    done
    log "splash failed: $*"
}

[ -z "$RINKY_DETACHED" ] && rotate_log
open_log

if [ -z "$RINKY_DETACHED" ]; then
    T0=$(now_ms)
    splash "Starting rinky..."
    phase splash "$T0"
    tc=$(now_ms)
    # Run from a tmpfs copy. cp + mv: a running copy keeps its (unlinked) file. The
    # temporary names carry this shell's pid: launchers started together each rename
    # their own copy (a shared name made the second mv fail, "No such file"). The
    # loader goes first: a binary in $RUN/bin always has its loader beside it.
    SELF=$DIR/rinky.sh
    t=.new.$$
    if mkdir -p "$RUN/bin" 2>/dev/null &&
        copy_pair "$t" &&
        cp "$DIR/rinky.sh" "$RUN/bin/.rinky.sh$t" && mv -f "$RUN/bin/.rinky.sh$t" "$RUN/bin/rinky.sh" &&
        cp "$DIR/rinky-watchdog.sh" "$RUN/bin/.rinky-watchdog.sh$t" &&
        mv -f "$RUN/bin/.rinky-watchdog.sh$t" "$RUN/bin/rinky-watchdog.sh"; then
        SELF=$RUN/bin/rinky.sh
    else
        rm -f "$RUN/bin/.rinky$t" "$RUN/bin/.rinky.sh$t" "$RUN/bin/.rinky-watchdog.sh$t" "$RUN/bin/.ld$t"
        log "cannot copy to $RUN/bin; running from $DIR"
        # /var (and /tmp) is a 64 MB tmpfs on the Kindle; a full one is the usual cause.
        log "free space: $(df -k "$RUN" /var /tmp 2>&1 | tr '\n' ';')"
    fi
    phase "copy to $RUN/bin" "$tc"
    # Detach so `stop lab126_gui` (which signals the framework's children) cannot kill us.
    RINKY_T0=$T0 RINKY_DIR=$DIR RINKY_DETACHED=1 setsid sh "$SELF" "$@" </dev/null >/dev/null 2>&1 3>&- &
    exit 0
fi
cd /

if [ -x "$RUN/bin/rinky" ] && [ "$(dirname "$0")" = "$RUN/bin" ]; then
    BIN=$RUN/bin/rinky
    WATCHDOG=$RUN/bin/rinky-watchdog.sh
else
    BIN=$DIR/rinky
    WATCHDOG=$DIR/rinky-watchdog.sh
fi

# One launcher at a time. The lock is the directory $RUN/lock; its owner writes its pid
# into $RUN/lock/pid right after mkdir (a new directory never holds an old pid). A lock
# with a dead owner is stale and is taken over. A lock without a pid belongs to a
# launcher between its mkdir and its pid write: it is stale only once it is older than a
# minute (an owner killed in that window), never at once (race test case 3b).
lock_fresh() { [ -n "$(find "$RUN" -maxdepth 1 -name lock -mmin -1 2>/dev/null)" ]; }
mkdir -p "$RUN"
tries=0
until mkdir "$RUN/lock" 2>/dev/null; do
    old=$(cat "$RUN/lock/pid" 2>/dev/null)
    if [ -z "$old" ]; then
        usleep 200000 2>/dev/null || sleep 1
        old=$(cat "$RUN/lock/pid" 2>/dev/null)
    fi
    if [ -n "$old" ] && kill -0 "$old" 2>/dev/null; then
        log "already running (launcher $old); exit"
        exit 1
    fi
    if [ -z "$old" ] && lock_fresh; then
        log "another launcher is starting (lock without pid); exit"
        exit 1
    fi
    tries=$((tries + 1))
    if [ $tries -gt 3 ]; then
        log "cannot take the lock $RUN/lock; exit"
        exit 1
    fi
    # Rename, then delete: of two launchers taking over the same stale lock only one
    # rename succeeds; the other goes round again and finds the winner's lock.
    log "removing stale lock (launcher ${old:-without pid} gone)"
    mv "$RUN/lock" "$RUN/.lock.stale.$$" 2>/dev/null && rm -rf "$RUN/.lock.stale.$$"
done
echo $$ >"$RUN/lock/pid"
echo $$ >"$RUN/launcher.pid"
rm -f "$RUN/done" "$RUN/rinky.pid" "$RUN/heartbeat" "$RUN/restart"
# rinky-ctl.sh restart only works with a launcher that has this file (an older
# launcher ends and starts the framework when rinky exits).
: >"$RUN/restart-capable"

if [ "$1" = "--library" ]; then
    shift
    set -- library "$@"
elif [ "$1" = "--" ]; then
    shift
else
    BOOK=${1:-$DIR/juliet.epub}
    [ $# -gt 0 ] && shift
    set -- run "$BOOK" "$@"
fi

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

# job_pids: "JOB PID NAME" for the main process of every running upstart job and all
# its descendants. Taken before the stops: a daemon whose job shell dies is
# reparented to init, and only this list still ties it to its job.
job_pids() {
    for d in /proc/[0-9]*; do
        read -r st 2>/dev/null <"$d/stat" || continue
        set -- ${st##*) }
        read -r name _ 2>/dev/null <"$d/comm" || continue
        echo "${d#/proc/} $2 $name"
    done >"$RUN/.procs"
    initctl list 2>/dev/null | sed -n 's|^\([^ ]*\) start/running, process \([0-9]*\)$|\1 \2|p' >"$RUN/.roots"
    awk 'NR == FNR { up[$1] = $2; name[$1] = $3; next }
        { job[$2] = $1 }
        END {
            for (p in up) {
                q = p; n = 0
                while (!(q in job) && (q in up) && q + 0 > 1 && n++ < 64) q = up[q]
                if (q in job) print job[q], p, name[p]
            }
        }' "$RUN/.procs" "$RUN/.roots"
    rm -f "$RUN/.procs" "$RUN/.roots"
}

# kill_leftovers FILE: kills what FILE (from job_pids) lists whose job has stopped but
# which still runs with the same name. rinky's own processes are never touched.
kill_leftovers() {
    left=
    while read -r job pid name; do
        status "$job" 2>/dev/null | grep -q "start/" && continue
        read -r now _ 2>/dev/null <"/proc/$pid/comm" || continue
        [ "$now" = "$name" ] || continue
        case $(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null) in *rinky*) continue ;; esac
        log "killing $name ($pid): still running after its job $job stopped"
        kill -TERM "$pid" 2>/dev/null
        left="$left $pid:$name"
    done <"$1"
    [ -n "$left" ] || return 0
    sleep 1
    for e in $left; do
        read -r now _ 2>/dev/null <"/proc/${e%%:*}/comm" && [ "$now" = "${e#*:}" ] && kill -KILL "${e%%:*}" 2>/dev/null
    done
}

# quiet_amazon: stops the running jobs of $QUIET_JOBS, all at once (one after another
# took 8 s on the PW12). Each is listed in $RUN/quieted before its stop, so a launcher
# killed halfway still gets them all started again.
quiet_amazon() {
    [ "${RINKY_QUIET:-1}" = 0 ] && { log "RINKY_QUIET=0: Amazon services left running"; return 0; }
    stops=
    for j in $QUIET_JOBS; do
        status "$j" 2>/dev/null | grep -q "start/" || continue
        echo "$j" >>"$RUN/quieted"
        stop "$j" >/dev/null 2>&1 &
        stops="$stops $!"
    done
    # Only these: a plain `wait` would also wait for the watchdog.
    [ -n "$stops" ] && wait $stops
    [ -s "$RUN/quieted" ] && log "stopped: $(tr '\n' ' ' <"$RUN/quieted")"
}

# wake_amazon: starts the jobs quiet_amazon stopped, the last stopped first.
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

# Undo what rinky changed ($RUN/restore lines "lipc i|s PUBLISHER PROP VALUE" and
# "fb KEY VALUE"). The binary handles both; the shell replays the lipc lines itself if
# the binary cannot run.
replay_journal() {
    J=$RUN/restore
    if [ -f "$J" ]; then
        log "restoring settings: $(tr '\n' ';' <"$J")"
        if ! run_bin "$BIN" device restore >&3 2>&3; then
            log "rinky device restore failed; replaying lipc lines in the shell"
            while read -r kind jt pub prop val; do
                [ "$kind" = lipc ] && lipc-set-prop "-$jt" "$pub" "$prop" "$val" >&3 2>&3
            done <"$J"
        fi
        rm -f "$J"
    else
        # No journal: still put the framebuffer flags back (night flag, fast mode).
        run_bin "$BIN" device restore >&3 2>&3 || true
    fi
}

# One framework start at a time (launcher and watchdog may both try); a lock whose
# owner died is taken over.
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

INK=
REASON=
T0=${RINKY_T0:-$(now_ms)}

# exit_reason STATUS: why rinky ended. The watchdog's note first (it killed it),
# then rinky's own $RUN/exit-reason, else what the status says.
exit_reason() {
    if [ -s "$RUN/watchdog-kill" ]; then
        echo "watchdog: $(head -n 1 "$RUN/watchdog-kill")"
    elif [ -s "$RUN/exit-reason" ]; then
        head -n 1 "$RUN/exit-reason"
    else
        case $1 in
            0) echo "normal (no reason file)" ;;
            132) echo "SIGILL (no reason file)" ;;
            134) echo "abort/panic (no reason file)" ;;
            135) echo "SIGBUS (no reason file)" ;;
            136) echo "SIGFPE (no reason file)" ;;
            137) echo "killed with SIGKILL" ;;
            139) echo "segfault (SIGSEGV)" ;;
            143) echo "SIGTERM (no reason file)" ;;
            *) echo "status $1 (no reason file)" ;;
        esac
    fi
}

cleanup() {
    trap '' INT TERM HUP
    if [ -n "$INK" ] && kill -0 "$INK" 2>/dev/null; then
        log "stopping rinky $INK"
        kill -TERM "$INK" 2>/dev/null
        i=0
        while kill -0 "$INK" 2>/dev/null && [ $i -lt 30 ]; do usleep 100000; i=$((i + 1)); done
        kill -KILL "$INK" 2>/dev/null
    fi
    t=$(now_ms)
    replay_journal
    phase "restore settings" "$t"
    t=$(now_ms)
    case ${STATUS:-} in
        132 | 134 | 135 | 136 | 139) splash "rinky crashed - details in crash log" ;;
        *) splash "Closing rinky..." ;;
    esac
    phase "closing splash" "$t"
    t=$(now_ms)
    start_framework
    phase "start framework" "$t"
    touch "$RUN/done"
    rm -f "$RUN/rinky.pid" "$RUN/launcher.pid" "$RUN/heartbeat"
    # Free the tmpfs copy of the binary: about 20 MB of the 64 MB RAM-backed /var that
    # the framework (and anything else in /tmp) needs once rinky is gone. The next
    # launcher copies it again; one that copied it just before this rm runs from $DIR.
    [ "$BIN" = "$RUN/bin/rinky" ] && rm -f "$RUN/bin/rinky" "$RUN/bin/ld-musl-armhf.so.1"
    # Only our own lock (a lock taken over by a newer launcher stays).
    [ "$(cat "$RUN/lock/pid" 2>/dev/null)" = "$$" ] && rm -rf "$RUN/lock"
    log "exit: reason=${REASON:-launcher stopped before rinky ran} status=${STATUS:-?} total $(($(now_ms) - T0)) ms"
    log "exit status ${STATUS:-?}"
}
trap 'STATUS=signal; REASON="launcher got a signal"; exit 143' INT TERM HUP
trap cleanup EXIT

log "start: $* (binary $BIN)"
RINKY_DIR=$DIR RINKY_DETACHED= setsid sh "$WATCHDOG" </dev/null >/dev/null 2>&1 3>&- &

# Stopping the framework sends SIGTERM to its process group; ignore it meanwhile.
trap '' TERM
t=$(now_ms)
job_pids >"$RUN/jobs-before" 2>/dev/null
log "stop lab126_gui"
(stop lab126_gui) >&3 2>&3
waited=0
while pidof KPPMainApp >/dev/null 2>&1 || pidof pillowd >/dev/null 2>&1; do
    if [ $waited -ge $STOP_DEADLINE ]; then
        log "framework still running after ${STOP_DEADLINE}s; giving up"
        STATUS=framework-stop-timeout
        REASON="framework did not stop in ${STOP_DEADLINE}s"
        exit 1
    fi
    sleep 1
    waited=$((waited + 1))
done
trap 'STATUS=signal; REASON="launcher got a signal"; exit 143' TERM
log "framework stopped after ${waited}s"
phase "stop framework" "$t"
# The framework's blanket module stays loaded with X and draws the Kindle's screensaver
# when powerd times out or the cover closes, over rinky's own (the book cover; the
# power key is fine: powerd ignores it). KOReader unloads it too. wake_amazon loads it.
if lipc-set-prop com.lab126.blanket unload screensaver >&3 2>&3; then
    : >"$RUN/blanket-unloaded"
    log "blanket: screensaver unloaded"
fi
# In the background: the slowest job's stop takes about 8 s, and rinky need not wait.
# wake_amazon waits for it ($RUN/quiet.pid) before it starts anything again.
t=$(now_ms)
(
    quiet_amazon
    kill_leftovers "$RUN/jobs-before"
    rm -f "$RUN/jobs-before"
    phase "quiet Amazon services" "$t"
) </dev/null &
echo $! >"$RUN/quiet.pid"
# The framework may have drawn over the first splash while it stopped.
splash "Starting rinky..."
rm -f "$RUN/exit-reason" "$RUN/watchdog-kill"

# restart_prepare: called when rinky ended and $RUN/restart exists (written by
# `rinky-ctl.sh restart`, which then sent SIGTERM to rinky). Undoes rinky's
# journaled settings (the next rinky journals its own), copies the installed binary
# to $RUN/bin like the first start did, and returns 0 when rinky may run again with
# the framework still stopped. Any failure returns 1: the launcher then ends and the
# normal exit path starts the framework.
restart_prepare() {
    rm -f "$RUN/rinky.pid" "$RUN/heartbeat" "$RUN/exit-reason" "$RUN/watchdog-kill"
    [ -x "$DIR/rinky" ] || { log "restart: $DIR/rinky is missing or not executable"; return 1; }
    if [ "$BIN" = "$RUN/bin/rinky" ]; then
        # Next to the old binary first; on a small tmpfs (32 MB /var on the PW3, PW4 and
        # Oasis, rinky about 24 MB) only after removing it (rinky has exited); else run
        # the installed binary from $DIR, as the first start does when it cannot copy.
        rt=.new.$$
        if copy_pair "$rt"; then
            :
        else
            rm -f "$RUN/bin/.rinky$rt" "$RUN/bin/.ld$rt" "$RUN/bin/rinky" "$RUN/bin/ld-musl-armhf.so.1"
            if copy_pair "$rt"; then
                log "restart: copied the new binary to $RUN/bin after removing the old one"
            else
                rm -f "$RUN/bin/.rinky$rt" "$RUN/bin/.ld$rt" "$RUN/bin/rinky" "$RUN/bin/ld-musl-armhf.so.1"
                BIN=$DIR/rinky
                log "restart: cannot copy the new binary to $RUN/bin; running it from $DIR"
            fi
        fi
    fi
    t=$(now_ms)
    replay_journal
    phase "restore settings (restart)" "$t"
    return 0
}

# crash_restart: called when rinky ended and $RUN/plugin-crashed exists (a fatal signal
# inside a native plugin). Returns 0 when rinky may run again at once, with the
# framework still stopped: the next rinky turns that plugin off. At most
# CRASH_RESTARTS (2) such restarts within 60 s, and none when the crash came while
# rinky was exiting (the marker's second line is `exiting`: the user asked to leave);
# then the marker stays (the next start still turns the plugin off) and the launcher
# takes the exit path.
CRASH_RESTARTS=${RINKY_CRASH_RESTARTS:-2}
CRASH_TIMES=
crash_restart() {
    if grep -qx exiting "$RUN/plugin-crashed" 2>/dev/null; then
        log "crash restart: the plugin crashed while rinky was exiting; not running it again"
        return 1
    fi
    now=$(now_ms)
    recent=
    for c in $CRASH_TIMES; do
        [ $((now - c)) -lt 60000 ] && recent="$recent $c"
    done
    set -- $recent
    if [ $# -ge "$CRASH_RESTARTS" ]; then
        log "crash restart: already $# in the last 60 s; ending"
        return 1
    fi
    CRASH_TIMES="$recent $now"
    log "crash restart: native plugin '$(head -n 1 "$RUN/plugin-crashed" 2>/dev/null)' crashed rinky; running it again without that plugin ($(($# + 1)) of $CRASH_RESTARTS within 60 s)"
    rm -f "$RUN/rinky.pid" "$RUN/heartbeat" "$RUN/exit-reason" "$RUN/watchdog-kill"
    t=$(now_ms)
    replay_journal
    phase "restore settings (crash restart)" "$t"
    splash "Starting rinky..."
    return 0
}

while :; do
    t=$(now_ms)
    ld=$(loader_for "$BIN")
    if [ -n "$ld" ]; then
        RINKY_LOG_FILE=$LOGFILE RINKY_RUN=$RUN "$ld" "$BIN" "$@" >&3 2>&3 3>&- &
    else
        RINKY_LOG_FILE=$LOGFILE RINKY_RUN=$RUN "$BIN" "$@" >&3 2>&3 3>&- &
    fi
    INK=$!
    echo "$INK" >"$RUN/rinky.pid"
    log "rinky pid $INK"
    wait "$INK"
    STATUS=$?
    INK=
    # rinky may have rotated the log while it ran: write to the current file again.
    exec 3>&-
    open_log
    phase "rinky run" "$t"
    REASON=$(exit_reason "$STATUS")
    case $STATUS in
        134) log "rinky exited with $STATUS (abort/panic)" ;;
        137) log "rinky exited with $STATUS (killed)" ;;
        139) log "rinky exited with $STATUS (segfault)" ;;
        *) log "rinky exited with $STATUS" ;;
    esac
    # Restart: swap the process, keep the framework stopped (no start/stop of lab126_gui).
    if [ -f "$RUN/restart" ]; then
        if restart_prepare; then
            rm -f "$RUN/restart"
            log "restart: running the installed rinky again, framework stays stopped"
            continue
        fi
        rm -f "$RUN/restart"
        log "restart failed; ending (the framework starts again)"
    fi
    if [ -f "$RUN/plugin-crashed" ] && crash_restart; then
        continue
    fi
    exit "$STATUS"
done
