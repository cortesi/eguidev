# Automation architecture

Eguidev separates app instrumentation, in-process automation, and external process lifecycle.

## Layers and MCP ownership

1. `eguidev` records widget, viewport, fixture, diagnostic, and frame state. It also owns the one
   shared automation condition and domain-error model.
2. `eguidev_runtime` attaches to an instrumented app. It owns strict Luau evaluation, waits,
   actions, screenshots, dumps, and the app MCP server.
3. `edev` launches the app and owns process lifecycle, CLI sessions, smoke suites, recording, and
   failure collection.

The app MCP server lists exactly `script_eval` and `script_api`. The Edev launcher MCP server lists
exactly `start`, `stop`, `restart`, and `status`; it never proxies app tools. Successful lifecycle
results include a descriptor with the launch id, transport, and direct app MCP endpoint. A caller
connects to that endpoint for scripting.

The CLI performs that sequence internally through one app session: register the launcher, start
the process, connect to the returned app endpoint, run the command, and shut down with the original
error taking precedence over cleanup errors.

## Script installation and evaluation

The canonical declaration is `crates/eguidev_runtime/luau/eguidev.d.luau`. `script_api` and
`edev docs` return its exact bytes. Every submitted source is parsed and checked in strict mode
before runtime construction or app side effects.

The trusted private library in `eguidev.luau` receives seven hidden native capability tables in a
fixed order: query, action, wait, capture, fixture, diagnostic, and record. It freezes its local
kernel, constructs the public `eguidev` namespace, and returns that namespace as the one declared
source value. Tenant scripts cannot resolve the hidden registry names and have no filesystem,
network, or module-import access.

Parsing, checking, compilation, execution, and async host calls share one script deadline. A
script failure becomes a structured `ScriptEvalOutcome`, not an MCP protocol failure. Reachable
`ImageRef` values also become MCP image content blocks.

## References and snapshots

`eguidev.widget(id)` and `eguidev.viewport(id)` create immutable live references without reading
state. A viewport-scoped query creates widget references that retain that scope. Reads resolve the
reference against the latest coherent capture; `state()` returns `nil` for an absent target.

Collections and returned records are frozen. Repeated construction of the same reference is
identity-stable within one evaluation. Widget snapshots carry `(viewport_id, id)`, the canonical
identity used by waits, dumps, diffs, observations, and errors.

## Conditions, fixtures, and actions

`WidgetCondition` and `ViewportCondition` are the shared readiness language. Widget waits accept a
condition or a strict Luau predicate. Absence is `{ present = false }`; actionable readiness is
`{ actionable = true }`. `Widget:expect(...)` extends the same condition with relations,
hierarchy, text-fit, and paint checks.

Fixture registration stores precondition and ready `FixtureTargetSpec` values. Rust validates
params and invokes the handler once, then returns values and any dynamic ready targets. The private
Luau library owns orchestration:

1. wait for every precondition;
2. invoke the raw native handler once;
3. unless `wait = false`, observe one fresh frame;
4. merge static and dynamic ready targets and wait for each condition;
5. return values and frozen observations.

High-level actions use the same private wait policy, re-resolve the live target immediately before
queuing input, and auto-settle by default. Raw `Viewport:input(...)` validates and queues one event
without a wait or settlement. This is the boundary for scripts that intentionally control the
frame transition themselves.

## Frame and input pipeline

`FrameGuard` installs an egui input/output plugin for each context. At each pass:

1. the input hook drains queued actions for that viewport and appends egui events;
2. egui processes the frame;
3. the output hook records identity diagnostics;
4. frame completion captures widget and viewport snapshots, applies viewport commands, wakes
   waiters and screenshot requests, and requests another repaint while runtime keep-alive is on.

Settle requires input and viewport commands to drain, an action frame to be processed, a clean
capture, a fresh frame, and the optional app-idle provider to agree. Timeout details identify the
incomplete phases and whether the target viewport produced frames.

## Egui diagnostics and failure bundles

The runtime journals `id_clash` and `rect_changed_id` warnings from completed viewport output.
Each evaluation begins at the current journal tail. Viewport reads and clears dismiss only that
evaluation's entries. Before returning, evaluation waits for diagnostic completion on every
targeted viewport; an earlier script error remains primary if this barrier also fails.

Smoke runs fail on undismissed diagnostics by default. Failure bundles can include the structured
outcome, tree dumps, diagnostics, viewport screenshots, and captured app output. All collection
uses `script_eval`, so the CLI and MCP paths share one automation implementation.

## Platform presentation

On macOS, a connected Edev session owns temporary background or foreground presentation. The
default background mode keeps covered windows rendering without changing ordinary direct app
launches. Child-viewport screenshot fallback uses native window capture and requires Screen
Recording permission. `eframe::Renderer::Glow` remains the recommended automation backend where
idle `wgpu` frame delivery stalls.
