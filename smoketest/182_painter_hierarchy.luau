-- publish_rect_container makes painter-drawn rects nest, so containment reads as
-- structure instead of as one overlap per child.
eguidev.fixture("basic.default")

local vp = eguidev.root
local canvas = eguidev.widget("board.canvas")
local squares = vp:widgets({ id_prefix = "board.square." })
assert(#squares == 9, "the demo board should publish nine squares, got " .. tostring(#squares))

local ids: { string } = {}
for _, square in ipairs(squares) do
    local parent = square:parent()
    assert(parent ~= nil, square.id .. " should have a parent")
    assert(
        (parent :: Widget).id == "board.canvas",
        square.id .. " should nest under board.canvas, got " .. (parent :: Widget).id
    )
    table.insert(ids, square.id)
end
local descendants = {}
for _, id in ids do table.insert(descendants, eguidev.widget(id)) end
canvas:expect({ descendants = descendants })

-- Every square sits inside the canvas, and none of that reports as an overlap.
for _, id in ipairs(ids) do
    eguidev.widget(id):expect({ within = canvas })
end
local issues = canvas:layout_issues()
assert(#issues == 0, "the painter hierarchy should report no layout issues")

-- children() sees the published squares from the canvas handle.
assert(#canvas:children() == 9, "the canvas should report nine children")

return { squares = #ids }
