-- A data anchor states readiness the widget set cannot express: the analysis
-- status label is identical before and after the staged pass completes.
eguidev.fixture("basic.default")

local idle = eguidev.widget("status.summary"):state()

assert(idle ~= nil)
assert(idle.label == "Analysis status", "the status label should be static")
local idle_data = idle.data
assert(idle_data ~= nil, "the status should publish app-domain data")
assert(idle_data.analysed == 0, "the idle state should have analysed nothing")
assert(idle_data.complete == false, "an idle pass must not claim completeness")

-- The fixture returns once its data anchor matches a fresh capture, so the
-- staged per-frame load has finished by the time eguidev.fixture() returns.
local result = eguidev.fixture("analysis.loaded", { games = 8 })
assert(result.values.games == 8, "the fixture should return the game count it loaded")

local loaded = eguidev.widget("status.summary"):state()

assert(loaded ~= nil)
assert(loaded.label == idle.label, "only the data should differ between the two states")
local loaded_data = loaded.data
assert(loaded_data ~= nil, "the loaded state should publish data")
assert(loaded_data.analysed == 8, "the pass should have analysed every game")
assert(loaded_data.complete == true, "the pass should report itself complete")
assert(loaded_data.pass_meta.kind == "analysis", "the table data anchor should match structurally")
assert(loaded_data.rgba[1] == 28, "Lua arrays stay 1-based")
eguidev.widget("status.summary"):expect({
    data = { pointer = "/pass_meta", equals = { kind = "analysis" } },
})
eguidev.widget("status.summary"):expect({
    data = { pointer = "/rgba/0", equals = 28 },
})

-- The fixture catalog exposes both scalar and structured data anchors.
local found_pass = false
local found_meta = false
for _, entry in ipairs(eguidev.fixtures()) do
    if entry.name == "analysis.loaded" then
        for _, target in ipairs(entry.ready) do
            local condition = target.condition
            local data = condition.data
            if data ~= nil and data.pointer == "/pass" then
                found_pass = true
                assert(data.equals == "analysis", "the scalar anchor should name the analysis pass")
            elseif data ~= nil and data.pointer == "/pass_meta" then
                found_meta = true
                assert(type(data.equals) == "table", "the metadata anchor should be a table")
                local metadata = data.equals :: { kind: string }
                assert(metadata.kind == "analysis", "the metadata anchor should name the analysis pass")
            end
        end
    end
end
assert(found_pass, "analysis.loaded should declare the /pass data anchor")
assert(found_meta, "analysis.loaded should declare the /pass_meta data anchor")

return { analysed = loaded_data.analysed }
