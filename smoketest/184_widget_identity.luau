-- (viewport_id, id) is the one widget identity, so a state a wait returns names
-- its own widget with no second lookup.
eguidev.fixture("basic.default")

local vp = eguidev.root
local submit = eguidev.widget("basic.submit")

local from_state = submit:state()

assert(from_state ~= nil)
assert(from_state.id == "basic.submit", "state() should carry the widget id")
assert(from_state.viewport_id == vp.id, "state() should carry the viewport id")

local from_wait = submit:wait({ actionable = true })
assert(from_wait ~= nil)
assert(from_wait.id == from_state.id, "a wait should return the same identity")
assert(from_wait.viewport_id == from_state.viewport_id, "a wait should return the same viewport")

local from_predicate = submit:wait(function(current)
    return current ~= nil and current.id == "basic.submit"
end)
assert(from_predicate ~= nil)
assert(from_predicate.id == "basic.submit", "a predicate wait should return the identity too")

local from_expect = eguidev.widget("basic.submit"):expect( { visible = true })
assert(from_expect ~= nil)
assert(from_expect.id == "basic.submit", "expect() should return the identity")
assert(from_expect.viewport_id == vp.id, "expect() should return the viewport id")

-- The pair distinguishes the same id across viewports when one exists.
local secondary = eguidev.viewports({ name = "secondary" })[1]
assert(secondary ~= nil, "the demo keeps a secondary viewport open")
local secondary_row = eguidev.widget("viewports.row.0"):state()
assert(secondary_row ~= nil)
assert(secondary_row.viewport_id ~= from_state.viewport_id, "the two rows live in two viewports")

-- Viewport-scoped waits return the same shape.
local scoped_row = (secondary :: Viewport):widgets({ id_prefix = "viewports.row.0" })[1]
assert(scoped_row ~= nil and scoped_row.id == "viewports.row.0")
local from_viewport_wait = scoped_row:wait({ actionable = true })
assert(from_viewport_wait ~= nil)
assert(from_viewport_wait.id == "viewports.row.0", "the viewport wait should name its widget")
assert(
    from_viewport_wait.viewport_id == secondary_row.viewport_id,
    "the viewport wait should name its viewport"
)

return {
    root_widget = from_state.viewport_id .. "/" .. from_state.id,
    secondary_widget = secondary_row.viewport_id .. "/" .. secondary_row.id,
}
