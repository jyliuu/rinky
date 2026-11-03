-- Streams from the mock server of tests/e2e/plugin-net.sh (settings.json holds `base`).
local p = {}

function p.on_book_open()
  local base = ink.settings.get("base")
  ink.log("starting requests to " .. tostring(base))
  ink.http.request({ url = base .. "/sse", stream = "sse" }, function(ev)
    if ev.type == "sse" then
      ink.log("sse " .. ev.event .. ": " .. ev.data)
    else
      ink.log("sse stream " .. ev.type .. " " .. tostring(ev.status or ev.message or ""))
    end
  end)
  ink.http.request({ url = base .. "/chunks", stream = "chunks" }, function(ev)
    if ev.type == "chunk" then
      ink.log("chunk of " .. #ev.data .. " bytes: " .. ev.data)
    else
      ink.log("chunk stream " .. ev.type .. " " .. tostring(ev.status or ev.message or ""))
    end
  end)
  ink.http.request({ url = base .. "/json", method = "POST", body = { question = "life" } }, function(ev)
    if ev.type == "done" then
      local j = ink.json.decode(ev.body)
      ink.log("json status " .. ev.status .. ", answer " .. tostring(j.answer) .. ", echo " .. tostring(j.echo))
    else
      ink.log("json " .. ev.type .. " " .. tostring(ev.message))
    end
  end)
  -- Not in hosts: refused before any connection is made.
  local ok, err = pcall(ink.http.get, "https://example.org/", print)
  ink.log("other host refused: " .. tostring(not ok) .. " (" .. tostring(err) .. ")")
end

return p
