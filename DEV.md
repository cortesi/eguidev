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

Run the full smoke suite with the root viewport occluded:

```sh
cargo xtask smoke-occlusion
```

Smoke scripts should prefer semantic waits and exact visual assertions over frame sleeps.
Use `Viewport:sample_pixels()` for fixed-color painter checks, and use
`Viewport:dismiss_popups()` or `fixture()` boundaries to isolate transient menus between tests.

## Background automation (occluded windows)

Stock eframe 0.35 stops running `App::ui` and painting when a window is minimized or
occluded (`ViewportInfo::visible()` gates `run_ui`), so automation would freeze as soon
as the developer's windows fully cover an instrumented app, and no `request_repaint()`
can revive it. `eguidev_runtime::attach` therefore installs two macOS process tweaks:

- `-[NSWindow occlusionState]` is replaced to always report the window visible, so winit
  never emits `Occluded(true)` and eframe keeps running the UI, painting, and servicing
  screenshots in a fully covered background window.
- The app is demoted to the accessory activation policy and deactivated, so launching an
  instrumented app does not raise its window or steal the developer's focus.

Both tweaks apply only when automation is attached (`--dev-mcp` style runs) and can be
disabled by setting `EGUIDEV_FOREGROUND` in the app environment. Never work around an
occlusion stall by raising the app window; developers keep using the machine while
automation runs. The long-term plan is an upstream eframe `NativeOptions` switch to run
the UI for occluded windows, which would replace the occlusion tweak.
