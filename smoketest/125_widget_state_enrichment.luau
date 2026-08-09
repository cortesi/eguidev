eguidev.fixture("basic.default")
local vp = eguidev.root

local choice = eguidev.widget("basic.choice"):state()

assert(choice ~= nil)
assert(choice.role == "combo_box")
assert(choice.options ~= nil, "combo_box options should be present")
assert(#(choice.options :: { string }) == 3, "combo_box options should expose all choices")
assert((choice.options :: { string })[2] == "Beta", "combo_box options should keep order")
eguidev.widget("basic.choice"):set_value(2)
eguidev.widget("basic.choice"):wait( function(widget)
    return widget ~= nil and widget.value == 2
end)

local drag_float = eguidev.widget("basic.drag.float"):state()

assert(drag_float ~= nil)
assert(drag_float.role == "drag_value")
assert(drag_float.range ~= nil, "ranged drag value should expose its range")
assert((drag_float.range :: Range).min == -10)
assert((drag_float.range :: Range).max == 10)

local drag_int = eguidev.widget("basic.drag.int"):state()

assert(drag_int ~= nil)
assert(drag_int.range ~= nil, "integer drag value should expose its range")
assert((drag_int.range :: Range).min == -5)
assert((drag_int.range :: Range).max == 20)

local toolbar = eguidev.widget("basic.toolbar.sync"):state()

assert(toolbar ~= nil)
assert(toolbar.role == "button")
assert(toolbar.selected == true, "selected-aware button should expose selected state")
eguidev.widget("basic.toolbar.sync"):click()
eguidev.widget("basic.toolbar.sync"):wait( function(widget)
    return widget ~= nil and widget.selected == false
end)

local toggle = eguidev.widget("basic.toggle"):state()

assert(toggle ~= nil)
assert(toggle.role == "toggle")
assert(toggle.value == false)
eguidev.widget("basic.toggle"):set_value(true)
eguidev.widget("basic.toggle"):wait( function(widget)
    return widget ~= nil and widget.value == true
end)

local mixed = eguidev.widget("basic.mixed"):state()

assert(mixed ~= nil)
assert(mixed.role == "checkbox")
assert(mixed.indeterminate == true, "indeterminate checkbox should expose third-state flag")

local name = eguidev.widget("basic.name"):state()

assert(name ~= nil)
assert(name.multiline == false)
assert(name.password == false)

local notes = eguidev.widget("basic.notes"):state()

assert(notes ~= nil)
assert(notes.multiline == true)
assert(notes.password == false)

local password = eguidev.widget("basic.password"):state()

assert(password ~= nil)
assert(password.role == "text_edit")
assert(password.multiline == false)
assert(password.password == true)

eguidev.widget("basic.mode.beta"):set_value(true)
eguidev.widget("basic.mode.beta"):wait( function(widget)
    return widget ~= nil and widget.value == true
end)
eguidev.widget("basic.select.2"):set_value(true)
eguidev.widget("basic.select.2"):wait( function(widget)
    return widget ~= nil and widget.value == true
end)
eguidev.widget("basic.select.1"):click()
local select_one = eguidev.widget("basic.select.1"):state()
assert(select_one ~= nil)
local select_two = eguidev.widget("basic.select.2"):state()
assert(select_two ~= nil)
assert(select_one.value == true)
assert(select_two.value == false)

local progress = eguidev.widget("basic.progress.percent"):state()

assert(progress ~= nil)
assert(progress.role == "progress_bar")
assert(progress.label == "42%")

local detail_progress = eguidev.widget("basic.progress.detail"):state()

assert(detail_progress ~= nil)
assert(detail_progress.role == "progress_bar")
assert(detail_progress.label == "Drag 1.5")

local color = eguidev.widget("basic.accent"):state()

assert(color ~= nil)
assert(color.role == "color_picker")
assert(color.value == "#409CFFFF")
assert(color.value_text == "#409CFFFF")

eguidev.widget("basic.accent"):set_value("#112233FF")
eguidev.widget("basic.accent"):wait( function(widget)
    return widget ~= nil and widget.value == "#112233FF"
end)
eguidev.widget("basic.accent.value"):wait( function(widget)
    return widget ~= nil and widget.label == "Accent: #112233FF"
end)

local floating_window = eguidev.widget("basic.window.surface"):state()

assert(floating_window ~= nil)
assert(floating_window.role == "window")

local ok = pcall(function()
    eguidev.widget("basic.intensity"):set_value(200.5)
end)
assert(not ok, "out-of-range slider write should fail")

ok = pcall(function()
    eguidev.widget("basic.choice"):set_value(9)
end)
assert(not ok, "out-of-range combo-box write should fail")

ok = pcall(function()
    eguidev.widget("basic.drag.int"):set_value(99)
end)
assert(not ok, "out-of-range drag-value write should fail")

ok = pcall(function()
    eguidev.widget("basic.accent"):set_value("#1234")
end)
assert(not ok, "invalid color text should fail")

local final_toolbar = eguidev.widget("basic.toolbar.sync"):state()
local final_accent = eguidev.widget("basic.accent"):state()
assert(final_toolbar ~= nil and final_accent ~= nil)
return {
    toolbar_selected = final_toolbar.selected,
    accent = final_accent.value,
}
