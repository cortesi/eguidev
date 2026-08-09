# DevMCP demo app

This folder documents the demo app used to exercise the DevMCP scripting surface and the
matching `mcp.json` configurations under `docs/examples/mcp/`.

## Prerequisites

- Build or install the `edev` launcher (`cargo build --locked -p edev` or
  `cargo install --path crates/edev`).
- The demo's dev-capable build uses the app-local `devtools` feature, which
  enables the optional `eguidev_runtime` dependency and calls
  `eguidev_runtime::attach()` when launched with `--dev-mcp`.
- This repository checks in a root `.edev.toml`, so `edev mcp` and `edev smoke`
  work without repeating the demo command line.

## eguidev_demo

One demo now covers both surfaces:

- The root viewport exercises instrumented text edits, slider, checkbox, menu, links, images, and
  the primary scroll area.
- A secondary viewport stays open by default and exercises multi-viewport enumeration, instrumented
  rows, scrolling, and a draggable region.

Run directly:

```sh
cargo run --locked -p eguidev_demo --features devtools -- --dev-mcp
```

Run through `edev`:

```sh
cargo run --locked -p edev -- mcp
```

MCP config example: `docs/examples/mcp/eguidev_demo.json`.

## Script behavior notes

- `Viewport:widgets()` omits clipped or hidden widgets unless `include_invisible` is `true`.
- Widget and viewport references are immutable and live. Read current fields through `state()`,
  which returns `nil` while the target is absent.
- Use `wait_viewport()` for one semantic viewport and `Viewport:widget()` for an exact scoped
  widget handle.
- `Widget:drag_to()` accepts a widget reference or absolute egui position.
  `Widget:drag_relative()` accepts a normalized target and optional normalized `from` position.
- `Viewport:resize()` is best-effort; window managers may clamp or ignore it. Verify the resulting
  `ViewportState` with `Viewport:wait(...)`.
- Raw `Viewport:input(...)` queues one event without waiting. High-level actions auto-settle.

## Smoke tests

Checked-in smoke coverage now has two layers:

- `edev smoke` runs the checked-in Luau smoketest suite.
- `edev smoke --list [--json]` prints the discovered script set without
  launching the demo. Add repeatable `--only GLOB` filters to narrow discovery;
  globs match the forward-slash display path, so `*` may match across
  directory separators.
- `edev smoke --verbose` keeps the same suite but also emits the extra suite summary and launcher
  output that are useful when debugging failures.
- Smoke scripts fail by default when they leave egui `id_clash` or `rect_changed_id` diagnostics
  undismissed. Use `Viewport:egui_diagnostics()` to inspect and dismiss expected warnings, or use
  `Viewport:clear_egui_diagnostics()` to dismiss them without returning them.
- `edev smoke --repeat N` runs the selected set multiple times against one app
  process. `edev smoke --until-fail N` uses the same repeated mode but stops at
  the first failure, up to `N` rounds. The suite timeout covers the whole
  invocation unless `--suite-timeout-secs` is provided.
- `edev smoke --bundle` writes failure bundles under `tmp/edev-bundles`, or use
  `edev smoke --bundle-dir PATH` to choose the directory. Each failed script gets a deterministic
  directory with `meta.json`, `failure.txt`, full tree dumps, diagnostics, viewport screenshots,
  app stderr, and captured app stdout. Repeated runs include the round in the bundle directory and
  `meta.json`.
- `edev smoke smoketest/*.luau` or `edev smoke path/to/ad_hoc_probe.luau` runs explicit scripts in
  the order provided, so you can use normal shell expansion or quick one-off probes outside the
  configured suite.
- This repository also keeps a smaller transport-only smoke as a local maintenance task via
  `cargo xtask smoke-edev`.

The suite still requires a GUI-capable environment. The demo uses the native eframe windowing
stack, so this is not a headless smoke path.

Record manual verification runs here when needed.

- 2026-01-23: Launched the demo app via
  `cargo run --locked -p eguidev_demo --features devtools -- --dev-mcp` using the glow renderer. The process was manually
  interrupted because this environment does not support interactive GUI verification.
- 2026-01-25: Used `script_eval` against the demo app to call
  `secondary:resize()` (640x480 then 900x700). `secondary:wait()` matched and
  `secondary:state()` reported updated inner and outer sizes.
