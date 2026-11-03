-- Streams one public-domain text (Project Gutenberg #1513, Romeo and Juliet) over HTTPS.
local p = {}

function p.on_book_open()
  local bytes, chunks, romeo = 0, 0, false
  ink.log("tls start")
  ink.http.request({ url = "https://www.gutenberg.org/cache/epub/1513/pg1513.txt", stream = "chunks", timeout = 60 }, function(ev)
    if ev.type == "response" then
      ink.log("tls response " .. ev.status .. " " .. tostring(ev.headers["content-type"]))
    elseif ev.type == "chunk" then
      bytes, chunks = bytes + #ev.data, chunks + 1
      romeo = romeo or ev.data:find("ROMEO", 1, true) ~= nil
    elseif ev.type == "done" then
      ink.log(string.format("tls done status %d, %d bytes in %d chunks, has ROMEO %s", ev.status, bytes, chunks, tostring(romeo)))
    else
      ink.log("tls error " .. tostring(ev.message))
    end
  end)
end

return p
