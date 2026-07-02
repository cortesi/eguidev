local require_occlusion = args.require_occlusion == true
local force_occluder = args.force_occluder == true

local function occluder_present(): boolean
    for _, viewport in ipairs(viewports()) do
        local ok = pcall(function()
            viewport:widget_get("viewports.occluder.status")
        end)
        if ok then
            return true
        end
    end
    return false
end

local function wait_for_occluder(label: string): boolean
    local ok, error_or_state = pcall(function()
        return root():wait_for(function(_state)
            return occluder_present()
        end)
    end)
    if not ok then
        log(label .. ": " .. tostring(error_or_state))
        return false
    end
    return true
end

local function root_occlusion_matches(expected_occluded: boolean, label: string): boolean
    local ok, state_or_error = pcall(function()
        return root():wait_for(function(state)
            return (state.occluded == true) == expected_occluded
        end)
    end)
    if not ok then
        log(label .. ": " .. tostring(state_or_error))
        return false
    end
    local state = state_or_error :: ViewportState
    return state.occluded == true
end

fixture("basic.default")

local normal = root():state()
assert(normal.minimized ~= true, "root viewport should not start minimized")
if force_occluder then
    assert(
        wait_for_occluder("forced occluder did not appear"),
        "forced occluder should be present"
    )
    if require_occlusion then
        local initial_occluded =
            root_occlusion_matches(true, "root initial occlusion mismatch")
        assert(initial_occluded, "root viewport should start occluded when the occluder is forced")
    end
else
    assert(normal.occluded ~= true, "root viewport should not start occluded")
end

fixture("viewports.occluded")

local cover_present = wait_for_occluder("occluder viewport did not appear")
assert(cover_present, "occluder viewport should be present")

local reported_occlusion = root():state().occluded == true

if require_occlusion then
    reported_occlusion = root_occlusion_matches(true, "root viewport did not report occluded")
    assert(reported_occlusion, "root viewport should report occluded when covered")
elseif not reported_occlusion then
    log("root viewport did not report occluded")
end

fixture("basic.default")
local reset_occluded = root():state().occluded == true
if force_occluder then
    assert(
        wait_for_occluder("forced occluder disappeared after reset"),
        "forced occluder should remain present"
    )
    if require_occlusion then
        reset_occluded = root_occlusion_matches(true, "root reset occlusion mismatch")
        assert(reset_occluded, "root viewport should remain occluded while the occluder is forced")
    end
else
    assert(reset_occluded ~= true, "root viewport should not remain occluded after reset")
end

return {
    occlusion_reported = reported_occlusion,
    force_occluder = force_occluder,
    require_occlusion = require_occlusion,
}
