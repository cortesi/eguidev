local available = eguidev.fixtures()
assert(#available >= 1, "demo should advertise fixtures")

local saw_basic_default = false
for _, fixture_info in ipairs(available) do
    if fixture_info.name == "basic.default" then
        saw_basic_default = true
        assert(#fixture_info.ready >= 1, "basic.default should advertise readiness targets")
    end
end
assert(saw_basic_default, "basic.default fixture should be present")

eguidev.fixture("basic.default")
local vp = eguidev.root

local submit = eguidev.widget("basic.submit")
local submit_state = submit:wait({ visible = true, enabled = true })
assert(submit_state ~= nil)
assert(submit_state.role == "button")

local ancestor_count = 1
local current = submit:parent()
while current ~= nil do
    ancestor_count += 1
    current = current:parent()
end
assert(ancestor_count >= 1, "ancestor walk should include the submit button")

local hit_point: Pos = {
    x = submit_state.interact_rect.min.x + 2,
    y = submit_state.interact_rect.min.y + 2,
}
local hit = vp:widgets_at(hit_point) :: any
assert(hit[1] ~= nil, "widget_at_point should find the submit area")

local measure = submit:text_measure() :: any
assert(measure.actual_size.x > 0, "text_measure should report width")

return {
    fixtures = #available,
    measured = measure.visible_text,
    tree_size = ancestor_count,
}
