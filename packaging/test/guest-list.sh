#!/bin/sh
# File list of a Kindle for the uninstall check (runs in the guest, busybox sh).
#   sh guest-list.sh > list.txt
# /mnt/us: type, path, size, mode, md5 for files.  /var/local: type and path only (logs and
# databases there change size all the time).  /tmp: rinky leftovers only.
# /mnt/us/.rinky-e2e (the scenarios' staging area) is left out.
find /mnt/us -xdev -path /mnt/us/.rinky-e2e -prune -o -print 2>/dev/null | sort | while IFS= read -r f; do
    if [ -L "$f" ]; then
        echo "L $f"
    elif [ -d "$f" ]; then
        echo "D $f"
    else
        echo "F $f $(stat -c '%s %a' "$f") $(md5sum "$f" | cut -d' ' -f1)"
    fi
done
find /var/local -xdev 2>/dev/null | sort | while IFS= read -r f; do
    if [ -d "$f" ]; then echo "D $f"; else echo "F $f"; fi
done
find /tmp -maxdepth 1 -name 'rinky*' 2>/dev/null | sort | sed 's/^/T /'
