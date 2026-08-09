-- A drag must move the widget by exactly the requested distance, wherever the
-- pointer happened to be beforehand.
local function center(rect: Rect): Pos
    return { x = (rect.min.x + rect.max.x) / 2, y = (rect.min.y + rect.max.y) / 2 }
end

local function approx(actual: number, expected: number, label: string)
    assert(
        math.abs(actual - expected) <= 1.0,
        string.format("%s (got %.2f, expected %.2f)", label, actual, expected)
    )
end

eguidev.fixture("basic.default")
local vp = eguidev.root

-- Park the pointer far from the drag region. The whole jump back to the region
-- must not read as part of the drag.
vp:input({ type = "pointer_move", position = { x = 700, y = 860 } })
vp:settle()

local drag = eguidev.widget("basic.drag")
local status = eguidev.widget("basic.status")
local drag_state = drag:state()
local status_state = status:state()
assert(drag_state ~= nil and drag_state.rect ~= nil)
assert(status_state ~= nil and status_state.rect ~= nil)
local before = center(drag_state.rect)
local target = center(status_state.rect)

drag:drag_to(status, { settle = true })
vp:settle()

local after_state = drag:state()
assert(after_state ~= nil and after_state.rect ~= nil)
local after = center(after_state.rect)
approx(after.x - before.x, target.x - before.x, "drag_to should move the region by the exact x delta")
approx(after.y - before.y, target.y - before.y, "drag_to should move the region by the exact y delta")

-- drag_relative takes positions inside the widget, not deltas, and applies the
-- distance between them exactly once.
eguidev.fixture("basic.default")
vp:input({ type = "pointer_move", position = { x = 20, y = 20 } })
vp:settle()

local region = eguidev.widget("basic.drag")
local origin_state = region:state()
assert(origin_state ~= nil and origin_state.rect ~= nil)
local origin = origin_state.rect
local width = origin.max.x - origin.min.x
-- Centre to three quarters across, at the same height.
region:drag_relative({ x = 0.75, y = 0.5 }, { from = { x = 0.5, y = 0.5 }, settle = true })
vp:settle()

local moved_state = region:state()
assert(moved_state ~= nil and moved_state.rect ~= nil)
local moved = moved_state.rect
approx(moved.min.x - origin.min.x, width * 0.25, "drag_relative should move by one quarter of the width")
approx(moved.min.y - origin.min.y, 0, "drag_relative should not move on the untouched axis")

return {
    drag_to_dx = after.x - before.x,
    drag_to_dy = after.y - before.y,
    relative_dx = moved.min.x - origin.min.x,
}
