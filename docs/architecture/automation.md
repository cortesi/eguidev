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
   script evaluation, screenshots, canonical widget tree dumps, and async
   automation waits.
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
- `script_api` proxies to the running app so app preludes are visible; while the app is stopped,
  `edev` serves the checked-in definitions directly.
- `status` reports the current lifecycle state, startup failure diagnostics, and app frame health
  when a running app client can answer the app-side `health` tool. Health proxy failures are
  reported inside the status payload instead of failing the lifecycle call.
- The host tool list is static for the lifetime of the launcher session; `edev` does not send
  `tools/list_changed` notifications.

Fixtures are applied by scripts via `fixture(name, params?)`, which validates typed params,
waits for static and handler-returned anchors, and returns handler values. Scripts can also call
`dump()` / `dump_text()` to capture the current widget tree across live viewports. The `edev dump`
command launches the app, optionally applies a fixture with `--param` values, waits for a fresh
capture when no fixture is applied, and then evaluates those same helpers, so command-line dumps
and script dumps share one runtime implementation.
Apps can register runtime-thread and UI-thread diagnostics through `DevMcp::diagnostic(...)` and
`DevMcp::diagnostic_ui(...)`. Scripts read those providers with `diagnostic(name)` for one payload
or `diagnostics()` for an all-provider snapshot whose provider errors are captured in an `errors`
table. The Luau prelude supplies `wait_until(predicate, options?)` for diagnostic-based polling.

Failure bundles:

- `edev smoke --bundle` writes bundles under the configured `[smoke] bundle_dir`, defaulting to
  `tmp/edev-bundles`; `--bundle-dir PATH` enables bundles and chooses the directory explicitly.
- Each failed script gets a deterministic
  `<safe-script-display-path>-<relative-path-hash>` directory that is overwritten on the next
  run of the same script.
- Bundles include `meta.json`, `failure.txt`, `tree.json`, `tree.txt`, `diagnostics.json`, one
  `viewport-*.jpg` per captured viewport, `app.stderr.log`, and `app.stdout.log`.
- `app.stdout.log` contains captured stdout only when stdout is not reserved for the stdio MCP
  transport; current stdio launches write an explanatory note instead.
- If post-failure collection fails, the bundle keeps the original failure files and records the
  collection problem in `collection-error.txt`.

For `eframe` apps, the required integration point is `FrameGuard` around rendered frames; the
first `FrameGuard` call registers an egui plugin that injects queued input into every viewport's
pass, so there is no separate raw-input hook for apps to wire up. Runtime-thread fixture handlers
registered with `DevMcp::on_fixture_runtime()` run through the attached runtime; UI-thread handlers
registered with `DevMcp::on_fixture_ui()` are queued and drained before the root registry is cleared
for the next frame. Frame capture and wait/screenshot wakeups remain owned by the instrumentation
boundary.

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

Returns a `SettleReport` with `settled`, `elapsed_ms`, and per-phase status. All phases must be
complete simultaneously:

- **input_drained**: no pending input actions remain for the target viewport.
- **commands_drained**: no pending viewport commands remain.
- **action_frame_processed**: a frame has run after the latest drained input action.
- **clean_capture**: a capture newer than the action drain has been observed.
- **fresh_frame**: the wait observed a new frame or capture after it started.
- **app_idle**: the optional app idle hook reports idle.

Apps register idle hooks with `DevMcp::on_idle(...)` for runtime-thread state or
`DevMcp::on_idle_ui(...)` for UI-thread state. UI idle runs at root frame end and the runtime
settle loop reads the cached result. Settle timeout details include `phases`, so failures identify
the exact phase that remained incomplete.

### Auto-settle

All high-level actions (`click`, `type_text`, `key`, `hover`, `drag`, `scroll`, `paste`, etc.)
auto-settle after performing the action, ensuring the UI has fully processed queued work and
repainted. Disable with `{ settle = false }`.

Pointer actions fail fast with `invisible_interaction` when the target widget is hidden or fully
clipped. Scripts should wait for visibility explicitly or call `scroll_into_view()` before
interacting with content that may be outside the viewport.

## Visual Assertions

`Viewport:sample_pixels(...)` captures one viewport image and samples all requested egui logical
points from the exact `ColorImage` before screenshot JPEG encoding. Scripts use this for fixed-color
or painter-only assertions. Use `hex` for exact color equality; `rgba` channels are script-facing
numbers and can be mixed with geometry in arithmetic threshold checks. Painter-only regions can be
published with
`eguidev::publish_rect_meta(ui, id, rect, meta)`, which transforms the rect through the current
layer and records it as enabled and unfocused by default.
