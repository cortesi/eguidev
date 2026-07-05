# Development Notes

## Documentation Comments in `.d.luau`

Good doc comments explain *why* and *how*, not *what* — the signature already says what.

- **Never recapitulate the signature.** Don't restate parameter names, types, or return
  types that are visible from the declaration itself. Instead, explain behavior that isn't
  obvious from the types alone: semantics, side effects, defaults, interactions between
  fields, and edge cases.
- **Never include trivial examples.** If the usage is obvious from the signature and the
  comment, an example adds nothing. Only include examples when they clarify a non-obvious
  interaction or a surprising calling convention.

## Smoketests

Run the default smoke wrapper from `xtask`:

```sh
cargo xtask smoke
```

That command runs the checked-in Luau smoketest suite through `edev smoke` and prints one line per
test by default.

List or narrow the discovered suite without launching the demo:

```sh
cargo xtask smoke --list
cargo xtask smoke --list --json --only '*visual*'
```

Run repeated rounds against one app process when chasing intermittent behavior:

```sh
cargo xtask smoke --only '*visual*' --repeat 5
cargo xtask smoke --until-fail 50
```

Enable extra smoketest debugging output:

```sh
cargo xtask smoke --verbose
```

Verbose smoke runs keep diagnostics in terminal output. `edev smoke` no longer writes a persistent
artifact directory by default.

Run the separate `edev mcp` transport smoke:

```sh
cargo xtask smoke-edev
```

Enable verbose transport logging:

```sh
cargo xtask smoke-edev --verbose
```

`edev mcp` keeps the launcher alive for the lifetime of the initialized stdio client. The
`mcp.idle_shutdown_after_secs` setting is a pre-client guard for abandoned launches: if no
client initializes before the idle window elapses, the launcher exits and cleans up. Once
the client is attached, stdin EOF or a process signal owns shutdown, and the `status` tool
reports `idle_shutdown.state = "suspended_while_client_attached"`. Pre-initialize
`list_tools` probes do not extend the guard; `initialize` is the client lifetime boundary.

Run the full smoke suite with the root viewport occluded:

```sh
cargo xtask smoke-occlusion
```

Smoke scripts should prefer semantic waits and exact visual assertions over frame sleeps.
Use `Widget:sample_pixels()`, `Widget:sample_grid()`, and `expect_painted()` for painter checks,
and use `Viewport:dismiss_popups()` or `fixture()` boundaries to isolate transient menus between
tests. Use `widget(id)` for cross-viewport lookup, or `viewport({ name = "..." })`,
`viewport({ title = "..." })`, or `viewport({ title_contains = "..." })` when you need a viewport
handle. Names and exact titles should be unique because ambiguous matches throw.
Use `hex` for exact color equality; `rgba` channels and geometry values are script-facing
numbers and can be mixed in arithmetic when a visual threshold is clearer than a fixed color.
On macOS, child-viewport screenshots fall back to Quartz window capture after a fresh child
frame fails to fulfill the normal egui screenshot event. The fallback needs a recorded
window title match and macOS Screen Recording permission; root screenshots still use the
egui event path directly.

Run one diagnostic script and keep its return value/images with:

```sh
cargo run -p edev -- eval tmp/probe.luau --out-dir tmp/probe-output --arg name=Sky
```

`edev eval` launches a one-shot app process from the configured `[app]` command, uses the
same `script_eval` engine as smoke scripts, prints the structured outcome JSON to stdout,
exits non-zero on script failure, and writes returned `ImageRef` JPEGs to the script
directory or `--out-dir`. It uses `[smoke].script_timeout_secs` and `[smoke].args` as
defaults when the matching eval CLI flags are omitted, then shuts the app down after the
eval; it does not attach to an already-running `edev mcp` app.

## Background automation (occluded windows)

Stock eframe 0.35 stops running `App::ui` and painting when a window is minimized or
occluded (`ViewportInfo::visible()` gates `run_ui`), so automation would freeze as soon
as the developer's windows fully cover an instrumented app, and no `request_repaint()`
can revive it. `eguidev_runtime::attach` therefore installs two macOS process tweaks:

- `-[NSWindow occlusionState]` is replaced to always report the window visible, so winit
  never emits `Occluded(true)` and eframe keeps running the UI, painting, and servicing
  screenshots in a fully covered background window.
- The original `occlusionState` and `isMiniaturized` values are still recorded locally.
  Script and status surfaces expose them as `ViewportState.os_occluded` and
  `ViewportState.os_minimized`. On macOS automation runs, `ViewportState.occluded` is
  the spoofed egui/winit value; use `os_occluded` when a test needs the real platform
  state.
- The app is demoted to the accessory activation policy and deactivated, so launching an
  instrumented app does not raise its window or steal the developer's focus.

Both tweaks apply only when automation is attached (`--dev-mcp` style runs) and can be
disabled by setting `EGUIDEV_FOREGROUND` in the app environment. Never work around an
occlusion stall by raising the app window; developers keep using the machine while
automation runs.

The local occlusion workaround is macOS-specific. Linux and Windows background occlusion
semantics are out of scope until a concrete downstream workflow needs them. Minimized
macOS windows should be treated separately from covered windows: if `os_minimized` is
true and captures stop, scripts should report that automation is paused by minimization
rather than trying to raise the window.
