-- Raw input must reach immediate viewports: click a row in the secondary viewport.
eguidev.fixture("viewports.default")
local secondary = eguidev.viewports({ title = "DevMCP secondary viewport" })[1]
assert(secondary ~= nil, "secondary viewport present")
local sv = secondary :: Viewport
local row = sv:widgets({ id_prefix = "viewports.row.0" })[1]
assert(row ~= nil and row.id == "viewports.row.0", "secondary row should exist")
row:wait({ actionable = true })
row:click()
eguidev.widget("basic.status"):wait( function(w)
    return w ~= nil and string.find(w.value_text, "Secondary row 0", 1, true) ~= nil
end)
