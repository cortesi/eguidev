-- Two combo boxes of one label keep an identity of their own, and
-- `layout_issues` reports a widget that shares the egui ID of another.
--
-- `egui::ComboBox::from_label` salts the ID of a box with the text of its
-- label, and the ID of a `Ui` covers every widget of a row or a panel. Two
-- boxes of one label would therefore take one ID, and with it one open list:
-- a click would open the list of one box and close the list of the other in
-- the same pass, and neither list would appear. A box takes the ID of the
-- widget as its salt, which is unique, so the two stay apart.
eguidev.fixture("basic.default")
local vp = eguidev.root

local first = eguidev.widget("basic.choice"):wait({ visible = true })
local second = eguidev.widget("basic.choice.second"):wait({ visible = true })
assert(first ~= nil and second ~= nil, "the window holds both boxes")
assert(first.label == second.label, "the two boxes carry one label")

-- No box shares the egui ID of another.
local shared = {}
for _, issue in vp:layout_issues() do
    if issue.kind == "shared_id" then
        table.insert(shared, table.concat(issue.widgets, " + "))
    end
end
assert(#shared == 0, "no widget shares an egui ID: " .. table.concat(shared, ", "))

-- Each box keeps a choice of its own.
eguidev.widget("basic.choice"):set_value(2)
local moved = eguidev.widget("basic.choice"):wait(function(state)
    local value: any = state and state.value
    return value == 2
end)
assert(moved ~= nil, "the first box takes a new choice")
local held = eguidev.widget("basic.choice.second"):state()
assert(held ~= nil and held.value == second.value, "the second box keeps its own choice")

-- A click opens the list of the box that was clicked. The rows of the list
-- are egui's own widgets, so the script clicks one by position: the list
-- draws under the box, and a click on its first row chooses "Alpha", which
-- is the choice at index 0.
eguidev.widget("basic.choice.second"):set_value(2)
eguidev.widget("basic.choice.second"):wait(function(state)
    local value: any = state and state.value
    return value == 2
end)
eguidev.widget("basic.choice.second"):click()
eguidev.wait_frames(2)
local box = eguidev.widget("basic.choice.second"):state()
assert(box ~= nil, "the box is still there with its list open")
local x = box.rect.min.x + 12
local y = box.rect.max.y + 14
vp:input({ type = "pointer_move", position = { x = x, y = y } })
vp:input({ type = "pointer_button", position = { x = x, y = y }, button = "primary", action = "press" })
vp:input({ type = "pointer_button", position = { x = x, y = y }, button = "primary", action = "release" })
local chosen = eguidev.widget("basic.choice.second"):wait(function(state)
    local value: any = state and state.value
    return value == 0
end)
assert(chosen ~= nil, "a click on the list of the second box chooses its first row")

return {
    label = first.label,
    first_value = moved.value,
    second_value = chosen.value,
}
