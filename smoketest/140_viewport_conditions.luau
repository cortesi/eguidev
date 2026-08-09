eguidev.fixture("viewports.default")

local secondary_vp = eguidev.wait_viewport({ title = "DevMCP secondary viewport" })

secondary_vp:focus()

-- exact inner size predicate
secondary_vp:resize({ inner_size = { x = 600, y = 400 } })
local res_inner = secondary_vp:wait(function(viewport)
    return viewport ~= nil and viewport.inner_size.x == 600 and viewport.inner_size.y == 400
end)
assert(res_inner ~= nil)
assert(res_inner.inner_size.x == 600 and res_inner.inner_size.y == 400)

-- We skip focused check because we are in no-focus mode.

return {
    tested = true
}
