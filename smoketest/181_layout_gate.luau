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

local vp = eguidev.root

-- The ordinary playground is clean, so a suite can gate on the whole viewport.
eguidev.fixture("basic.default")
local playground = vp:layout_issues()
assert(#playground == 0, "the default playground should be layout-clean: " .. describe(playground))

-- A scroll area that fills the viewport gives its rows the viewport clip rect.
eguidev.fixture("layout.gate", { offset = 400 })
local scroll = eguidev.widget("gate.scroll"):state()
assert(scroll ~= nil)
local scroll_layout = scroll.layout
assert(scroll_layout ~= nil, "the gate scroll area should carry layout metadata")
local viewport_state = vp:state()
assert(viewport_state ~= nil)
local viewport_size = viewport_state.inner_size
assert(viewport_size ~= nil)
local clip = (scroll_layout :: WidgetLayout).clip_rect
assert(
    math.abs(clip.max.x - viewport_size.x) <= 1 and math.abs(clip.max.y - viewport_size.y) <= 1,
    "the gate scroll area should fill the viewport"
)

-- Rows scrolled out of the viewport are intentional, not defects.
local rows_outside = 0
for _, row in ipairs(vp:widgets({ id_prefix = "gate.row." })) do
    local row_state = row:state()
    assert(row_state ~= nil)
    local rect = row_state.rect
    assert(rect ~= nil)
    if rect.min.y < 0 or rect.max.y > viewport_size.y then
        rows_outside += 1
    end
end
assert(rows_outside > 0, "the gate fixture should scroll rows out of the viewport")

local clean = vp:layout_issues()
assert(#clean == 0, "a scrolled viewport-filling list should report nothing: " .. describe(clean))

-- A rect published without a response inherits the scroll region.
local marker = eguidev.widget("gate.marker"):state()
assert(marker ~= nil)
assert(marker.layout == nil, "a published rect carries no layout of its own")
assert(#eguidev.widget("gate.marker"):layout_issues() == 0, "the marker should be clean")

-- The same scope, checked from one row, still resolves the enclosing scroll area.
local scoped = eguidev.widget("gate.row.0"):layout_issues()
assert(#scoped == 0, "a scoped check should see the ancestor scroll area: " .. describe(scoped))

-- Positions beyond the declared extent and off the non-scrollable axis still report.
eguidev.fixture("layout.gate", { offset = 400, offenders = true })
local dirty = vp:layout_issues()
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
