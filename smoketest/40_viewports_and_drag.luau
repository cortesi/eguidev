eguidev.fixture("basic.default")
local vp = eguidev.root

eguidev.widget("basic.submit"):click()
eguidev.widget("basic.status"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Saved", 1, true) ~= nil
end)

local drag_detail_before = (assert(eguidev.widget("basic.drag.detail"):state())).value_text

assert(drag_detail_before ~= nil)
local drag_widget = eguidev.widget("basic.drag")
local drag_state = drag_widget:state()
assert(drag_state ~= nil)
local drag_target: Pos = {
    x = drag_state.interact_rect.max.x,
    y = drag_state.interact_rect.max.y,
}

eguidev.widget("basic.drag"):drag_to(
    drag_target,
    {
        settle = false,
    }
)
eguidev.widget("basic.drag.detail"):wait( function(widget)
    return widget ~= nil and widget.value_text ~= "" and widget.value_text ~= drag_detail_before
end)

eguidev.widget("basic.scroll"):scroll_to({
    align = "bottom",
    settle = false,
})
eguidev.widget("basic.scroll.row.49"):wait( function(widget)
    return widget ~= nil and widget.visible
end)

return {
    drag_detail = (assert(eguidev.widget("basic.drag.detail"):state())).value_text,
}
