local function find_fixture(name: string): Fixture
    for _, fixture_info in ipairs(eguidev.fixtures()) do
        if fixture_info.name == name then
            return fixture_info :: Fixture
        end
    end
    error("missing fixture " .. name)
end

local function scroll_offset(viewport: Viewport, widget_id: string): number
    local state = eguidev.widget(widget_id):state()
    assert(state ~= nil)
    local scroll = state.scroll_state
    assert(scroll ~= nil, widget_id .. " should expose scroll_state")
    return (scroll :: ScrollAreaMeta).offset.y
end

local basic_scrolled = find_fixture("basic.scrolled")
assert(basic_scrolled.params ~= nil, "basic.scrolled should advertise params")
local basic_params = basic_scrolled.params :: { FixtureParam }
assert(#basic_params == 1, "basic.scrolled should expose one param")
assert(basic_params[1].name == "offset")
assert(basic_params[1].kind == "float")
assert(basic_params[1].default == 300)
assert(basic_scrolled.tags ~= nil, "basic.scrolled should advertise tags")
assert((basic_scrolled.tags :: { string })[1] == "basic")

local default_result = eguidev.fixture("basic.scrolled")
assert(default_result.values.offset == 300, "eguidev.fixture() should return defaulted values")
eguidev.widget("basic.scroll"):wait( function(widget)
    if widget == nil then return false end
    local scroll = widget.scroll_state
    return scroll ~= nil and math.abs(scroll.offset.y - 300) <= 0.75
end)

local explicit_result = eguidev.fixture("basic.scrolled", { offset = 180 })
assert(explicit_result.values.offset == 180, "eguidev.fixture() should return explicit values")
eguidev.widget("basic.scroll"):wait( function(widget)
    if widget == nil then return false end
    local scroll = widget.scroll_state
    return scroll ~= nil and math.abs(scroll.offset.y - 180) <= 0.75
end)

local type_ok, type_error = pcall(function()
    eguidev.fixture("basic.scrolled", { offset = "bad" })
end)
assert(not type_ok, "fixture param type errors should be catchable")
assert(
    string.find(tostring(type_error), "expected float", 1, true) ~= nil,
    "type error should mention the expected kind"
)

local unknown_ok = pcall(function()
    eguidev.fixture("basic.scrolled", { missing = 1 })
end)
assert(not unknown_ok, "unknown fixture params should be rejected")

local below_range_ok = pcall(function()
    eguidev.fixture("basic.scrolled", { offset = -1 })
end)
assert(not below_range_ok, "below-range fixture params should be rejected")

local above_range_ok = pcall(function()
    eguidev.fixture("basic.scrolled", { offset = 9999 })
end)
assert(not above_range_ok, "above-range fixture params should be rejected")

local secondary_result = eguidev.fixture("viewports.scrolled", { offset = 160 })
assert(secondary_result.values.offset == 160)
local secondary = eguidev.viewports({ name = "secondary" })[1]
assert(secondary ~= nil, "secondary viewport should exist")
local secondary_vp = secondary :: Viewport
eguidev.widget("viewports.scroll"):wait( function(widget)
    if widget == nil then return false end
    local scroll = widget.scroll_state
    return scroll ~= nil and math.abs(scroll.offset.y - 160) <= 32
end)

return {
    basic_offset = scroll_offset(eguidev.root, "basic.scroll"),
    secondary_offset = scroll_offset(secondary_vp, "viewports.scroll"),
}
