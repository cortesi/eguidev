--!strict

local function projectTargets(targets: { FixtureTarget }): { any }
    local result: { any } = {}
    for _, target in targets do
        local widget = target.widget :: any
        local projected: any = {
            widget_id = widget.id,
            condition = target.condition,
        }
        if widget.__viewport_id ~= nil then
            projected.viewport_id = widget.__viewport_id
        end
        table.insert(result, projected)
    end
    return result
end

local result = {}
for _, fixture in eguidev.fixtures() do
    local projected: any = {
        name = fixture.name,
        description = fixture.description,
    }
    if #fixture.preconditions > 0 then
        projected.preconditions = projectTargets(fixture.preconditions)
    end
    if #fixture.params > 0 then
        projected.params = fixture.params
    end
    if #fixture.ready > 0 then
        projected.ready = projectTargets(fixture.ready)
    end
    if #fixture.tags > 0 then
        projected.tags = fixture.tags
    end
    table.insert(result, projected)
end
return result
