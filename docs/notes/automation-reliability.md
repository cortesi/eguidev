# Automation Reliability Notes

## Summary

This document records the design decisions behind automation reliability in eguidev.
The design goal is deterministic scripting behavior with typed, diagnosable failures.

## Resolved failure modes

1. Fixture execution
- Fixtures are applied by scripts via `fixture()` with auto-settle.
- Restart is fixture-agnostic; it restarts the app and returns phase timing.

2. `wait_for_widget` predicate safety
- Waits evaluate explicit predicates over typed widget or viewport snapshots.
- Widget predicates receive `nil` while a widget is missing, so appearance and
  disappearance use the same API.
- Timeouts return typed `timeout` errors with structured wait diagnostics rather
  than soft `{ matched = false }` success results.

3. Keyboard target routing
- `key` accepts an optional `target` parameter to resolve + focus before delivery.
- `type_text` accepts `focus_timeout_ms` for explicit focus handshake.
- Targeted delivery emits typed routing failures:
  `target_not_focusable`, `focus_not_acquired`, `target_detached`.

4. Settle waits
- `Viewport:wait_for_settle()` returns a `SettleReport` with phase status for input drain,
  command drain, action-frame processing, clean capture, fresh frame, and optional app idle.
- Apps can add deterministic domain-idle checks with `DevMcp::on_idle(...)` or
  `DevMcp::on_idle_ui(...)`.
- All high-level actions auto-settle by default, ensuring the UI has processed all queued
  work and repainted before returning. Disable with `{ settle = false }`.
- Wait and screenshot timeouts include frame observations for the target viewport,
  global frame counts, last-frame age, and settle phases so repaint stalls are diagnosable.

5. Deterministic click completion
- `click()` auto-settles by default, so the UI processes queued work and repaints
  before the action returns.
- Follow-up state checks are expressed as explicit waits after the action, which
  keeps action options data-shaped and timeout behavior consistent across the API.
- Pointer actions fail fast with `invisible_interaction` when the target widget is hidden or fully
  clipped. Scripts should wait for visibility explicitly or call `scroll_into_view()` before
  interacting with content that may be outside the viewport.

6. Fixture reset contract and boundary cleanup
- Fixture apply boundaries clear transient DevMCP state (queued input/commands, queued widget
  value updates, value-override consumer tracking, scroll overrides, and overlay debug artifacts)
  to avoid cross-run leakage.
- The same cleanup closes egui popups/menus and stops active text input on captured
  contexts. Scripts can call `Viewport:dismiss_popups()` for the same viewport-scoped path.
- Fixtures are baseline-reset by contract: each fixture must be independently invokable, isolated
  from prior app state, and safe to apply in any order.

7. Runtime-owned repaint and visual determinism
- `DevMcp::finish_frame` owns runtime keep-alive when hooks are attached and `keep_alive`
  is enabled.
- Automation options default to disabling egui animations while the runtime is attached;
  scripts can override this with `configure({ animations = true })`.
- `Viewport:sample_pixels(...)` and widget-relative `Widget:sample_pixels(...)` sample exact
  `ColorImage` RGBA data before JPEG encoding. `Widget:sample_grid(nx, ny)` samples a clipped
  visible widget area from one capture, and `expect_painted(id, min_colors?)` catches flat
  painter-only regions published with `publish_rect_meta`. Use `hex` for exact color equality;
  use `rgba` channel arithmetic only for threshold checks.

## Intentional strict semantics

- Wait predicates are explicit; there is no secondary wait-condition DSL to interpret.
- Targeted key delivery fails fast instead of silently dropping delivery.
- Actions auto-settle; callers must explicitly opt out when needed.
- Custom settable widgets that publish values but never call `take_widget_value_override()` surface
  `override_not_consumed` instead of silently ignoring script-driven `set_value()`.
