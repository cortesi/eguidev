fixture("basic.default")
local vp = root()
local target = vp:widget_get("basic.visual.sample_target"):state()
local center = {
    x = (target.rect.min.x + target.rect.max.x) / 2,
    y = (target.rect.min.y + target.rect.max.y) / 2,
}
local samples = vp:sample_pixels({ center })

assert(samples[1].hex == "#2f80edff", "sample target color should be exact")
local channel_math = samples[1].rgba[1] + ((target.rect.max.x - target.rect.min.x) / 2)
assert(channel_math > samples[1].rgba[1], "sampled rgba values should support arithmetic")

vp:widget_get("basic.menu.actions"):click()
assert_widget_exists("basic.menu.actions.mark_ready")
local immediate = vp:sample_pixels({ center })
vp:wait_for_settle()
local settled = vp:sample_pixels({ center })
assert(immediate[1].hex == "#2f80edff", "immediate sample should be stable")
assert(settled[1].hex == "#2f80edff", "settled sample should be stable")

local gutter = vp:widget_get("demo.gutter.error_marker"):state()
local gutter_center = {
    x = (gutter.rect.min.x + gutter.rect.max.x) / 2,
    y = (gutter.rect.min.y + gutter.rect.max.y) / 2,
}
local gutter_samples = vp:sample_pixels({ gutter_center })
assert(gutter_samples[1].hex == "#e5484dff", "gutter marker color should be exact")

return { sample = samples[1], gutter = gutter_samples[1] }
