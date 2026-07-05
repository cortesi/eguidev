fixture("basic.default")

local before = capture()
local submit = widget("basic.submit")
assert(submit:state().label == "Submit", "widget() should find the submit button")
assert(try_widget("basic.missing") == nil, "try_widget() should return nil for missing widgets")
expect("basic.submit", { role = "button", label = "Submit", visible = true, enabled = true })
expect_absent("basic.missing")
expect_above("basic.submit", "basic.menu.actions")
expect_no_overlap("basic.submit", "basic.menu.actions")
expect_within("basic.submit", "basic.panel")
expect_text_fits("basic.submit")

widget("basic.menu.actions"):click()
expect("basic.menu.actions.mark_ready", { visible = true })
local menu_diff = before:diff({ id_prefix = "basic.menu.actions." })
assert(#menu_diff.changes > 0, "capture diff should report menu changes")

widget("basic.menu.actions.mark_ready"):click()
demo.expect_status_contains("Menu ready clicked")
assert(
    string.find(demo.status_label(), "Menu ready clicked", 1, true) ~= nil,
    "app prelude helper should read status text"
)

fixture("viewports.default")
local secondary = viewport({ name = "secondary" })
assert(secondary ~= nil, "secondary viewport should exist")
local secondary_vp = secondary :: Viewport
local secondary_scroll = widget("viewports.scroll")
assert(
    secondary_scroll.viewport_id == secondary_vp.id,
    "unqualified widget() should find the secondary viewport scroll area"
)
expect("viewports.scroll", { viewport = "secondary", visible = true })
expect("viewports.row.0", { viewport = "secondary", visible = true })

return {
    submit = submit.id,
    menu_changes = #menu_diff.changes,
    status = demo.status_label(),
    secondary = secondary_scroll.viewport_id,
}
