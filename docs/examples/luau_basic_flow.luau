fixture("basic.empty")
widget("login.username"):type_text("alice", { clear = true })
widget("login.password"):type_text("secret", { clear = true })
widget("login.submit"):click()

local status = expect("login.status", { value_text_contains = "Welcome" })

return status
