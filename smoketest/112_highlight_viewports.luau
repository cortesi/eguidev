eguidev.fixture("viewports.scrolled")
local root = eguidev.root
local secondary = eguidev.wait_viewport({ name = "secondary" })
local rect = { min = { x = 20, y = 80 }, max = { x = 80, y = 120 } }
local sample = { x = 21, y = 100 }
local root_before = root:sample_pixels({ sample })[1].hex
local secondary_before = secondary:sample_pixels({ sample })[1].hex

local ok, message = pcall(function()
    root:show_highlight(rect, "#ff0000")
    secondary:show_highlight(rect, "#0000ff")
    assert(root:sample_pixels({ sample })[1].hex == "#ff0000ff", "root highlight was replaced")
    assert(secondary:sample_pixels({ sample })[1].hex == "#0000ffff", "secondary highlight was replaced")

    secondary:clear_highlights()
    assert(root:sample_pixels({ sample })[1].hex == "#ff0000ff", "secondary clear removed root highlight")
    assert(secondary:sample_pixels({ sample })[1].hex == secondary_before, "secondary highlight remains")

    secondary:show_highlight(rect, "#0000ff")
    root:clear_highlights()
    assert(root:sample_pixels({ sample })[1].hex == root_before, "root highlight remains")
    assert(secondary:sample_pixels({ sample })[1].hex == "#0000ffff", "root clear removed secondary highlight")
end)

root:clear_highlights()
secondary:clear_highlights()
eguidev.fixture("basic.default")
assert(ok, tostring(message))
return true
