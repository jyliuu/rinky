#!/bin/sh
# rinky uninstaller. Installed as /mnt/us/rinky/uninstall.sh.
#
#   sh /mnt/us/rinky/uninstall.sh [--keep-data]
#
# Puts the Kindle back the way it was before rinky was installed:
#   1. stops a running rinky (its launcher starts the Kindle framework again) and
#      makes sure the framework runs;
#   2. removes rinky's per-book files: <book>.sdr/rinky* next to the books
#      (KOReader's files in the same .sdr folders are left alone), the .sdr folder
#      itself only when it is empty afterwards and is not listed in
#      /mnt/us/rinky/.install-sdr (the .sdr folders that existed before rinky
#      was installed), and old <book>.rinky files;
#   3. removes /mnt/us/extensions/rinky (the KUAL menu), and /mnt/us/extensions too
#      when the installer created it and it is empty; and documents/rinky.sh;
#   4. removes /mnt/us/rinky (program, settings, library index, logs, plugins,
#      caches) and the runtime directory /tmp/rinky.
#
#   --keep-data   remove only the program files (the ones listed in FILES), the KUAL
#                 menu and the runtime directory; keep settings, notes, positions,
#                 plugin data and .install-sdr, so a later install continues where you
#                 left off.
#
# Never reads or writes /mnt/us/koreader.
# Environment (tests): RINKY_PREFIX=DIR puts /mnt/us under DIR;
#                      RINKY_RUN=DIR is the runtime directory (default /tmp/rinky).
set -u

say() { echo "uninstall: $*"; }
die() {
    echo "uninstall: error: $*" >&2
    exit 1
}

KEEP=0
for arg in "$@"; do
    case "$arg" in
        --keep-data) KEEP=1 ;;
        -h | --help)
            sed -n '2,26p' "$0"
            exit 0
            ;;
        *) die "unknown option $arg" ;;
    esac
done

# This script lives in the folder it removes: run from a copy in /tmp.
if [ -z "${RINKY_UNINSTALL_COPY:-}" ]; then
    copy=${TMPDIR:-/tmp}/rinky-uninstall.$$.sh
    cp "$0" "$copy" || die "cannot copy $0 to $copy"
    RINKY_UNINSTALL_COPY=$copy exec sh "$copy" "$@"
fi
trap 'rm -f "$RINKY_UNINSTALL_COPY"' EXIT

PREFIX=${RINKY_PREFIX:-}
US=$PREFIX/mnt/us
DEST=$US/rinky
EXTROOT=$US/extensions
EXT=$EXTROOT/rinky
RUN=${RINKY_RUN:-/tmp/rinky}
STATE=$DEST/.install-state
SDRLIST=$DEST/.install-sdr

[ -d "$US" ] || die "$US does not exist (is this a Kindle?)"

alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }

# Exit to Kindle first: rinky-ctl.sh stops rinky and its watchdog and makes
# sure the framework runs. Without it (a broken install) at least stop the launcher.
stop_rinky() {
    if [ -f "$DEST/rinky-ctl.sh" ]; then
        RINKY_PREFIX=$PREFIX RINKY_RUN=$RUN sh "$DEST/rinky-ctl.sh" stop
    else
        pid=$(cat "$RUN/launcher.pid" 2>/dev/null)
        alive "$pid" && kill -TERM "$pid" 2>/dev/null && sleep 5
        pid=$(cat "$RUN/rinky.pid" 2>/dev/null)
        alive "$pid" && kill -KILL "$pid" 2>/dev/null
    fi
}

# Per-book files: <book>.sdr/rinky*. Folders are searched on the whole userstore
# except /mnt/us/koreader, /mnt/us/system and our own folder (removed whole below).
# kept_sdr DIR: DIR (absolute) existed before rinky was installed. Without a list
# (an install copied over USB that never started) no folder counts as kept.
kept_sdr() {
    [ -f "$SDRLIST" ] || return 1
    grep -Fxq -e "${1#"$US"/}" "$SDRLIST" 2>/dev/null
    [ $? != 1 ] # found, or the list cannot be read: keep the folder
}

remove_book_files() {
    list=${TMPDIR:-/tmp}/rinky-uninstall.$$.list
    find "$US" \( -path "$US/koreader" -o -path "$US/system" -o -path "$DEST" \) -prune -o \
        -type f \( -path "*.sdr/rinky*" -o -name "*.rinky" \) -print >"$list" 2>/dev/null
    removed=0
    while IFS= read -r f; do
        [ -n "$f" ] || continue
        rm -f "$f" && removed=$((removed + 1))
        case "$f" in
            *.sdr/*)
                d=$(dirname "$f")
                kept_sdr "$d" || rmdir "$d" 2>/dev/null
                ;;
        esac
    done <"$list"
    rm -f "$list"
    say "removed $removed per-book file(s)"
}

# An install or update in progress holds /mnt/us/rinky/.install.lock (see install.sh).
install_running() {
    owner=$(cat "$DEST/.install.lock/owner" 2>/dev/null)
    [ -n "$owner" ] && alive "${owner%% *}" &&
        [ "${owner#* }" = "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)" ]
}
install_running && die "an install or update is running (pid ${owner%% *}); try again when it has finished"

# An in-app update holds /mnt/us/rinky/.update.lock (its pid inside) while it
# downloads, checks and installs. A file left by a killed update names a dead pid (or,
# after a restart, a process that is not rinky).
update_running() {
    upid=$(cat "$DEST/.update.lock" 2>/dev/null)
    alive "$upid" || return 1
    [ -r "/proc/$upid/cmdline" ] || return 0
    tr '\0' ' ' <"/proc/$upid/cmdline" | grep -q rinky
}
update_running && die "an update is running (pid $upid); try again when it has finished"

stop_rinky
if [ "$KEEP" = 0 ]; then
    remove_book_files
fi

if [ -d "$EXT" ]; then
    rm -rf "$EXT" && say "removed $EXT"
fi
# The library entry install.sh created (only when it is still rinky's).
SCRIPTLET=$US/documents/rinky.sh
if grep -qx 'scriptlet=documents/rinky.sh' "$STATE" 2>/dev/null && grep -qx '# Author: rinky' "$SCRIPTLET" 2>/dev/null; then
    rm -f "$SCRIPTLET" && say "removed $SCRIPTLET"
fi
if grep -qx 'created=extensions' "$STATE" 2>/dev/null; then
    rmdir "$EXTROOT" 2>/dev/null && say "removed $EXTROOT (created by the installer)"
fi

if [ "$KEEP" = 1 ]; then
    if [ -f "$DEST/FILES" ]; then
        while IFS= read -r rel; do
            case "$rel" in
                /* | *..*) say "warning: skipped unsafe line in FILES: $rel" ;;
                rinky/*) rm -f "$DEST/${rel#rinky/}" ;;
            esac
        done <"$DEST/FILES"
    fi
    rm -rf "$DEST/.update" "$DEST/.install.lock"
    rm -f "$STATE" "$DEST/.update.lock"
    rmdir "$DEST/bin" 2>/dev/null
    rmdir "$DEST/fonts" 2>/dev/null
    say "removed the program files; kept your data in $DEST"
else
    rm -rf "$DEST" && say "removed $DEST"
fi

rm -rf "$RUN"
sync
say "done; the Kindle is as it was before rinky was installed"
exit 0
