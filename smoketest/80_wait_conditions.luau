fixture("basic.default")
local vp = root()

-- wait_for_widget with a presence predicate
local exists_result = vp:wait_for_widget("basic.submit", function(widget)
    return true
end)
assert(exists_result ~= nil)

-- wait_for_widget with a visibility predicate
local vis_result = vp:wait_for_widget("basic.submit", function(widget)
    return widget.visible
end)
assert(vis_result ~= nil and vis_result.visible)

-- explicit visible waits
local widget_visible = vp:wait_for_widget_visible("basic.submit")
assert(widget_visible.visible)
local handle_visible = vp:widget_get("basic.submit"):wait_for_visible()
assert(handle_visible.visible)

-- wait_for_widget with an enabled predicate
local enabled_result = vp:wait_for_widget("basic.submit", function(widget)
    return widget.enabled
end)
assert(enabled_result ~= nil and enabled_result.enabled)

-- wait_for_widget with a text contains predicate on the status widget
local contains_result = vp:wait_for_widget("basic.status", function(widget)
    return string.find(widget.value_text, "Waiting", 1, true) ~= nil
end)
assert(contains_result ~= nil)

-- wait_for_widget with an exact widget value predicate
local current_name = vp:widget_get("basic.name"):state().value
local equals_result = vp:wait_for_widget("basic.name", function(widget)
    return widget.value == current_name
end)
assert(equals_result ~= nil)

-- wait_for_widget with a label contains predicate
local label_result = vp:wait_for_widget("basic.submit", function(widget)
    return widget.label ~= nil and string.find(widget.label :: string, "Submit", 1, true) ~= nil
end)
assert(label_result ~= nil)

-- wait_for_widget with a label equals predicate
local exact_label = vp:wait_for_widget("basic.submit", function(widget)
    return widget.label == "Submit"
end)
assert(exact_label ~= nil)

-- wait_for_widget with predicate function
local pred_result = vp:wait_for_widget("basic.status", function(widget)
    return string.find(widget.value_text, "Waiting", 1, true) ~= nil
end)
assert(pred_result ~= nil)

-- wait_for_widget with a focus predicate after focusing
vp:widget_get("basic.name"):focus()
local focus_result = vp:wait_for_widget("basic.name", function(widget)
    return widget.focused
end)
assert(focus_result ~= nil and focus_result.focused)

-- wait_for_settle
local settle_report = vp:wait_for_settle()
assert(settle_report.settled, "wait_for_settle should return a settled report")
assert(#settle_report.phases >= 5, "settle report should include phase status")
local saw_app_idle = false
for _, phase in ipairs(settle_report.phases) do
    if phase.phase == "app_idle" then
        saw_app_idle = true
        assert(phase.complete, "demo app idle provider should report idle")
    end
end
assert(saw_app_idle, "demo should expose app_idle settle phase")

-- unconsumed custom widget overrides fail with a typed, actionable error
fixture("viewports.default")
local secondary = viewport({ name = "secondary" })
assert(secondary ~= nil, "secondary viewport should exist")
local override_ok, override_err = pcall(function()
    (secondary :: Viewport):widget_get("viewports.unwired.value"):set_value(5)
end)
assert(not override_ok, "unwired custom widget should reject unconsumed set_value override")
assert(
    string.find(tostring(override_err), "take_widget_value_override", 1, true) ~= nil,
    "unconsumed override error should name the missing consumer"
)
fixture("basic.default")

-- assert_widget_exists
expect("basic.submit", { visible = true, enabled = true })

-- assert_widget_value on the name field
expect("basic.name", { value = "Sky" })

-- timeout diagnostics are catchable through pcall as surfaced error text
configure({ timeout_ms = 10, poll_interval_ms = 1 })
local ok, err = pcall(function()
    return vp:wait_for_widget("basic.missing.timeout", function(_widget)
        return true
    end)
end)
assert(not ok)
assert(string.find(tostring(err), "Timed out waiting for widget predicate", 1, true) ~= nil)
assert(
    string.find(tostring(err), "No target viewport frames were observed", 1, true) ~= nil
        or string.find(tostring(err), "Diagnosis: Target viewport produced frames", 1, true) ~= nil
)

return {
    conditions_tested = 16,
}
