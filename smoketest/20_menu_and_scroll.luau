eguidev.fixture("basic.default")
local vp = eguidev.root

eguidev.widget("basic.link.docs"):hover()

eguidev.widget("basic.menu.actions"):click()
eguidev.widget("basic.menu.actions.mark_ready"):click()
eguidev.widget("basic.status"):expect( { value_text_contains = "Menu ready clicked" })

eguidev.widget("basic.advanced"):set_value(true)
eguidev.widget("basic.advanced.action"):click()
eguidev.widget("basic.status"):expect( { value_text_contains = "Advanced action clicked." })

eguidev.widget("basic.scroll"):scroll_into_view()
eguidev.widget("basic.scroll"):scroll({ x = 0, y = -120 })
eguidev.widget("basic.scroll"):scroll_to({ align = "bottom" })
eguidev.widget("basic.scroll.row.49"):expect( { visible = true })
vp:settle()

return eguidev.widget("basic.status"):state()
