-- A data anchor states readiness the widget set cannot express: the analysis
-- status label is identical before and after the staged pass completes.
fixture("basic.default")

local idle = widget("status.summary"):state()
assert(idle.label == "Analysis status", "the status label should be static")
local idle_data = idle.data
assert(idle_data ~= nil, "the status should publish app-domain data")
assert(idle_data.analysed == 0, "the idle state should have analysed nothing")
assert(idle_data.complete == false, "an idle pass must not claim completeness")

-- The fixture returns once its data anchor matches a fresh capture, so the
-- staged per-frame load has finished by the time fixture() returns.
local values = fixture("analysis.loaded", { games = 8 })
assert(values.games == 8, "the fixture should return the game count it loaded")

local loaded = widget("status.summary"):state()
assert(loaded.label == idle.label, "only the data should differ between the two states")
local loaded_data = loaded.data
assert(loaded_data ~= nil, "the loaded state should publish data")
assert(loaded_data.analysed == 8, "the pass should have analysed every game")
assert(loaded_data.complete == true, "the pass should report itself complete")

-- The fixture catalog exposes the data anchor in its readiness contract.
local found = false
for _, entry in ipairs(fixtures()) do
    if entry.name == "analysis.loaded" then
        for _, anchor in ipairs(entry.anchors) do
            if typeof(anchor.check) == "table" and (anchor.check :: any).data ~= nil then
                found = true
                assert(
                    (anchor.check :: any).data.pointer == "/pass",
                    "the declared data anchor should point at /pass"
                )
            end
        end
    end
end
assert(found, "analysis.loaded should declare a data anchor")

return { analysed = loaded_data.analysed }
