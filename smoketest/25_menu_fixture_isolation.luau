fixture("basic.default")
local vp = root()

vp:widget_get("basic.menu.actions"):click()
assert_widget_exists("basic.menu.actions.mark_ready")

fixture("basic.default")
vp:wait_for_widget_absent("basic.menu.actions.mark_ready")
vp:wait_for_widget_absent("basic.menu.actions.reset_status")

vp:widget_get("basic.menu.actions"):click()
assert_widget_exists("basic.menu.actions.mark_ready")

return vp:widget_get("basic.menu.actions.mark_ready"):state()
