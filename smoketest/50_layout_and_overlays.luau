eguidev.fixture("basic.default")
local vp = eguidev.root
local submit = eguidev.widget("basic.submit")

local layout = vp:layout_issues()
assert(type(layout) == "table", "layout_issues() should return an issues array")

local submit_layout = submit:layout_issues()
assert(#submit_layout == 0, "submit subtree should be layout-clean in the default fixture")

local highlighted = submit:show_highlight("#00ff00")
assert(highlighted.rect.max.x > highlighted.rect.min.x, "show_highlight should return a rect")
submit:clear_highlight()

vp:show_debug_overlay({ mode = "bounds", show_labels = true })
vp:clear_debug_overlay()

return {
    issues = #layout,
    submit_issues = #submit_layout,
}
