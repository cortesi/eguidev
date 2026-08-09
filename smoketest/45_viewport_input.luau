-- Raw input must reach immediate viewports: click a row in the secondary viewport.
eguidev.fixture("viewports.default")
local sv = eguidev.wait_viewport({ title = "DevMCP secondary viewport" })
local row = sv:widget("viewports.row.0")
row:wait({ actionable = true })
row:click()
eguidev.widget("basic.status"):wait( function(w)
    return w ~= nil and string.find(w.value_text, "Secondary row 0", 1, true) ~= nil
end)
