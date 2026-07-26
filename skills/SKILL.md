---
name: eguidev
description: MCP workflow for driving instrumented egui apps with Luau scripts.
---

# eguidev

eguidev exposes an instrumented egui app through MCP and the `edev` CLI. Agents
drive the app with complete Luau scripts; the MCP lifecycle tools manage a
persistent app process, while CLI evaluations launch an app for one script.

This skill is workflow guidance, not an API reference.

**First action:** call `script_api` before writing Luau. If the app's MCP tools
are unavailable but the repository has an `.edev.toml`, run `edev docs`
instead. Both return the canonical `eguidev.d.luau` reference for scripting
types, functions, and conventions.


## MCP Surface

The MCP server exposes six tools:

- `start`, `stop`, `restart`, `status` for lifecycle
- `script_api` for the canonical Luau API
- `script_eval` for all app inspection, interaction, waits, screenshots, and
  verification

Do not change global MCP configuration merely to unblock one repository task.
Declare the server in the app repository instead. For Codex that is the
repository's own `.codex/config.toml`, never the user-wide
`~/.codex/config.toml`. Edit the project file directly, because `codex mcp add`
writes user-wide configuration.

```toml
[mcp_servers.my-app-edev]
command = "edev"
args = ["mcp"]
```

`edev` finds `.edev.toml` by walking up from its working directory, stopping at
the repository root. A config inside the app repository therefore needs no `cwd`
of its own, whether it sits at the root or under `.codex/`.

Set `cwd` only when the config lives outside the app repository, such as an
umbrella directory holding several app repositories. The upward walk never
descends, so point `cwd` at the app repository root:

```toml
[mcp_servers.my-app-edev]
command = "edev"
args = ["mcp"]
cwd = "my-app"
```

Start Codex from the app repository, and restart it after adding the server.
Project config is ignored until the repository is trusted. Add
`startup_timeout_sec` when the command builds `edev` from source, as this
repository's `.codex/config.toml` does.

If the configured tools are unavailable, use `edev docs`, place self-contained
probes under the app's `tmp/` directory, run them with `edev eval <path>`, and
use `edev smoke` for persistent scenarios.

The CLI answers the discovery questions faster than a throwaway script:

- `edev docs` prints the canonical Luau reference.
- `edev fixtures` lists fixture names, descriptions, params, tags, and anchors.
  `edev fixtures --markdown` renders the same catalog for docs.
- `edev fixture NAME --param k=v` applies one fixture.
- `edev dump --fixture NAME` prints the widget tree for a baseline.
- `edev eval PATH` runs one script and prints its structured outcome.

## Instrumentation

Widget ids are the canonical selectors. Explicit ids must be unique in a
captured frame; duplicate ids are an instrumentation fault. Generated ids are
opaque and should not be relied on across restarts.

Use Rust API docs for signatures and details. If `ruskel` is installed, prefer
`ruskel eguidev` and `ruskel eguidev_runtime` to inspect the current public API;
use `--search <term>` to find specific items.

When editing instrumentation, prefer established Rust APIs:

- standard widgets: `dev_*` helpers from `DevUiExt`
- custom widgets that have an `egui::Response`: `track_widget(...)`,
  `track_widget_with_meta(...)`, or `track_response(...)`
- painter-drawn geometry, which has no response: `publish_rect_meta(...)`, and
  `publish_rect_container(...)` when the rect is a parent for further rects
- hierarchy: `container(...)`, or `begin_container(...)` when the container
  widget itself is recorded after its contents
- set `WidgetMeta::visible`; it defaults to `false` and an invisible widget is
  hidden from the default lookups
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
widget("item.0.delete"):click()

-- Verify
local remaining = expect("items.count", { value_text_contains = "2" })

-- Report
return { remaining = remaining.value_text }
```

Use `log()` for intermediate diagnostics and return a compact summary at the
end.

By default, `click()` waits until input has drained and a clean post-action
capture is available. Still use `wait_for_widget` or `wait_for` predicates for
state that can update asynchronously or through app work queued after the click.
For lists that filter, reorder, scroll, or virtualize rows, prefer a
viewport-scoped `wait_for_widget` predicate on the expected post-action row.
This keeps the assertion tied to the viewport and tolerates the old row handle
disappearing while the new captured row set is installed.


## Lifecycle

- **NEVER** use `pkill`, `kill`, ctrl-c, or shell commands to manage the app.
- Use `start` to ensure the app is running, `restart` for a fresh process.
- Let Edev own every process it launches. Normal stop and launcher failure clean
  up the complete managed process group; no manual process cleanup should be
  required between runs.
- Treat `.edev-instances/` in the app working directory as Edev-owned live
  launcher and app state. Add it to the app repository's `.gitignore`; never
  commit or edit its generated records.
- Do not delete `.edev-instances/` while an Edev launcher or managed app is
  active. Rely on Edev to remove owned records during normal shutdown and prune
  stale records when the next launcher registers; registry deletion is not
  process management.
- Call `fixture()` at the start of scripts for a known in-app baseline.
- `fixture()` waits for declared readiness anchors on fresh captures; still
  wait/assert the specific widget or viewport state your script depends on.
- Use `fixture_raw()` only for manual or debugging setup flows.


## Choosing A Wait

The wrong wait is the main source of flake.

| The script is waiting for            | Use                              |
| ------------------------------------ | -------------------------------- |
| a widget to exist and be interactable | `w:wait_for_visible()`           |
| a widget to reach a state             | `w:wait_for(predicate)`          |
| a widget to disappear                 | `w:wait_for_absent()`            |
| a scroll area to stabilize            | `w:wait_for_scroll_ready()`      |
| a viewport-wide condition             | `vp:wait_for(predicate)`         |
| queued input to be applied            | `vp:wait_for_settle()`           |
| an app-wide condition                 | `wait_until(predicate)`          |

`wait_for_visible` holds the same condition pointer input requires, so a widget
that satisfies it can be clicked. `scroll_into_view()` already waits for that
condition, so an interaction straight after it needs no manual wait.

`wait_for_frames()` counts frames and proves nothing about content. Reach for it
only when the script genuinely means "let some frames pass".

Every wait takes `WaitOptions` (`timeout_ms`, `poll_interval_ms`, `viewport`)
and every action takes `ActionOptions` (`settle`) or an action-specific
extension of it, in the last argument position.


## Inspection

Prefer programmatic inspection over screenshots:

- Globals resolve handles: `widget(id)`, `try_widget(id)`, `root()`,
  `viewport(...)`. Everything else is a method on the handle you get back:
  `vp:widget_list(...)`, `vp:widget_get(id)`, `w:state()`, `w:children()`,
  `w:parent()`.
- Use `widget(id)` for cross-viewport lookup. Use `viewport({ name = "..." })`,
  `viewport({ title = "..." })`, or `viewport({ title_contains = "..." })`
  when you need a viewport handle; keep names and titles unique because
  ambiguous matches throw.
- `widget_list` is a `Viewport` method: `vp:widget_list({ label = "..." })`,
  `vp:widget_list({ label_contains = "..." })`, `vp:widget_list({ role = "button" })`,
  or `vp:widget_list({ id_prefix = "settings" })` discovers widgets without
  fetching state for every item.
- `wait_for_widget` with predicates for state readiness -- widget existence
  does not imply state readiness.
- `expect(...)`, `expect_absent(...)`, geometry assertions, `expect_text_fits`,
  `expect_tree`, and `expect_painted` for common verification.
- `capture():diff()` to compare widget state before and after an action.
- `vp:check_layout()` or `w:check_layout()` for layout problems. Structure
  resolves against the whole viewport, so a widget-scoped check still
  recognizes an enclosing scroll area, clip region, or container.
- `w:text_measure()` for text sizing and truncation.

Use `vp:screenshot()` or `w:screenshot()` only when the question is genuinely visual: alignment,
clipping, rendering quality, image content. Returned `ImageRef` values produce
image blocks in the MCP response.
On macOS, child viewport screenshots can fall back to native Quartz window capture after the
egui screenshot event path times out; this requires Screen Recording permission and a unique
recorded window title.
Use `vp:sample_pixels(...)` or `w:sample_pixels(...)` for exact fixed-color
assertions; it samples RGBA data
before screenshot JPEG encoding. Prefer `hex` for exact color equality. `rgba`
channels are Luau numbers, so use them for arithmetic thresholds only when that
is clearer than an exact fixed-color check.


## Background Automation

On macOS, a connected Edev session owns temporary automation presentation.
Background is the default and keeps covered windows rendering by making AppKit
report the window visible to winit/eframe. It also keeps the app out of the Dock
and avoids taking focus. This is a local runtime shim, not an upstream eframe
dependency.

- `ViewportState.occluded` is the egui/winit value after that shim.
- `ViewportState.os_occluded` is the real platform occlusion state when observed.
- `ViewportState.os_minimized` is the real platform minimized state when observed.
- Set `[app].presentation = "foreground"` in `.edev.toml`, or use the command's
  `--presentation foreground` override, for manual foreground automation.
- `status` reports the requested presentation and, when available, the observed
  activation policy, executable path, bundle root, and bundle identifier.
- Runtime attachment without a connected client leaves ordinary app presentation
  unchanged, and disconnect restores the policy that preceded the session.

Use `os_occluded` for strict occlusion assertions when it is present. Keep
default smoke and script flows focused on whether frames, widgets, screenshots,
and pixel samples continue to work while the app is covered.

Launch the app's ordinary executable. Do not create per-run `.app` wrappers or
bundle identifiers for automation, and do not add app-owned per-frame activation
assertions that fight an active background session. Edev owns managed-process
lifecycle and should leave no Dock, LaunchServices, or process residue after a
run.


## Smoketest Scripts

Smoketest scripts follow the same shape as good `script_eval` scripts:

- Call `fixture()` before interacting with the UI.
- After `fixture()`, wait for state not covered by the fixture's anchors.
- Assert visible app behavior, not internals.
- Keep scripts independent -- no reliance on state from earlier tests.
- Use assertions for pass/fail and `log()` for diagnostics.
- Treat smoketests as regression tests and executable API documentation.
- Use `edev smoke --list [--json]` while authoring. Repeat `--only GLOB` to
  select more scripts; a script runs when it matches any pattern. Use
  `--repeat N` or `--until-fail N` to hunt flaky settle or rendering behavior,
  and `--bundle` / `--bundle-dir PATH` for failure bundles.


## Iterative Development Workflow

1. **Modify code** -- make changes to the app or its instrumentation.
2. **Lint** -- `cargo xtask tidy` to catch issues before running.
3. **Restart** -- call `restart` to pick up code changes.
4. **Inspect** -- use `script_eval` to explore widget state, verify layout,
   and exercise interactions. Start with `fixtures()` to discover baselines.
5. **Verify** -- write a complete script that sets up, acts, and asserts.
6. **Smoketest** -- run the suite to confirm nothing else broke.

When debugging:
- Use `w:state()` to inspect current role, value, geometry, and
  enabled/visible/focused state. Every state carries its own `(viewport_id, id)`
  pair, so a state a wait returns already names its widget.
- Use `vp:dismiss_popups()` or a fresh `fixture()` when open menus or transient
  focus might leak between actions.
- Use `vp:show_debug_overlay("bounds")` to visualize widget rects.
- Use `vp:check_layout()` to find clipping, overflow, and overlap issues.
- Use `log()` liberally -- logs appear in the script result payload.
