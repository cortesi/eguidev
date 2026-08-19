eguidev.fixture("basic.default")
eguidev.wait(function()
    return eguidev.diagnostic("demo.runtime").ready == true
end)

local runtime = eguidev.diagnostic("demo.runtime")
assert(runtime.status ~= nil, "runtime diagnostic should include status")
assert(runtime.secondary_visible ~= nil, "runtime diagnostic should include viewport state")

local ui = eguidev.diagnostic("demo.ui")
assert(ui.ready == true, "UI diagnostic should report readiness")
assert(ui.viewport_id ~= nil, "UI diagnostic should include viewport id")

local snapshot = eguidev.diagnostics()
assert(snapshot.values["demo.runtime"].ready == true, "snapshot should include runtime diagnostic")
assert(snapshot.values["demo.ui"].ready == true, "snapshot should include UI diagnostic")
assert(next(snapshot.errors) == nil, "diagnostic snapshot should not report errors")
