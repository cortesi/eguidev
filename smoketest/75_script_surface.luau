eguidev.fixture("basic.default")

local before = eguidev.capture()
local submit = eguidev.widget("basic.submit")
local submit_state = submit:state()
assert(submit_state ~= nil and submit_state.label == "Submit", "eguidev.widget() should find the submit button")
local missing = eguidev.widget("basic.missing")
assert(missing:state() == nil, "state() should return nil for missing widgets")
missing:wait({ present = false })
local menu = eguidev.widget("basic.menu.actions")
submit:expect({
    role = "button",
    label = "Submit",
    visible = true,
    enabled = true,
    above = { widget = menu },
    no_overlap = menu,
    within = eguidev.widget("basic.panel"),
    text_fits = true,
})

eguidev.widget("basic.menu.actions"):click()
eguidev.widget("basic.menu.actions.mark_ready"):expect( { visible = true })
local menu_diff = before:diff({ id_prefix = "basic.menu.actions." })
assert(#menu_diff.changes > 0, "capture diff should report menu changes")

eguidev.widget("basic.menu.actions.mark_ready"):click()
local status = eguidev.widget("basic.status")
local status_state = status:expect({ value_text_contains = "Menu ready clicked" })
assert(status_state ~= nil)
assert(
    string.find(status_state.value_text, "Menu ready clicked", 1, true) ~= nil,
    "the status should record the menu action"
)

eguidev.fixture("viewports.default")
local secondary = eguidev.viewports({ name = "secondary" })[1]
assert(secondary ~= nil, "secondary viewport should exist")
local secondary_vp = secondary :: Viewport
local secondary_scroll = secondary_vp:widgets({ id_prefix = "viewports.scroll" })[1]
assert(secondary_scroll ~= nil and secondary_scroll.id == "viewports.scroll")
local secondary_scroll_state = secondary_scroll:state()
assert(secondary_scroll_state ~= nil)
assert(
    secondary_scroll_state.viewport_id == secondary_vp.id,
    "the scoped widget should carry the secondary viewport identity"
)
secondary_scroll:expect({ visible = true })
local secondary_row = secondary_vp:widgets({ id_prefix = "viewports.row.0" })[1]
assert(secondary_row ~= nil and secondary_row.id == "viewports.row.0")
secondary_row:expect({ visible = true })

return {
    submit = submit.id,
    menu_changes = #menu_diff.changes,
    status = status_state.value_text,
    secondary = secondary_scroll_state.viewport_id,
}
