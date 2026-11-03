-- Tries to leave the sandbox; logs what happened. Used by tests/e2e/plugin-broken.sh.
local p = {}

local function try(label, f)
  local ok, err = pcall(f)
  if ok then ink.log(label .. " allowed!") else ink.log(label .. " refused: " .. tostring(err)) end
end

function p.on_book_open()
  ink.log("os.execute = " .. tostring(os.execute))
  ink.log("os.exit = " .. tostring(os.exit))
  ink.log("io.popen = " .. tostring(io.popen))
  ink.log("debug = " .. tostring(debug))
  try("io.open(/etc/passwd)", function() return assert(io.open("/etc/passwd")) end)
  try("files.write(../escape.txt)", function() return ink.files.write("../escape.txt", "x") end)
  try("require(../x)", function() return require("../x") end)
  try("string.rep 1 GB", function() return #string.rep("x", 1024 * 1024 * 1024) end)
  assert(ink.files.write("own.txt", "mine"))
  if ink.files.read("own.txt") == "mine" then ink.log("own data file ok") end
end

return p
