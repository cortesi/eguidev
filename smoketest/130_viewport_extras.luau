eguidev.fixture("viewports.default")

local root_vp = eguidev.root

-- List viewports and find secondary
local vp_list = eguidev.viewports()
assert(#vp_list >= 2, "should have root and secondary viewports")
local secondary = eguidev.viewports({ name = "secondary" })[1]
assert(secondary ~= nil, "secondary viewport should be present")
local secondary_vp = secondary :: Viewport
local secondary_info = secondary_vp:state()
assert(secondary_info ~= nil)
assert(secondary_info.name == "secondary", "secondary viewport should expose its semantic name")
local focused_match = eguidev.viewports({ name = "secondary", focused = secondary_info.focused })[1]
assert(
    focused_match ~= nil and (focused_match :: Viewport).id == secondary_vp.id,
    "focused filter should preserve the named viewport when it matches"
)
local focused_mismatch = eguidev.viewports({ name = "secondary", focused = not secondary_info.focused })[1]
assert(focused_mismatch == nil, "focused filter should reject the named viewport when it differs")

local root_info = root_vp:state()

assert(root_info ~= nil)
assert(root_info.inner_size.x > 0 and root_info.inner_size.y > 0, "root viewport should be ready")

secondary_vp:focus()
eguidev.widget("viewports.scroll"):wait({ scroll_ready = true })

-- scroll_to in secondary viewport
eguidev.widget("viewports.scroll"):scroll_to({
    align = "top",
    settle = false,
})
eguidev.widget("viewports.row.0"):wait( function(widget)
    return widget ~= nil and widget.visible
end)
eguidev.widget("viewports.scroll"):wait( function(widget)
    if widget == nil then return false end
    local scroll = widget.scroll_state
    return scroll ~= nil and math.abs(scroll.offset.y) < 0.75
end)

-- Scroll to bottom in secondary viewport
local open_status = eguidev.widget("viewports.open"):state()
assert(open_status ~= nil)
assert(string.find(open_status.value_text, "true", 1, true) ~= nil,
    "secondary should be open")

-- viewports.scrolled fixture
eguidev.fixture("viewports.scrolled")
eguidev.widget("viewports.scroll"):wait( function(widget)
    if widget == nil then return false end
    local scroll = widget.scroll_state
    return scroll ~= nil and scroll.offset.y > 0
end)
local status = eguidev.widget("basic.status"):wait({ label = "Fixture: secondary viewport scrolled" })
assert(status ~= nil)

eguidev.fixture("viewports.duplicate_names")
eguidev.wait_frames(1)
assert(#eguidev.viewports({ name = "secondary" }) >= 2, "duplicate fixture should expose both viewports")
eguidev.fixture("basic.default")
eguidev.wait_frames(1)
assert(#eguidev.viewports({ name = "secondary" }) <= 1, "duplicate viewport should clear after reset")

return {
    viewport_id = secondary_vp.id,
    viewport_name = secondary_info.name,
    viewports_count = #vp_list,
}
