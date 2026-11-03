-- Has no network permission: the request is refused before any connection is made.
return {
  on_book_open = function()
    local ok, err = pcall(ink.http.get, "http://10.0.2.2:1/", print)
    ink.log("without permission refused: " .. tostring(not ok) .. " (" .. tostring(err) .. ")")
  end,
}
