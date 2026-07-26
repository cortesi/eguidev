-- (viewport_id, id) is the one widget identity, so a state a wait returns names
-- its own widget with no second lookup.
fixture("basic.default")

local vp = root()
local submit = vp:widget_get("basic.submit")

local from_state = submit:state()
assert(from_state.id == "basic.submit", "state() should carry the widget id")
assert(from_state.viewport_id == vp.id, "state() should carry the viewport id")

local from_wait = submit:wait_for_visible()
assert(from_wait.id == from_state.id, "a wait should return the same identity")
assert(from_wait.viewport_id == from_state.viewport_id, "a wait should return the same viewport")

local from_predicate = submit:wait_for(function(current)
    return current.id == "basic.submit"
end)
assert(from_predicate.id == "basic.submit", "a predicate wait should return the identity too")

local from_expect = expect("basic.submit", { visible = true })
assert(from_expect.id == "basic.submit", "expect() should return the identity")
assert(from_expect.viewport_id == vp.id, "expect() should return the viewport id")

-- The pair distinguishes the same id across viewports when one exists.
local secondary = viewport({ name = "secondary" })
assert(secondary ~= nil, "the demo keeps a secondary viewport open")
local secondary_row = (secondary :: Viewport):widget_get("viewports.row.0"):state()
assert(secondary_row.viewport_id ~= from_state.viewport_id, "the two rows live in two viewports")

-- Viewport-scoped waits return the same shape.
local from_viewport_wait = (secondary :: Viewport):wait_for_widget_visible("viewports.row.0")
assert(from_viewport_wait.id == "viewports.row.0", "the viewport wait should name its widget")
assert(
    from_viewport_wait.viewport_id == secondary_row.viewport_id,
    "the viewport wait should name its viewport"
)

return {
    root_widget = from_state.viewport_id .. "/" .. from_state.id,
    secondary_widget = secondary_row.viewport_id .. "/" .. secondary_row.id,
}
