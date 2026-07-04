fixture("basic.overlay_reset_probe")
local vp = root()

vp:widget_get("overlay.fixture_probe.input"):type_text("sticky", { clear = true })
vp:wait_for_widget("overlay.fixture_probe.input", function(widget)
    return widget.value == "sticky"
end)

fixture("basic.overlay_reset_probe")
vp:wait_for_widget("overlay.fixture_probe.input", function(widget)
    return widget.value == ""
end)

local widget_image = vp:widget_get("basic.preview.image"):screenshot()
assert(widget_image.type == "image_ref")

local viewport_image = vp:screenshot()
assert(viewport_image.type == "image_ref")

local secondary = viewport({ title = "DevMCP secondary viewport" })
assert(secondary ~= nil, "secondary viewport should exist")

local secondary_image = secondary:screenshot()
assert(secondary_image.type == "image_ref")

local drag = secondary:widget_get("viewports.drag"):state()
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
