--!strict

-- This trusted library is private. Rust supplies the seven capability tables
-- as ordered inputs before the VM applies the tenant sandbox.

type QueryKernel = {
    widget_find: (id: string, options: any?) -> any?,
    widget_state: (widget: any) -> any?,
    widget_parent: (widget: any) -> any?,
    widget_children: (widget: any) -> { any },
    widget_viewport: (widget: any) -> any?,
    viewport_list: () -> { any },
    viewport_state: (viewport: any) -> any?,
    viewport_widget_list: (viewport: any, options: any?) -> { any },
    viewport_widget_at_point: (viewport: any, position: Pos, options: any?) -> { any },
    dump: (options: any?) -> any,
    dump_text: (options: any?) -> string,
    capture: () -> any,
}

type ActionKernel = {
    widget_click: (widget: any, options: any?) -> (),
    widget_hover: (widget: any, options: any?) -> (),
    widget_focus: (widget: any, options: any?) -> (),
    widget_type_text: (widget: any, text: string, options: any?) -> (),
    widget_set_value: (widget: any, value: WidgetValue, options: any?) -> (),
    widget_drag_position: (widget: any, position: Pos, options: any?) -> (),
    widget_drag_relative: (widget: any, target: Vec, options: any?) -> (),
    widget_drag_to: (widget: any, target: any, options: any?) -> (),
    widget_scroll: (widget: any, delta: Vec, options: any?) -> (),
    widget_scroll_to: (widget: any, options: any) -> (),
    widget_scroll_into_view: (widget: any, options: any?) -> (),
    viewport_dismiss_popups: (viewport: any, options: any?) -> (),
    viewport_key: (viewport: any, key: string, options: any?) -> (),
    viewport_paste: (viewport: any, text: string, options: any?) -> (),
    viewport_focus: (viewport: any, options: any?) -> (),
    viewport_resize: (viewport: any, options: ResizeOptions) -> (),
    viewport_input: (viewport: any, event: RawInputEvent) -> (),
    widget_show_highlight: (widget: any, color: string) -> any,
    widget_clear_highlight: (widget: any) -> (),
    widget_show_debug_overlay: (widget: any, mode: any?, options: any?) -> (),
    widget_clear_debug_overlay: (widget: any) -> (),
    viewport_show_highlight: (viewport: any, rect: Rect, color: string) -> any,
    viewport_clear_highlights: (viewport: any) -> (),
    viewport_show_debug_overlay: (viewport: any, mode: any?, options: any?) -> (),
    viewport_clear_debug_overlay: (viewport: any) -> (),
}

type WaitKernel = {
    capture: (options: any?) -> any,
    frames: (count: number, options: any?) -> number,
    widget_wait: (
        widget: any,
        predicate: (state: any?) -> boolean,
        options: any?,
        condition: string
    ) -> any?,
    widget_wait_absent: (widget: any, options: any?) -> (),
    viewport_wait: (viewport: any, predicate: (state: any?) -> boolean, options: any?) -> any?,
    viewport_settle: (viewport: any, options: any?) -> any,
}

type CaptureKernel = {
    diff: (capture: any, options: any?) -> any,
    widget_text_measure: (widget: any) -> any,
    widget_layout_issues: (widget: any) -> { any },
    viewport_layout_issues: (viewport: any) -> { any },
    widget_screenshot: (widget: any) -> any,
    viewport_screenshot: (viewport: any) -> any,
    widget_sample_pixels: (widget: any, positions: { Pos }) -> { any },
    widget_sample_grid: (widget: any, nx: number, ny: number) -> { any },
    viewport_sample_pixels: (viewport: any, positions: { Pos }) -> { any },
}

type FixtureKernel = {
    list: () -> { any },
    apply: (name: string, params: any?) -> any,
}

type DiagnosticKernel = {
    get: (name: string) -> any,
    all: () -> any,
    egui: (viewport: any) -> any,
    clear_egui: (viewport: any) -> (),
}

type RecordKernel = {
    args: () -> ScriptArgs,
    array: (values: { any }) -> { any },
    configure: (options: ScriptOptions) -> (),
    log: (value: any) -> (),
    assertion: (passed: boolean, message: string) -> (),
}

local queryValue, actionValue, waitValue, captureValue, fixtureValue, diagnosticValue, recordValue = ...
local query = queryValue :: QueryKernel
local action = actionValue :: ActionKernel
local waitKernel = waitValue :: WaitKernel
local captureKernel = captureValue :: CaptureKernel
local fixtureKernel = fixtureValue :: FixtureKernel
local diagnosticKernel = diagnosticValue :: DiagnosticKernel
local record = recordValue :: RecordKernel

local defaults = {
    timeout_ms = 5000,
    poll_interval_ms = 16,
    settle = true,
    animations = false,
}

local errorMetatable = table.freeze({
    __tostring = function(value: any): string
        return `{value.code}: {value.message}`
    end,
})

local function freezeOwned(value: any, seen: any?): any
    if type(value) ~= "table" or table.isfrozen(value) then
        return value
    end
    if getmetatable(value) ~= nil then
        return value
    end
    local visited: any = seen or {}
    if visited[value] then
        return value
    end
    visited[value] = true
    for key, child in pairs(value) do
        freezeOwned(key, visited)
        freezeOwned(child, visited)
    end
    return table.freeze(value)
end

local kernel = table.freeze({
    query = query,
    action = action,
    wait = waitKernel,
    capture = captureKernel,
    fixture = fixtureKernel,
    diagnostic = diagnosticKernel,
    record = record,
})

local function eguidevError(
    code: ErrorCode,
    message: string,
    operation: string?,
    target: ErrorTarget?,
    details: any?
): Error
    local value = {
        code = code,
        message = message,
        operation = operation,
        target = target,
        details = freezeOwned(details),
    }
    return table.freeze(setmetatable(value, errorMetatable)) :: any
end

local function fail(
    code: ErrorCode,
    message: string,
    operation: string?,
    target: ErrorTarget?,
    details: any?
): never
    error(eguidevError(code, message, operation, target, details), 0)
end

local function copyOptions(options: any?): any
    local result: any = {}
    if options ~= nil then
        for key, value in pairs(options) do
            result[key] = value
        end
    end
    return result
end

local function waitOptions(options: WaitOptions?): any
    local result = copyOptions(options)
    result.timeout_ms = math.min(result.timeout_ms or defaults.timeout_ms, defaults.timeout_ms)
    result.poll_interval_ms = result.poll_interval_ms or defaults.poll_interval_ms
    return result
end

local function actionOptions(options: ActionOptions?): any
    local result = waitOptions(options)
    if result.settle == nil then
        result.settle = defaults.settle
    end
    return result
end

local function configure(options: ScriptOptions)
    if options.timeout_ms ~= nil then
        defaults.timeout_ms = options.timeout_ms
    end
    if options.poll_interval_ms ~= nil then
        defaults.poll_interval_ms = options.poll_interval_ms
    end
    if options.settle ~= nil then
        defaults.settle = options.settle
    end
    if options.animations ~= nil then
        defaults.animations = options.animations
    end
    kernel.record.configure(options)
end

local function jsonPointer(value: any, pointer: string): (boolean, any)
    if pointer == "" then
        return true, value
    end
    if string.sub(pointer, 1, 1) ~= "/" then
        return false, nil
    end
    local current = value
    for rawPart in string.gmatch(string.sub(pointer, 2), "[^/]+") do
        if type(current) ~= "table" then
            return false, nil
        end
        local part = string.gsub(string.gsub(rawPart, "~1", "/"), "~0", "~")
        current = current[part]
        if current == nil then
            return false, nil
        end
    end
    return true, current
end

local function widgetConditionMatches(state: WidgetState?, condition: WidgetCondition): boolean
    local present = state ~= nil
    if condition.present ~= nil and condition.present ~= present then
        return false
    end
    if not present then
        return condition.present == false
    end
    local current = state :: WidgetState
    if condition.actionable == true then
        local visibleFraction = if current.layout ~= nil then current.layout.visible_fraction else 1
        if not current.visible or not current.enabled or visibleFraction <= 0 then
            return false
        end
    end
    if condition.visible ~= nil and current.visible ~= condition.visible then return false end
    if condition.enabled ~= nil and current.enabled ~= condition.enabled then return false end
    if condition.focused ~= nil and current.focused ~= condition.focused then return false end
    if condition.selected ~= nil and current.selected ~= condition.selected then return false end
    if condition.role ~= nil and current.role ~= condition.role then return false end
    if condition.label ~= nil and current.label ~= condition.label then return false end
    local labelContains = condition.label_contains
    if labelContains ~= nil then
        local label = current.label
        if label == nil or string.find(label, labelContains, 1, true) == nil then
            return false
        end
    end
    if condition.value ~= nil and current.value ~= condition.value then return false end
    if condition.value_text_contains ~= nil then
        if string.find(current.value_text, condition.value_text_contains, 1, true) == nil then
            return false
        end
    end
    if condition.scroll_at ~= nil then
        local scroll = current.scroll_state
        if scroll == nil then return false end
        local tolerance = condition.scroll_at.tolerance or 0.5
        if math.abs(scroll.offset.x - condition.scroll_at.offset.x) > tolerance then return false end
        if math.abs(scroll.offset.y - condition.scroll_at.offset.y) > tolerance then return false end
    end
    if condition.data ~= nil then
        local found, selected = jsonPointer(current.data, condition.data.pointer)
        if not found or selected ~= condition.data.equals then return false end
    end
    return true
end

local function widgetConditionDescription(condition: WidgetCondition): string
    if condition.actionable == true then
        return "interaction-ready condition; call scroll_into_view() before wait({ actionable = true })"
    end
    return "condition"
end

local function viewportConditionMatches(state: ViewportState?, condition: ViewportCondition): boolean
    local present = state ~= nil
    if condition.present ~= nil and condition.present ~= present then return false end
    if not present then return condition.present == false end
    local current = state :: ViewportState
    if condition.name ~= nil and current.name ~= condition.name then return false end
    if condition.title ~= nil and current.title ~= condition.title then return false end
    if condition.title_contains ~= nil then
        if current.title == nil or string.find(current.title, condition.title_contains, 1, true) == nil then
            return false
        end
    end
    if condition.focused ~= nil and current.focused ~= condition.focused then return false end
    if condition.minimized ~= nil and current.minimized ~= condition.minimized then return false end
    if condition.occluded ~= nil and current.occluded ~= condition.occluded then return false end
    if condition.maximized ~= nil and current.maximized ~= condition.maximized then return false end
    if condition.fullscreen ~= nil and current.fullscreen ~= condition.fullscreen then return false end
    if condition.frame_at_least ~= nil and current.frame_count < condition.frame_at_least then return false end
    return true
end

local widgetMethods = {} :: any
local viewportMethods = {} :: any
local captureMethods = {} :: any
local widgetCache: { [string]: Widget } = {}
local viewportCache: { [string]: Viewport } = {}

local function widget(id: string, viewportId: string?): Widget
    local cacheKey = (viewportId or "") .. "\0" .. id
    local cached = widgetCache[cacheKey]
    if cached ~= nil then return cached end
    local reference: Widget = table.freeze(setmetatable({
        id = id,
        __viewport_id = viewportId,
    }, widgetMethods)) :: any
    widgetCache[cacheKey] = reference
    return reference
end

local function viewport(id: string): Viewport
    local cached = viewportCache[id]
    if cached ~= nil then return cached end
    local reference: Viewport = table.freeze(setmetatable({ id = id }, viewportMethods)) :: any
    viewportCache[id] = reference
    return reference
end

widgetMethods.__index = widgetMethods
widgetMethods.__eq = function(left: Widget, right: Widget): boolean
    local leftRaw = left :: any
    local rightRaw = right :: any
    return left.id == right.id and leftRaw.__viewport_id == rightRaw.__viewport_id
end
widgetMethods.__tostring = function(value: Widget): string return `Widget({value.id})` end
viewportMethods.__index = viewportMethods
viewportMethods.__eq = function(left: Viewport, right: Viewport): boolean return left.id == right.id end
viewportMethods.__tostring = function(value: Viewport): string return `Viewport({value.id})` end

local function freezeState(value: any?): any?
    if value == nil then return nil end
    return freezeOwned(value)
end

local function waitLoop(predicate: () -> boolean, options: WaitOptions?, viewportId: string?)
    local resolved = waitOptions(options)
    local deadline = os.clock() * 1000 + resolved.timeout_ms
    while true do
        if predicate() then return end
        local remaining = math.max(0, math.ceil(deadline - os.clock() * 1000))
        if remaining == 0 then
            fail("timeout", "condition did not match before its deadline", "wait", nil, {
                timeout_ms = resolved.timeout_ms,
            })
        end
        resolved.timeout_ms = remaining
        resolved.viewport_id = viewportId
        kernel.wait.capture(resolved)
    end
end

function widgetMethods.state(self: Widget): WidgetState?
    return freezeState(kernel.query.widget_state(self)) :: WidgetState?
end

local function scopedWidgetTarget(self: Widget): any
    local state = self:state()
    if state == nil then return self end
    return table.freeze({
        id = self.id,
        viewport_id = state.viewport_id,
        __viewport_id = state.viewport_id,
    })
end

function widgetMethods.viewport(self: Widget): Viewport?
    local result = kernel.query.widget_viewport(scopedWidgetTarget(self))
    if result == nil then return nil end
    return viewport(result.id)
end

function widgetMethods.parent(self: Widget): Widget?
    local result = kernel.query.widget_parent(scopedWidgetTarget(self))
    if result == nil then return nil end
    return widget(result.id, result.viewport_id)
end

function widgetMethods.children(self: Widget): { Widget }
    local result: any = {}
    for _, child in kernel.query.widget_children(scopedWidgetTarget(self)) do
        table.insert(result, widget(child.id, child.viewport_id))
    end
    return kernel.record.array(result) :: { Widget }
end

function widgetMethods.wait(
    self: Widget,
    condition: WidgetCondition | WidgetPredicate,
    options: WaitOptions?
): WidgetState?
    if type(condition) ~= "function" then
        local recordCondition = condition :: WidgetCondition
        if recordCondition.present == false then
            kernel.wait.widget_wait_absent(scopedWidgetTarget(self), waitOptions(options))
            return nil
        end
    end
    local priorScroll: ScrollAreaMeta? = nil
    local description = if type(condition) == "function"
        then "predicate"
        else widgetConditionDescription(condition :: WidgetCondition)
    local state = kernel.wait.widget_wait(scopedWidgetTarget(self), function(rawState: any?): boolean
        local currentState = freezeState(rawState) :: WidgetState?
        if type(condition) == "function" then
            return (condition :: WidgetPredicate)(currentState)
        end
        local recordCondition = condition :: WidgetCondition
        if not widgetConditionMatches(currentState, recordCondition) then
            priorScroll = nil
            return false
        end
        if recordCondition.scroll_ready == true then
            if currentState == nil or currentState.scroll_state == nil then return false end
            local current = currentState.scroll_state
            local stable = priorScroll ~= nil
                and priorScroll.offset.x == current.offset.x
                and priorScroll.offset.y == current.offset.y
                and priorScroll.content_size.x == current.content_size.x
                and priorScroll.content_size.y == current.content_size.y
            priorScroll = current
            return stable
        end
        return true
    end, waitOptions(options), description)
    return freezeState(state) :: WidgetState?
end

local function recordExpectation(passed: boolean, message: string)
    kernel.record.assertion(passed, message)
end

function widgetMethods.expect(
    self: Widget,
    expectation: WidgetExpectation?,
    options: WaitOptions?
): WidgetState?
    local wanted: WidgetExpectation = expectation or {} :: any
    local ok, stateOrError = pcall(function(): WidgetState?
        return self:wait(wanted, options)
    end)
    if not ok then
        local failure = eguidevError(
            "expectation_failed",
            `Widget({self.id}) expectation failed`,
            "widget.expect",
            { kind = "widget", id = self.id },
            { cause = stateOrError, expected = wanted }
        )
        recordExpectation(false, `Widget({self.id}) expectation failed`)
        error(failure, 0)
    end
    local state = stateOrError :: WidgetState?
    local passed = true
    local leftOf = wanted.left_of
    if state ~= nil and leftOf ~= nil then
        local other = leftOf.widget:state()
        passed = other ~= nil and state.rect.max.x + (leftOf.min_gap or 0) <= other.rect.min.x
    end
    local above = wanted.above
    if passed and state ~= nil and above ~= nil then
        local other = above.widget:state()
        passed = other ~= nil and state.rect.max.y + (above.min_gap or 0) <= other.rect.min.y
    end
    local noOverlap = wanted.no_overlap
    if passed and state ~= nil and noOverlap ~= nil then
        local other = noOverlap:state()
        passed = other ~= nil and (state.rect.max.x <= other.rect.min.x
            or other.rect.max.x <= state.rect.min.x
            or state.rect.max.y <= other.rect.min.y
            or other.rect.max.y <= state.rect.min.y)
    end
    local within = wanted.within
    if passed and state ~= nil and within ~= nil then
        local other = within:state()
        passed = other ~= nil
            and state.rect.min.x >= other.rect.min.x
            and state.rect.min.y >= other.rect.min.y
            and state.rect.max.x <= other.rect.max.x
            and state.rect.max.y <= other.rect.max.y
    end
    local wantedDescendants = wanted.descendants
    if passed and wantedDescendants ~= nil then
        local descendants: any = {}
        local function collect(current: Widget)
            for _, child in current:children() do
                descendants[child.id] = true
                collect(child)
            end
        end
        collect(self)
        for _, child in wantedDescendants do
            if not descendants[child.id] then passed = false break end
        end
    end
    if passed and wanted.text_fits == true then
        passed = not self:text_measure().ellipsis
    end
    local painted = wanted.painted
    if passed and painted ~= nil then
        local samples = self:sample_grid(24, 24)
        local colors = {}
        for _, sample in samples do colors[sample.hex] = true end
        local count = 0
        for _ in colors do count += 1 end
        local minimumColors: number = painted.min_colors or 2
        passed = count >= minimumColors
    end
    recordExpectation(passed, `Widget({self.id}) expectation`)
    if not passed then
        fail("expectation_failed", `Widget({self.id}) expectation failed`, "widget.expect", {
            kind = "widget",
            id = self.id,
        }, { expected = wanted, observed = state })
    end
    return state
end

function widgetMethods.click(self: Widget, options: ClickOptions?)
    self:wait({ actionable = true }, options)
    kernel.action.widget_click(scopedWidgetTarget(self), actionOptions(options))
end

function widgetMethods.hover(self: Widget, options: HoverOptions?)
    self:wait({ actionable = true }, options)
    kernel.action.widget_hover(scopedWidgetTarget(self), actionOptions(options))
end

function widgetMethods.focus(self: Widget, options: ActionOptions?)
    self:wait({ present = true, visible = true, enabled = true }, options)
    kernel.action.widget_focus(scopedWidgetTarget(self), actionOptions(options))
end

function widgetMethods.type_text(self: Widget, text: string, options: TypeTextOptions?)
    self:wait({ actionable = true }, options)
    kernel.action.widget_type_text(scopedWidgetTarget(self), text, actionOptions(options))
end

function widgetMethods.set_value(self: Widget, value: WidgetValue, options: ActionOptions?)
    self:wait({ actionable = true }, options)
    kernel.action.widget_set_value(scopedWidgetTarget(self), value, actionOptions(options))
end

function widgetMethods.drag_to(self: Widget, target: Pos | Widget, options: DragOptions?)
    self:wait({ actionable = true }, options)
    local candidate: any = target
    if candidate.id ~= nil then
        local targetWidget = target :: Widget
        targetWidget:wait({ actionable = true }, options)
        kernel.action.widget_drag_to(
            scopedWidgetTarget(self),
            scopedWidgetTarget(targetWidget),
            actionOptions(options)
        )
    else
        kernel.action.widget_drag_position(
            scopedWidgetTarget(self),
            target :: Pos,
            actionOptions(options)
        )
    end
end

function widgetMethods.drag_relative(self: Widget, target: Vec, options: DragOptions?)
    self:wait({ actionable = true }, options)
    local resolved = actionOptions(options)
    kernel.action.widget_drag_relative(scopedWidgetTarget(self), target, resolved)
end

function widgetMethods.scroll(self: Widget, delta: Vec, options: ActionOptions?)
    self:wait({ actionable = true }, options)
    kernel.action.widget_scroll(scopedWidgetTarget(self), delta, actionOptions(options))
end

function widgetMethods.scroll_to(self: Widget, options: ScrollToOptions)
    self:wait({ present = true }, options)
    kernel.action.widget_scroll_to(scopedWidgetTarget(self), actionOptions(options))
end

function widgetMethods.scroll_into_view(self: Widget, options: ActionOptions?)
    self:wait({ present = true }, options)
    kernel.action.widget_scroll_into_view(scopedWidgetTarget(self), actionOptions(options))
end

function widgetMethods.text_measure(self: Widget): TextMeasure
    return freezeOwned(kernel.capture.widget_text_measure(scopedWidgetTarget(self))) :: TextMeasure
end

function widgetMethods.layout_issues(self: Widget): { LayoutIssue }
    return freezeOwned(kernel.capture.widget_layout_issues(scopedWidgetTarget(self))) :: { LayoutIssue }
end

function widgetMethods.screenshot(self: Widget): ImageRef
    return freezeOwned(kernel.capture.widget_screenshot(scopedWidgetTarget(self))) :: ImageRef
end

function widgetMethods.sample_pixels(self: Widget, positions: { Pos }): { PixelSample }
    return freezeOwned(kernel.capture.widget_sample_pixels(scopedWidgetTarget(self), positions))
        :: { PixelSample }
end

function widgetMethods.sample_grid(self: Widget, nx: number, ny: number): { PixelSample }
    return freezeOwned(kernel.capture.widget_sample_grid(scopedWidgetTarget(self), nx, ny))
        :: { PixelSample }
end

function widgetMethods.show_highlight(
    self: Widget,
    color: string,
    options: ActionOptions?
): HighlightResult
    return freezeOwned(kernel.action.widget_show_highlight(scopedWidgetTarget(self), color))
        :: HighlightResult
end

function widgetMethods.clear_highlight(self: Widget, options: ActionOptions?)
    kernel.action.widget_clear_highlight(scopedWidgetTarget(self))
end

function widgetMethods.show_debug_overlay(self: Widget, options: DebugOverlayOptions?)
    local resolved = actionOptions(options)
    kernel.action.widget_show_debug_overlay(scopedWidgetTarget(self), resolved.mode, resolved)
end

function widgetMethods.clear_debug_overlay(self: Widget, options: ActionOptions?)
    kernel.action.widget_clear_debug_overlay(scopedWidgetTarget(self))
end

function viewportMethods.state(self: Viewport): ViewportState?
    return freezeState(kernel.query.viewport_state(self)) :: ViewportState?
end

function viewportMethods.wait(
    self: Viewport,
    condition: ViewportCondition | ViewportPredicate,
    options: WaitOptions?
): ViewportState?
    local state = kernel.wait.viewport_wait(self, function(rawState: any?): boolean
        local currentState = freezeState(rawState) :: ViewportState?
        if type(condition) == "function" then
            return (condition :: ViewportPredicate)(currentState)
        end
        return viewportConditionMatches(currentState, condition :: ViewportCondition)
    end, waitOptions(options))
    return freezeState(state) :: ViewportState?
end

function viewportMethods.expect(
    self: Viewport,
    condition: ViewportCondition?,
    options: WaitOptions?
): ViewportState?
    local wanted: ViewportCondition = condition or {} :: any
    local ok, stateOrError = pcall(function(): ViewportState?
        return self:wait(wanted, options)
    end)
    local failure = if not ok
        then eguidevError(
            "expectation_failed",
            `Viewport({self.id}) expectation failed`,
            "viewport.expect",
            { kind = "viewport", id = self.id },
            { cause = stateOrError, expected = wanted }
        )
        else nil
    recordExpectation(ok, `Viewport({self.id}) expectation`)
    if not ok then
        error(failure, 0)
    end
    return stateOrError :: ViewportState?
end

function viewportMethods.settle(self: Viewport, options: WaitOptions?): SettleReport
    return freezeOwned(kernel.wait.viewport_settle(self, waitOptions(options))) :: SettleReport
end

function viewportMethods.wait_capture(self: Viewport, options: WaitOptions?): ViewportState
    local resolved = waitOptions(options)
    resolved.viewport_id = self.id
    kernel.wait.capture(resolved)
    local state = self:state()
    if state == nil then
        fail("not_found", `Viewport({self.id}) is absent`, "viewport.wait_capture", {
            kind = "viewport",
            id = self.id,
        }, nil)
    end
    return state :: ViewportState
end

local function widgetFilterOptions(filter: WidgetFilter?, viewportId: string?): any
    local result = copyOptions(filter)
    result.viewport_id = viewportId
    result.include_invisible = true
    return result
end

local function filterWidgetReferences(filter: WidgetFilter?, viewportId: string?): { Widget }
    local result: any = {}
    local rawViewports: any = if viewportId ~= nil
        then { { id = viewportId } }
        else kernel.query.viewport_list()
    for _, rawViewport in rawViewports do
        local viewportReference = viewport(rawViewport.id)
        for _, raw in kernel.query.viewport_widget_list(
            viewportReference,
            widgetFilterOptions(filter, viewportId)
        ) do
            local reference = widget(raw.id, raw.viewport_id)
            local state = reference:state()
            if state ~= nil
                and (filter == nil or filter.visible == nil or state.visible == filter.visible)
                and (filter == nil or filter.enabled == nil or state.enabled == filter.enabled)
                and (filter == nil or filter.focused == nil or state.focused == filter.focused)
                and (filter == nil or filter.selected == nil or state.selected == filter.selected)
            then
                table.insert(result, reference)
            end
        end
    end
    table.sort(result :: any, function(left: any, right: any): boolean return left.id < right.id end)
    return kernel.record.array(result) :: { Widget }
end

function viewportMethods.widgets(self: Viewport, filter: WidgetFilter?): { Widget }
    return filterWidgetReferences(filter, self.id)
end

function viewportMethods.widget(self: Viewport, id: string): Widget
    return widget(id, self.id)
end

function viewportMethods.widgets_at(self: Viewport, position: Pos): { Widget }
    local result: any = {}
    for _, raw in kernel.query.viewport_widget_at_point(self, position, true) do
        table.insert(result, widget(raw.id, raw.viewport_id))
    end
    return kernel.record.array(result) :: { Widget }
end

function viewportMethods.dismiss_popups(self: Viewport, options: ActionOptions?)
    kernel.action.viewport_dismiss_popups(self, actionOptions(options))
end

function viewportMethods.key(self: Viewport, key: string, options: KeyOptions?)
    kernel.action.viewport_key(self, key, actionOptions(options))
end

function viewportMethods.paste(self: Viewport, text: string, options: ActionOptions?)
    kernel.action.viewport_paste(self, text, actionOptions(options))
end

function viewportMethods.focus(self: Viewport, options: ActionOptions?)
    kernel.action.viewport_focus(self, actionOptions(options))
end

function viewportMethods.resize(self: Viewport, options: ResizeOptions)
    kernel.action.viewport_resize(self, actionOptions(options))
end

function viewportMethods.input(self: Viewport, event: RawInputEvent)
    kernel.action.viewport_input(self, event)
end

function viewportMethods.egui_diagnostics(self: Viewport): EguiDiagnosticBatch
    return freezeOwned(kernel.diagnostic.egui(self)) :: EguiDiagnosticBatch
end

function viewportMethods.clear_egui_diagnostics(self: Viewport)
    kernel.diagnostic.clear_egui(self)
end

function viewportMethods.layout_issues(self: Viewport): { LayoutIssue }
    return freezeOwned(kernel.capture.viewport_layout_issues(self)) :: { LayoutIssue }
end

function viewportMethods.screenshot(self: Viewport): ImageRef
    return freezeOwned(kernel.capture.viewport_screenshot(self)) :: ImageRef
end

function viewportMethods.sample_pixels(self: Viewport, positions: { Pos }): { PixelSample }
    return freezeOwned(kernel.capture.viewport_sample_pixels(self, positions)) :: { PixelSample }
end

function viewportMethods.show_highlight(
    self: Viewport,
    rect: Rect,
    color: string,
    options: ActionOptions?
): HighlightResult
    return freezeOwned(kernel.action.viewport_show_highlight(self, rect, color))
        :: HighlightResult
end

function viewportMethods.clear_highlights(self: Viewport, options: ActionOptions?)
    kernel.action.viewport_clear_highlights(self)
end

function viewportMethods.show_debug_overlay(self: Viewport, options: DebugOverlayOptions?)
    local resolved = actionOptions(options)
    kernel.action.viewport_show_debug_overlay(self, resolved.mode, resolved)
end

function viewportMethods.clear_debug_overlay(self: Viewport, options: ActionOptions?)
    kernel.action.viewport_clear_debug_overlay(self)
end

captureMethods.__index = captureMethods

function captureMethods.diff(self: Capture, options: DiffOptions?): TreeDiff
    return freezeOwned(kernel.capture.diff(self, options)) :: TreeDiff
end

table.freeze(widgetMethods)
table.freeze(viewportMethods)
table.freeze(captureMethods)

local function capture(): Capture
    local result = kernel.query.capture()
    setmetatable(result, captureMethods)
    return freezeOwned(result) :: Capture
end

local function viewports(filter: ViewportFilter?): { Viewport }
    local result: any = {}
    for _, raw in kernel.query.viewport_list() do
        local reference = viewport(raw.id)
        local state = reference:state()
        local titleContains = if filter ~= nil then filter.title_contains else nil
        local title = if state ~= nil then state.title else nil
        if state ~= nil
            and (filter == nil or filter.name == nil or state.name == filter.name)
            and (filter == nil or filter.focused == nil or state.focused == filter.focused)
            and (filter == nil or filter.title == nil or title == filter.title)
            and (titleContains == nil
                or (title ~= nil and string.find(title, titleContains, 1, true) ~= nil))
        then
            table.insert(result, reference)
        end
    end
    table.sort(result :: any, function(left: any, right: any): boolean return left.id < right.id end)
    return kernel.record.array(result) :: { Viewport }
end

local function waitViewport(filter: ViewportFilter, options: WaitOptions?): Viewport
    local matches = viewports(filter)
    local ok, cause = pcall(function()
        if #matches == 1 then return end
        waitLoop(function(): boolean
            matches = viewports(filter)
            return #matches == 1
        end, options, nil)
    end)
    if ok then return matches[1] end

    local rawCause: any = cause
    if type(rawCause) ~= "table" or rawCause.code ~= "timeout" then
        error(cause, 0)
    end
    local candidates: any = {}
    for _, candidate in matches do table.insert(candidates, candidate.id) end
    local ambiguous = #matches > 1
    local identifier = filter.name or filter.title or filter.title_contains or "filter"
    return fail(
        "not_found",
        if ambiguous
            then "Viewport filter remained ambiguous until its deadline"
            else "No viewport matched the filter before its deadline",
        "wait_viewport",
        { kind = "viewport", id = identifier },
        {
            reason = if ambiguous then "ambiguous" else "absent",
            candidates = candidates,
            expected = filter,
            cause = cause,
        }
    )
end

local function widgets(queryOptions: WidgetQuery?): { Widget }
    local selectedViewport = if queryOptions ~= nil then queryOptions.viewport else nil
    local viewportId = if selectedViewport ~= nil
        then selectedViewport.id
        else nil
    return filterWidgetReferences(queryOptions, viewportId)
end

local function dumpOptions(options: DumpOptions?): any
    if options == nil then return nil end
    local result = copyOptions(options)
    if options.viewport ~= nil then result.viewport = options.viewport.id end
    if options.root ~= nil then result.root = options.root.id end
    return result
end

local function fixtureTarget(raw: any): FixtureTarget
    return table.freeze({
        widget = widget(raw.widget_id, raw.viewport_id),
        condition = freezeOwned(raw.condition or {}),
    }) :: any
end

local function fixtureRecord(raw: any): Fixture
    local preconditions: any = {}
    for _, target in raw.preconditions or {} do table.insert(preconditions, fixtureTarget(target)) end
    local ready: any = {}
    for _, target in raw.ready or {} do table.insert(ready, fixtureTarget(target)) end
    return freezeOwned({
        name = raw.name,
        description = raw.description,
        preconditions = preconditions,
        ready = ready,
        params = raw.params or {},
        tags = raw.tags or {},
    }) :: Fixture
end

local function fixtures(): { Fixture }
    local result: any = {}
    for _, raw in kernel.fixture.list() do table.insert(result, fixtureRecord(raw)) end
    table.sort(result :: any, function(left: any, right: any): boolean return left.name < right.name end)
    return kernel.record.array(result) :: { Fixture }
end

local function waitFixturePrecondition(
    fixtureName: string,
    target: FixtureTarget,
    options: FixtureOptions?
)
    local ok, cause = pcall(function()
        target.widget:wait(target.condition, options)
    end)
    if ok then return end
    local rawCause: any = cause
    if type(rawCause) ~= "table" or rawCause.code ~= "timeout" then
        error(cause, 0)
    end
    fail(
        "fixture_failed",
        `Fixture({fixtureName}) precondition was not satisfied`,
        "fixture",
        { kind = "fixture", id = fixtureName },
        {
            reason = "precondition_unmet",
            widget = target.widget.id,
            condition = target.condition,
            cause = cause,
            hint = "this fixture is a transition; establish its preconditions first",
        }
    )
end

local function fixture(name: string, params: FixtureParams?, options: FixtureOptions?): FixtureResult
    local selected: Fixture? = nil
    for _, candidate in fixtures() do
        if candidate.name == name then
            selected = candidate
            break
        end
    end
    if selected ~= nil then
        for _, target in selected.preconditions do
            waitFixturePrecondition(name, target, options)
        end
    end
    local shouldWait = options == nil or options.wait ~= false
    local raw: any = kernel.fixture.apply(name, params) or {}
    local observations: any = {}
    if selected ~= nil then
        for _, target in selected.preconditions do
            table.insert(observations, freezeOwned({
                phase = "precondition",
                widget = target.widget,
                state = target.widget:state(),
            }))
        end
    end
    if shouldWait then
        kernel.wait.frames(1, waitOptions(options))
        local ready: any = {}
        if selected ~= nil then
            for _, target in selected.ready do table.insert(ready, target) end
        end
        for _, target in raw.ready or {} do table.insert(ready, fixtureTarget(target)) end
        for _, target in ready do
            local state = target.widget:wait(target.condition, options)
            table.insert(observations, freezeOwned({
                phase = "ready",
                widget = target.widget,
                state = state,
            }))
        end
    end
    return freezeOwned({ values = raw.values or {}, observations = observations }) :: FixtureResult
end

local public: Eguidev = {
    args = freezeOwned(kernel.record.args()) :: ScriptArgs,
    root = viewport("root"),
    log = function(value: any) kernel.record.log(value) end,
    configure = configure,
    wait = function(predicate: () -> boolean, options: WaitOptions?) waitLoop(predicate, options, nil) end,
    wait_frames = function(count: number?, options: WaitOptions?): number
        return kernel.wait.frames(count or 1, waitOptions(options))
    end,
    widget = widget,
    widgets = widgets,
    viewport = viewport,
    viewports = viewports,
    wait_viewport = waitViewport,
    capture = capture,
    dump = function(options: DumpOptions?): TreeDump
        return freezeOwned(kernel.query.dump(dumpOptions(options))) :: TreeDump
    end,
    dump_text = function(options: DumpOptions?): string
        return kernel.query.dump_text(dumpOptions(options))
    end,
    fixture = fixture,
    fixtures = fixtures,
    diagnostic = function(name: string): any return freezeOwned(kernel.diagnostic.get(name)) end,
    diagnostics = function(): DiagnosticsSnapshot
        return freezeOwned(kernel.diagnostic.all()) :: DiagnosticsSnapshot
    end,
}

return table.freeze(public)
