eguidev.fixture("viewports.default")
local secondary = eguidev.wait_viewport({ name = "secondary" })
local before = eguidev.capture()

local ok, message = pcall(function()
    local toggle = eguidev.widget("viewports.toggle")
    toggle:scroll_into_view()
    toggle:click()
    assert(secondary:wait({ present = false }) == nil, "closed viewport should be absent")
    assert(secondary:state() == nil)
    assert(#eguidev.viewports({ name = "secondary" }) == 0)
    for _, viewport in ipairs(eguidev.dump().viewports) do
        assert(viewport.id ~= secondary.id, "closed viewport remains in dump")
    end
    assert(table.find(before:diff().viewports_removed, secondary.id) ~= nil, "close missing from diff")

    local closed = eguidev.capture()
    toggle:click()
    local reopened = secondary:wait({ present = true })
    assert(reopened ~= nil, "existing handle should revive")
    assert(reopened.id == secondary.id, "reopened viewport changed identity")
    assert(table.find(closed:diff().viewports_added, secondary.id) ~= nil, "reopen missing from diff")
end)

eguidev.fixture("basic.default")
assert(ok, tostring(message))
return true
