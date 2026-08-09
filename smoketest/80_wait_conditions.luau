eguidev.fixture("basic.default")
local vp = eguidev.root

-- Widget:wait with a presence predicate
local exists_result = eguidev.widget("basic.submit"):wait( function(widget)
    return true
end)
assert(exists_result ~= nil)

-- Widget:wait with a visibility predicate
local vis_result = eguidev.widget("basic.submit"):wait( function(widget)
    return widget ~= nil and widget.visible
end)
assert(vis_result ~= nil and vis_result.visible)

-- explicit visible waits
local widget_visible = eguidev.widget("basic.submit"):wait({ actionable = true })
assert(widget_visible ~= nil)
assert(widget_visible.visible)
local handle_visible = eguidev.widget("basic.submit"):wait({ actionable = true })
assert(handle_visible ~= nil)
assert(handle_visible.visible)

-- Widget:wait with an enabled predicate
local enabled_result = eguidev.widget("basic.submit"):wait( function(widget)
    return widget ~= nil and widget.enabled
end)
assert(enabled_result ~= nil and enabled_result.enabled)

-- Widget:wait with a text contains predicate on the status widget
local contains_result = eguidev.widget("basic.status"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Waiting", 1, true) ~= nil
end)
assert(contains_result ~= nil)

-- Widget:wait with an exact widget value predicate
local current_name = (assert(eguidev.widget("basic.name"):state())).value
assert(current_name ~= nil)
local equals_result = eguidev.widget("basic.name"):wait( function(widget)
    return widget ~= nil and widget.value == current_name
end)
assert(equals_result ~= nil)

-- Widget:wait with a label contains predicate
local label_result = eguidev.widget("basic.submit"):wait( function(widget)
    return widget ~= nil and widget.label ~= nil and string.find(widget.label :: string, "Submit", 1, true) ~= nil
end)
assert(label_result ~= nil)

-- Widget:wait with a label equals predicate
local exact_label = eguidev.widget("basic.submit"):wait( function(widget)
    return widget ~= nil and widget.label == "Submit"
end)
assert(exact_label ~= nil)

-- Widget:wait with predicate function
local pred_result = eguidev.widget("basic.status"):wait( function(widget)
    return widget ~= nil and string.find(widget.value_text, "Waiting", 1, true) ~= nil
end)
assert(pred_result ~= nil)

-- Widget:wait with a focus predicate after focusing
eguidev.widget("basic.name"):focus()
local focus_result = eguidev.widget("basic.name"):wait( function(widget)
    return widget ~= nil and widget.focused
end)
assert(focus_result ~= nil and focus_result.focused)

-- wait_for_settle
local settle_report = vp:settle()
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
eguidev.fixture("viewports.default")
local secondary = eguidev.wait_viewport({ name = "secondary" })
local override_ok, override_err = pcall(function()
    eguidev.widget("viewports.unwired.value"):set_value(5)
end)
assert(not override_ok, "unwired custom widget should reject unconsumed set_value override")
assert(
    string.find(tostring(override_err), "take_widget_value_override", 1, true) ~= nil,
    "unconsumed override error should name the missing consumer"
)
eguidev.fixture("basic.default")

-- Widget:expect
eguidev.widget("basic.submit"):expect( { visible = true, enabled = true })

-- assert_widget_value on the name field
eguidev.widget("basic.name"):expect( { value = "Sky" })

-- timeout diagnostics are catchable through pcall as surfaced error text
eguidev.configure({ timeout_ms = 10, poll_interval_ms = 1 })
local ok, err = pcall(function()
    return eguidev.widget("basic.missing.timeout"):wait( function(_widget)
        return true
    end)
end)
assert(not ok)
assert(type(err) == "table", "wait errors should be structured tables")
local waitError = err :: any
assert(waitError.code == "timeout", "wait errors should carry the stable timeout code")
assert(
    string.find(waitError.message, "Timed out waiting for widget predicate", 1, true) ~= nil,
    waitError.message
)

return {
    conditions_tested = 16,
}
