eguidev.fixture("basic.empty")
eguidev.widget("basic.name"):type_text("alice", { clear = true })
eguidev.widget("basic.password"):type_text("secret", { clear = true })
eguidev.widget("basic.submit"):click()

local status = eguidev.widget("basic.status"):expect({ value_text_contains = "Saved" })

return status
