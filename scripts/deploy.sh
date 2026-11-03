#!/bin/bash
# Quick install of the current tree on your own Kindle: build, package, copy only what
# changed, restart rinky, check the running commit. Only the user asks for this;
# agents never contact a real device on their own.
#
#   scripts/deploy.sh HOST [PORT]            build with the `deploy` profile (no LTO,
#                                            incremental: a change in one crate rebuilds
#                                            in seconds), install, restart
#   scripts/deploy.sh HOST [PORT] --release  fat-LTO release build (about 1.5 min per
#                                            change; what packages for others use)
#   scripts/deploy.sh HOST [PORT] --no-restart  install only; the running rinky
#                                            uses the new version after its next start
#   scripts/deploy.sh HOST [PORT] --full     copy the whole package (about 20 MB) and run
#                                            install.sh --upgrade, as before
#
# Default (fast path): after the build the script packages the tree, unpacks the package
# on the Mac and compares the sha256 of every packaged file with the device's installed
# /mnt/us/rinky/SHA256SUMS (read over ssh). Only the files that differ (usually the
# binary, plus VERSION and SHA256SUMS) go over the link, in one gzip-compressed tar.
# On the device, each file is moved to <target>.fast<pid> next to its installed path, its
# sha256 is checked there, and only when every file checks out are they renamed over the
# old files (the binary and scripts first, VERSION, then SHA256SUMS last). A running
# rinky keeps its old binary: nothing is written in place. Then the installed files
# are checked against the new SHA256SUMS and rinky restarts.
# The script falls back to the full path (and says why) when the device has no
# SHA256SUMS, FILES or .install-state, when the package file list differs from the
# installed one (files added or removed: install.sh creates folders and removes old
# files), or when install.sh itself changed (it writes the library entry and the
# install state). Launcher scripts and the KUAL menu are plain files; they go the fast
# way. The full path unpacks under /mnt/us (the Kindle's /tmp is a small RAM disk) and
# runs install.sh --upgrade, which works while rinky runs.
#
# Each step prints its time. Environment: CARGO_TARGET_DIR (default target/),
# CARGO_BUILD_JOBS (default: all cores), SSH_USER (default root), DEPLOY_SSH_OPTS (extra ssh
# arguments, word-split, e.g. "-i KEY -o StrictHostKeyChecking=no" for the emulator),
# DEPLOY_SSH_MUX=0 (one ssh connection per step instead of one shared connection).
set -euo pipefail

HOST=${1:?usage: scripts/deploy.sh HOST [PORT] [--release] [--no-restart] [--full]}
shift
PORT=2222
if [ $# -gt 0 ] && [[ $1 != --* ]]; then
    PORT=$1
    shift
fi
PROFILE=deploy
RESTART=1
FULL=0
for arg in "$@"; do
    case "$arg" in
        --release) PROFILE=release ;;
        --no-restart) RESTART=0 ;;
        --full) FULL=1 ;;
        *) echo "deploy: unknown option $arg" >&2; exit 2 ;;
    esac
done

cd "$(dirname "$0")/.."
OUT=out/package-device
W=$(mktemp -d /tmp/ikd.XXXXXX)
SSH=(ssh -p "$PORT" -o BatchMode=yes -o ConnectTimeout=10)
if [ -n "${DEPLOY_SSH_OPTS:-}" ]; then
    # shellcheck disable=SC2206
    SSH+=($DEPLOY_SSH_OPTS)
fi
if [ "${DEPLOY_SSH_MUX:-1}" = 1 ]; then
    SSH+=(-o ControlMaster=auto -o ControlPath="$W/m" -o ControlPersist=120)
fi
SSH+=("${SSH_USER:-root}@$HOST")
cleanup() {
    ssh -O exit -o ControlPath="$W/m" "${SSH_USER:-root}@$HOST" >/dev/null 2>&1 || true
    rm -rf "$W"
}
trap cleanup EXIT

now() { perl -MTime::HiRes=time -e 'printf "%.2f\n", time'; }
T0=$(now)
T_LAST=$T0
lap() {
    local t
    t=$(now)
    awk -v a="$T_LAST" -v b="$t" -v n="$1" 'BEGIN { printf "deploy: %-24s %6.1f s\n", n, b - a }'
    T_LAST=$t
}
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# package.sh builds: its flags remap the build machine's paths, which its check of the
# binary requires (a plain cargo build fails that check), and it runs build-dropbear.sh
# (a no-op when target/dropbear/dropbearmulti is up to date).
scripts/package.sh --allow-dirty --profile "$PROFILE" --out "$OUT" >/dev/null
PKG=$(ls "$OUT"/rinky-*-kindlehf.tar.gz)
SUM=$(sha256 "$PKG" | cut -d' ' -f1)
echo "deploy: built $(basename "$PKG") ($PROFILE), sha256 $SUM"
lap "build and package"

# --- fast path --------------------------------------------------------------------
# Returns 0 when the changed files are installed; 1 (reason printed) when the full path
# must run instead. Nothing on the device is changed before the checks pass.
fast_path() {
    local S=$W/pkg rc=0 changed rel hash mode ssum vsum
    mkdir -p "$S"
    tar -xzf "$PKG" -C "$S"
    lap "unpack package"

    "${SSH[@]}" 'cd /mnt/us/rinky 2>/dev/null && [ -f SHA256SUMS ] && [ -f FILES ] && [ -f .install-state ] && { cat SHA256SUMS; echo "@@FILES"; cat FILES; }' >"$W/dev.txt" || rc=$?
    if [ "$rc" = 255 ]; then
        echo "deploy: ssh to $HOST port $PORT failed" >&2
        exit 1
    fi
    if [ "$rc" != 0 ]; then
        echo "deploy: no complete install on the device (SHA256SUMS, FILES or .install-state missing)"
        return 1
    fi
    awk '/^@@FILES$/ { exit } { print }' "$W/dev.txt" >"$W/old.sums"
    awk 'f { print } /^@@FILES$/ { f = 1 }' "$W/dev.txt" >"$W/old.files"
    lap "read device sums"

    if ! cmp -s "$W/old.files" "$S/rinky/FILES"; then
        echo "deploy: the package file list differs from the installed one"
        return 1
    fi
    changed=$(awk 'NR == FNR { o[$2] = $1; next } o[$2] != $1 { print $1 " " $2 }' "$W/old.sums" "$S/rinky/SHA256SUMS")
    if grep -q ' rinky/install.sh$' <<<"$changed"; then
        echo "deploy: install.sh changed"
        return 1
    fi

    # Order: every changed file, then VERSION and FILES when they changed, SHA256SUMS last.
    : >"$W/order"
    printf '%s\n' "$changed" | grep -v -e '^$' | grep -v -e ' rinky/VERSION$' -e ' rinky/FILES$' -e ' rinky/SHA256SUMS$' >"$W/order" || true
    for rel in rinky/VERSION rinky/FILES; do
        printf '%s\n' "$changed" | grep -e " $rel\$" >>"$W/order" || true
    done
    ssum=$(sha256 "$S/rinky/SHA256SUMS" | cut -d' ' -f1)
    echo "$ssum rinky/SHA256SUMS" >>"$W/order"

    : >"$W/list"
    : >"$S/.fast-manifest"
    while read -r hash rel; do
        [ -n "$rel" ] || continue
        mode=644
        [ -x "$S/$rel" ] && mode=755
        echo "$hash $rel $mode" >>"$S/.fast-manifest"
        echo "$rel" >>"$W/list"
    done <"$W/order"
    echo ".fast-manifest" >>"$W/list"
    vsum=$(sed -n 's/^version=//p' "$S/rinky/VERSION" | head -n 1)
    echo "deploy: $(($(wc -l <"$W/order") - 1)) files differ (plus SHA256SUMS):"
    sed -e 's/^[0-9a-f]* /deploy:   /' "$W/order"

    if tar --version 2>/dev/null | grep -q bsdtar; then
        (cd "$S" && COPYFILE_DISABLE=1 tar --format ustar --uid 0 --gid 0 --uname root --gname root \
            --no-xattrs --no-mac-metadata -cf - -T "$W/list" | gzip -6 >"$W/fast.tar.gz")
    else
        (cd "$S" && tar --format=ustar --owner=0 --group=0 --numeric-owner -cf - -T "$W/list" | gzip -6 >"$W/fast.tar.gz")
    fi
    lap "pack changed files"

    "${SSH[@]}" "cat > /mnt/us/.rinky-fast.tar.gz" <"$W/fast.tar.gz"
    echo "deploy: sent $(wc -c <"$W/fast.tar.gz" | tr -d ' ') bytes (the full package is $(wc -c <"$PKG" | tr -d ' '))"
    lap "copy"

    "${SSH[@]}" sh -s -- "$RESTART" "$vsum" <<'EOF'
restart=$1 version=$2
cd /mnt/us || exit 1
tmp=fast$$
placed=
ok=0
t0=$(date +%s)
step() { echo "deploy:   device $1: $(($(date +%s) - t0)) s"; t0=$(date +%s); }
finish() {
    if [ "$ok" != 1 ]; then
        for f in $placed; do rm -f "$f"; done
        echo "deploy: fast install failed; the installed files are as they were" >&2
    fi
    rm -rf .rinky-fast .rinky-fast.tar.gz
}
trap finish EXIT
rm -rf .rinky-fast && mkdir .rinky-fast
tar -xzf .rinky-fast.tar.gz -C .rinky-fast || { echo "deploy: the copy is damaged (tar failed)" >&2; exit 1; }
rm -f .rinky-fast.tar.gz
step unpack
# Step 1: each file to <target>.fast<pid> next to its target, sha256 checked there.
while read -r sum rel mode; do
    dst=/mnt/us/$rel
    [ -d "$(dirname "$dst")" ] || { echo "deploy: no folder for $rel" >&2; exit 1; }
    mv .rinky-fast/"$rel" "$dst.$tmp" || exit 1
    placed="$placed $dst.$tmp"
    chmod "$mode" "$dst.$tmp"
    got=$(sha256sum "$dst.$tmp" | cut -d' ' -f1)
    [ "$got" = "$sum" ] || { echo "deploy: $rel damaged on the device ($got)" >&2; exit 1; }
done <.rinky-fast/.fast-manifest
step "place + sha256"
# Step 2: rename over the installed files (the manifest lists SHA256SUMS last).
while read -r sum rel mode; do
    mv -f "/mnt/us/$rel.$tmp" "/mnt/us/$rel" || exit 1
done <.rinky-fast/.fast-manifest
placed=
ok=1
if [ -f rinky/.install-state ]; then
    sed "s/^version=.*/version=$version/" rinky/.install-state >rinky/.install-state.$tmp &&
        mv -f rinky/.install-state.$tmp rinky/.install-state
fi
sync
step rename
bad=$(sha256sum -c rinky/SHA256SUMS 2>&1 | grep -vc ': OK$' || true)
[ "$bad" = 0 ] || { echo "deploy: $bad installed files fail their checksum" >&2; exit 1; }
step "check all files"
if [ "$restart" = 1 ]; then
    # restart: the launcher swaps the rinky process and the Kindle framework stays
    # stopped (an older launcher, or no rinky running: ctl does stop + start).
    n0=$(grep -c 'start: rinky' rinky/rinky.log 2>/dev/null || true)
    nohup sh rinky/rinky-ctl.sh restart --library >>rinky/scriptlet.log 2>&1 </dev/null &
    i=0
    while [ "$i" -lt 60 ]; do
        n=$(grep -c 'start: rinky' rinky/rinky.log 2>/dev/null || true)
        [ "${n:-0}" -gt "${n0:-0}" ] && break
        sleep 1
        i=$((i + 1))
    done
fi
EOF
    lap "install + restart"
    return 0
}

full_path() {
    "${SSH[@]}" "cat > /mnt/us/.rinky-deploy.tar.gz" <"$PKG"
    lap "copy (full package)"
    "${SSH[@]}" sh -s -- "$SUM" "$RESTART" <<'EOF'
set -e
want=$1 restart=$2
cd /mnt/us
got=$(sha256sum .rinky-deploy.tar.gz | cut -d' ' -f1)
[ "$got" = "$want" ] || { echo "deploy: copy damaged ($got)" >&2; rm -f .rinky-deploy.tar.gz; exit 1; }
rm -rf .rinky-pkg && mkdir .rinky-pkg
tar -xzf .rinky-deploy.tar.gz -C .rinky-pkg
sh .rinky-pkg/rinky/install.sh --upgrade | grep -E 'installed|error' || true
rm -rf .rinky-pkg .rinky-deploy.tar.gz
bad=$(sha256sum -c rinky/SHA256SUMS 2>&1 | grep -vc ': OK$' || true)
[ "$bad" = 0 ] || { echo "deploy: $bad installed files fail their checksum" >&2; exit 1; }
if [ "$restart" = 1 ]; then
    # restart: the launcher swaps the rinky process and the Kindle framework stays
    # stopped (an older launcher, or no rinky running: ctl does stop + start).
    n0=$(grep -c 'start: rinky' rinky/rinky.log 2>/dev/null || true)
    nohup sh rinky/rinky-ctl.sh restart --library >>rinky/scriptlet.log 2>&1 </dev/null &
    i=0
    while [ "$i" -lt 60 ]; do
        n=$(grep -c 'start: rinky' rinky/rinky.log 2>/dev/null || true)
        [ "${n:-0}" -gt "${n0:-0}" ] && break
        sleep 1
        i=$((i + 1))
    done
fi
EOF
    lap "install + restart"
}

if [ "$FULL" = 1 ]; then
    echo "deploy: --full: copying the whole package"
    full_path
elif fast_path; then
    echo "deploy: fast path done"
else
    echo "deploy: falling back to the full package"
    full_path
fi

if [ "$RESTART" = 1 ]; then
    lap "wait for start"
    "${SSH[@]}" "sh /mnt/us/rinky/rinky-ctl.sh status | head -1; grep 'start: rinky' /mnt/us/rinky/rinky.log | tail -1 | sed 's/ pid.*//'"
fi
echo "deploy: done in $(awk -v a="$T0" -v b="$(now)" 'BEGIN { printf "%.1f", b - a }') s"
