-- Writes Lua table literals with KOReader's own serializer (frontend/dump.lua), for the
-- ink-store Lua literal parser tests. Run with LuaJIT (KOReader's interpreter):
--   luajit gen-dump.lua /path/to/koreader OUTDIR
-- Outputs: nasty.lua (edge-case strings and numbers), loop.lua (a table that contains
-- itself, which dump writes as `nil --[[ LOOP: ... ]]`).
local root, out = arg[1], arg[2]
package.path = root .. "/frontend/?.lua;" .. root .. "/base/?.lua;" .. package.path
_G.unpack = _G.unpack or table.unpack
local dump = require("dump")

local function write(name, data)
    local f = assert(io.open(out .. "/" .. name, "wb"))
    -- Same layout as LuaSettings:flush / DocSettings:flush.
    f:write("-- " .. name .. "\nreturn ", dump(data), "\n")
    f:close()
end

local nasty = {
    quote = 'say "hi"',
    backslash = "a\\b\\\\c",
    newline = "line1\nline2\n",
    cr = "a\rb",
    nul = "a\0b",
    nul_digit = "a\0" .. "1b",
    controls = "\1\2\7\8\9\11\12\27\31\127",
    cjk = "\228\184\173\230\150\135 text",
    emoji = "\240\159\152\128",
    bad_utf8 = "x\255y",
    empty = "",
    int = 42,
    neg = -7,
    zero = 0,
    half = 0.5,
    third = 1 / 3,
    big = 1e300,
    tiny = 1e-300,
    two53 = 2 ^ 53,
    negzero = -0.0,
    huge = math.huge,
    neghuge = -math.huge,
    yes = true,
    no = false,
    list = { 10, 20, 30, "x", { 1, 2 } },
    holes = { [1] = "a", [3] = "c" },
    numkeys = { [-1] = "m", [0] = "z", [2.5] = "f" },
    boolkeys = { [true] = "t", [false] = "f" },
    nested = { a = { b = { c = { d = "deep" } } } },
    empty_table = {},
}
nasty.nested_list = { nasty.list[5], { "q" } }
write("nasty.lua", nasty)

local loop = { name = "loop" }
loop.self = loop
write("loop.lua", loop)

-- KOReader book metadata in the shapes KOReader writes (ReaderAnnotation:buildAnnotation,
-- ReaderRolling/ReaderPaging, BookStatusWidget). Self-made data; the text is public domain
-- (Shakespeare, Romeo and Juliet).
local ok_ordered = pcall(require, "ffi/util")
local function write_meta(name, data)
    local f = assert(io.open(out .. "/" .. name, "wb"))
    f:write("-- " .. name .. "\nreturn ", dump(data, nil, ok_ordered), "\n")
    f:close()
end

write_meta("metadata.rolling.epub.lua", {
    annotations = {
        {   -- highlight without note
            chapter = "ACT I", color = "yellow", datetime = "2026-10-01 20:15:03",
            drawer = "lighten", page = "/body/DocFragment[5]/body/div/p[12]/text().0",
            pageno = 14, pos0 = "/body/DocFragment[5]/body/div/p[12]/text().0",
            pos1 = "/body/DocFragment[5]/body/div/p[12]/text().38",
            text = "Two households, both alike in dignity",
        },
        {   -- highlight with a note (multi-line, quotes)
            chapter = "ACT II", datetime = "2026-10-02 08:00:00", datetime_updated = "2026-10-03 09:30:00",
            drawer = "underscore", page = "/body/DocFragment[9]/body/div/p[40]/text()[2].5",
            pageno = 61, pos0 = "/body/DocFragment[9]/body/div/p[40]/text()[2].5",
            pos1 = "/body/DocFragment[9]/body/div/p[41]/text().12",
            text = "What's in a name? That which we call a rose\nBy any other word would smell as sweet",
            note = "The \"name\" speech.\nSee also act III.",
        },
        {   -- page bookmark (no pos0)
            chapter = "ACT III", datetime = "2026-10-04 22:10:00",
            page = "/body/DocFragment[14]/body/div/p[2]/text().0", pageno = 95,
            text = "in ACT III",
        },
        {   -- bookmark with a note
            datetime = "2026-10-05 07:00:00", page = "/body/DocFragment[20]/body/div/p[1]/text().0",
            note = "come back here", text = "",
        },
    },
    annotations_paging = { { page = 3, datetime = "2026-01-01 00:00:00", text = "from another engine" } },
    cre_dom_version = 20240114,
    doc_pages = 180,
    doc_path = "/mnt/us/Books/juliet.epub",
    doc_props = {
        authors = "William Shakespeare\nAnother Editor", language = "en", series = "Tragedies",
        series_index = 3, title = "Romeo and Juliet",
    },
    last_xpointer = "/body/DocFragment[12]/body/div/p[7]/text().42",
    partial_md5_checksum = "cf7ce9b8d4e135c8ac340f18d1e58c85",
    percent_finished = 0.53846153846154,
    summary = { modified = "2026-10-05", note = "Read again in spring.", rating = 4, status = "abandoned" },
    stats = { authors = "William Shakespeare", highlights = 2, notes = 1, pages = 180, title = "Romeo and Juliet" },
})

write_meta("metadata.paging.pdf.lua", {
    annotations = {
        {
            datetime = "2026-09-10 10:00:00", drawer = "lighten", page = 4, pageno = 4,
            pboxes = { { x = 72.5, y = 100, w = 200.25, h = 12 }, { x = 72.5, y = 114, w = 80, h = 12 } },
            pos0 = { page = 4, rotation = 0, x = 73, y = 105, zoom = 1 },
            pos1 = { page = 4, rotation = 0, x = 152, y = 120, zoom = 1 },
            text = "a highlighted sentence on page four",
        },
        {
            datetime = "2026-09-11 11:00:00", drawer = "strikeout", note = "pdf note", page = 9,
            pboxes = { { x = 10, y = 20, w = 30, h = 10 } },
            pos0 = { page = 9, x = 11, y = 21 }, pos1 = { page = 9, x = 39, y = 29 },
            text = "struck",
        },
        { datetime = "2026-09-12 12:00:00", page = 12, text = "Page 12" },
    },
    annotations_rolling = { { page = "/body/p[1]/text().0", datetime = "2026-01-01 00:00:00", text = "x" } },
    doc_pages = 40,
    last_page = 9,
    partial_md5_checksum = "41cce710f34e5ec21315e19c99821415",
    percent_finished = 0.225,
    summary = { status = "complete", rating = 5 },
})

-- KOReader before 2024: bookmarks + highlight, one highlight older than 2014 that is
-- only in `highlight`, and an automatic bookmark text.
write_meta("metadata.legacy.epub.lua", {
    bookmarks = {
        {   -- highlight with a user note in `text`
            chapter = "Prologue", datetime = "2020-05-01 10:00:00", highlighted = true,
            notes = "Two households", page = "/body/DocFragment[3]/body/p[1]/text().0",
            pos0 = "/body/DocFragment[3]/body/p[1]/text().0", pos1 = "/body/DocFragment[3]/body/p[1]/text().14",
            text = "my note",
        },
        {   -- highlight whose `text` is the automatic label
            datetime = "2020-05-02 11:00:00", highlighted = true, notes = "star-cross'd lovers",
            page = "/body/DocFragment[3]/body/p[6]/text().3",
            pos0 = "/body/DocFragment[3]/body/p[6]/text().3", pos1 = "/body/DocFragment[3]/body/p[6]/text().21",
            text = "Page 2 star-cross'd lovers @ 2020-05-02 11:00:00",
        },
        {   -- page bookmark
            datetime = "2020-05-03 12:00:00", notes = "in Prologue", page = "/body/DocFragment[4]/body/p[1]/text().0",
            text = "",
        },
    },
    highlight = {
        [2] = {
            { datetime = "2020-05-01 10:00:00", drawer = "invert", text = "Two households",
              pos0 = "/body/DocFragment[3]/body/p[1]/text().0", pos1 = "/body/DocFragment[3]/body/p[1]/text().14" },
            { datetime = "2020-05-02 11:00:00", drawer = "underscore", text = "star-cross'd lovers",
              pos0 = "/body/DocFragment[3]/body/p[6]/text().3", pos1 = "/body/DocFragment[3]/body/p[6]/text().21" },
        },
        [7] = {
            { datetime = "2013-01-01 09:00:00", drawer = "lighten", text = "an old highlight",
              pos0 = "/body/DocFragment[7]/body/p[2]/text().0", pos1 = "/body/DocFragment[7]/body/p[2]/text().16" },
            { datetime = "2013-01-01 09:05:00", text = "no position, skipped" },
        },
    },
    last_xpointer = "/body/DocFragment[4]/body/p[3]/text().7",
    percent_finished = 1,
})

write_meta("metadata.legacy-paging.pdf.lua", {
    bookmarks = {
        { datetime = "2019-02-02 10:00:00", highlighted = true, notes = "pdf text", page = 3,
          pos0 = { x = 50, y = 60 }, pos1 = { x = 150, y = 72 }, text = "pdf text" },
        { datetime = "2019-02-03 10:00:00", notes = "Page 5", page = 5, text = "remember" },
    },
    highlight = {
        [3] = { { datetime = "2019-02-02 10:00:00", drawer = "lighten", text = "pdf text",
                  pos0 = { x = 50, y = 60 }, pos1 = { x = 150, y = 72 },
                  pboxes = { { x = 48, y = 58, w = 104, h = 16 } } } },
    },
    last_page = 5,
    highlights_imported = true,
})
