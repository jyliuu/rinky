#!/bin/sh
# Sets rinky files aside and back, for the packaging E2E scenarios (guest, busybox sh).
#
#   sh guest-aside.sh save      copy them into $ASIDE, then delete them from /mnt/us
#   sh guest-aside.sh restore   put them back from $ASIDE, then delete $ASIDE
#
# The packaging scenarios need a Kindle without rinky. In a full E2E run an earlier
# scenario may have run `scripts/emu-slot.sh deploy` (which writes /mnt/us/rinky) or
# left per-book sidecars. "rinky files" are the paths uninstall.sh removes:
# /mnt/us/rinky, /mnt/us/extensions/rinky, and <book>.sdr/rinky* and
# *.rinky outside /mnt/us/koreader and /mnt/us/system. /mnt/us/koreader is never read.
# $ASIDE is on /mnt/us (the copy of /mnt/us/rinky holds the 20 MB binary; the guest
# /tmp is a 64 MB tmpfs) under /mnt/us/.rinky-e2e, which guest-list.sh leaves out.
# Copies are numbered ($ASIDE/1, $ASIDE/2, ...) so uninstall.sh, which deletes
# <book>.sdr/rinky* anywhere on /mnt/us, cannot match them.
ASIDE=${ASIDE:-/mnt/us/.rinky-e2e/aside}
LIST=$ASIDE.list
US=/mnt/us

case "${1:-}" in
    save)
        if [ -e "$ASIDE" ] || [ -e "$LIST" ]; then
            echo "aside: $ASIDE exists (an earlier run was not restored)"
            exit 1
        fi
        mkdir -p "$ASIDE" || exit 1
        {
            [ -e "$US/rinky" ] && echo "$US/rinky"
            [ -e "$US/extensions/rinky" ] && echo "$US/extensions/rinky"
            find "$US" \( -path "$US/koreader" -o -path "$US/system" -o -path "$US/rinky" -o -path "$US/.rinky-e2e" \
                -o -path "$US/extensions/rinky" \) -prune -o \
                -type f \( -path "*.sdr/rinky*" -o -name "*.rinky" \) -print 2>/dev/null
        } >"$LIST"
        n=0
        while IFS= read -r p; do
            [ -n "$p" ] || continue
            n=$((n + 1))
            if ! cp -a "$p" "$ASIDE/$n"; then
                echo "aside: cannot copy $p; nothing deleted"
                exit 1
            fi
        done <"$LIST"
        # Delete only after every copy succeeded.
        while IFS= read -r p; do
            [ -n "$p" ] && rm -rf "$p"
        done <"$LIST"
        echo "aside: saved $n path(s) in $ASIDE"
        ;;
    restore)
        if [ ! -f "$LIST" ]; then
            echo "aside: nothing to restore"
            exit 0
        fi
        n=0
        while IFS= read -r p; do
            [ -n "$p" ] || continue
            n=$((n + 1))
            rm -rf "$p"
            if ! { mkdir -p "$(dirname "$p")" && cp -a "$ASIDE/$n" "$p"; }; then
                echo "aside: cannot restore $p (copy kept in $ASIDE/$n)"
                exit 1
            fi
        done <"$LIST"
        rm -rf "$ASIDE" "$LIST"
        echo "aside: restored $n path(s)"
        ;;
    *)
        sed -n '2,6p' "$0"
        exit 2
        ;;
esac
