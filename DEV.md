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

## Tests

Run the test lane from `xtask`:

```sh
cargo xtask test
```

Only smoketests launch a real app, so only they put a window on the desktop. Tests in this lane must
stay headless. Lifecycle tests that need a managed app process launch the `edev_test_app` binary,
which serves the app side of the launcher handshake over a reserved loopback endpoint and opens no
window.

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

Repeat `--only` to select more scripts. A script runs when it matches any
pattern, so narrow with one more specific glob rather than with a second flag.

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

Record a macOS demo movie while running the smoke suite:

```sh
cargo run -p edev -- record tmp/spacecurve-smoke.mov -- cargo run --features devtools -- --dev-mcp
cargo run -p edev -- record tmp/spacecurve-smoke.mov --only '*layout*'
```

`edev record OUTFILE ...` is the native macOS recording boundary. It records one selected app
window to a `.mov` file with ScreenCaptureKit direct recording, H.264 video at the window's native
pixel resolution, and no audio. It does not invoke `screencapture`, `ffmpeg`, or another recorder
program. The terminal or process running `edev` needs macOS Screen Recording permission. The root
viewport title selects the window by default; pass `--window-title <TITLE>` to disambiguate or
select another single window.

Recording stays in `edev`. The portable runtime and Luau viewport state do not expose native window
ids or ScreenCaptureKit details. The MVP does not record secondary viewports, microphone audio, or
system audio. Long `--until-fail` runs produce one movie for the whole suite and can create large
files.

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
Use `Widget:sample_pixels()`, `Widget:sample_grid()`, and
`Widget:expect({ painted = ... })` for painter checks. Use
`Viewport:dismiss_popups()` or `eguidev.fixture()` boundaries to isolate transient menus between
tests. Use `eguidev.widget(id)` for a global live reference, or
`eguidev.viewports({ name = "..." })` and `Viewport:widgets(...)` for explicit viewport scope.
Names and exact titles should be unique because ambiguous discovery is an instrumentation fault.
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
can revive it. On macOS, `eguidev_runtime::attach` installs the process-wide observation
hook, but attachment alone leaves native presentation and occlusion behavior unchanged.

When an Edev client connects, its typed presentation setting controls the session:

- `background`, the default, temporarily applies the accessory activation policy,
  deactivates without raising the app, and makes `-[NSWindow occlusionState]` report the
  window visible to winit. Eframe keeps running the UI, painting, and servicing
  screenshots while another window covers the app.
- `foreground` keeps ordinary foreground presentation while retaining Eguidev's real
  window-state observations and automation support. Configure it with
  `presentation = "foreground"` under `[app]` or the command's
  `--presentation foreground` override.
- Disconnect restores the activation policy that preceded the session and disables the
  occlusion shim. A later connection starts a fresh session transition.

The original `occlusionState` and `isMiniaturized` values are always recorded locally.
Script and status surfaces expose them as `ViewportState.os_occluded` and
`ViewportState.os_minimized`. During background automation,
`ViewportState.occluded` is the spoofed egui/winit value; use `os_occluded` when a test
needs the real platform state. Edev status also reports the requested presentation and,
when available, the observed activation policy, executable path, bundle root, and bundle
identifier. These identity fields are diagnostics, not launch requirements.

Edev owns managed-process lifetime as well as presentation. On macOS, losing the owning
launcher terminates the complete app process group without a later cleanup command.
Applications should therefore launch their ordinary executable, keep a stable installed
bundle when they have one, and avoid automation-only or per-run `.app` identities. App
code must not repeatedly force foreground activation while a background session is
connected.

The local occlusion workaround is macOS-specific. Linux and Windows background occlusion
semantics are out of scope until a concrete downstream workflow needs them. Minimized
macOS windows should be treated separately from covered windows: if `os_minimized` is
true and captures stop, scripts should report that automation is paused by minimization
rather than trying to raise the window.
