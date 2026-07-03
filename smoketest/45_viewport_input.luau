-- Raw input must reach immediate viewports: click a row in the secondary viewport.
fixture("viewports.default")
local secondary: Viewport? = nil
for _, vp in ipairs(viewports()) do
    local st = vp:state()
    if st.title ~= nil and string.find(st.title :: string, "secondary", 1, true) ~= nil then
        secondary = vp
    end
end
assert(secondary ~= nil, "secondary viewport present")
local sv = secondary :: Viewport
sv:wait_for_widget_visible("viewports.row.0")
sv:widget_get("viewports.row.0"):click()
root():wait_for_widget("basic.status", function(w)
    return string.find(w.value_text, "Secondary row 0", 1, true) ~= nil
end)
