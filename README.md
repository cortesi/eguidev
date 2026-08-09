# eguidev

[![Discord](https://img.shields.io/discord/1381424110831145070?style=flat-square&logo=rust)](https://discord.gg/fHmRmuBDxF)
[![Crates.io](https://img.shields.io/crates/v/eguidev.svg)](https://crates.io/crates/eguidev)
[![Documentation](https://docs.rs/eguidev/badge.svg)](https://docs.rs/eguidev)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Like [Playwright](https://playwright.dev/) for [egui](https://github.com/emilk/egui)
apps. eguidev lets AI agents drive your UI end-to-end -- inspecting widget
state, injecting input, taking screenshots of windows or individual widgets --
all from inside the process with no pixel guessing.

Join the [Discord server](https://discord.gg/fHmRmuBDxF) for discussion and
release updates.

## How it works

eguidev instruments your app from the inside. You tag widgets with string ids,
and eguidev captures their state (role, label, value, geometry) at every frame
boundary and injects input in step with the real event loop. It also captures
egui debug identity warnings from each completed viewport pass.

Agents talk to the app through MCP, and the agent-facing surface is
[Luau](https://luau.org/) scripts rather than fine-grained RPC: a single
`script_eval` call can inspect widgets, click and type, wait for state changes,
take screenshots, and return structured results in one round trip.

Three pieces make this work:

- **`eguidev`** -- the instrumentation library your app depends on. Compiles
  for native and `wasm32`.
- **`eguidev_runtime`** -- the native-only embedded runtime: script evaluation,
  screenshots, and the in-process MCP server. Attached once at app startup.
- **`edev`** -- the CLI and lifecycle launcher. It starts the app, reports its
  direct MCP endpoint, and runs scripts, fixtures, and smoketest suites.

## Getting started

**1. Instrument your app.** Add `eguidev` as a dependency, create a `DevMcp`
handle, wrap each viewport frame with `frame_scope`, and tag widgets with the
`dev_*` helpers:

```rust
eguidev::frame_scope(&self.devmcp, ui, "root", |ui| {
    ui.dev_text_edit("app.name", &mut self.name);
    if ui.dev_button("app.submit", "Submit").clicked() { /* ... */ }
});
```

**2. Attach the runtime.** Put `eguidev_runtime` behind an app feature and
enable it in one bootstrap location:

```toml
[features]
devtools = ["dep:eguidev_runtime"]
```

```rust
let devmcp = eguidev_runtime::attach(devmcp);
```

The `DevMcp` handle is inert until the runtime is attached, so widget code
stays unconditional and `wasm32` builds are unaffected.

**3. Tell `edev` how to launch your app.** Install the CLI with
`cargo install edev`, then drop a `.edev.toml` next to your project with the
full launch command:

```toml
[app]
command = ["cargo", "run", "-p", "myapp", "--features", "devtools"]
# presentation = "background" # default; use "foreground" for manual sessions
```

See [`examples/edev.toml`](./examples/edev.toml) for a commented reference of
all options.

On macOS, presentation belongs to the connected Edev session. The default
`background` mode keeps covered windows rendering without adding a Dock item or
stealing focus; `foreground` preserves ordinary foreground presentation for
manual automation. Attaching `eguidev_runtime` alone does not change activation
policy or occlusion behavior, so the same executable remains suitable for a
normal direct launch. Edev also owns managed-process cleanup, including when the
launcher exits unexpectedly; applications should not create per-run `.app`
bundles or identities for automation.

**4. Connect your agent.** Register `edev mcp` as an MCP server -- for
example, with Claude Code:

```sh
claude mcp add eguidev -- edev mcp
```

The launcher gives the agent `start`, `stop`, `restart`, and `status`. A
successful lifecycle result contains the direct app MCP endpoint, where the app
exposes `script_api` and `script_eval`. The repo also ships an agent skill at
[`skills/SKILL.md`](./skills/SKILL.md) that teaches agents the workflow.

## Beyond the MCP server

The same scripting surface powers developer-facing tooling:

- `edev smoke` runs a directory of self-contained `.luau` smoketests against
  the live app -- regression tests that double as executable documentation of
  your UI. Use `--list [--json]` to inspect the selected set without launching
  the app, `--only GLOB` to filter discovered scripts, `--repeat N` or
  `--until-fail N` to hunt intermittent failures, and `--bundle` or
  `--bundle-dir PATH` to write failure bundles with tree dumps, diagnostics,
  screenshots, script logs, app stderr, and stdout notes/logs when the transport
  leaves stdout available.
- `edev record OUTFILE ...` runs the same smoke suite while recording one native
  app window to a `.mov` file. Recording is macOS-only, uses ScreenCaptureKit
  directly rather than an external recorder program, requires Screen Recording
  permission for the terminal or process running `edev`, captures H.264 video at
  the window's native pixel resolution, and keeps native recording details out
  of the runtime and Luau APIs. The root viewport title selects the window by
  default; use `--window-title <TITLE>` when you need another single window.
- `edev eval` runs a single script and prints the structured result.
- `edev dump` prints a canonical widget tree dump, optionally after applying
  a fixture with `--param key=value` or restricting output to one viewport.
  Without a fixture, it waits for a fresh capture before dumping.
- `edev fixtures` / `edev fixture <name>` list registered fixtures, pass typed
  params with `--param key=value`, optionally skip anchor waits with
  `--no-wait`, and launch the app in a known baseline state for manual testing.
- Scripts use the single `eguidev` namespace, immutable widget and viewport
  references, shared conditions, `Widget:expect(...)`, and capture diffs.
- Each script result includes undismissed egui `id_clash` and
  `rect_changed_id` warnings. `edev smoke` fails on these warnings by default,
  so transient identity errors cannot pass unnoticed. Set
  `[smoke] fail_on_egui_diagnostics = false` to retain the warnings without
  failing the script.
- Scripts that intentionally produce a warning can inspect and dismiss it with
  `Viewport:egui_diagnostics()`, or dismiss it without returning it with
  `Viewport:clear_egui_diagnostics()`. Both methods wait for the completed
  output of the target viewport.
- Apps can register `DevMcp::diagnostic(...)`, `DevMcp::diagnostic_ui(...)`,
  fixture catalogs, and runtime/UI-thread fixture handlers. Scripts read them
  with `eguidev.diagnostic(...)`, `eguidev.diagnostics()`, and
  `eguidev.fixtures()`.

Run `edev --help` for the details.

## Documentation

- Luau scripting API: `edev docs`, or the `script_api` MCP tool. The
  definition file is
  [`eguidev.d.luau`](./crates/eguidev_runtime/luau/eguidev.d.luau).
- Rust API: [docs.rs/eguidev](https://docs.rs/eguidev), including integration
  details for custom widgets, fixtures, and multi-viewport apps.

## License

MIT
