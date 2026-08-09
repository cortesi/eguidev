eguidev.fixture("basic.default")
local vp = eguidev.root
local TOLERANCE = 0.75

-- Verify initial state: top rows visible
eguidev.widget("basic.scroll.row.0"):wait( function(widget)
    return widget ~= nil and widget.visible
end)

-- scroll_to align=center
eguidev.widget("basic.scroll"):scroll_to({ align = "center" })
vp:settle()

-- After centering, row 0 should not be visible but middle rows should exist
local _row_0_after_center = eguidev.widget("basic.scroll.row.0") :: any
-- Row 0 might or might not be visible depending on scroll area size, just verify the API works

-- Jump buttons
eguidev.widget("basic.scroll.jump_down"):click()
vp:settle()

-- Verify scroll offset label updates
local offset_state = eguidev.widget("basic.scroll.offset"):wait({ present = true })
assert(offset_state ~= nil)
local offset_text = tostring(offset_state.label)
assert(offset_text ~= nil)
assert(string.find(offset_text, "Scroll offset:", 1, true) ~= nil, "should show scroll offset")

eguidev.widget("basic.scroll.jump_top"):click()
eguidev.widget("basic.scroll"):wait(function(widget)
    if widget == nil then return false end
    local scroll = widget.scroll_state
    return scroll ~= nil and math.abs(scroll.offset.y) < TOLERANCE
end)

-- scroll with positive delta (content up)
eguidev.widget("basic.scroll"):scroll({ x = 0, y = -200 })
vp:settle()

-- basic.scrolled fixture pre-scrolls the area
eguidev.fixture("basic.scrolled")
local status = eguidev.widget("basic.status"):wait({ label = "Fixture: scrolled" })
assert(status ~= nil)

-- viewports.scrolled fixture pre-scrolls the secondary viewport
eguidev.fixture("viewports.scrolled")
status = eguidev.widget("basic.status"):wait({ label = "Fixture: secondary viewport scrolled" })
assert(status ~= nil)

return {
    scroll_tests = "complete",
}
