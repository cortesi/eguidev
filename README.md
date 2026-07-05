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
boundary and injects input in step with the real event loop.

Agents talk to the app through MCP, and the agent-facing surface is
[Luau](https://luau.org/) scripts rather than fine-grained RPC: a single
`script_eval` call can inspect widgets, click and type, wait for state changes,
take screenshots, and return structured results in one round trip.

Three pieces make this work:

- **`eguidev`** -- the instrumentation library your app depends on. Compiles
  for native and `wasm32`.
- **`eguidev_runtime`** -- the native-only embedded runtime: script evaluation,
  screenshots, and the in-process MCP server. Attached once at app startup.
- **`edev`** -- the CLI. It launches your app, proxies MCP to the agent, and
  runs scripts, fixtures, and smoketest suites directly.

## Getting started

**1. Instrument your app.** Add `eguidev` as a dependency, create a `DevMcp`
handle, wrap each frame with `FrameGuard`, and tag widgets with the `dev_*`
helpers:

```rust
let _guard = FrameGuard::new(&self.devmcp, &ctx);
ui.dev_text_edit("app.name", &mut self.name);
if ui.dev_button("app.submit", "Submit").clicked() { /* ... */ }
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
```

See [`examples/edev.toml`](./examples/edev.toml) for a commented reference of
all options.

**4. Connect your agent.** Register `edev mcp` as an MCP server -- for
example, with Claude Code:

```sh
claude mcp add eguidev -- edev mcp
```

The agent gets tools to start, stop, and observe the app, plus `script_eval`
to drive it. The repo also ships an agent skill at
[`skills/SKILL.md`](./skills/SKILL.md) that teaches agents the workflow.

## Beyond the MCP server

The same scripting surface powers developer-facing tooling:

- `edev smoke` runs a directory of self-contained `.luau` smoketests against
  the live app -- regression tests that double as executable documentation of
  your UI. Add `--bundle` or `--bundle-dir PATH` to write failure bundles with
  tree dumps, diagnostics, screenshots, script logs, app stderr, and stdout
  notes/logs when the transport leaves stdout available.
- `edev eval` runs a single script and prints the structured result.
- `edev dump` prints a canonical widget tree dump, optionally after applying
  a fixture with `--param key=value` or restricting output to one viewport.
  Without a fixture, it waits for a fresh capture before dumping.
- `edev fixtures` / `edev fixture <name>` list registered fixtures, pass typed
  params with `--param key=value`, optionally skip anchor waits with
  `--no-wait`, and launch the app in a known baseline state for manual testing.
- Apps can register `DevMcp::diagnostic(...)` and `DevMcp::diagnostic_ui(...)`
  providers for structured state that scripts read with `diagnostic(...)`,
  `diagnostics()`, and `wait_until(...)`.

Run `edev --help` for the details.

## Documentation

- Luau scripting API: `edev docs`, or the `script_api` MCP tool. The
  definition file is
  [`eguidev.d.luau`](./crates/eguidev_runtime/luau/eguidev.d.luau).
- Rust API: [docs.rs/eguidev](https://docs.rs/eguidev), including integration
  details for custom widgets, fixtures, and multi-viewport apps.

## License

MIT
