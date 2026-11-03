-- Sample rinky plugin: word count and reading timer.
--
-- Shows how a Lua plugin uses the plugin API (docs/plugins.md):
--   hooks       on_book_open, on_page_change, on_selection, on_suspend, on_resume, on_close
--   registries  reader menu items, a selection action, a gesture, a settings page,
--               a library action and a status-bar widget
--   document    ink.doc.page_text / read_around / current_position / metadata / page
--   UI          toast, dialog, menu, Markdown viewer, keyboard input
--   storage     ink.settings (per plugin), ink.book (per book), ink.files (data folder)
--
-- Reading time counts the time between page turns, at most IDLE_CAP seconds per page,
-- and stops while the device sleeps.

local IDLE_CAP = 300 -- seconds; a longer pause between two pages counts as this much

local p = {}

local session = nil -- { start, last, seconds, pages, words }

local function count_words(text)
  local n = 0
  for _ in string.gmatch(text or "", "%S+") do n = n + 1 end
  return n
end

local function minutes(seconds)
  return math.floor((seconds or 0) / 60 + 0.5)
end

local function fmt_time(seconds)
  local m = minutes(seconds)
  if m < 60 then return m .. " min" end
  return string.format("%d h %02d min", m // 60, m % 60)
end

-- Words on the current page: the engine's page text when it has one, otherwise an
-- estimate from the text around the reading position.
local function page_words()
  local text = ink.doc.page_text()
  if text then return count_words(text), true end
  local around = ink.doc.read_around(ink.doc.current_position(), 1200)
  return count_words(around), false
end

local function show_timer()
  return ink.settings.get("show_timer") ~= false
end

local function book_seconds()
  return (ink.book.get("seconds") or 0) + (session and session.seconds or 0)
end

local function update_status()
  if not session then return end
  if show_timer() then
    ink.ui.status("timer", "Read " .. fmt_time(book_seconds()))
  else
    ink.ui.status("timer", "")
  end
end

-- Adds the time since the last page turn to the session.
local function tick()
  if not session or not session.last then return end
  local now = os.time()
  session.seconds = session.seconds + math.min(now - session.last, IDLE_CAP)
  session.last = now
end

local function save_book()
  if not session then return end
  tick()
  ink.book.set("seconds", (ink.book.get("seconds") or 0) + session.seconds)
  ink.book.set("pages", (ink.book.get("pages") or 0) + session.pages)
  ink.book.set("words", (ink.book.get("words") or 0) + session.words)
  session.seconds, session.pages, session.words = 0, 0, 0
end

-- Registration ---------------------------------------------------------------------------

function p.on_load(reg)
  reg.status_widget("timer", "right", "")
  reg.menu_item("stats", "Word count and reading time", 10)
  reg.menu_item("note", "Add a note...", 11)
  reg.selection_action("count", "Count words")
  reg.gesture("timer_toast", "two_finger_tap", "Show reading time")
  reg.settings_page("timer", "Reading timer")
  reg.library_action("reading_time", "Reading time")
end

-- Lifecycle ------------------------------------------------------------------------------

function p.on_book_open()
  session = { start = os.time(), last = os.time(), seconds = 0, pages = 0, words = 0 }
  local m = ink.doc.metadata()
  ink.log("book opened: " .. (m.title or "?") .. ", read so far " .. fmt_time(book_seconds()))
  update_status()
end

function p.on_page_change(page)
  if not session then return end
  tick()
  if session.page and page ~= session.page then
    session.pages = session.pages + 1
    session.words = session.words + (session.page_words or 0)
  end
  session.page = page
  session.page_words = page_words()
  update_status()
end

function p.on_selection(sel)
  ink.log("selection of " .. count_words(sel.text) .. " words on page " .. sel.page)
end

function p.on_suspend()
  tick()
  if session then session.last = nil end -- sleeping time does not count
  ink.log("paused at " .. fmt_time(book_seconds()))
end

function p.on_resume()
  if session then
    session.last = os.time()
    ink.ui.toast("Welcome back: " .. fmt_time(book_seconds()) .. " read in this book")
  end
  update_status()
end

function p.on_close()
  if not session then return end
  tick()
  local seconds, pages = session.seconds, session.pages
  save_book()
  local m = ink.doc.metadata()
  local line = string.format("%s,%q,%d,%d\n", os.date("!%Y-%m-%dT%H:%M:%SZ"), m.title or "", seconds, pages)
  ink.files.append("sessions.csv", line)
  ink.ui.toast("Session: " .. fmt_time(seconds) .. ", " .. pages .. " pages")
  session = nil
end

-- Actions --------------------------------------------------------------------------------

local function stats()
  tick()
  local words, exact = page_words()
  local total = book_seconds()
  local pages = (ink.book.get("pages") or 0) + session.pages
  local words_read = (ink.book.get("words") or 0) + session.words
  local lines = {
    "# " .. (ink.doc.metadata().title or "This book"),
    "",
    "## This page",
    "",
    "- Page " .. (ink.doc.page() or "?") .. " of " .. (ink.doc.page_count() or "?"),
    "- " .. (exact and "Words on this page: " or "Words near this position: ") .. "**" .. words .. "**",
    "",
    "## Reading time",
    "",
    "| | Session | Book |",
    "|---|---|---|",
    "| Time | " .. fmt_time(session.seconds) .. " | " .. fmt_time(total) .. " |",
    "| Pages turned | " .. session.pages .. " | " .. pages .. " |",
  }
  if total >= 60 and pages > 0 then
    lines[#lines + 1] = ""
    lines[#lines + 1] = string.format("About **%.1f pages per hour**.", pages * 3600 / total)
    if words_read > 0 then
      lines[#lines + 1] = string.format("About **%d words per minute**.", math.floor(words_read * 60 / total + 0.5))
    end
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "> Pauses longer than " .. minutes(IDLE_CAP) .. " minutes count as " .. minutes(IDLE_CAP) .. " minutes; sleep does not count."
  ink.ui.view({ title = "Word count and reading time", markdown = table.concat(lines, "\n") })
end

local function add_note()
  ink.ui.input({ prompt = "Note for page " .. (ink.doc.page() or "?") }, function(text)
    if text == "" then return end
    local m = ink.doc.metadata()
    ink.files.append("notes.md", string.format("- %s, %s p. %s: %s\n", os.date("%Y-%m-%d"), m.title or "?", ink.doc.page() or "?", text))
    ink.ui.toast("Note saved")
  end)
end

local function settings_page()
  local label = "Show the timer in the status bar: " .. (show_timer() and "on" or "off")
  ink.ui.menu({ title = "Reading timer", items = { { label, "toggle" }, { "Reset the time of this book", "reset" } } }, function(choice)
    if choice == "toggle" then
      ink.settings.set("show_timer", not show_timer())
      update_status()
      ink.ui.toast("Timer in the status bar: " .. (show_timer() and "on" or "off"))
    elseif choice == "reset" then
      ink.ui.dialog({ title = "Reset reading time?", body = "The time and page count of this book go back to zero.",
                      buttons = { { "Cancel", "cancel" }, { "Reset", "reset" } } }, function(answer)
        if answer == "reset" then
          ink.book.set("seconds", nil)
          ink.book.set("pages", nil)
          ink.book.set("words", nil)
          if session then session.seconds, session.pages, session.words = 0, 0, 0 end
          update_status()
          ink.ui.toast("Reading time reset")
        end
      end)
    end
  end)
end

function p.on_action(id)
  if id == "stats" then
    stats()
  elseif id == "note" then
    add_note()
  elseif id == "count" then
    local sel = ink.doc.selection()
    if sel then
      ink.ui.toast(count_words(sel.text) .. " words, " .. utf8.len(sel.text) .. " characters")
    else
      ink.ui.toast("Nothing is selected")
    end
  elseif id == "timer_toast" then
    tick()
    ink.ui.toast("Reading for " .. fmt_time(session and session.seconds or 0) .. " (book: " .. fmt_time(book_seconds()) .. ")")
  elseif id == "settings:timer" then
    settings_page()
  end
end

function p.on_library_action(id, book)
  if id == "reading_time" then
    local name = book:match("([^/]+)$") or book
    ink.ui.toast(name .. ": " .. fmt_time(ink.book.get("seconds")) .. ", " .. (ink.book.get("pages") or 0) .. " pages")
  end
end

return p
