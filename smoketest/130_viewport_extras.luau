eguidev.fixture("viewports.default")

local root_vp = eguidev.root

-- List viewports and find secondary
local vp_list = eguidev.viewports()
assert(#vp_list >= 2, "should have root and secondary viewports")
local secondary_vp = eguidev.wait_viewport({ name = "secondary" })
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

-- Duplicate names intentionally prevent widget lookup, including fixture ready
-- checks and capture waits. Frame waits still let the app apply and clear them.
local function wait_for_duplicate_names(expected: boolean)
    local deadline = os.clock() + 5
    while (#eguidev.viewports({ name = "secondary" }) >= 2) ~= expected do
        assert(os.clock() < deadline, "duplicate viewport names did not reach the expected state")
        eguidev.wait_frames(1)
    end
end

local duplicates_ok, duplicates_err = pcall(function()
    eguidev.fixture("viewports.duplicate_names", nil, { wait = false })
    wait_for_duplicate_names(true)
    local capture_ok, capture_err = pcall(function()
        root_vp:wait_capture()
    end)
    assert(not capture_ok, "duplicate viewport names must prevent capture waits")
    assert(type(capture_err) == "table", "instrumentation errors should be structured tables")
    local capture_error = capture_err :: any
    assert(capture_error.code == "instrumentation_fault", "duplicate names should report an instrumentation fault")
end)
eguidev.fixture("basic.default", nil, { wait = false })
wait_for_duplicate_names(false)
assert(duplicates_ok, tostring(duplicates_err))

return {
    viewport_id = secondary_vp.id,
    viewport_name = secondary_info.name,
    viewports_count = #vp_list,
}
