---
name: eguidev
description: MCP workflow for driving instrumented egui apps with Luau scripts.
---

# eguidev

eguidev exposes an instrumented egui app through MCP. Agents drive the app with
complete Luau scripts via `script_eval`; the MCP lifecycle tools manage the app
process.

This skill assumes the eguidev MCP connection already works. It is workflow
guidance, not an API reference.

**First action:** call `script_api` before writing Luau. The returned
`eguidev.d.luau` is the canonical reference for scripting types, functions, and
conventions.


## MCP Surface

The MCP server exposes six tools:

- `start`, `stop`, `restart`, `status` for lifecycle
- `script_api` for the canonical Luau API
- `script_eval` for all app inspection, interaction, waits, screenshots, and
  verification

## Instrumentation

Widget ids are the canonical selectors. Explicit ids must be unique in a
captured frame; duplicate ids are an instrumentation fault. Generated ids are
opaque and should not be relied on across restarts.

Use Rust API docs for signatures and details. If `ruskel` is installed, prefer
`ruskel eguidev` and `ruskel eguidev_runtime` to inspect the current public API;
use `--search <term>` to find specific items.

When editing instrumentation, prefer established Rust APIs:

- standard widgets: `dev_*` helpers from `DevUiExt`
- hierarchy: `container()`
- publish state with `track_response_full(...)` or `id_with_meta(...)`
- consume queued `set_value(...)` updates with
  `take_widget_value_override(...)` before rendering the widget


## Scripting

Write strict Luau. `--!strict` is implicit for all `script_eval` scripts.
Use `script_api` as the canonical scripting API. See `docs/luau.md` only for
Luau syntax help.


## Complete Scripts

Each `script_eval` call must be a self-contained script that performs setup,
action, and verification in one evaluation. Do not drive the app step-by-step
across multiple `script_eval` calls.

A well-structured script has four phases:

```luau
-- Setup
fixture("basic.with_items")
local vp = root()

-- Act
vp:widget_get("item.0.delete"):click()

-- Verify
local remaining = vp:wait_for_widget("items.count", function(widget)
    return widget.value_text == "2"
end)
assert(remaining ~= nil, "items.count should update")

-- Report
return { remaining = remaining.value_text }
```

Use `log()` for intermediate diagnostics and return a compact summary at the
end.

By default, `click()` waits until input has drained and a clean post-action
capture is available. Still use `wait_for_widget` or `wait_for` predicates for
state that can update asynchronously or through app work queued after the click.


## Lifecycle

- **NEVER** use `pkill`, `kill`, ctrl-c, or shell commands to manage the app.
- Use `start` to ensure the app is running, `restart` for a fresh process.
- Call `fixture()` at the start of scripts for a known in-app baseline.
- `fixture()` waits for declared readiness anchors on fresh captures; still
  wait/assert the specific widget or viewport state your script depends on.
- Use `fixture_raw()` only for manual or debugging setup flows.


## Inspection

Prefer programmatic inspection over screenshots:

- `widget_list`, `widget_get`, `state()`, `children()`, `parent()` for
  structure and values.
- Use `viewport({ title = "..." })` or `viewport({ title_contains = "..." })`
  to find secondary windows by title instead of hand-rolling `viewports()` loops;
  keep titles unique because ambiguous matches throw.
- Use `widget_list({ label = "..." })`, `widget_list({ label_contains = "..." })`,
  `widget_list({ role = "button" })`, or `widget_list({ id_prefix = "settings" })`
  to discover widgets without fetching state for every item.
- `wait_for_widget` with predicates for state readiness -- widget existence
  does not imply state readiness.
- `check_layout()` for layout problems (clipping, overflow, overlap).
- `text_measure()` for text sizing and truncation.

Use `screenshot()` only when the question is genuinely visual: alignment,
clipping, rendering quality, image content. Returned `ImageRef` values produce
image blocks in the MCP response.
On macOS, child viewport screenshots can fall back to native Quartz window capture after the
egui screenshot event path times out; this requires Screen Recording permission and a unique
recorded window title.
Use `sample_pixels()` for exact fixed-color assertions; it samples RGBA data
before screenshot JPEG encoding. Prefer `hex` for exact color equality. `rgba`
channels are Luau numbers, so use them for arithmetic thresholds only when that
is clearer than an exact fixed-color check.


## Background Automation

On macOS automation runs, `eguidev_runtime::attach` keeps covered windows
rendering by making AppKit report the window visible to winit/eframe. This is a
local runtime shim, not an upstream eframe dependency.

- `ViewportState.occluded` is the egui/winit value after that shim.
- `ViewportState.os_occluded` is the real platform occlusion state when observed.
- `ViewportState.os_minimized` is the real platform minimized state when observed.
- `EGUIDEV_FOREGROUND` disables the background automation tweaks for manual
  foreground debugging.

Use `os_occluded` for strict occlusion assertions when it is present. Keep
default smoke and script flows focused on whether frames, widgets, screenshots,
and pixel samples continue to work while the app is covered.


## Smoketest Scripts

Smoketest scripts follow the same shape as good `script_eval` scripts:

- Call `fixture()` before interacting with the UI.
- After `fixture()`, wait for state not covered by the fixture's anchors.
- Assert visible app behavior, not internals.
- Keep scripts independent -- no reliance on state from earlier tests.
- Use assertions for pass/fail and `log()` for diagnostics.
- Treat smoketests as regression tests and executable API documentation.


## Iterative Development Workflow

1. **Modify code** -- make changes to the app or its instrumentation.
2. **Lint** -- `cargo xtask tidy` to catch issues before running.
3. **Restart** -- call `restart` to pick up code changes.
4. **Inspect** -- use `script_eval` to explore widget state, verify layout,
   and exercise interactions. Start with `fixtures()` to discover baselines.
5. **Verify** -- write a complete script that sets up, acts, and asserts.
6. **Smoketest** -- run the suite to confirm nothing else broke.

When debugging:
- Use `state()` on a widget handle to inspect current role, value, geometry,
  enabled/visible/focused state.
- Use `dismiss_popups()` or a fresh `fixture()` when open menus or transient focus
  might leak between actions.
- Use `show_debug_overlay("bounds")` to visualize widget rects.
- Use `check_layout()` to find clipping, overflow, and overlap issues.
- Use `log()` liberally -- logs appear in the script result payload.
