fixture("viewports.default")

local root_vp = root()

-- List viewports and find secondary
local vp_list = viewports()
assert(#vp_list >= 2, "should have root and secondary viewports")
local secondary = viewport({ name = "secondary" })
assert(secondary ~= nil, "secondary viewport should be present")
local secondary_vp = secondary :: Viewport
local secondary_info = secondary_vp:state()
assert(secondary_info.name == "secondary", "secondary viewport should expose its semantic name")
local focused_match = viewport({ name = "secondary", focused = secondary_info.focused })
assert(
    focused_match ~= nil and (focused_match :: Viewport).id == secondary_vp.id,
    "focused filter should preserve the named viewport when it matches"
)
local focused_mismatch = viewport({ name = "secondary", focused = not secondary_info.focused })
assert(focused_mismatch == nil, "focused filter should reject the named viewport when it differs")

local root_info = root_vp:state()
assert(root_info.inner_size.x > 0 and root_info.inner_size.y > 0, "root viewport should be ready")

secondary_vp:focus()
secondary_vp:widget_get("viewports.scroll"):wait_for_scroll_ready()

-- scroll_to in secondary viewport
secondary_vp:widget_get("viewports.scroll"):scroll_to({
    align = "top",
    settle = false,
})
secondary_vp:wait_for_widget("viewports.row.0", function(widget)
    return widget.visible
end)
secondary_vp:wait_for_widget("viewports.scroll", function(widget)
    local scroll = widget.scroll_state
    return scroll ~= nil and math.abs(scroll.offset.y) < 0.75
end)

-- Scroll to bottom in secondary viewport
local open_status = root_vp:widget_get("viewports.open"):state()
assert(string.find(open_status.value_text, "true", 1, true) ~= nil,
    "secondary should be open")

-- viewports.scrolled fixture
fixture("viewports.scrolled")
secondary_vp:wait_for_widget("viewports.scroll", function(widget)
    local scroll = widget.scroll_state
    return scroll ~= nil and scroll.offset.y > 0
end)
assert(
    tostring(root_vp:widget_get("basic.status"):state().label)
        == "Fixture: secondary viewport scrolled"
)

fixture_raw("viewports.duplicate_names")
local duplicate_ok = true
for _ = 1, 3 do
    wait_for_frames(1)
    local ok = pcall(function()
        local _duplicate = viewport({ name = "secondary" })
    end)
    duplicate_ok = ok
    if not duplicate_ok then
        break
    end
end
assert(not duplicate_ok, "duplicate viewport names should be a hard automation fault")
fixture_raw("basic.default")
local reset_ok = false
for _ = 1, 5 do
    wait_for_frames(1)
    local ok = pcall(function()
        local _cleared = viewport({ name = "secondary" })
    end)
    reset_ok = ok
    if reset_ok then
        break
    end
end
assert(reset_ok, "duplicate viewport name fault should clear after reset")

return {
    viewport_id = secondary_vp.id,
    viewport_name = secondary_info.name,
    viewports_count = #vp_list,
}
