--!strict

local function projectTargets(targets: { FixtureTarget }): { any }
    if #targets == 0 then
        return targets
    end
    local result = {}
    for _, target in targets do
        table.insert(result, {
            widget_id = target.widget.id,
            condition = target.condition,
        })
    end
    return result
end

local result = {}
for _, fixture in eguidev.fixtures() do
    table.insert(result, {
        name = fixture.name,
        description = fixture.description,
        preconditions = projectTargets(fixture.preconditions),
        ready = projectTargets(fixture.ready),
        params = fixture.params,
        tags = fixture.tags,
    })
end
return result
