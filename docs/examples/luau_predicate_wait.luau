local vp = root()

wait_until(function()
    local current = try_widget("status")
    return current ~= nil and current:state().label == "Ready"
end)

return widget("status"):state()
