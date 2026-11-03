-- Dumps what KOReader's crengine (DOM version 20240114, the latest) makes of an EPUB:
-- TOC and page-map xpointers and every visible word start, as JSON. These files are the
-- expected values of the xpointer tests (crates/ink-html/tests/xpointer.rs).
-- Usage (with a KOReader desktop build; nothing under the KOReader tree is written):
--   KO_BUILD=.../base/build/<host> KO_DATA=.../koreader/data INK_FONTS=fonts CRE_CACHE=/tmp/x \
--   luajit dump.lua BOOK.epub OUT.json [keep_every_nth_word]
local KO = os.getenv("KO_BUILD") or error("set KO_BUILD to a KOReader desktop build (base/build/<host>)")
local KODATA = os.getenv("KO_DATA") or error("set KO_DATA to the KOReader data folder")
local FONTS = os.getenv("INK_FONTS") or "fonts"
package.cpath = KO .. "/?.so;" .. KO .. "/libs/?.so;" .. package.cpath
local cre = require("libs/libkoreader-cre")
local book, out, nth = arg[1], arg[2], tonumber(arg[3] or "1")

cre.initCache(os.getenv("CRE_CACHE") or "/tmp/cre-cache", 0, true)
cre.initHyphDict(KODATA .. "/hyph/")
for _, f in ipairs({"NotoSerif-Regular.ttf", "NotoSerif-Bold.ttf", "NotoSerif-Italic.ttf"}) do
  cre.registerFont(FONTS .. "/" .. f)
end
local doc = cre.newDocView(1272, 1696, 1)
doc:setIntProperty("crengine.render.requested_dom_version", cre.getLatestDomVersion())
doc:setStyleSheet(KODATA .. "/epub.css")
assert(doc:loadDocument(book), "load failed")
doc:renderDocument()

local function esc(s)
  s = tostring(s)
  s = s:gsub('\\', '\\\\'):gsub('"', '\\"'):gsub('\n', '\\n'):gsub('\r', '\\r'):gsub('\t', '\\t')
  s = s:gsub('[%z\1-\31]', function(c) return string.format('\\u%04x', c:byte()) end)
  return '"' .. s .. '"'
end

local parts = {}
local function w(s) parts[#parts + 1] = s end
w('{\n"book": ' .. esc(book:match("[^/]+$")) .. ',\n')
w('"dom_version": ' .. cre.getLatestDomVersion() .. ',\n')
w('"pages": ' .. doc:getPages() .. ',\n')
w('"toc": [')
local toc = doc:getToc() or {}
for i, t in ipairs(toc) do
  w((i > 1 and ',' or '') .. '\n  {"title": ' .. esc(t.title) .. ', "depth": ' .. t.depth .. ', "xpointer": ' .. esc(t.xpointer) .. '}')
end
w('\n],\n"page_map": [')
local pm = doc:getPageMap() or {}
for i, t in ipairs(pm) do
  w((i > 1 and ',' or '') .. '\n  {"label": ' .. esc(t.label) .. ', "xpointer": ' .. esc(t.xpointer) .. '}')
end
w('\n],\n"words": [')
local xp = "/body/DocFragment[1]/body"
local n, kept = 0, 0
while true do
  local nx = doc:getNextVisibleWordStart(xp)
  if not nx or nx == xp then break end
  xp = nx
  n = n + 1
  if (n - 1) % nth == 0 then
    local xe = doc:getNextVisibleWordEnd(xp)
    local text = xe and doc:getTextFromXPointers(xp, xe) or ""
    w((kept > 0 and ',' or '') .. '\n  [' .. esc(xp) .. ', ' .. esc(text) .. ']')
    kept = kept + 1
  end
end
w('\n],\n"word_count": ' .. n .. '\n}\n')
local f = assert(io.open(out, "w"))
f:write(table.concat(parts))
f:close()
print(book, "pages", doc:getPages(), "words", n, "kept", kept, "toc", #toc, "pagemap", #pm)
