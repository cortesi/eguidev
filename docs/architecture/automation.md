# Automation State Machine

This document describes the runtime state machine used by `eguidev` automation calls and
`edev` lifecycle orchestration.

## Layering

The automation stack now has three explicit layers:

1. App instrumentation: always-available `eguidev` APIs such as `DevMcp`,
   `FrameGuard`, `DevUiExt`, widget metadata types, and fixture
   registration/collection.
2. Optional embedded runtime: provided by the native-only
   `eguidev_runtime` crate, attached once through
   `eguidev_runtime::attach()`, and responsible for the in-process MCP server,
   script evaluation, screenshots, and async automation waits.
3. `edev` launcher: external process lifecycle and stable host tool surface
   (`start`, `stop`, `restart`, `status`, `script_eval`, `script_api`).

Default and release builds keep layer 1 only. Dev-capable builds add layer 2
behind one app-local feature boundary such as
`devtools = ["dep:eguidev_runtime"]`, and `edev` sits entirely outside the app
binary.

## Lifecycle and fixture flow (`edev`)

`edev mcp` exposes a fixed host tool list:
`start`, `stop`, `restart`, `status`, `script_eval`, and `script_api`.

Launcher lifecycle:

1. The launcher starts in `not_running`.
2. `start` transitions launcher state to `starting` unless the app is already running.
3. `restart` also transitions to `starting`, but first tears down any running app process.
4. App process spawn, MCP handshake, and a minimal `script_eval` readiness probe complete.
5. Successful startup transitions the launcher to `running`.
6. Failed startup transitions the launcher to `startup_failed` and records startup output.
7. `stop` leaves the launcher in `not_running`. Under the current locking model, a `stop`
   request issued while startup is in flight is serialized behind that transition rather than
   interrupting it.

Tool hosting:

- `script_eval` is proxied to the running app and is the only app-dependent host tool.
- `script_api` is served directly by `edev` from checked-in definitions, so it remains callable
  even while the app is stopped.
- `status` reports the current lifecycle state, startup failure diagnostics, and app frame health
  when a running app client can answer the app-side `health` tool. Health proxy failures are
  reported inside the status payload instead of failing the lifecycle call.
- The host tool list is static for the lifetime of the launcher session; `edev` does not send
  `tools/list_changed` notifications.

Fixtures are applied by scripts via `fixture()`, which auto-settles after application.
For `eframe` apps, the required integration point is `FrameGuard` around rendered frames; the
first `FrameGuard` call registers an egui plugin that injects queued input into every viewport's
pass, so there is no separate raw-input hook for apps to wire up. Fixture handlers registered with
`DevMcp::on_fixture()` run directly through the attached runtime, while frame capture and
wait/screenshot wakeups remain owned by the instrumentation boundary.

Renderer note:

- `eframe::Renderer::Glow` is currently the recommended backend for automation.
- Some `wgpu`-backed `eframe` integrations can stall idle-frame delivery under
  automation waits, screenshots, or fixture transitions. Wait timeout details include target
  viewport frame observations and last-frame age to distinguish state mismatches from repaint
  stalls.

Failure points:
- Build failure before app handshake.
- Script runtime not ready during the startup readiness probe.

## Input pipeline (`eguidev`)

1. Tool calls enqueue `InputAction` and `ViewportCommand` events into `ActionQueue`.
2. The `InputInjectionPlugin` egui plugin's `input_hook` drains queued actions for the pass's
   viewport and appends egui events, running inside `Context::begin_pass` for every viewport.
3. Frame processing consumes injected egui events.
4. `end_frame` captures widget/input snapshots, applies viewport commands, invokes the attached
   runtime hooks for frame waiters, screenshot capture, and fixture wakeups, and requests the next
   immediate repaint when runtime keep-alive is enabled.

Keyboard delivery modes:
- Ambient (`key`, `input_key`): routed through normal focus state.
- Targeted (`key` with `target` option): resolve target, focus handshake, then delivery.

## Wait loops (`eguidev`)

### `wait_for_widget`

- Polls widget resolution + condition matching.
- Supports explicit poll interval and timeout.
- Condition may be a map (strict, typed keys) or a Luau predicate function.

### `wait_for_settle`

A single composite check combining:
- **InputSettled**: no pending input/command queue and input snapshot available.
- **RepaintIdle**: no repaint requested for viewport.

Both conditions must hold simultaneously. Returns a structured result with `matched` and
`elapsed_ms`.

### Auto-settle

All high-level actions (`click`, `type_text`, `key`, `hover`, `drag`, `scroll`, `paste`, etc.)
auto-settle after performing the action, ensuring the UI has fully processed queued work and
repainted. Disable with `settle: #{enabled: false}`.

## Visual Assertions

`Viewport:sample_pixels(...)` captures one viewport image and samples all requested egui logical
points from the exact `ColorImage` before screenshot JPEG encoding. Scripts use this for fixed-color
or painter-only assertions. Use `hex` for exact color equality; `rgba` channels are script-facing
numbers and can be mixed with geometry in arithmetic threshold checks. Painter-only regions can be
published with
`eguidev::publish_rect_meta(ui, id, rect, meta)`, which transforms the rect through the current
layer and records it as enabled and unfocused by default.
