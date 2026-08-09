eguidev.fixture("basic.default")
local vp = eguidev.root

eguidev.widget("basic.menu.actions"):click()
eguidev.widget("basic.menu.actions.mark_ready"):expect({ present = true })

eguidev.fixture("basic.default")
eguidev.widget("basic.menu.actions.mark_ready"):wait({ present = false })
eguidev.widget("basic.menu.actions.reset_status"):wait({ present = false })

eguidev.widget("basic.menu.actions"):click()
eguidev.widget("basic.menu.actions.mark_ready"):expect({ present = true })

return eguidev.widget("basic.menu.actions.mark_ready"):state()
