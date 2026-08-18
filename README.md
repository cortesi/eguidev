# eguidev

[![Discord](https://img.shields.io/discord/1381424110831145070?style=flat-square&logo=rust)](https://discord.gg/fHmRmuBDxF)
[![Crates.io](https://img.shields.io/crates/v/eguidev.svg)](https://crates.io/crates/eguidev)
[![Documentation](https://docs.rs/eguidev/badge.svg)](https://docs.rs/eguidev)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

eguidev is an automation library for [egui](https://github.com/emilk/egui) apps.
It is like [Playwright](https://playwright.dev/), but it runs inside your app
process and reads real widget state instead of screen pixels. You tag each
widget with a string id. [Luau](https://luau.org/) scripts then use that id to
inspect the widget, click, type, wait, and take screenshots. Each action runs in
step with the event loop of the app.

Write these scripts as a checked-in smoketest suite, or run one script at a time
during development. You can also give the same surface to an AI agent through
MCP.

Join the [Discord server](https://discord.gg/fHmRmuBDxF) for discussion and
release updates.

## Scripting your UI

A script refers to each widget by the id you gave it. Each action waits until
the widget is ready for input, and then settles the UI before it returns.
Scripts therefore need no sleeps. This example is part of a checked-in smoketest
for the demo app:

<!-- snips: smoketest/10_basic_form.luau#form_flow -->
```lua
eguidev.fixture("basic.empty")

eguidev.widget("basic.name"):type_text("Luau", { clear = true })
eguidev.widget("basic.enabled"):set_value(true)
eguidev.widget("basic.intensity"):set_value(73.25)
eguidev.widget("basic.submit"):click()

local status = eguidev.widget("basic.status")
    :expect({ value_text_contains = "Saved Luau" })
assert(status ~= nil, "submit should update status")
```

Scripts are strict Luau. eguidev checks each script before it runs, and the
sandbox has no filesystem, network, or module imports. eguidev adds one global,
`eguidev`. You can run the same script in three ways:

- **`edev eval script.luau`** runs one script and prints its structured result.
- **`edev smoke`** runs your full suite against one live app.
- **`script_eval` over MCP** lets an agent drive the app with its own scripts.

### What a script can see and do

- **Widget state** -- the role, label, value, geometry, and hierarchy of each
  tagged widget, captured at every frame boundary.
- **Real input** -- clicks, typing, drags, and scrolls, injected into the event
  loop of the app.
- **Waits** -- typed conditions and predicates over live state, plus `settle()`
  for queued work. A bad wait is the main cause of flaky UI automation, so
  eguidev supplies one shared condition model.
- **Assertions** -- `Widget:expect(...)` checks state, relative geometry,
  hierarchy, text fit, and painter output. eguidev records each call.
- **Screenshots and pixels** -- capture one viewport or one widget. Sample exact
  pixels when a visual claim needs proof.
- **Tree dumps** -- canonical widget-tree text for one viewport.
- **Layout diagnostics** -- overlap and clipping defects, text measurement, and
  the identity warnings of egui.
- **Fixtures** -- named states that your app registers. Each script starts from
  a declared baseline.

## Smoketests

`edev smoke` runs a directory of independent `.luau` scripts against one managed
app process. These scripts are regression tests, and they are also executable
documentation of your UI:

```console
$ edev smoke
[PASS] 10_basic_form.luau (412ms)
[PASS] 20_menu_and_scroll.luau (508ms)
[FAIL] 30_input_and_events.luau (1203ms): Widget(basic.status) expectation failed
```

Each script sets its own start state with a fixture, and asserts visible
behavior instead of app internals. No script depends on another script.
`edev smoke` ignores return values: assertions decide pass or fail, and
`eguidev.log(...)` adds evidence to the result.

Use these flags while you write a suite:

- `--list [--json]` shows the selected scripts and does not launch the app.
- `--only GLOB` narrows the run. `--fail-fast` stops at the first failure.
- `--repeat N` and `--until-fail N` find intermittent failures across rounds.
- `--bundle` writes a failure bundle for each failure. A bundle holds the tree
  dump, diagnostics, screenshots, script logs, and app output.

Each script result includes the egui `id_clash` and `rect_changed_id` warnings
that no script dismissed. `edev smoke` fails on these warnings by default, so an
intermittent identity bug cannot pass unnoticed. To keep the warnings without a
failure, set `[smoke] fail_on_egui_diagnostics = false`. A script that causes a
warning on purpose can consume it with `Viewport:egui_diagnostics()`.

`edev record OUT.mov` runs the same suite and records the app window. This
command works only on macOS. It uses ScreenCaptureKit directly, not an external
recorder program, and writes H.264 video at the native pixel resolution of the
window. The process that runs `edev` must have Screen Recording permission. The
title of the root viewport selects the window. Use `--window-title` to select a
different window.

## Agent automation

Register the `edev` launcher as an MCP server. For example, with Claude Code:

```sh
claude mcp add eguidev -- edev mcp
```

The launcher supplies four tools: `start`, `stop`, `restart`, and `status`. A
successful lifecycle result includes the direct endpoint of the app, and the app
serves `script_api` and `script_eval` at that endpoint. The agent surface is a
complete script, not fine-grained RPC, so one call can set up, interact, assert,
and report in one round trip.

This repository also includes an agent skill at
[`skills/SKILL.md`](./skills/SKILL.md) that teaches agents this workflow.

## Setup

eguidev has three crates, and you add them in this order:

- **`eguidev`** is the instrumentation library that your app depends on. It
  compiles for native and `wasm32` targets.
- **`eguidev_runtime`** is the native-only embedded runtime. It supplies script
  evaluation, screenshots, and the in-process MCP server.
- **`edev`** is the CLI and lifecycle launcher. It runs scripts, suites, and
  fixtures against a managed app process.

**1. Instrument your app.** Put a `DevMcp` handle in your app state. Wrap each
viewport frame with `frame_scope`. Tag each widget with a `dev_*` helper.

```rust
eguidev::frame_scope(&self.devmcp, ui, "root", |ui| {
    ui.dev_text_edit("app.name", &mut self.name);
    if ui.dev_button("app.submit", "Submit").clicked() { /* ... */ }
});
```

**2. Attach the runtime** in one bootstrap location.

```toml
[dependencies]
eguidev_runtime = "0.1"
```

```rust
let devmcp = eguidev_runtime::attach(devmcp);
```

`attach` returns the inert handle unless Edev supplies `EGUIDEV_MCP_ADDR`. A
usual `cargo run` therefore starts a usual app, with no server and no change to
presentation.

**3. Configure the launcher.** Install the CLI with `cargo install edev`. Then
put a `.edev.toml` file in your project root:

```toml
[app]
command = ["cargo", "run", "--locked", "-p", "myapp"]

[smoke]
suite_dir = "smoketest"
```

On macOS, the default `background` presentation continues to render covered
windows. It adds no Dock item and does not take the focus. Set
`presentation = "foreground"` for manual sessions. See
[`examples/edev.toml`](./examples/edev.toml) for a commented reference of every
option.

Run `edev` with no arguments to see the full command list.

## Documentation

- Luau scripting: the API reference is
  [`eguidev.d.luau`](./crates/eguidev_runtime/luau/eguidev.d.luau), which
  `edev docs` and the `script_api` MCP tool return without change. For the
  language itself, see [luau.org](https://luau.org/).
- Worked example: the `eguidev_demo` app and its suite in
  [`smoketest/`](./smoketest). See
  [`docs/examples/README.md`](./docs/examples/README.md).

## License

MIT
