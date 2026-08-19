--!strict

-- `script_eval` runs strict Luau inside the instrumented app process. Each
-- script is checked against this exact file before any submitted code runs.
-- A script can inspect one coherent frame, queue input, wait for state changes,
-- apply fixtures, and capture images without another MCP round trip.
-- The final value becomes JSON in `ScriptEvalOutcome`. Image references that
-- remain reachable from that value also produce MCP image content blocks.
-- Logs, Eguidev assertions, fixture applications, timing, and egui diagnostics
-- are present in the same outcome. A failure returns `success = false` with its
-- source location and does not become an MCP protocol error.
--
-- This file is the complete scripting reference. `script_api` and `edev docs`
-- return these bytes without a fence, header, prelude, or generated text.
--
-- Model expectations:
-- - Write strict Luau. Inline `script_eval` scripts are strict even when the
--   host omits `--!strict`.
-- - Treat widget ids as the one canonical selector in the API.
-- - Prefer explicit ids when you care about meaning, reuse, or stable scripts.
-- - Explicit widget ids must be unique within a captured frame. Duplicate
--   explicit ids block automation until instrumentation is fixed.
-- - Generated ids are opaque tokens. They are best-effort stable within the
--   current app session, but they are not expected to survive restart.
-- - `eguidev.widget(id)` and `eguidev.viewport(id)` create immutable live
--   references without a lookup. Read current frame data through `state()`.
-- - Prefer `fixture()`, waits, and assertions over ad-hoc retries.
-- - High-level actions auto-settle unless you disable settling explicitly.
-- - Highlights and debug overlays update immediately. They do not wait or
--   settle.
-- - Return `ImageRef` values from `screenshot()` when you want image blocks in
--   the MCP response.
-- - The sandbox has no filesystem access, no network access, and no module
--   imports.

export type Pos = {
    x: number,
    y: number,
}

export type Vec = {
    x: number,
    y: number,
}

export type Rect = {
    min: Pos,
    max: Pos,
}

export type EguiDiagnosticKind = "id_clash" | "rect_changed_id"

export type EguiDiagnostic = {
    kind: EguiDiagnosticKind,
    severity: "warning",
    message: string,
    viewport_id: string,
    frame: number,
    rect: Rect?,
}

export type EguiDiagnosticBatch = {
    entries: { EguiDiagnostic },
    --- Undismissed journal entries that were overwritten before this read.
    dropped: number,
}

export type Modifiers = {
    ctrl: boolean?,
    shift: boolean?,
    alt: boolean?,
    command: boolean?,
}

export type ImageRef = {
    type: "image_ref",
    id: string,
}

export type ScriptArgValue = string | number | boolean

export type ScriptArgs = { [string]: ScriptArgValue }

export type WidgetRole =
    "button"
    | "link"
    | "image"
    | "label"
    | "text_edit"
    | "slider"
    | "checkbox"
    | "combo_box"
    | "radio"
    | "drag_value"
    | "toggle"
    | "selectable"
    | "separator"
    | "spinner"
    | "scroll_area"
    | "menu_button"
    | "collapsing_header"
    | "window"
    | "progress_bar"
    | "color_picker"
    | "unknown"

--- Widget values match the widget role: booleans for checkboxes/toggles,
--- numbers for sliders/drag values/combo boxes (zero-based index), strings
--- for text edits.
export type WidgetValue = boolean | number | string

export type WidgetLayout = {
    --- Size the widget requested before layout constraints were applied.
    desired_size: Vec,
    --- Size actually assigned to the widget by the layout engine.
    actual_size: Vec,
    --- Clip rectangle that bounds the visible area for this widget.
    clip_rect: Rect,
    --- Whether any part of the widget falls outside the clip rect.
    clipped: boolean,
    --- Whether the widget extends beyond its allocated layout slot.
    overflow: boolean,
    --- Rectangle available to the widget before it was laid out.
    available_rect: Rect,
    --- Fraction of the widget's area that is visible after clipping (0..1).
    visible_fraction: number,
}

export type ScrollAreaMeta = {
    offset: Vec,
    viewport_size: Vec,
    content_size: Vec,
    max_offset: Vec,
}

export type Range = {
    min: number,
    max: number,
}

-- A widget exposes only its id. Handles returned by viewport-scoped discovery
-- retain that viewport scope internally. Widget equality and all later state,
-- relationship, wait, action, and observation calls preserve `(viewport_id,
-- id)` identity across fresh captures.
declare class Widget
    read id: string
    read state: (self: Widget) -> WidgetState?
    --- Checks the current state first, then polls fresh captures. A predicate
    --- receives `nil` while the widget is absent.
    read wait: (
        self: Widget,
        condition: WidgetCondition | WidgetPredicate,
        options: WaitOptions?
    ) -> WidgetState?
    --- Waits for a match and records one assertion. A failed match throws
    --- `expectation_failed` after it records the failed assertion.
    read expect: (
        self: Widget,
        expectation: WidgetExpectation?,
        options: WaitOptions?
    ) -> WidgetState?

    read viewport: (self: Widget) -> Viewport?
    read parent: (self: Widget) -> Widget?
    read children: (self: Widget) -> { Widget }

    read click: (self: Widget, options: ClickOptions?) -> ()
    read hover: (self: Widget, options: HoverOptions?) -> ()
    read focus: (self: Widget, options: ActionOptions?) -> ()
    read type_text: (self: Widget, text: string, options: TypeTextOptions?) -> ()
    read set_value: (self: Widget, value: WidgetValue, options: ActionOptions?) -> ()
    --- A position uses viewport-local logical points. A widget target must be
    --- in the same viewport.
    read drag_to: (self: Widget, target: Pos | Widget, options: DragOptions?) -> ()
    --- The target is normalized within the widget, from 0 to 1 on each axis.
    read drag_relative: (self: Widget, target: Vec, options: DragOptions?) -> ()
    read scroll: (self: Widget, delta: Vec, options: ActionOptions?) -> ()
    read scroll_to: (self: Widget, options: ScrollToOptions) -> ()
    read scroll_into_view: (self: Widget, options: ActionOptions?) -> ()

    read text_measure: (self: Widget) -> TextMeasure
    read layout_issues: (self: Widget) -> { LayoutIssue }
    --- Waits for a fresh screenshot and crops it to the widget.
    read screenshot: (self: Widget) -> ImageRef
    --- Waits for one fresh screenshot. Positions are logical offsets from the
    --- widget rectangle's minimum point.
    read sample_pixels: (self: Widget, positions: { Pos }) -> { PixelSample }
    --- Samples an evenly spaced grid over the widget's visible rectangle.
    read sample_grid: (self: Widget, nx: number, ny: number) -> { PixelSample }

    --- Visual aids persist until cleared. These methods do not wait or settle.
    read show_highlight: (self: Widget, color: string) -> HighlightResult
    read clear_highlight: (self: Widget) -> ()
    read show_debug_overlay: (self: Widget, options: DebugOverlayOptions?) -> ()
    read clear_debug_overlay: (self: Widget) -> ()
end

-- `(viewport_id, id)` is the one widget identity across handles, states,
-- dumps, deltas, waits, and error details, so a state alone names its widget.
export type WidgetState = {
    id: string,
    viewport_id: string,
    parent_id: string?,
    child_ids: { string },
    rect: Rect,
    interact_rect: Rect,
    role: WidgetRole,
    label: string?,
    value: WidgetValue?,
    data: any?,
    --- String representation of value. Empty string when value is nil.
    --- Bool → "true"/"false", Float → decimal, Int → decimal, Text → verbatim.
    value_text: string,
    layout: WidgetLayout?,
    --- scroll_area only.
    scroll_state: ScrollAreaMeta?,
    --- slider, drag_value only.
    range: Range?,
    --- combo_box only.
    options: { string }?,
    --- button only when selected-aware metadata is recorded.
    selected: boolean?,
    --- checkbox only when indeterminate-aware metadata is recorded.
    indeterminate: boolean?,
    --- text_edit only.
    multiline: boolean?,
    --- text_edit only. True when input is masked.
    password: boolean?,
    enabled: boolean,
    visible: boolean,
    focused: boolean,
}

export type ViewportState = {
    id: string,
    name: string?,
    title: string?,
    outer_pos: Pos?,
    outer_size: Vec?,
    inner_size: Vec,
    focused: boolean,
    minimized: boolean?,
    occluded: boolean?,
    --- Real platform minimized state when eguidev can observe it.
    os_minimized: boolean?,
    --- Real platform occlusion state when eguidev can observe it. On macOS
    --- automation runs this can differ from `occluded`, which is forced false
    --- so eframe keeps rendering in covered windows.
    os_occluded: boolean?,
    maximized: boolean?,
    fullscreen: boolean?,
    frame_count: number,
    pixels_per_point: number,
    pointer_pos: Pos?,
}

export type DumpOptions = {
    viewport: Viewport?,
    root: Widget?,
    --- Defaults to true because dumps are a debugging view.
    include_invisible: boolean?,
    --- Defaults to "core". "full" adds interaction, layout, scroll, range,
    --- and layer fields.
    fields: ("core" | "full")?,
}

export type WidgetDump = {
    id: string,
    role: WidgetRole,
    label: string?,
    value: WidgetValue?,
    value_text: string,
    data: any?,
    rect: Rect,
    enabled: boolean,
    visible: boolean,
    focused: boolean,
    selected: boolean?,
    interact_rect: Rect?,
    layout: WidgetLayout?,
    scroll_state: ScrollAreaMeta?,
    range: Range?,
    layer: string?,
    children: { WidgetDump },
}

export type ViewportDump = {
    id: string,
    name: string?,
    title: string?,
    focused: boolean,
    minimized: boolean?,
    occluded: boolean?,
    inner_size: Vec,
    frame_count: number,
    widgets: { WidgetDump },
}

export type TreeDump = {
    viewports: { ViewportDump },
}

export type DiffOptions = {
    --- Movement below this logical-point distance is ignored. Defaults to 0.5.
    move_epsilon: number?,
    --- Defaults to false.
    include_invisible: boolean?,
    id_prefix: string?,
}

export type WidgetDelta = {
    id: string,
    viewport_id: string,
    change: "added" | "removed" | "changed",
    fields: { string },
    before: WidgetState?,
    after: WidgetState?,
}

export type TreeDiff = {
    changes: { WidgetDelta },
    viewports_added: { string },
    viewports_removed: { string },
}

--- An immutable, immediate registry snapshot. `diff()` compares this baseline
--- with registry state at the time of the call.
export type Capture = {
    frame: number,
    diff: (self: Capture, options: DiffOptions?) -> TreeDiff,
}

--- Exact pixel sampled from a fresh viewport capture.
--- `position` preserves the method's coordinate space: viewport-local or
--- widget-relative logical points. `physical` is the viewport pixel after
--- scaling and flooring. `rgba` values support arithmetic; `hex` is preferred
--- for exact fixed-color assertions.
export type PixelSample = {
    position: Pos,
    physical: { number },
    rgba: { number },
    hex: string,
}

declare class Viewport
    read id: string
    read state: (self: Viewport) -> ViewportState?
    --- Checks the current state first, then polls fresh captures. A predicate
    --- receives `nil` while the viewport is absent.
    read wait: (
        self: Viewport,
        condition: ViewportCondition | ViewportPredicate,
        options: WaitOptions?
    ) -> ViewportState?
    --- Waits for a match and records one assertion. A failed match throws
    --- `expectation_failed` after it records the failed assertion.
    read expect: (
        self: Viewport,
        condition: ViewportCondition?,
        options: WaitOptions?
    ) -> ViewportState?

    read settle: (self: Viewport, options: WaitOptions?) -> SettleReport
    --- Waits for a fresh viewport capture and throws `not_found` if the
    --- viewport is then absent.
    read wait_capture: (self: Viewport, options: WaitOptions?) -> ViewportState
    read widget: (self: Viewport, id: string) -> Widget
    read widgets: (self: Viewport, filter: WidgetFilter?) -> { Widget }
    --- Uses viewport-local logical points and returns topmost widgets first.
    read widgets_at: (self: Viewport, position: Pos) -> { Widget }

    read dismiss_popups: (self: Viewport, options: ActionOptions?) -> ()
    read key: (self: Viewport, key: string, options: KeyOptions?) -> ()
    read paste: (self: Viewport, text: string, options: ActionOptions?) -> ()
    read focus: (self: Viewport, options: ActionOptions?) -> ()
    --- Requests the constraints as one atomic operation. The window manager
    --- can clamp or ignore them, so inspect the resulting state.
    read resize: (self: Viewport, options: ResizeOptions) -> ()
    --- Queues exactly one raw event. It does not wait or settle.
    read input: (self: Viewport, event: RawInputEvent) -> ()

    --- Waits for completed evaluation-scoped entries, returns them in journal
    --- order, and dismisses them for this evaluation.
    read egui_diagnostics: (self: Viewport) -> EguiDiagnosticBatch
    --- Dismisses evaluation-scoped entries without returning them.
    read clear_egui_diagnostics: (self: Viewport) -> ()
    --- Waits for a fresh screenshot of the viewport.
    read screenshot: (self: Viewport) -> ImageRef
    --- Waits for one fresh screenshot. Positions are viewport-local logical
    --- points.
    read sample_pixels: (self: Viewport, positions: { Pos }) -> { PixelSample }
    read layout_issues: (self: Viewport) -> { LayoutIssue }

    read show_highlight: (
        self: Viewport,
        rect: Rect,
        color: string
    ) -> HighlightResult
    read clear_highlights: (self: Viewport) -> ()
    read show_debug_overlay: (self: Viewport, options: DebugOverlayOptions?) -> ()
    read clear_debug_overlay: (self: Viewport) -> ()
end

--- High-level input actions settle after they queue input. Widget actions first
--- wait for their required widget state. Calls inherit the current evaluation
--- defaults: 5000 ms timeout, 16 ms polling, and settling enabled. A call
--- cannot extend the configured timeout or the script deadline.
export type ActionOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
}

export type ClickOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    --- Defaults to "primary".
    button: "primary" | "secondary" | "middle"?,
    --- Defaults to 1 and accepts values from 1 through 3.
    click_count: number?,
}

--- Populated fields use AND. An empty filter includes visible and invisible
--- widgets. Discovery returns handles in widget ID order.
export type WidgetFilter = {
    role: WidgetRole?,
    id_prefix: string?,
    label: string?,
    label_contains: string?,
    visible: boolean?,
    enabled: boolean?,
    focused: boolean?,
    selected: boolean?,
}

export type WidgetQuery = WidgetFilter & {
    viewport: Viewport?,
}

export type ViewportFilter = {
    name: string?,
    title: string?,
    title_contains: string?,
    focused: boolean?,
}

export type HoverOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    --- Normalized within the widget from 0 to 1. Defaults to the center.
    position: Vec?,
    --- Defaults to zero.
    duration_ms: number?,
}

export type TypeTextOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    clear: boolean?,
    enter: boolean?,
}

export type DragOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    --- Normalized within the source widget from 0 to 1. Defaults to the center.
    from: Vec?,
}

--- Targets a scroll area. `offset` takes precedence over `align`; at least one
--- must be present. Offsets use logical points and are clamped to the content.
export type ScrollToOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    offset: Vec?,
    align: "top" | "center" | "bottom"?,
}

export type KeyOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    repeat_count: number?,
    target: Widget?,
}

export type RawInputAction = "press" | "release"
export type PointerButton = "primary" | "secondary" | "middle"

--- Raw pointer positions use viewport-local logical points. Scroll deltas use
--- logical points.
export type RawInputEvent =
    { type: "pointer_move", position: Pos }
    | {
        type: "pointer_button",
        position: Pos,
        button: PointerButton,
        action: RawInputAction,
        modifiers: Modifiers?,
    }
    | {
        type: "key",
        key: string,
        action: RawInputAction,
        modifiers: Modifiers?,
    }
    | { type: "text", text: string }
    | { type: "scroll", delta: Vec, modifiers: Modifiers? }

-- Closed set of issue kinds returned by `layout_issues()`.
export type LayoutIssueKind =
    "overlap"
    | "clipping"
    | "overflow"
    | "zero_size"
    | "text_truncation"
    | "offscreen"

-- Selects what the persistent debug overlay visualizes:
--
-- - "bounds"     — stroke around every widget rect.
-- - "margins"    — bounds + available_rect, showing the gap between layout
--                  slot and actual widget.
-- - "clipping"   — bounds + clipped_rect, revealing partial/full clipping.
-- - "overlaps"   — highlights intersection rects of sibling widgets that
--                  share a parent.
-- - "focus"      — bounds for focused widgets only.
-- - "layers"     — bounds with egui layer id in the label.
-- - "containers" — bounds for widgets that have children only, hiding leaves.
export type OverlayDebugMode =
    "bounds"
    | "margins"
    | "clipping"
    | "overlaps"
    | "focus"
    | "layers"
    | "containers"

-- Unset fields use defaults. Colors are hex "#RRGGBB" or "#RRGGBBAA".
export type DebugOverlayOptions = {
    mode: OverlayDebugMode?,
    -- Defaults to true.
    show_labels: boolean?,
    -- Append pixel dimensions to labels. No effect when show_labels is false.
    -- Defaults to true.
    show_sizes: boolean?,
    -- Label font size in logical points. Defaults to 10.
    label_font_size: number?,
    -- Defaults to semi-transparent green.
    bounds_color: string?,
    -- Used by "clipping" and "margins" modes. Defaults to semi-transparent red.
    clip_color: string?,
    -- Used by "overlaps" mode. Defaults to semi-transparent yellow.
    overlap_color: string?,
}

export type ResizeOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    inner_size: Vec?,
    min_size: Vec?,
    max_size: Vec?,
    increments: Vec?,
    resizable: boolean?,
}

export type HighlightResult = {
    -- The actual rect that was highlighted (useful when the widget target was
    -- used rather than an explicit rect).
    rect: Rect,
}

-- A single semantic layout problem reported by `layout_issues()`.
export type LayoutIssue = {
    kind: LayoutIssueKind,
    widgets: { string },
    message: string,
    -- Overlaps use the intersection rect. Other issue kinds use the primary
    -- widget rect when one is useful.
    rect: Rect?,
}

export type TextMeasureLine = {
    text: string,
    --- The rendered width of this line in logical points.
    width: number,
}

export type TextMeasure = {
    text: string,
    --- The portion of text actually visible after layout and clipping.
    visible_text: string,
    --- The size the text would need if unconstrained.
    desired_size: Vec,
    --- The size the text actually occupies after layout.
    actual_size: Vec,
    --- Height of a single text line in logical points.
    line_height: number,
    lines: { TextMeasureLine },
    --- Whether the text was truncated and an ellipsis character was inserted.
    ellipsis: boolean,
}

export type FixtureParamKind = "bool" | "int" | "float" | "text"

export type FixtureParam = {
    name: string,
    kind: FixtureParamKind,
    description: string,
    --- Its absence makes the parameter required.
    default: WidgetValue?,
    --- Restricts the parameter to these exact values.
    choices: { WidgetValue }?,
    --- Inclusive numeric lower bound.
    min: number?,
    --- Inclusive numeric upper bound.
    max: number?,
}

export type FixtureParams = { [string]: WidgetValue }

export type FixtureValues = { [string]: WidgetValue }

export type SettlePhase =
    "input_drained"
    | "commands_drained"
    | "action_frame_processed"
    | "clean_capture"
    | "fresh_frame"
    | "app_idle"

export type SettlePhaseStatus = {
    phase: SettlePhase,
    complete: boolean,
    detail: string,
}

export type SettleReport = {
    settled: boolean,
    elapsed_ms: number,
    phases: { SettlePhaseStatus },
}

export type Fixture = {
    name: string,
    description: string,
    preconditions: { FixtureTarget },
    ready: { FixtureTarget },
    params: { FixtureParam },
    tags: { string },
}

export type DataCondition = {
    --- RFC 6901 JSON Pointer. An empty pointer selects all widget data.
    pointer: string,
    equals: any,
}

export type ScrollAtCondition = {
    offset: Vec,
    --- Maximum difference on each axis. Defaults to 0.5 logical points.
    tolerance: number?,
}

--- Populated fields use AND. An empty condition means `present = true`.
--- `present = false` cannot appear with a state field.
export type WidgetCondition = {
    present: boolean?,
    --- Requires presence, visibility, enabled state, and a positive visible
    --- fraction.
    actionable: boolean?,
    visible: boolean?,
    enabled: boolean?,
    focused: boolean?,
    selected: boolean?,
    role: WidgetRole?,
    label: string?,
    label_contains: string?,
    value: WidgetValue?,
    value_text_contains: string?,
    --- Requires scroll state that is unchanged across two fresh captures.
    scroll_ready: boolean?,
    scroll_at: ScrollAtCondition?,
    data: DataCondition?,
}

--- Populated fields use AND. An empty condition means `present = true`.
--- `present = false` cannot appear with a state field.
export type ViewportCondition = {
    present: boolean?,
    name: string?,
    title: string?,
    title_contains: string?,
    focused: boolean?,
    minimized: boolean?,
    occluded: boolean?,
    maximized: boolean?,
    fullscreen: boolean?,
    frame_at_least: number?,
}

export type WidgetPredicate = (state: WidgetState?) -> boolean
export type ViewportPredicate = (state: ViewportState?) -> boolean

export type RelationExpectation = {
    widget: Widget,
    --- Required distance between rectangles. Defaults to zero.
    min_gap: number?,
}

export type PaintExpectation = {
    --- Distinct colors required in a 24 by 24 visible-area sample. Defaults to 2.
    min_colors: number?,
}

--- Relation checks use widget rectangles. `descendants` requires every target
--- anywhere in the recursive subtree. `text_fits` requires no ellipsis.
export type WidgetExpectation = WidgetCondition & {
    left_of: RelationExpectation?,
    above: RelationExpectation?,
    no_overlap: Widget?,
    within: Widget?,
    descendants: { Widget }?,
    text_fits: boolean?,
    painted: PaintExpectation?,
}

export type FixtureTarget = {
    widget: Widget,
    condition: WidgetCondition,
}

export type FixtureOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    --- False skips the fresh frame and all readiness waits. Preconditions and
    --- the fixture handler still run.
    wait: boolean?,
}

export type FixtureObservation = {
    phase: "precondition" | "ready",
    widget: Widget,
    state: WidgetState?,
}

export type FixtureResult = {
    values: FixtureValues,
    observations: { FixtureObservation },
}

--- Changes defaults only for the current evaluation. Initial defaults are
--- 5000 ms, 16 ms, settling enabled, and animations disabled.
export type ScriptOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    animations: boolean?,
}

--- A wait inherits current evaluation defaults. Its timeout cannot extend the
--- configured timeout or the script deadline.
export type WaitOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
}

export type ErrorCode =
    "invalid_argument"
    | "not_found"
    | "not_actionable"
    | "instrumentation_fault"
    | "timeout"
    | "unsupported"
    | "fixture_failed"
    | "diagnostic_failed"
    | "expectation_failed"
    | "internal"

export type ErrorTarget = {
    kind: "widget" | "viewport" | "fixture" | "diagnostic",
    id: string,
}

--- A public failure throws this frozen table. `pcall` can catch it. An
--- unhandled value becomes `success = false` in the script outcome.
export type Error = {
    code: ErrorCode,
    message: string,
    operation: string?,
    target: ErrorTarget?,
    details: any?,
}

export type DiagnosticErrorInfo = {
    code: string,
    message: string,
    details: any?,
}

export type DiagnosticsSnapshot = {
    values: { [string]: any },
    errors: { [string]: DiagnosticErrorInfo },
}

export type Eguidev = {
    args: ScriptArgs,
    root: Viewport,

    log: (value: any) -> (),
    configure: (options: ScriptOptions) -> (),

    --- Checks immediately, then polls fresh captures.
    wait: (predicate: () -> boolean, options: WaitOptions?) -> (),
    --- Waits for one new app-wide frame by default.
    wait_frames: (count: number?, options: WaitOptions?) -> number,

    widget: (id: string) -> Widget,
    widgets: (query: WidgetQuery?) -> { Widget },
    viewport: (id: string) -> Viewport,
    viewports: (filter: ViewportFilter?) -> { Viewport },
    --- Waits until exactly one viewport matches. Absence and ambiguity produce
    --- distinct `not_found` details at the deadline.
    wait_viewport: (filter: ViewportFilter, options: WaitOptions?) -> Viewport,

    --- Takes an immediate immutable registry snapshot.
    capture: () -> Capture,
    dump: (options: DumpOptions?) -> TreeDump,
    dump_text: (options: DumpOptions?) -> string,

    --- Waits for static preconditions, applies the handler once, waits one new
    --- frame, then waits for static and handler-provided readiness targets.
    fixture: (name: string, params: FixtureParams?, options: FixtureOptions?) -> FixtureResult,
    fixtures: () -> { Fixture },

    --- Reads one application-defined provider and throws `diagnostic_failed`
    --- if that provider fails.
    diagnostic: (name: string) -> any,
    --- Reads all application-defined providers and records failures by name.
    diagnostics: () -> DiagnosticsSnapshot,
}

declare eguidev: Eguidev
