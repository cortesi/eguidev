-- Every wait accepts WaitOptions and every action accepts its options table.
-- A Rust test proves the declaration says so; this proves the runtime agrees.
eguidev.fixture("basic.default")

local vp = eguidev.root
local WAIT: WaitOptions = { timeout_ms = 4000, poll_interval_ms = 16 }
local ACT: ActionOptions = { settle = true }
local CLICK: ClickOptions = { settle = true }
local DRAG: DragOptions = { settle = true }

-- Widget waits.
local submit = eguidev.widget("basic.submit")
submit:wait({ actionable = true }, WAIT)
submit:wait(function(current)
    return current ~= nil and current.visible
end, WAIT)
eguidev.widget("basic.scroll"):wait({ scroll_ready = true }, WAIT)

-- Open the collapsing section so a real handle exists, then close it again and
-- wait for that handle to disappear.
eguidev.widget("basic.advanced"):click({ settle = true })
local summary = eguidev.widget("basic.advanced.summary")
eguidev.widget("basic.advanced"):click({ settle = true })
summary:wait({ present = false }, WAIT)

-- Viewport waits.
eguidev.widget("basic.submit"):wait({ actionable = true }, WAIT)
eguidev.widget("basic.submit"):wait( function(current)
    return current ~= nil and current.visible
end, WAIT)
eguidev.widget("basic.advanced.summary"):wait({ present = false }, WAIT)
vp:wait(function(state)
    return state ~= nil and state.frame_count > 0
end, WAIT)
vp:settle(WAIT)
vp:wait_capture(WAIT)

-- Global waits.
vp:wait_capture(WAIT)
eguidev.wait_frames(1, WAIT)
eguidev.wait(function()
    return (assert(eguidev.widget("basic.submit"):state())).visible
end, WAIT)

-- Widget actions.
submit:click(CLICK)
submit:hover({ settle = true })
submit:focus(ACT)
eguidev.widget("basic.name"):type_text("Rain", { settle = true, clear = true })
eguidev.widget("basic.intensity"):set_value(50, ACT)
eguidev.widget("basic.drag"):drag_relative({ x = 0.1, y = 0 }, DRAG)
eguidev.widget("basic.drag"):drag_to(eguidev.widget("basic.submit"), DRAG)
eguidev.widget("basic.scroll"):scroll({ x = 0, y = -20 }, ACT)
eguidev.widget("basic.scroll"):scroll_to({ align = "top", settle = true })
eguidev.widget("basic.scroll.row.20"):scroll_into_view(ACT)

-- Viewport actions.
vp:key("escape", { settle = true })
vp:paste("x", ACT)
vp:dismiss_popups(ACT)

-- Options really are honoured: an unreachable wait must respect its timeout.
local started = os.clock()
local ok = pcall(function()
    eguidev.widget("no.such.widget"):wait({ actionable = true }, { timeout_ms = 200, poll_interval_ms = 16 })
end)
local elapsed_ms = (os.clock() - started) * 1000
assert(not ok, "waiting for a missing widget must fail")
assert(elapsed_ms < 3000, "the wait should honour timeout_ms, took " .. tostring(elapsed_ms))

return { elapsed_ms = elapsed_ms }
