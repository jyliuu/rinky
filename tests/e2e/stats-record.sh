#!/bin/bash
# e2e-cycles: 2
# Reading statistics (c-stats): the compiled-in `stats` plugin records page times in the
# real reader, and the library's Stats button shows them.
# 1. Starts the reader on juliet.epub with a fresh statistics.sqlite3, shows three pages
#    for 8 s each (wall time: pages under 5 s are not recorded; the first page also
#    counts the launch wait) and a fourth for 2 s,
#    then exits. Checks the `stats: database` log line and, in the pulled database, one
#    book row and three page records of 8 s (the 2 s page is not recorded), one session.
# 2. Starts the library, taps Stats (Overview), Books, then Back and Exit; screenshots.
# Coordinates are the 1272 x 1696 portrait layout.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

DB=$DEST/statistics.sqlite3
in_log() { guest "grep -q -- '$1' $DEST/rinky.log"; }
log_count_at_least() { [ "$(guest "grep -c -- '$1' $DEST/rinky.log" 2>/dev/null || echo 0)" -ge "$2" ]; }
wait_home 240 home-before
guest "rm -f $DB $DB-journal" || fail "remove $DB"
start_reader
wait_until 30 "first page" in_log "on_page_change" || fail "no on_page_change in the log"
not_home page1
sleep 8; tap 1000 900; sleep 8; tap 1000 900; sleep 8; tap 1000 900; sleep 2
not_home page4
exit_device
save_logs
cp "$OUT/rinky.log" "$OUT/rinky-reader.log"
grep -E "stats" "$OUT/rinky.log" >"$OUT/stats-lines.txt"
grep -qF "stats: database $DB" "$OUT/rinky.log" || fail "rinky.log lacks: stats: database $DB"
if grep -E "WARN.*stats" "$OUT/rinky.log"; then fail "stats warnings in the log"; fi
"$SLOT" pull "$NAME" "$DB" "$OUT/statistics.sqlite3" >/dev/null || fail "pull $DB"
python3 - "$OUT/statistics.sqlite3" <<'PY' >"$OUT/db.txt" 2>&1 || { cat "$OUT/db.txt"; fail "statistics rows"; }
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
books = c.execute("SELECT title, md5, total_read_time, total_read_pages FROM book").fetchall()
pages = c.execute("SELECT page, duration, total_pages FROM page_stat_data ORDER BY start_time").fetchall()
sessions = c.execute("SELECT duration, pages, source FROM session").fetchall()
print("book", books); print("page_stat_data", pages); print("session", sessions)
assert len(books) == 1 and "Romeo" in books[0][0], books
assert len(pages) == 3, pages
# The first page also covers the launch wait and screenshot before the first tap.
assert 7 <= pages[0][1] <= 60 and all(7 <= d <= 15 for _, d, _ in pages[1:]), pages
assert [p for p, _, _ in pages] == sorted({p for p, _, _ in pages}), pages
assert len(sessions) == 1 and sessions[0][1] == 3, sessions
PY
log "db: $(tr '\n' ' ' <"$OUT/db.txt")"

# Library -> Stats (after the stock framework is back from the reader run).
wait_clean_home 300 home-between
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sh $DEST/rinky.sh --library" || fail "launch library"
wait_until 120 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
wait_until 120 "library scan logged" in_log "library scan:" || fail "no 'library scan:' line"
nap 6
not_home library
tap 1098 68   # Stats icon (no gear while Settings is in the nav bar)
wait_until 30 "statistics screen" in_log "screen: statistics$" || fail "Stats did not open the statistics screen"
nap 4
not_home stats-overview
tap 934 200
nap 4
not_home stats-books
tap 83 75
wait_until 30 "back in the library" log_count_at_least "screen: library$" 2 || fail "Back did not return to the library"
nap 3
tap 1147 75
wait_until 30 "rinky exited" reader_gone || fail "rinky did not exit"
save_logs
wait_clean_home 300 home-after
pass "stats plugin wrote 3 page records and a session; library Stats -> Overview, Books, Back"
