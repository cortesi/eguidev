-- publish_rect_container makes painter-drawn rects nest, so containment reads as
-- structure instead of as one overlap per child.
fixture("basic.default")

local vp = root()
local canvas = vp:widget_get("board.canvas")
local squares = vp:widget_list({ id_prefix = "board.square." })
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
expect_tree("board.canvas", ids)

-- Every square sits inside the canvas, and none of that reports as an overlap.
for _, id in ipairs(ids) do
    expect_within(id, "board.canvas")
end
local issues = canvas:check_layout()
assert(#issues == 0, "the painter hierarchy should report no layout issues")

-- children() sees the published squares from the canvas handle.
assert(#canvas:children() == 9, "the canvas should report nine children")

return { squares = #ids }
