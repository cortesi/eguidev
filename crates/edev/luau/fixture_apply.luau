--!strict

local name = eguidev.args.__fixture_name
if type(name) ~= "string" then
    error("missing fixture name")
end
local shouldWait = eguidev.args.__fixture_wait ~= false
local params: FixtureParams = {}
for key, value in eguidev.args do
    if string.sub(key, 1, 10) ~= "__fixture_" then
        params[key] = value
    end
end
return eguidev.fixture(name, params, { wait = shouldWait })
