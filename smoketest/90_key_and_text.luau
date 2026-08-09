eguidev.fixture("basic.default")
local vp = eguidev.root

-- Enable input diagnostics to observe key events and modifiers
eguidev.widget("basic.input.debug"):set_value(true)
eguidev.widget("basic.input.key_event"):expect({ present = true })

-- key with modifiers via combo string
vp:key("ctrl-A", { target = eguidev.widget("basic.name") })
eguidev.widget("basic.input.key_event"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "A", 1, true) ~= nil
end)

-- key with shift modifier
vp:key("shift-B", { target = eguidev.widget("basic.name") })
eguidev.widget("basic.input.key_event"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "B", 1, true) ~= nil
end)

-- key with alt modifier
vp:key("alt-C", { target = eguidev.widget("basic.name") })
eguidev.widget("basic.input.key_event"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "C", 1, true) ~= nil
end)

-- key with repeat_count
eguidev.widget("basic.name"):type_text("", { clear = true })
vp:key("arrowright", { target = eguidev.widget("basic.name"), repeat_count = 3 })
eguidev.widget("basic.input.key_event"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "ArrowRight", 1, true) ~= nil
end)

-- type_text with clear
eguidev.widget("basic.name"):type_text("Fresh", { clear = true })
eguidev.widget("basic.name"):wait( function(widget)
    return widget ~= nil and widget.value == "Fresh"
end)

-- type_text with enter (should trigger Enter key event)
eguidev.widget("basic.name"):type_text("WithEnter", { clear = true, enter = true })
eguidev.widget("basic.input.key_event"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Enter", 1, true) ~= nil
end)

-- Viewport:input key press and release with modifiers
vp:input({ type = "key", key = "tab", action = "press", modifiers = { shift = true } })
vp:input({ type = "key", key = "tab", action = "release", modifiers = { shift = true } })
eguidev.widget("basic.input.key_event"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Tab", 1, true) ~= nil
end)

-- Viewport:input inserts raw text
eguidev.widget("basic.name"):focus()
eguidev.widget("basic.name"):type_text("", { clear = true })
vp:input({ type = "text", text = "raw" })
eguidev.widget("basic.name"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "raw", 1, true) ~= nil
end)

-- Viewport:input with secondary button
local submit = eguidev.widget("basic.submit")
local submit_state = submit:state()
assert(submit_state ~= nil)
local center: Pos = {
    x = (submit_state.interact_rect.min.x + submit_state.interact_rect.max.x) / 2,
    y = (submit_state.interact_rect.min.y + submit_state.interact_rect.max.y) / 2,
}
vp:input({ type = "pointer_button", position = center, button = "secondary", action = "press" })
vp:input({ type = "pointer_button", position = center, button = "secondary", action = "release" })

-- Viewport:input with middle button
vp:input({ type = "pointer_button", position = center, button = "middle", action = "press" })
vp:input({ type = "pointer_button", position = center, button = "middle", action = "release" })

-- Viewport:input scroll with modifiers
eguidev.widget("basic.input.debug"):set_value(true)
vp:input({ type = "scroll", delta = { x = 0, y = -10 }, modifiers = { ctrl = true } })
eguidev.widget("basic.input.scroll_events"):wait( function(widget)
    return widget ~= nil and widget.value_text ~= "Scroll events: 0"
end)

-- viewport state reports expected fields
local state = vp:state()
assert(state ~= nil)
assert(state.pixels_per_point > 0, "should report pixels_per_point")

return {
    input_state_ppp = state.pixels_per_point,
}
