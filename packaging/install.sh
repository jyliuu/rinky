#!/bin/sh
# rinky installer. Shipped in the install package as rinky/install.sh.
#
#   sh <unpacked package>/rinky/install.sh [--upgrade]
#
# Copies the package's rinky/ folder to /mnt/us/rinky and its
# extensions/rinky/ folder (the KUAL menu) to /mnt/us/extensions/rinky.
# A Kobo package (it has nm/rinky, the NickelMenu entry) goes to
# /mnt/onboard/.adds/rinky and /mnt/onboard/.adds/nm/rinky instead.
# Only the files listed in rinky/FILES are written. Settings, the library index,
# logs, plugins and caches already in /mnt/us/rinky are kept.
#
#   --upgrade   allowed while rinky runs (the in-app update uses it). The running
#               binary and launcher keep their old copies until they exit.
#
# Without --upgrade the installer refuses to run while rinky runs.
#
# All or nothing: every file is first copied to <name>.new<pid> next to its target.
# Only when every copy succeeded are they renamed over the old files, with VERSION,
# FILES, SHA256SUMS and .install-state last. A failed copy (for example no space left)
# deletes the copies made so far and leaves the installed version as it was.
# One install at a time: /mnt/us/rinky/.install.lock (a folder with the owner's pid).
#
# Creates the library entry /mnt/us/documents/rinky.sh (a scriptlet that starts
# rinky from the Kindle library) unless a different file already has that name;
# an identical one is rewritten. It is listed in .install-state as
# scriptlet=documents/rinky.sh, so uninstall.sh removes it.
#
# Records what it created in /mnt/us/rinky/.install-state, so uninstall.sh can put
# the Kindle back exactly as it was (for example /mnt/us/extensions, when KUAL is not
# installed and that folder did not exist before). The first install also lists the
# book .sdr folders that already exist in /mnt/us/rinky/.install-sdr; uninstall.sh
# never removes those.
#
# Environment (tests): RINKY_PREFIX=DIR puts /mnt/us under DIR;
#                      RINKY_RUN=DIR is the runtime directory (default /tmp/rinky).
set -u

PKG=$(cd "$(dirname "$0")/.." && pwd)
PREFIX=${RINKY_PREFIX:-}
KOBO=0
[ -f "$PKG/nm/rinky" ] && KOBO=1
if [ "$KOBO" = 1 ]; then
    US=$PREFIX/mnt/onboard
    DEST=$US/.adds/rinky
    EXTROOT=$US/.adds/nm
    EXT=$EXTROOT
    EXTNAME=.adds/nm
else
    US=$PREFIX/mnt/us
    DEST=$US/rinky
    EXTROOT=$US/extensions
    EXT=$EXTROOT/rinky
    EXTNAME=extensions
fi
RUN=${RINKY_RUN:-/tmp/rinky}
STATE=$DEST/.install-state
SDRLIST=$DEST/.install-sdr
LOCK=$DEST/.install.lock
SCRIPTLET=$US/documents/rinky.sh
UPGRADE=0
TMP=new$$

say() { echo "install: $*"; }
die() {
    echo "install: error: $*" >&2
    exit 1
}

for arg in "$@"; do
    case "$arg" in
        --upgrade) UPGRADE=1 ;;
        -h | --help)
            sed -n '2,34p' "$0"
            exit 0
            ;;
        *) die "unknown option $arg" ;;
    esac
done

[ -d "$US" ] || die "$US does not exist (is this a $([ "$KOBO" = 1 ] && echo Kobo || echo Kindle)?)"
[ -f "$PKG/rinky/FILES" ] || die "not an rinky package: $PKG/rinky/FILES is missing"
[ -f "$PKG/rinky/rinky" ] || die "not an rinky package: $PKG/rinky/rinky is missing"
[ -f "$PKG/rinky/SHA256SUMS" ] || die "not an rinky package: $PKG/rinky/SHA256SUMS is missing"
VERSION=$(sed -n 's/^version=//p' "$PKG/rinky/VERSION" 2>/dev/null | head -n 1)
[ -n "$VERSION" ] || die "$PKG/rinky/VERSION has no version= line"

# Checksums of the package files (rinky/SHA256SUMS). Catches a truncated copy
# before anything is written.
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$PKG" && sha256sum -c rinky/SHA256SUMS >/dev/null 2>&1) ||
        die "package files do not match rinky/SHA256SUMS (damaged copy?)"
    say "package checksums ok"
elif command -v shasum >/dev/null 2>&1; then
    (cd "$PKG" && shasum -a 256 -c rinky/SHA256SUMS >/dev/null 2>&1) ||
        die "package files do not match rinky/SHA256SUMS (damaged copy?)"
    say "package checksums ok"
else
    say "warning: no sha256sum on this system; package checksums not verified"
fi

# The new file list, read once: nothing below reads the package's FILES again.
NEW_FILES=$(cat "$PKG/rinky/FILES") || die "cannot read $PKG/rinky/FILES"
[ -n "$NEW_FILES" ] || die "$PKG/rinky/FILES is empty"

target_of() {
    case "$1" in
        /* | *..*) return 1 ;;
        rinky/*) echo "$DEST/${1#rinky/}" ;;
        extensions/rinky/*) [ "$KOBO" = 0 ] && echo "$EXT/${1#extensions/rinky/}" ;;
        nm/rinky) [ "$KOBO" = 1 ] && echo "$EXT/rinky" ;;
        *) return 1 ;;
    esac
}

# listed REL: REL is a line of the new FILES (exact match, no grep, no file read).
listed() {
    case "
$NEW_FILES
" in
        *"
$1
"*) return 0 ;;
    esac
    return 1
}

# Every line is checked before anything is written.
while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    target_of "$rel" >/dev/null || die "unsafe or unexpected path in FILES: $rel"
    [ -f "$PKG/$rel" ] || die "FILES lists $rel but the package does not contain it"
done <<EOF
$NEW_FILES
EOF
for rel in rinky/VERSION rinky/FILES rinky/SHA256SUMS; do
    listed "$rel" || die "FILES does not list $rel"
done

alive() { [ -n "$1" ] && kill -0 "$1" 2>/dev/null; }
if [ -d "$RUN/lock" ] && alive "$(cat "$RUN/launcher.pid" 2>/dev/null)"; then
    [ "$UPGRADE" = 1 ] || die "rinky is running; exit it first (or use --upgrade)"
    say "rinky is running; it uses the new version after the next start"
fi

# Folders this run creates; removed again if the install fails. DEST is created before
# the lock (the lock lives in it); the other two only by the run that holds the lock.
NEW_DEST=0 NEW_EXTROOT=0 NEW_EXT=0
[ -d "$DEST" ] || NEW_DEST=1
LOCKED=0
DONE=0

# On any exit before the end: delete this run's .new copies, the lock, and the folders
# this run created (only when empty). A run that never got the lock removes none of
# them: they may belong to the run that holds it.
# shellcheck disable=SC2329 # called by the EXIT trap
cleanup() {
    if [ "$DONE" != 1 ]; then
        while IFS= read -r rel; do
            dst=$(target_of "$rel") && rm -f "$dst.$TMP"
        done <<EOF
$NEW_FILES
EOF
        rm -f "$STATE.$TMP" "$SDRLIST.$TMP" "$SCRIPTLET.$TMP"
    fi
    if [ "$LOCKED" = 1 ] && [ "$DONE" != 1 ]; then
        [ "$NEW_EXT" = 1 ] && [ "$KOBO" = 0 ] && rmdir "$EXT/bin" "$EXT" 2>/dev/null
        [ "$NEW_EXTROOT" = 1 ] && rmdir "$EXTROOT" 2>/dev/null
    fi
    [ "$LOCKED" = 1 ] && rm -rf "$LOCK"
    # Empty only when no other run holds a lock in it.
    [ "$DONE" != 1 ] && [ "$NEW_DEST" = 1 ] && rmdir "$DEST" 2>/dev/null
    return 0
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

mkdir -p "$DEST" || die "cannot create $DEST"

# The lock: a folder with "pid boot-id" of its owner. An owner that is gone (or from an
# earlier boot) leaves a stale lock, which is taken over: renamed aside, and given back
# if the folder renamed is no longer the stale one (another run took it over first).
boot_id() { cat /proc/sys/kernel/random/boot_id 2>/dev/null; }
lock_owner_alive() {
    owner=$(cat "$LOCK/owner" 2>/dev/null)
    [ -n "$owner" ] || return 1
    alive "${owner%% *}" && [ "${owner#* }" = "$(boot_id)" ]
}
busy() { die "another install or update is running${1:+ (pid $1)}; try again when it has finished"; }
if ! mkdir "$LOCK" 2>/dev/null; then
    # A lock without an owner line may be one that is being taken right now.
    [ -s "$LOCK/owner" ] || sleep 1
    lock_owner_alive && busy "${owner%% *}"
    stale=$owner
    mv "$LOCK" "$LOCK.stale$$" 2>/dev/null || busy
    if [ "$(cat "$LOCK.stale$$/owner" 2>/dev/null)" != "$stale" ]; then
        [ -e "$LOCK" ] || mv "$LOCK.stale$$" "$LOCK" 2>/dev/null
        rm -rf "$LOCK.stale$$"
        busy
    fi
    say "removed a stale install lock (${stale:-no owner})"
    rm -rf "$LOCK.stale$$"
    mkdir "$LOCK" 2>/dev/null || busy
fi
LOCKED=1
echo "$$ $(boot_id)" >"$LOCK/owner" || die "cannot write $LOCK/owner"
[ -d "$EXTROOT" ] || NEW_EXTROOT=1
[ -d "$EXT" ] || NEW_EXT=1

# Copies left by an earlier install that was killed (only one install runs at a time).
while IFS= read -r rel; do
    dst=$(target_of "$rel") && rm -f "$dst".new[0-9]*
done <<EOF
$NEW_FILES
EOF
rm -f "$STATE".new[0-9]* "$SDRLIST".new[0-9]*

# What earlier installs created, plus extensions/ when this run creates it.
CREATED=
[ -f "$STATE" ] && CREATED=$(grep '^created=' "$STATE")
if [ ! -d "$EXTROOT" ]; then
    mkdir -p "$EXTROOT" || die "cannot create $EXTROOT"
    CREATED="${CREATED:+$CREATED
}created=$EXTNAME"
fi

# Old file list, for removing files that the new version no longer ships.
OLD_FILES=
if [ -f "$DEST/FILES" ]; then
    OLD_FILES=$(cat "$DEST/FILES") || die "cannot read $DEST/FILES"
fi

# 1. Copy every file to <target>.new<pid>. Nothing installed changes in this step.
count=0
while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    dst=$(target_of "$rel") || die "unexpected path in FILES: $rel"
    mkdir -p "$(dirname "$dst")" || die "cannot create $(dirname "$dst")"
    cp "$PKG/$rel" "$dst.$TMP" || die "cannot write $dst.$TMP (no space left?); nothing was changed"
    if [ -x "$PKG/$rel" ]; then chmod 755 "$dst.$TMP" 2>/dev/null; fi
    count=$((count + 1))
done <<EOF
$NEW_FILES
EOF

# The library entry (Kindle scriptlet). The text must stay byte for byte the same
# between versions: an identical file counts as ours.
scriptlet_text() {
    printf '%s\n' '#!/bin/sh' '# Name: rinky' '# Author: rinky' '# DontUseFBInk' "trap ':' TERM" \
        'sh /mnt/us/rinky/rinky-ctl.sh start --library >>/mnt/us/rinky/scriptlet.log 2>&1'
}
SCRIPTLET_STATE=
rm -f "$SCRIPTLET".new[0-9]*
if [ "$KOBO" = 1 ]; then
    : # Kobo: NickelMenu starts rinky.
elif [ ! -d "$US/documents" ]; then
    say "no $US/documents folder; library entry not created"
elif [ ! -e "$SCRIPTLET" ] || [ "$(cat "$SCRIPTLET" 2>/dev/null)" = "$(scriptlet_text)" ]; then
    scriptlet_text >"$SCRIPTLET.$TMP" || die "cannot write $SCRIPTLET.$TMP (no space left?); nothing was changed"
    SCRIPTLET_STATE=scriptlet=documents/rinky.sh
else
    say "warning: $SCRIPTLET exists and is not rinky's library entry; left as it is"
fi

{
    echo "version=$VERSION"
    echo "installed=$(date '+%Y-%m-%d %H:%M:%S')"
    if [ -n "$CREATED" ]; then echo "$CREATED"; fi
    if [ -n "$SCRIPTLET_STATE" ]; then echo "$SCRIPTLET_STATE"; fi
} >"$STATE.$TMP" || die "cannot write $STATE.$TMP (no space left?); nothing was changed"

# The .sdr folders that exist before rinky first runs (paths relative to /mnt/us).
# Written once; later installs keep the list of the first one. A folder that already
# holds an rinky file is left out (an upgrade from a version without this list:
# rinky may have created that folder).
if [ ! -f "$SDRLIST" ] && [ "$KOBO" = 0 ]; then
    (cd "$US" && find . \( -path ./koreader -o -path ./system -o -path ./rinky -o -path ./extensions \) -prune -o \
        -type d -name '*.sdr' -print -prune 2>/dev/null) | sed 's|^\./||' | while IFS= read -r d; do
        ls -d "$US/$d"/rinky* >/dev/null 2>&1 || echo "$d"
    done >"$SDRLIST.$TMP" || die "cannot write $SDRLIST.$TMP (no space left?); nothing was changed"
fi

# 2. Rename the copies over the old files: program and scripts first, then the KUAL
#    menu, then the files that name the version, then the install state.
commit() {
    mv -f "$1.$TMP" "$1" || die "cannot rename $1.$TMP to $1; run the installer again"
}
while IFS= read -r rel; do
    case "$rel" in "" | rinky/VERSION | rinky/FILES | rinky/SHA256SUMS) continue ;; esac
    commit "$(target_of "$rel")"
done <<EOF
$NEW_FILES
EOF
commit "$DEST/SHA256SUMS"
commit "$DEST/FILES"
commit "$DEST/VERSION"
[ -f "$SDRLIST.$TMP" ] && commit "$SDRLIST"
[ -f "$SCRIPTLET.$TMP" ] && commit "$SCRIPTLET"
commit "$STATE"
DONE=1

# Files of the old version that the new one does not ship.
while IFS= read -r rel; do
    [ -n "$rel" ] || continue
    listed "$rel" && continue
    dst=$(target_of "$rel") || {
        say "warning: skipped unsafe line in the old FILES: $rel"
        continue
    }
    rm -f "$dst" && say "removed old file $dst"
done <<EOF
$OLD_FILES
EOF

chmod 755 "$DEST/rinky" "$DEST/rinky.sh" "$DEST/rinky-watchdog.sh" "$DEST/rinky-ctl.sh" \
    "$EXT/bin/kual.sh" 2>/dev/null
sync

say "rinky $VERSION installed in $DEST ($count files)"
if [ "$KOBO" = 1 ]; then
    say "NickelMenu entry: $EXT/rinky"
    say "start: NickelMenu > rinky, or: sh $DEST/rinky.sh"
    exit 0
fi
say "KUAL menu: $EXT"
[ -n "$SCRIPTLET_STATE" ] && say "library entry: $SCRIPTLET"
say "start: KUAL > rinky > Start rinky, or: sh $DEST/rinky-ctl.sh start"
exit 0
