-- check_layout must be usable as a build gate: a scrolled viewport-filling list
-- reports nothing, and only positions no ancestor explains report.
local function kinds(issues: { LayoutIssue }): { [string]: number }
    local counts: { [string]: number } = {}
    for _, issue in ipairs(issues) do
        counts[issue.kind] = (counts[issue.kind] or 0) + 1
    end
    return counts
end

local function offscreen_ids(issues: { LayoutIssue }): { string }
    local names: { string } = {}
    for _, issue in ipairs(issues) do
        if issue.kind == "offscreen" then
            table.insert(names, issue.widgets[1])
        end
    end
    table.sort(names)
    return names
end

local function describe(issues: { LayoutIssue }): string
    local parts: { string } = {}
    for _, issue in ipairs(issues) do
        table.insert(parts, issue.kind .. ":" .. table.concat(issue.widgets, "+"))
    end
    return table.concat(parts, ", ")
end

local vp = root()

-- The ordinary playground is clean, so a suite can gate on the whole viewport.
fixture("basic.default")
local playground = vp:check_layout()
assert(#playground == 0, "the default playground should be layout-clean: " .. describe(playground))

-- A scroll area that fills the viewport gives its rows the viewport clip rect.
fixture("layout.gate", { offset = 400 })
local scroll = vp:widget_get("gate.scroll"):state()
local scroll_layout = scroll.layout
assert(scroll_layout ~= nil, "the gate scroll area should carry layout metadata")
local viewport_size = vp:state().inner_size
local clip = (scroll_layout :: WidgetLayout).clip_rect
assert(
    math.abs(clip.max.x - viewport_size.x) <= 1 and math.abs(clip.max.y - viewport_size.y) <= 1,
    "the gate scroll area should fill the viewport"
)

-- Rows scrolled out of the viewport are intentional, not defects.
local rows_outside = 0
for _, row in ipairs(vp:widget_list({ id_prefix = "gate.row.", include_invisible = true })) do
    local rect = row:state().rect
    if rect.min.y < 0 or rect.max.y > viewport_size.y then
        rows_outside += 1
    end
end
assert(rows_outside > 0, "the gate fixture should scroll rows out of the viewport")

local clean = vp:check_layout()
assert(#clean == 0, "a scrolled viewport-filling list should report nothing: " .. describe(clean))

-- A rect published without a response inherits the scroll region.
local marker = vp:widget_get("gate.marker"):state()
assert(marker.layout == nil, "a published rect carries no layout of its own")
assert(#vp:widget_get("gate.marker"):check_layout() == 0, "the marker should be clean")

-- The same scope, checked from one row, still resolves the enclosing scroll area.
local scoped = vp:widget_get("gate.row.0"):check_layout()
assert(#scoped == 0, "a scoped check should see the ancestor scroll area: " .. describe(scoped))

-- Positions beyond the declared extent and off the non-scrollable axis still report.
fixture("layout.gate", { offset = 400, offenders = true })
local dirty = vp:check_layout()
local reported = table.concat(offscreen_ids(dirty), ",")
assert(
    reported == "gate.beyond_extent,gate.off_axis",
    "expected exactly the two offenders, got: " .. describe(dirty)
)

return {
    rows_outside_viewport = rows_outside,
    clean_kinds = kinds(clean),
    reported = reported,
}
