---
name: eguidev
description: MCP workflow for driving instrumented egui apps with strict Luau scripts.
---

# eguidev

Eguidev drives an instrumented egui app through self-contained strict Luau scripts. This skill is
workflow guidance. Always read `script_api` before writing a script; when only the CLI is
available, `edev docs` returns the same canonical declaration bytes.

## MCP boundaries

The Edev launcher exposes four lifecycle tools: `start`, `stop`, `restart`, and `status`. A
successful lifecycle result includes the direct app connection descriptor. Connect to its endpoint
to use the app-owned `script_api` and `script_eval` tools. Do not expect the launcher to proxy app
tools.

Do not change user-wide MCP configuration for one repository. Put an Edev launcher declaration in
the trusted project configuration:

```toml
[mcp_servers.my-app-edev]
command = "edev"
args = ["mcp"]
```

Edev locates `.edev.toml` by walking upward to the repository root. Set `cwd` only when the MCP
configuration lives outside the app repository.

The CLI manages the direct connection automatically:

- `edev docs` prints the canonical Luau declaration.
- `edev fixtures` lists fixture names, conditions, params, and tags.
- `edev fixture NAME --param key=value` applies one fixture.
- `edev dump --fixture NAME` prints a baseline widget tree.
- `edev eval PATH` evaluates one script.
- `edev smoke` runs independent checked-in scenarios against one managed app.

## Instrumentation

Widget ids are canonical selectors. Explicit ids must be unique within a captured frame. Generated
ids are opaque and should not be relied on across restarts.

For Rust signatures, inspect the current `eguidev` and `eguidev_runtime` APIs. Prefer the existing
instrumentation boundaries:

- `DevUiExt::dev_*` for standard widgets;
- `track_widget`, `track_widget_with_meta`, or `track_response` for custom widgets with a response;
- `publish_rect_meta` for painter-only geometry;
- `container` or `begin_container` for hierarchy;
- `take_widget_value_override` before rendering a custom value-controlled widget.

## Complete scripts

Each `script_eval` source must perform setup, action, verification, and reporting in one
evaluation. Eguidev contributes one global namespace, `eguidev`; do not use legacy globals.

```luau
eguidev.fixture("basic.with_items")

local delete = eguidev.widget("item.0.delete")
delete:click()

local remaining = eguidev.widget("items.count"):expect({ value_text_contains = "2" })
assert(remaining ~= nil)
return { remaining = remaining.value_text }
```

Use `eguidev.log(...)` for intermediate evidence and return a compact structured summary. The
sandbox has no filesystem, network, or module-import access.

## References and state

`eguidev.widget(id)` and `eguidev.viewport(id)` construct immutable live references without a
lookup. Call `state()` for the latest snapshot and handle `nil` explicitly. A state carries its
own `(viewport_id, id)` identity.

Use scoped discovery when the same widget id can occur in more than one viewport:

```luau
local secondary = eguidev.wait_viewport({ name = "secondary" })
local rows = secondary:widgets({ id_prefix = "row." })
```

Prefer programmatic inspection:

- `w:state()`, `w:parent()`, and `w:children()` for one widget;
- `eguidev.widgets(...)`, `vp:widgets(...)`, and `vp:widgets_at(...)` for discovery;
- `eguidev.dump(...)` or `eguidev.dump_text(...)` for a tree;
- `before:diff(...)` for before/after state;
- `vp:layout_issues()` or `w:layout_issues()` for layout defects;
- `w:text_measure()` for text layout;
- pixel samples for exact visual checks;
- screenshots only when the question is genuinely visual.

Return `ImageRef` values when the MCP response should include image blocks.

## Conditions and waits

The wrong wait is the main source of flaky automation. Use the shared condition model:

| Need | Call |
| --- | --- |
| widget is interactable | `w:wait({ actionable = true })` |
| widget reaches state | `w:wait({ ... })` or `w:wait(predicate)` |
| widget disappears | `w:wait({ present = false })` |
| scroll area stabilizes | `w:wait({ scroll_ready = true })` |
| viewport reaches state | `vp:wait({ ... })` or `vp:wait(predicate)` |
| queued high-level work settles | `vp:settle()` |
| app-wide predicate becomes true | `eguidev.wait(predicate)` |

Every wait accepts `{ timeout_ms, poll_interval_ms }`. Predicates receive optional state, so test
for `nil`. Use `eguidev.wait_frames(...)` only when elapsed frames are themselves the requirement.

`Widget:expect(...)` supports state conditions plus relations, hierarchy, text-fit, and painter
checks. Prefer it over hand-coded geometry when the declaration provides the needed expectation.

## Actions and input

High-level widget and viewport actions wait for their target and auto-settle. Disable settlement
only to inspect an intentional intermediate state. `scroll_into_view()` leaves the widget ready for
the next interaction.

`Viewport:input(...)` is different: it validates and queues exactly one raw input event without a
wait or settle. Follow it with an explicit state wait or `vp:settle()` when needed.

## Fixtures

Call a fixture without preconditions at the start of an independent scenario; it is a baseline.
A fixture with preconditions is a transition, so establish its declared entry state first.
Preconditions always run. By default the fixture then waits for one fresh frame and all declared
ready conditions. Pass `{ wait = false }` only for intentional intermediate-state inspection; this
skips ready waits, not preconditions.

Inspect `eguidev.fixtures()` to discover names and typed params. A fixture result contains handler
values and precondition/ready observations with live widget references and optional state.

## Lifecycle and smoke tests

- Never use `pkill`, shell `kill`, or manual process cleanup for an Edev-managed app.
- Use lifecycle `restart` after code changes and reconnect through the new descriptor.
- Treat `.edev-instances/` as live Edev-owned state; never commit, edit, or delete it while a
  launcher is active.
- Keep smoke scripts independent and begin with a fixture.
- Assert visible behavior, not app internals.
- For Cargo-backed apps, include `--locked` in the launch command so smoke runs fail on dependency
  drift instead of rewriting the tracked lockfile.
- Use `edev smoke --list`, `--only`, `--repeat`, and `--until-fail` while authoring.
- Enable failure bundles when durable screenshots, dumps, diagnostics, and app output are useful.

On macOS, the Edev session owns temporary background or foreground presentation. Launch the normal
app executable and do not create per-run app wrappers. Child-viewport native screenshot fallback
requires Screen Recording permission.

## Development loop

1. Change the app or instrumentation.
2. Run the repository's tidy gate.
3. Restart through Edev and reconnect to the returned app endpoint.
4. Inspect fixtures and live state.
5. Run one complete script with setup, action, and assertions.
6. Run the relevant smoke suite.
