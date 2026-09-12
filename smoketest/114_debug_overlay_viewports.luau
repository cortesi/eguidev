eguidev.fixture("viewports.default")
local root = eguidev.root
local secondary = eguidev.wait_viewport({ name = "secondary" })
local root_widget = root:widget("basic.submit")
local secondary_widget = secondary:widget("viewports.row.0")

local function border_color(widget: Widget): string
    local state = widget:state()
    assert(state ~= nil)
    return widget:sample_pixels({ { x = 0.5, y = (state.rect.max.y - state.rect.min.y) / 2 } })[1].hex
end

local root_before = border_color(root_widget)
local secondary_before = border_color(secondary_widget)
local red: DebugOverlayOptions = { show_labels = false, bounds_color = "#ff0000" }
local blue: DebugOverlayOptions = { show_labels = false, bounds_color = "#0000ff" }

local ok, message = pcall(function()
    root:show_debug_overlay(red)
    secondary_widget:show_debug_overlay(blue)
    assert(border_color(root_widget) == "#ff0000ff", "root debug overlay was replaced")
    assert(border_color(secondary_widget) == "#0000ffff", "secondary debug overlay was replaced")

    secondary_widget:clear_debug_overlay()
    assert(border_color(root_widget) == "#ff0000ff", "widget clear removed another viewport's overlay")
    assert(border_color(secondary_widget) == secondary_before, "secondary debug overlay remains")

    root_widget:show_debug_overlay(red)
    secondary:show_debug_overlay(blue)
    root:clear_debug_overlay()
    assert(border_color(root_widget) == root_before, "root debug overlay remains")
    assert(border_color(secondary_widget) == "#0000ffff", "viewport clear removed another viewport's overlay")
end)

root:clear_debug_overlay()
secondary:clear_debug_overlay()
eguidev.fixture("basic.default")
assert(ok, tostring(message))
return true
