fixture("basic.default")
local vp = root()

vp:widget_get("basic.link.docs"):hover()

vp:widget_get("basic.menu.actions"):click()
widget("basic.menu.actions.mark_ready"):click()
expect("basic.status", { value_text_contains = "Menu ready clicked" })

vp:widget_get("basic.advanced"):set_value(true)
widget("basic.advanced.action"):click()
expect("basic.status", { value_text_contains = "Advanced action clicked." })

vp:widget_get("basic.scroll"):scroll_into_view()
vp:widget_get("basic.scroll"):scroll({ x = 0, y = -120 })
vp:widget_get("basic.scroll"):scroll_to({ align = "bottom" })
expect("basic.scroll.row.49", { visible = true })
vp:wait_for_settle()

return vp:widget_get("basic.status"):state()
