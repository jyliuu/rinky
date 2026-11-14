#!/bin/sh
# KUAL actions for rinky (see ../menu.json). KUAL runs this with the extension
# folder (/mnt/us/extensions/rinky) as the working directory.
#
#   kual.sh start                 start rinky (library); the Kindle UI stops meanwhile
#   kual.sh exit                  Exit to Kindle: stop rinky, make sure the Kindle UI runs
#   kual.sh update                download and install the latest release (needs Wi-Fi)
#   kual.sh uninstall             remove rinky and all its data
#   kual.sh uninstall-keep-data   remove the program, keep settings, notes and positions
#
# Messages appear at the bottom of the screen (FBInk from the jailbreak, else eips) and
# in /mnt/us/rinky/kual.log. The update's full output goes to update.log. Each log
# is cut to its last 64 KiB when it grows past 256 KiB.
set -u

PREFIX=${RINKY_PREFIX:-}
DEST=$PREFIX/mnt/us/rinky
LOG=$DEST/kual.log
UPDATE_LOG=$DEST/update.log

# trim_log FILE: keep the last 64 KiB once FILE is larger than 256 KiB.
trim_log() {
    [ -f "$1" ] || return 0
    size=$(wc -c <"$1" 2>/dev/null) || return 0
    [ "${size:-0}" -gt 262144 ] || return 0
    tail -c 65536 "$1" >"$1.cut$$" 2>/dev/null && mv -f "$1.cut$$" "$1"
    rm -f "$1.cut$$"
}
trim_log "$LOG"
trim_log "$UPDATE_LOG"

FBINK=
for f in /var/local/kmc/bin/fbink /mnt/us/libkh/bin/fbink /usr/bin/fbink; do
    [ -x "$f" ] && FBINK=$f && break
done

# msg TEXT [ROW_FROM_BOTTOM]: on screen at most 36 characters, so a message stays on
# one line above the bottom of the screen.
msg() {
    echo "$1"
    [ -d "$DEST" ] && echo "[kual $(date '+%H:%M:%S')] $1" >>"$LOG" 2>/dev/null
    row=${2:-1}
    short=$(echo "$1" | cut -c1-36)
    if [ -n "$FBINK" ]; then
        "$FBINK" -qpm -y $((-4 - row)) "$short" >/dev/null 2>&1
    elif command -v eips >/dev/null 2>&1; then
        eips 0 $((40 - row)) "$short" >/dev/null 2>&1
    fi
}

need_install() {
    [ -f "$DEST/rinky" ] && return 0
    msg "rinky is not installed"
    exit 1
}

cmd=${1:-}
case "$cmd" in
    start)
        need_install
        sh "$DEST/rinky-ctl.sh" start --library >>"$LOG" 2>&1
        ;;
    exit)
        need_install
        msg "Stopping rinky..."
        sh "$DEST/rinky-ctl.sh" stop >>"$LOG" 2>&1
        msg "rinky stopped; Kindle UI is on"
        ;;
    update)
        need_install
        msg "Checking for an rinky update..." 2
        # Run the updater from a copy on tmpfs. install.sh renames the new binary over
        # $DEST/rinky; on the Kindle's userstore (a FUSE file system) a process running
        # from the replaced file loses the pages it has not read yet and dies with
        # SIGBUS ("Bus error"; the in-app Update page runs from the launcher's copy in
        # /tmp/rinky/bin and is not affected). The musl loader, when the install has
        # one, is copied too. A copy that fails (full tmpfs) runs from $DEST as before.
        upd=${TMPDIR:-/tmp}/rinky-update.$$
        bin=$DEST/rinky
        ld=
        if mkdir -p "$upd" 2>/dev/null && cp "$DEST/rinky" "$upd/rinky" 2>/dev/null && chmod 755 "$upd/rinky"; then
            bin=$upd/rinky
            if [ -f "$DEST/lib/ld-musl-armhf.so.1" ]; then
                if cp "$DEST/lib/ld-musl-armhf.so.1" "$upd/ld-musl-armhf.so.1" 2>/dev/null && chmod 755 "$upd/ld-musl-armhf.so.1"; then
                    ld=$upd/ld-musl-armhf.so.1
                else
                    bin=$DEST/rinky
                fi
            fi
        fi
        [ "$bin" = "$DEST/rinky" ] && echo "[kual $(date '+%H:%M:%S')] update: cannot copy the binary to $upd; running it from $DEST" >>"$LOG" 2>/dev/null
        if [ -n "$ld" ]; then
            out=$("$ld" "$bin" update install 2>&1)
        else
            out=$("$bin" update install 2>&1)
        fi
        status=$?
        rm -rf "$upd"
        echo "$out" >>"$UPDATE_LOG"
        last=$(echo "$out" | tail -n 1)
        case "$last" in
            "installed rinky "*) msg "Installed $(echo "$last" | sed 's/^installed rinky \([^;]*\).*/\1/'); restart rinky" 1 ;;
            "up to date"*) msg "rinky is up to date" 1 ;;
            *"checksum mismatch"*) msg "Update refused: bad checksum" 1 ;;
            *"another update is running"* | *"another install or update is running"*) msg "An update is already running" 1 ;;
            *"No space left"* | *"no space left"*) msg "Update failed: Kindle is full" 1 ;;
            *) msg "Update failed; see update.log" 1 ;;
        esac
        exit "$status"
        ;;
    uninstall | uninstall-keep-data)
        need_install
        msg "Removing rinky..." 2
        if [ "$cmd" = uninstall ]; then
            out=$(sh "$DEST/uninstall.sh" 2>&1)
        else
            out=$(sh "$DEST/uninstall.sh" --keep-data 2>&1)
        fi
        status=$?
        echo "$out"
        if [ "$status" = 0 ]; then
            msg "rinky removed; reopen KUAL" 1
        else
            # Only a failed uninstall leaves a log behind (in RAM, gone after a restart).
            echo "$out" >"${TMPDIR:-/tmp}/rinky-uninstall.log"
            msg "Uninstall failed; log in /tmp" 1
        fi
        exit "$status"
        ;;
    *)
        sed -n '2,13p' "$0"
        exit 2
        ;;
esac
