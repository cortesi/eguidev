eguidev.fixture("basic.default")
local vp = eguidev.root

-- Separator widgets exist
eguidev.widget("basic.separator.primary"):expect({ present = true })
eguidev.widget("basic.separator.scroll"):expect({ present = true })
eguidev.widget("viewports.separator.primary"):expect({ present = true })

local sep = eguidev.widget("basic.separator.primary")
local sep_state = sep:state()
assert(sep_state ~= nil and sep_state.role == "separator")

-- Spinner widget
eguidev.widget("basic.spinner.loading"):expect({ present = true })
local spinner = eguidev.widget("basic.spinner.loading")
local spinner_state = spinner:state()
assert(spinner_state ~= nil)
local spinner_rect = spinner_state.rect
assert(spinner_rect ~= nil)
assert(spinner_rect.max.x > spinner_rect.min.x, "spinner should have width")

-- Image widget
eguidev.widget("basic.preview.image"):expect({ present = true })
local image = eguidev.widget("basic.preview.image")
local image_state = image:state()
assert(image_state ~= nil and image_state.role == "image")

-- Link clicks update counter
local before_text = tostring((assert(eguidev.widget("basic.links.count"):state())).label)
assert(before_text ~= nil)
assert(before_text == "Link clicks: 0")

eguidev.widget("basic.link.docs"):click()
eguidev.widget("basic.links.count"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Link clicks: 1", 1, true) ~= nil
end)

-- Menu action counter
local menu_before_text = tostring((assert(eguidev.widget("basic.menu.count"):state())).label)
assert(menu_before_text ~= nil)
assert(menu_before_text == "Menu actions: 0")

eguidev.widget("basic.menu.actions"):click()
eguidev.widget("basic.menu.actions.reset_status"):expect({ present = true })
eguidev.widget("basic.menu.actions.reset_status"):click()
eguidev.widget("basic.menu.count"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Menu actions: 1", 1, true) ~= nil
end)

-- Hover on a link
eguidev.widget("basic.link.reference"):hover()
vp:settle()

-- Hover with position offset
eguidev.widget("basic.link.docs"):hover({ position = { x = 0.5, y = 0.5 } })
vp:settle()

-- Collapsing section: verify advanced section toggle
eguidev.widget("basic.advanced"):set_value(false)
-- Advanced widgets should not be visible when collapsed (use include_invisible to check)
eguidev.widget("basic.advanced"):set_value(true)
eguidev.widget("basic.advanced.action"):expect({ present = true })
eguidev.widget("basic.advanced.summary"):expect({ present = true })
local summary_text = tostring((assert(eguidev.widget("basic.advanced.summary"):state())).label)
assert(summary_text ~= nil)
assert(summary_text == "This section is visible when expanded.")

-- Input diagnostics collapsing section
eguidev.widget("basic.input.debug"):set_value(true)
eguidev.widget("basic.input.events"):expect({ present = true })
eguidev.widget("basic.input.modifiers"):expect({ present = true })
eguidev.widget("basic.input.raw_scroll"):expect({ present = true })
eguidev.widget("basic.input.smooth_scroll"):expect({ present = true })

eguidev.widget("basic.input.debug"):set_value(false)

-- Slider value round-trip
eguidev.widget("basic.intensity"):set_value(99.5)
local slider = eguidev.widget("basic.intensity"):state()
assert(slider ~= nil)
-- Slider value is now a plain number
local slider_val = slider.value :: any
assert(slider_val ~= nil, "slider value should exist")
assert(slider_val >= 99 and slider_val <= 100, "slider should be near 99.5")

-- Checkbox round-trip
eguidev.widget("basic.enabled"):set_value(false)
eguidev.widget("basic.enabled"):wait( function(widget)
    return widget ~= nil and widget.value == false
end)
eguidev.widget("basic.enabled"):set_value(true)
eguidev.widget("basic.enabled"):wait( function(widget)
    return widget ~= nil and widget.value == true
end)

return {
    tests = "complete",
}
