-- Every wait accepts WaitOptions and every action accepts its options table.
-- A Rust test proves the declaration says so; this proves the runtime agrees.
fixture("basic.default")

local vp = root()
local WAIT: WaitOptions = { timeout_ms = 4000, poll_interval_ms = 16 }
local ACT: ActionOptions = { settle = true }

-- Widget waits.
local submit = vp:widget_get("basic.submit")
submit:wait_for_visible(WAIT)
submit:wait_for(function(current)
    return current.visible
end, WAIT)
vp:widget_get("basic.scroll"):wait_for_scroll_ready(WAIT)

-- Open the collapsing section so a real handle exists, then close it again and
-- wait for that handle to disappear.
vp:widget_get("basic.advanced"):click({ settle = true })
local summary = vp:widget_get("basic.advanced.summary")
vp:widget_get("basic.advanced"):click({ settle = true })
summary:wait_for_absent(WAIT)

-- Viewport waits.
vp:wait_for_widget_visible("basic.submit", WAIT)
vp:wait_for_widget("basic.submit", function(current)
    return current.visible
end, WAIT)
vp:wait_for_widget_absent("basic.advanced.summary", WAIT)
vp:wait_for(function(state)
    return state.frame_count > 0
end, WAIT)
vp:wait_for_settle(WAIT)
vp:wait_for_capture(WAIT)

-- Global waits.
wait_for_capture(WAIT)
wait_for_frames(1, WAIT)
wait_until(function()
    return vp:widget_get("basic.submit"):state().visible
end, WAIT)

-- Widget actions.
submit:click(ACT)
submit:hover({ settle = true })
submit:focus(ACT)
vp:widget_get("basic.name"):type_text("Rain", { settle = true, clear = true })
vp:widget_get("basic.intensity"):set_value(50, ACT)
vp:widget_get("basic.drag"):drag_relative({ x = 0.1, y = 0 }, nil, ACT)
vp:widget_get("basic.drag"):drag_to(vp:widget_get("basic.submit"), ACT)
vp:widget_get("basic.scroll"):scroll({ x = 0, y = -20 }, ACT)
vp:widget_get("basic.scroll"):scroll_to({ align = "top", settle = true })
vp:widget_get("basic.scroll.row.20"):scroll_into_view(ACT)

-- Viewport actions.
vp:key("escape", { settle = true })
vp:paste("x", ACT)
vp:dismiss_popups(ACT)

-- Options really are honoured: an unreachable wait must respect its timeout.
local started = os.clock()
local ok = pcall(function()
    vp:wait_for_widget_visible("no.such.widget", { timeout_ms = 200, poll_interval_ms = 16 })
end)
local elapsed_ms = (os.clock() - started) * 1000
assert(not ok, "waiting for a missing widget must fail")
assert(elapsed_ms < 3000, "the wait should honour timeout_ms, took " .. tostring(elapsed_ms))

return { elapsed_ms = elapsed_ms }
