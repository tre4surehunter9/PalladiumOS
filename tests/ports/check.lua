-- Copyright (c) 2026 tre4surehunter9
-- Checks that the Lua port works on PalladiumOS: the language, and the
-- parts that depend on the OS (files, processes, time, memory). Run by
-- tests/usermode.rs; exits 0 when everything passes.

-- language and the standard library
assert(7 // 2 == 3 and 7 / 2 == 3.5 and 2^10 == 1024.0 and math.type(1) == "integer")
assert(string.format("%5.2f|%d|%x|%s", math.pi, 42, 255, "s") == " 3.14|42|ff|s")
assert(("hello world"):gsub("o", "0") == "hell0 w0rld")
assert(("key=value"):match("(%w+)=(%w+)") == "key")
assert(utf8.len("héllo") == 5)
assert(string.unpack("<i4", string.pack("<i4", -7)) == -7)
local t = {5, 2, 8, 1}
table.sort(t, function(a, b) return a > b end)
assert(table.concat(t, ",") == "8,5,2,1")
local co = coroutine.wrap(function(a) local b = coroutine.yield(a + 1) return b * 2 end)
assert(co(1) == 2 and co(10) == 20)
local mt = setmetatable({}, {__index = function(_, k) return k .. "!" end})
assert(mt.hi == "hi!")
local ok, err = pcall(error, {code = 7})
assert(not ok and err.code == 7)
assert(tostring(1e100) == "1e+100" and tostring(0.1) == "0.1")
assert(math.floor(-3.5) == -4 and math.abs(math.sin(math.pi)) < 1e-15)

-- memory: a big string, many tables, garbage collection
local big = string.rep("x", 8 * 1024 * 1024)
assert(#big == 8388608)
big = nil
local list = {}
for i = 1, 200000 do list[i] = {i} end
assert(#list == 200000 and list[123456][1] == 123456)
list = nil
collectgarbage()
assert(collectgarbage("count") < 4096)

-- files
local name = os.tmpname()
local f = assert(io.open(name, "w"))
for i = 1, 100 do f:write(i, "\n") end
f:close()
local sum = 0
for line in io.lines(name) do sum = sum + tonumber(line) end
assert(sum == 5050)
f = assert(io.open(name, "r"))
assert(f:seek("end") == 292)
f:close()
assert(os.rename(name, name .. ".2"))
assert(io.open(name) == nil)
assert(os.remove(name .. ".2"))

-- processes
assert(os.execute("true") == true)
local _, how, code = os.execute("exit 5")
assert(how == "exit" and code == 5)
local p = assert(io.popen("echo one; echo two"))
assert(p:read("a") == "one\ntwo\n")
assert(p:close())
local w = assert(io.popen("wc -l > " .. "/tmp/lua-wc", "w"))
w:write("a\nb\nc\n")
w:close()
assert(io.open("/tmp/lua-wc"):read("n") == 3)
os.remove("/tmp/lua-wc")

-- time and environment
assert(os.time() > 1700000000)
assert(os.date("!%Y", 0) == "1970")
assert(os.time({year = 2000, month = 1, day = 1, hour = 0}) > 0)
-- CPU time advances in timer ticks (10 ms): spin until it does.
local c, n = os.clock(), 0
repeat n = n + 1 until os.clock() > c or n > 100000000
assert(os.clock() > c)
assert(os.getenv("NO_SUCH_VARIABLE") == nil)
math.randomseed(42)
local r = math.random(1, 100)
assert(r >= 1 and r <= 100)

print("lua checks passed")
