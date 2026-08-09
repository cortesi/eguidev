-- One predicate governs pointer admission, the actionable wait, and the poll that
-- scroll_into_view runs, so a wait that passes cannot be followed by a click
-- that fails for the same reason.
eguidev.fixture("basic.default")

local vp = eguidev.root

-- The row starts scrolled out of the inner list.
local row = eguidev.widget("basic.scroll.row.40")
local initial = row:state()
assert(initial ~= nil)
assert(not initial.visible, "row 40 should start out of view")

-- The plan's promise: scroll then interact, with no manual wait between them.
row:scroll_into_view()
row:click()

local ready = row:state()

assert(ready ~= nil)
assert(ready.visible, "scroll_into_view should leave the row visible")
local layout = ready.layout
assert(layout ~= nil, "a rendered row should carry layout metadata")
assert(
    (layout :: WidgetLayout).visible_fraction > 0,
    "scroll_into_view should leave a non-zero visible fraction"
)

-- The shared actionable condition holds, so the same row passes the wait.
local waited = row:wait({ actionable = true })
assert(waited ~= nil)
assert(waited.id == "basic.scroll.row.40", "the wait should return the row it waited on")

-- A fully clipped widget fails the wait, and the error names the remedy.
eguidev.fixture("basic.default")
local clipped = eguidev.widget("basic.scroll.row.40")
local ok, err = pcall(function()
    clipped:wait({ actionable = true }, { timeout_ms = 250, poll_interval_ms = 16 })
end)
assert(not ok, "a fully clipped row must not satisfy the actionable condition")
local message = tostring(err)
assert(
    string.find(message, "interaction-ready", 1, true) ~= nil,
    "the timeout should name the interaction-ready condition: " .. message
)
assert(
    string.find(message, "scroll_into_view", 1, true) ~= nil,
    "the timeout should name the remedy: " .. message
)

-- The pointer-admission error names the same remedy.
local click_ok, click_err = pcall(function()
    eguidev.widget("basic.scroll.row.40"):click({ settle = false })
end)
assert(not click_ok, "a fully clipped row must not accept a click")
assert(
    string.find(tostring(click_err), "wait({ actionable = true })", 1, true) ~= nil,
    "the invisible-interaction error should name the actionable wait: " .. tostring(click_err)
)

return { row = ready.viewport_id .. "/" .. ready.id }
