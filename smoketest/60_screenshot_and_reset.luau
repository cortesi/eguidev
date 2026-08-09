eguidev.fixture("basic.overlay_reset_probe")
local vp = eguidev.root

eguidev.widget("overlay.fixture_probe.input"):type_text("sticky", { clear = true })
eguidev.widget("overlay.fixture_probe.input"):wait( function(widget)
    return widget ~= nil and widget.value == "sticky"
end)

eguidev.fixture("basic.overlay_reset_probe")
eguidev.widget("overlay.fixture_probe.input"):wait( function(widget)
    return widget ~= nil and widget.value == ""
end)

local widget_image = eguidev.widget("basic.preview.image"):screenshot()
assert(widget_image.type == "image_ref")

local viewport_image = vp:screenshot()
assert(viewport_image.type == "image_ref")

local secondary = eguidev.wait_viewport({ title = "DevMCP secondary viewport" })

local secondary_image = secondary:screenshot()
assert(secondary_image.type == "image_ref")

local drag = eguidev.widget("viewports.drag"):state()

assert(drag ~= nil)
local drag_sample_point = {
    x = drag.rect.min.x + 20,
    y = drag.rect.min.y + 20,
}
local secondary_samples = secondary:sample_pixels({ drag_sample_point })
assert(#secondary_samples == 1, "secondary sample should return one pixel")
assert(
    secondary_samples[1].hex == "#208060ff",
    "secondary drag sample should match the fixed painter color"
)

return {
    secondary_image = secondary_image,
    secondary_sample = secondary_samples[1],
    viewport_image = viewport_image,
    widget_image = widget_image,
}
