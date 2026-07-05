fixture("basic.default")

local function find_widget(widgets: { WidgetDump }, id: string): any
    for _, widget in ipairs(widgets) do
        if widget.id == id then
            return widget
        end
        local child = find_widget(widget.children, id)
        if child ~= nil then
            return child
        end
    end
    return nil
end

local root_options: DumpOptions = { viewport = "root", root = "basic.panel", fields = "full" }
local root_tree = dump(root_options) :: any
assert(#root_tree.viewports == 1, "root viewport dump should be present")
local root_dump = root_tree.viewports[1]
assert(root_dump.id == "root", "root dump should use the root viewport id")
assert(root_dump.frame_count > 0, "root dump should include a per-viewport frame count")
assert(#root_dump.widgets == 1, "root subtree dump should include exactly the requested root")
assert(root_dump.widgets[1].id == "basic.panel", "subtree root should be basic.panel")

local sample = find_widget(root_dump.widgets, "basic.visual.sample_target")
assert(sample ~= nil, "subtree dump should include descendants")
local sample_dump = sample :: WidgetDump
assert(sample_dump.role == "image", "sample target should expose its role")
assert(sample_dump.data ~= nil, "sample target should expose structured data")
assert(sample_dump.data.kind == "sample_target", "structured widget data should be preserved")
assert(sample_dump.data.color == "#2f80ed", "structured widget data should keep values")

local scroll = find_widget(root_dump.widgets, "basic.scroll")
assert(scroll ~= nil, "full subtree dump should include the scroll area")
assert((scroll :: WidgetDump).scroll_state ~= nil, "full dump should include scroll metadata")

local visible_only_options: DumpOptions = {
    viewport = "root",
    root = "basic.root_scroll",
    include_invisible = false,
}
local visible_only_tree = dump(visible_only_options) :: any
assert(find_widget(visible_only_tree.viewports[1].widgets, "basic.scroll.row.49") == nil,
    "include_invisible=false should filter invisible widgets")
local all_widgets_options: DumpOptions = { viewport = "root", root = "basic.root_scroll" }
local all_widgets_tree = dump(all_widgets_options) :: any
assert(find_widget(all_widgets_tree.viewports[1].widgets, "basic.scroll.row.49") ~= nil,
    "include_invisible should default to true for debugging dumps")

local text_options: DumpOptions = { viewport = "root", root = "basic.panel", fields = "full" }
local text = dump_text(text_options)
assert(string.find(text, 'viewport root "Egui DevMCP Demo"', 1, true) ~= nil,
    "text dump should include the viewport header")
assert(string.find(text, "  basic.panel unknown", 1, true) ~= nil,
    "text dump should include the subtree root")
assert(string.find(text, "basic.visual.sample_target image", 1, true) ~= nil,
    "text dump should include descendant widgets")
assert(string.find(text, 'data={"color":"#2f80ed","kind":"sample_target"}', 1, true) ~= nil,
    "text dump should include compact structured data")
assert(string.find(text, "app.tabs", 1, true) == nil,
    "subtree text dump should exclude widgets outside the requested root")

fixture("viewports.default")
local secondary_options: DumpOptions = { viewport = "secondary" }
local secondary_tree = dump(secondary_options) :: any
assert(#secondary_tree.viewports == 1, "semantic viewport selector should restrict dump")
local secondary_dump = secondary_tree.viewports[1]
assert(secondary_dump.name == "secondary", "secondary dump should include semantic name")
assert(secondary_dump.frame_count > 0, "secondary dump should include per-viewport frame count")
assert(find_widget(secondary_dump.widgets, "viewports.scroll") ~= nil,
    "secondary dump should include widgets in that viewport")

return {
    root_widgets = #root_dump.widgets,
    secondary_name = secondary_dump.name,
    text_len = string.len(text),
}
