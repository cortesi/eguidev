eguidev.fixture("viewports.default")

local secondary_vp = eguidev.wait_viewport({ title = "DevMCP secondary viewport" })

local original = secondary_vp:state()
assert(original ~= nil)
local original_size = original.inner_size

secondary_vp:focus()

-- exact inner size predicate
secondary_vp:resize({ inner_size = { x = 600, y = 400 } })
local res_inner = secondary_vp:wait(function(viewport)
    return viewport ~= nil and viewport.inner_size.x == 600 and viewport.inner_size.y == 400
end)
assert(res_inner ~= nil)
assert(res_inner.inner_size.x == 600 and res_inner.inner_size.y == 400)

-- We skip focused check because we are in no-focus mode.

-- Restore the geometry this script found. A resize outlives the script that
-- makes it, so a later smoketest would otherwise inherit a window size that no
-- fixture resets and that the app never asked for.
secondary_vp:resize({ inner_size = original_size })
local restored = secondary_vp:wait(function(viewport)
    return viewport ~= nil
        and viewport.inner_size.x == original_size.x
        and viewport.inner_size.y == original_size.y
end)
assert(restored ~= nil)

return {
    tested = true
}
