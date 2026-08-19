eguidev.fixture("basic.default")

eguidev.wait(function()
    local current = eguidev.widget("basic.status"):state()
    return current ~= nil
end)

return eguidev.widget("basic.status"):state()
