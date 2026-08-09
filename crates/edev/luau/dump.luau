--!strict

local fixtureName = eguidev.args.__fixture_name
if type(fixtureName) == "string" then
    local params: FixtureParams = {}
    for key, value in eguidev.args do
        if string.sub(key, 1, 10) ~= "__fixture_" and string.sub(key, 1, 7) ~= "__dump_" then
            params[key] = value
        end
    end
    eguidev.fixture(fixtureName, params)
elseif eguidev.args.__dump_wait_capture == true then
    eguidev.root:wait_capture()
end

local options: DumpOptions = {}
local viewportId = eguidev.args.__dump_viewport
if type(viewportId) == "string" then
    options.viewport = eguidev.viewport(viewportId)
end
if eguidev.args.__dump_json == true then
    return eguidev.dump(options)
end
return eguidev.dump_text(options)
