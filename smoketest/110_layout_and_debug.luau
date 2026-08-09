eguidev.fixture("basic.default")
local vp = eguidev.root

local full_check = vp:layout_issues()
assert(type(full_check) == "table", "check_layout should return the inspected issues array")

local submit = eguidev.widget("basic.submit")
local scoped = submit:layout_issues()
assert(#scoped == 0, "submit subtree should be layout-clean in the default fixture")

-- overlay_debug with all modes
local modes = { "bounds", "margins", "clipping", "overlaps", "focus", "layers", "containers" }
for _, mode in ipairs(modes) do
    vp:show_debug_overlay({ mode = mode :: any })
end
vp:clear_debug_overlay()

-- show_debug_overlay with options
vp:show_debug_overlay({
    mode = "bounds",
    show_labels = true,
    show_sizes = true,
    label_font_size = 12.0,
    bounds_color = "#ff0000",
})
vp:clear_debug_overlay()

-- widget show_highlight / clear_highlight
local highlighted = submit:show_highlight("#ff00ff")
assert(highlighted.rect.max.x > highlighted.rect.min.x, "show_highlight should return a rect")
submit:clear_highlight()

-- viewport show_highlight with explicit rect / clear_highlights
local submit_state = submit:state()
assert(submit_state ~= nil)
local highlight_rect = vp:show_highlight(submit_state.interact_rect, "#00ffff")
assert(highlight_rect ~= nil)
assert(
    highlight_rect.rect.max.x > highlight_rect.rect.min.x,
    "viewport show_highlight should return a rect"
)
vp:clear_highlights()

return {
    issues = #full_check,
    scoped_issues = #scoped,
    modes_tested = #modes,
}
