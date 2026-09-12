-- Hover duration is elapsed time, regardless of the app's frame rate.
eguidev.fixture("basic.default")

local submit = eguidev.widget("basic.submit")
submit:hover()

local started = os.clock()
submit:hover({ duration_ms = 80 })
local elapsed_ms = (os.clock() - started) * 1000
assert(elapsed_ms >= 80, "hover must honour duration_ms, took " .. tostring(elapsed_ms))

return { elapsed_ms = elapsed_ms }
