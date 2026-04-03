# eguidev

[![Discord](https://img.shields.io/discord/1381424110831145070?style=flat-square&logo=rust)](https://discord.gg/fHmRmuBDxF)
[![Crates.io](https://img.shields.io/crates/v/eguidev.svg)](https://crates.io/crates/eguidev)
[![Documentation](https://docs.rs/eguidev/badge.svg)](https://docs.rs/eguidev)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

AI-assisted development tooling for [egui](https://github.com/emilk/egui).
`eguidev` handles cross-target instrumentation inside your app,
`eguidev_runtime` adds the native-only embedded automation runtime, and `edev`
manages lifecycle plus the external MCP tool surface.

Join the [Discord server](https://discord.gg/fHmRmuBDxF) for discussion and
release updates.

## How It Works

eguidev instruments your app from inside the process. It captures widget state
at frame boundaries and injects input through egui's `raw_input_hook` before
events are consumed. Automation stays aligned with the real event loop without
pixel guessing.

The agent-facing surface is Luau, not fine-grained RPC. On native builds,
`eguidev_runtime` runs scripts inside the app process against the latest
captured frame, so one script can inspect widgets, queue input, wait for state
changes, and return structured results in a single round trip.

## Structure

- **`eguidev` crate** -- instrument your egui app. Tag widgets with `dev_*`
  helpers, wrap frames with `FrameGuard`, forward raw input. This is the
  cross-target instrumentation layer and remains valid for `wasm32`.
- **`eguidev_runtime` crate** -- native-only embedded runtime. Attach it once
  in app bootstrap code to enable script evaluation, screenshots, smoketests,
  and the in-process MCP server.
- **`edev` binary** -- the MCP launcher. Starts and stops the app, proxies
  `script_eval`, serves the Luau API definition. Run it with
  `edev mcp -- <cargo args>`.

## Build modes

- **Instrumentation only**: depend on `eguidev` alone. This works for native
  and `wasm32` targets.
- **Native embedded runtime**: add an app-local feature such as
  `devtools = ["dep:eguidev_runtime"]`, then call
  `eguidev_runtime::attach(devmcp)` in one bootstrap location. Keep widget code
  unconditional.

## Configuration

`edev` reads `.edev.toml` from the current directory upward, stopping at the
nearest git root. All subcommands share this config; CLI flags override file
values. See [`examples/edev.toml`](./examples/edev.toml) for a commented
reference with all options.

The only required field is `app.command` -- the full argv to launch the app
with DevMCP enabled. `edev` does not synthesize cargo flags. The command can
also be passed after `--` on any subcommand.

## Fixtures

List registered fixtures or launch the app from a known baseline for manual
testing:

```sh
edev fixtures                  # start app, print fixtures, exit
edev fixture basic.default     # start app, apply fixture, keep running
```

`edev fixture` applies the named fixture and blocks until ctrl-c. Use it to
get the app into a repeatable state for interactive work.

## Smoketests

eguidev includes a built-in smoketest runner. A smoketest suite is a directory
of self-contained `.luau` scripts. The configured suite is discovered
recursively and executed in lexicographic order by relative path. Explicit
script arguments to `edev smoke` run in the order provided. Every script
establishes its own state via `fixture()`, exercises the UI, and asserts
outcomes.

```sh
edev smoke
edev smoke --verbose
edev smoke smoketest/*.luau
edev smoke smoketest/10_basic.luau tmp/ad_hoc_probe.luau
```

Smoketests run against the live app through the same `script_eval` path agents
use. They double as regression tests and as executable documentation for your
scripting surface.

## API Reference

The canonical scripting reference is
[`eguidev.d.luau`](./crates/eguidev_runtime/luau/eguidev.d.luau) -- a strict
Luau type definition covering viewports, widgets, actions, waits, fixtures,
and assertions. Fetch it at any time with `script_api` or `edev --script-docs`.

For the Rust API, run `ruskel eguidev` or see the crate-level doc comments.

The repo also ships a ready-to-use agent skill at
[`skills/SKILL.md`](./skills/SKILL.md). For Codex-style setups, install it by
copying or symlinking it into your local skills directory as
`~/.codex/skills/eguidev/SKILL.md`, then invoke the `eguidev` skill in your
agent workflow.

## License

MIT
