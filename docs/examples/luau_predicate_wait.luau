local vp = eguidev.root

eguidev.wait(function()
    local current = eguidev.widget("status"):state()
    return current ~= nil and current.label == "Ready"
end)

return eguidev.widget("status"):state()
