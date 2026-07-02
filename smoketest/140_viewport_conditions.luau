fixture("viewports.default")

local function secondary_viewport(): Viewport?
    for _, viewport in ipairs(viewports()) do
        if viewport.id ~= "root" then
            local ok = pcall(function()
                viewport:widget_get("viewports.scroll")
            end)
            if ok then
                return viewport
            end
        end
    end
    return nil
end

local secondary: Viewport? = nil
secondary = secondary_viewport()
assert(secondary ~= nil, "secondary viewport should be present")
local secondary_vp = secondary :: Viewport

secondary_vp:focus()

-- exact inner size predicate
secondary_vp:set_inner_size({ x = 600, y = 400 })
local res_inner = secondary_vp:wait_for(function(viewport)
    return viewport.inner_size.x == 600 and viewport.inner_size.y == 400
end)
assert(res_inner.inner_size.x == 600 and res_inner.inner_size.y == 400)

-- We skip focused check because we are in no-focus mode.

return {
    tested = true
}
