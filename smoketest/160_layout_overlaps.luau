-- check_overlaps must report unrelated siblings that intersect, and stay quiet
-- about containment, separate layers, and scrolled content.
local function overlap_pairs(issues: { LayoutIssue }): { string }
    local found: { string } = {}
    for _, issue in ipairs(issues) do
        if issue.kind == "overlap" then
            table.insert(found, table.concat(issue.widgets, "+"))
        end
    end
    table.sort(found)
    return found
end

local vp = root()

-- The playground stacks a floating window over the panel and nests a painter
-- board inside its canvas. Neither is a defect.
fixture("basic.default")
local playground = overlap_pairs(vp:check_layout())
assert(
    #playground == 0,
    "the default playground should report no overlaps, got " .. table.concat(playground, ", ")
)

-- Two published siblings that intersect on screen are a defect and must report.
fixture("layout.gate", { offset = 0, offenders = true })
local reported = overlap_pairs(vp:check_layout())
assert(
    table.concat(reported, ",") == "gate.overlap.a+gate.overlap.b",
    "expected exactly the deliberate overlapping pair, got " .. table.concat(reported, ", ")
)

local first = vp:widget_get("gate.overlap.a"):state()
local second = vp:widget_get("gate.overlap.b"):state()
assert(first.rect.max.x > second.rect.min.x, "the offender rects should really intersect")

-- The same pair reports from a scope that only holds the scroll subtree.
local scoped = overlap_pairs(vp:widget_get("gate.scroll"):check_layout())
assert(
    table.concat(scoped, ",") == "gate.overlap.a+gate.overlap.b",
    "a scoped check should report the same pair, got " .. table.concat(scoped, ", ")
)

return { reported = reported }
