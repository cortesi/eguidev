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

export type LayoutOverflow = boolean

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
export type Widget = {
    id: string,
    state: (self: Widget) -> WidgetState?,
    wait: (
        self: Widget,
        condition: WidgetCondition | WidgetPredicate,
        options: WaitOptions?
    ) -> WidgetState?,
    expect: (
        self: Widget,
        expectation: WidgetExpectation?,
        options: WaitOptions?
    ) -> WidgetState?,

    viewport: (self: Widget) -> Viewport?,
    parent: (self: Widget) -> Widget?,
    children: (self: Widget) -> { Widget },

    click: (self: Widget, options: ClickOptions?) -> (),
    hover: (self: Widget, options: HoverOptions?) -> (),
    focus: (self: Widget, options: ActionOptions?) -> (),
    type_text: (self: Widget, text: string, options: TypeTextOptions?) -> (),
    set_value: (self: Widget, value: WidgetValue, options: ActionOptions?) -> (),
    drag_to: (self: Widget, target: Pos | Widget, options: DragOptions?) -> (),
    drag_relative: (self: Widget, target: Vec, options: DragOptions?) -> (),
    scroll: (self: Widget, delta: Vec, options: ActionOptions?) -> (),
    scroll_to: (self: Widget, options: ScrollToOptions) -> (),
    scroll_into_view: (self: Widget, options: ActionOptions?) -> (),

    text_measure: (self: Widget) -> TextMeasure,
    layout_issues: (self: Widget) -> { LayoutIssue },
    screenshot: (self: Widget) -> ImageRef,
    sample_pixels: (self: Widget, positions: { Pos }) -> { PixelSample },
    sample_grid: (self: Widget, nx: number, ny: number) -> { PixelSample },

    show_highlight: (self: Widget, color: string, options: ActionOptions?) -> HighlightResult,
    clear_highlight: (self: Widget, options: ActionOptions?) -> (),
    show_debug_overlay: (self: Widget, options: DebugOverlayOptions?) -> (),
    clear_debug_overlay: (self: Widget, options: ActionOptions?) -> (),
}

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
    include_invisible: boolean?,
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
    move_epsilon: number?,
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

export type Capture = {
    frame: number,
    diff: (self: Capture, options: DiffOptions?) -> TreeDiff,
}

--- Exact pixel sampled from a fresh viewport capture.
--- `position` is the requested egui logical point. `physical` is the sampled
--- pixel coordinate after multiplying by the viewport's pixels-per-point and
--- flooring. `rgba` values are Luau numbers and safe for arithmetic; `hex` is
--- the preferred exact fixed-color assertion.
export type PixelSample = {
    position: Pos,
    physical: { number },
    rgba: { number },
    hex: string,
}

export type Viewport = {
    id: string,
    state: (self: Viewport) -> ViewportState?,
    wait: (
        self: Viewport,
        condition: ViewportCondition | ViewportPredicate,
        options: WaitOptions?
    ) -> ViewportState?,
    expect: (
        self: Viewport,
        condition: ViewportCondition?,
        options: WaitOptions?
    ) -> ViewportState?,

    settle: (self: Viewport, options: WaitOptions?) -> SettleReport,
    wait_capture: (self: Viewport, options: WaitOptions?) -> ViewportState,
    widgets: (self: Viewport, filter: WidgetFilter?) -> { Widget },
    widgets_at: (self: Viewport, position: Pos) -> { Widget },

    dismiss_popups: (self: Viewport, options: ActionOptions?) -> (),
    key: (self: Viewport, key: string, options: KeyOptions?) -> (),
    paste: (self: Viewport, text: string, options: ActionOptions?) -> (),
    focus: (self: Viewport, options: ActionOptions?) -> (),
    resize: (self: Viewport, options: ResizeOptions) -> (),
    input: (self: Viewport, event: RawInputEvent) -> (),

    egui_diagnostics: (self: Viewport) -> EguiDiagnosticBatch,
    clear_egui_diagnostics: (self: Viewport) -> (),
    screenshot: (self: Viewport) -> ImageRef,
    sample_pixels: (self: Viewport, positions: { Pos }) -> { PixelSample },
    layout_issues: (self: Viewport) -> { LayoutIssue },

    show_highlight: (
        self: Viewport,
        rect: Rect,
        color: string,
        options: ActionOptions?
    ) -> HighlightResult,
    clear_highlights: (self: Viewport, options: ActionOptions?) -> (),
    show_debug_overlay: (self: Viewport, options: DebugOverlayOptions?) -> (),
    clear_debug_overlay: (self: Viewport, options: ActionOptions?) -> (),
}

-- High-level actions auto-settle by default. Use `configure()` to change
-- global timeout, poll interval, and settle behavior.
export type ActionOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
}

export type ClickOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    button: "primary" | "secondary" | "middle"?,
    click_count: number?,
}

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
    position: Vec?,
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
    from: Vec?,
}

-- scroll_to targets a scroll area widget. Use offset for explicit coordinates or align for
-- a coarse jump within the scroll area.
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

-- Closed set of issue kinds returned by `check_layout()`.
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
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
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

-- A single semantic layout problem reported by `check_layout()`.
export type LayoutIssue = {
    kind: LayoutIssueKind,
    widgets: { string },
    message: string,
    -- Overlaps use the intersection rect. Other issue kinds use the primary
    -- widget rect when one is useful.
    rect: Rect?,
}

export type TextMeasureLine = {
    --- The text content of this line.
    text: string,
    --- The rendered width of this line in logical points.
    width: number,
}

export type TextMeasure = {
    --- The full text string of the widget.
    text: string,
    --- The portion of text actually visible after layout and clipping.
    visible_text: string,
    --- The size the text would need if unconstrained.
    desired_size: Vec,
    --- The size the text actually occupies after layout.
    actual_size: Vec,
    --- Height of a single text line in logical points.
    line_height: number,
    --- Per-line breakdown of text content and rendered width.
    lines: { TextMeasureLine },
    --- Whether the text was truncated and an ellipsis character was inserted.
    ellipsis: boolean,
}

export type FixtureParamKind = "bool" | "int" | "float" | "text"

export type FixtureParam = {
    name: string,
    kind: FixtureParamKind,
    description: string,
    default: WidgetValue?,
    choices: { WidgetValue }?,
    min: number?,
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
    pointer: string,
    equals: any,
}

export type ScrollAtCondition = {
    offset: Vec,
    tolerance: number?,
}

export type WidgetCondition = {
    present: boolean?,
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
    scroll_ready: boolean?,
    scroll_at: ScrollAtCondition?,
    data: DataCondition?,
}

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
    min_gap: number?,
}

export type PaintExpectation = {
    min_colors: number?,
}

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

export type ScriptOptions = {
    timeout_ms: number?,
    poll_interval_ms: number?,
    settle: boolean?,
    animations: boolean?,
}

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

    wait: (predicate: () -> boolean, options: WaitOptions?) -> (),
    wait_frames: (count: number?, options: WaitOptions?) -> number,

    widget: (id: string) -> Widget,
    widgets: (query: WidgetQuery?) -> { Widget },
    viewport: (id: string) -> Viewport,
    viewports: (filter: ViewportFilter?) -> { Viewport },

    capture: () -> Capture,
    dump: (options: DumpOptions?) -> TreeDump,
    dump_text: (options: DumpOptions?) -> string,

    fixture: (name: string, params: FixtureParams?, options: FixtureOptions?) -> FixtureResult,
    fixtures: () -> { Fixture },

    diagnostic: (name: string) -> any,
    diagnostics: () -> DiagnosticsSnapshot,
}

declare eguidev: Eguidev
