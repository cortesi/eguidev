eguidev.fixture("basic.empty")
local vp = eguidev.root

eguidev.widget("basic.name"):focus()
vp:paste("Pasted from smoke")
vp:settle()

eguidev.widget("basic.name"):type_text("Luau", { clear = true })
eguidev.widget("basic.enabled"):set_value(true)
eguidev.widget("basic.enabled"):wait( function(widget)
    return widget ~= nil and widget.value == true
end)
eguidev.widget("basic.intensity"):set_value(73.25)

eguidev.widget("basic.submit"):click()
local status = eguidev.widget("basic.status"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Saved Luau", 1, true) ~= nil
end)
assert(status ~= nil, "submit should update status")

local links = vp:widgets({ role = "link", id_prefix = "basic.link." }) :: any
assert(#links == 2)

local preview = eguidev.widget("basic.preview.image")
local preview_state = preview:state()
assert(preview_state ~= nil and preview_state.role == "image")
local final_status = status :: WidgetState
assert(final_status.value_text ~= "Fixture: empty")

return {
    link_count = #links,
    status = final_status.value_text,
}
