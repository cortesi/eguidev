eguidev.fixture("basic.empty")
eguidev.widget("login.username"):type_text("alice", { clear = true })
eguidev.widget("login.password"):type_text("secret", { clear = true })
eguidev.widget("login.submit"):click()

local status = eguidev.widget("login.status"):expect( { value_text_contains = "Welcome" })

return status
