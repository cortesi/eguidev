eguidev.fixture("basic.default")
local vp = eguidev.root

eguidev.widget("basic.input.debug"):set_value(true)
eguidev.widget("basic.input.pointer"):expect({ present = true })

local submit = eguidev.widget("basic.submit")
local submit_state = submit:state()
assert(submit_state ~= nil)
local center: Pos = {
    x = (submit_state.interact_rect.min.x + submit_state.interact_rect.max.x) / 2,
    y = (submit_state.interact_rect.min.y + submit_state.interact_rect.max.y) / 2,
}

vp:input({ type = "pointer_move", position = center })
eguidev.widget("basic.input.pointer"):wait( function(widget)
    return widget ~= nil and widget.value_text ~= "" and string.find(widget.value_text, "none", 1, true) == nil
end)

vp:input({ type = "pointer_button", position = center, button = "primary", action = "press" })
vp:input({ type = "pointer_button", position = center, button = "primary", action = "release" })
eguidev.widget("basic.status"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Saved Sky", 1, true) ~= nil
end)

eguidev.widget("basic.name"):focus()
eguidev.widget("basic.name"):type_text("", { clear = true })
vp:input({ type = "text", text = "++" })
eguidev.widget("basic.name"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "++", 1, true) ~= nil
end)

vp:key("enter", { target = eguidev.widget("basic.name") })
eguidev.widget("basic.input.key_event"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Enter", 1, true) ~= nil
end)

vp:input({ type = "key", key = "escape", action = "press" })
vp:input({ type = "key", key = "escape", action = "release" })
eguidev.widget("basic.input.key_event"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Escape", 1, true) ~= nil
end)

local state = vp:state()

assert(state ~= nil)
assert(state.pixels_per_point > 0, "viewport state should expose pixels per point")

vp:input({ type = "scroll", delta = { x = 0, y = -24 } })
vp:settle()
eguidev.widget("basic.input.scroll_events"):wait( function(widget)
    return widget ~= nil and widget.value_text ~= "Scroll events: 0"
end)

local final_status = eguidev.widget("basic.status"):state()

assert(final_status ~= nil)

return {
    pixels_per_point = state.pixels_per_point,
    status = final_status.value_text,
}
