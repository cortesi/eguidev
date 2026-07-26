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

fixture("basic.default")
local vp = root()

-- Park the pointer far from the drag region. The whole jump back to the region
-- must not read as part of the drag.
vp:raw_pointer_move({ x = 700, y = 860 })
vp:wait_for_settle()

local drag = vp:widget_get("basic.drag")
local status = vp:widget_get("basic.status")
local before = center(drag:state().rect)
local target = center(status:state().rect)

drag:drag_to(status, { settle = true })
vp:wait_for_settle()

local after = center(drag:state().rect)
approx(after.x - before.x, target.x - before.x, "drag_to should move the region by the exact x delta")
approx(after.y - before.y, target.y - before.y, "drag_to should move the region by the exact y delta")

-- drag_relative takes positions inside the widget, not deltas, and applies the
-- distance between them exactly once.
fixture("basic.default")
vp:raw_pointer_move({ x = 20, y = 20 })
vp:wait_for_settle()

local region = vp:widget_get("basic.drag")
local origin = region:state().rect
local width = origin.max.x - origin.min.x
-- Centre to three quarters across, at the same height.
region:drag_relative({ x = 0.75, y = 0.5 }, { x = 0.5, y = 0.5 }, { settle = true })
vp:wait_for_settle()

local moved = region:state().rect
approx(moved.min.x - origin.min.x, width * 0.25, "drag_relative should move by one quarter of the width")
approx(moved.min.y - origin.min.y, 0, "drag_relative should not move on the untouched axis")

return {
    drag_to_dx = after.x - before.x,
    drag_to_dy = after.y - before.y,
    relative_dx = moved.min.x - origin.min.x,
}
