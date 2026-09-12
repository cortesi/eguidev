# Automation Reliability Notes

## Summary

This document records the design decisions behind automation reliability in eguidev.
The design goal is deterministic scripting behavior with typed, diagnosable failures.

## Resolved failure modes

1. Fixture execution
- Fixtures are applied by scripts via `eguidev.fixture()` with shared precondition and ready
  conditions.
- Restart is fixture-agnostic; it restarts the app and returns phase timing.

2. Wait predicate safety
- Waits evaluate explicit predicates over typed widget or viewport snapshots.
- Widget predicates receive `nil` while a widget is missing, so appearance and
  disappearance use the same API.
- Timeouts return typed `timeout` errors with structured wait diagnostics rather
  than soft `{ matched = false }` success results.

3. Keyboard target routing
- `key` accepts an optional `target` parameter to resolve + focus before delivery.
- `type_text` uses the action timeout for its explicit focus handshake.
- Targeted delivery emits typed routing failures:
  `target_not_focusable`, `focus_not_acquired`, `target_detached`.

4. Settle waits
- `Viewport:settle()` returns a `SettleReport` with phase status for input drain,
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
  clipped. Scripts should wait for `{ actionable = true }` or call `scroll_into_view()` before
  interacting with content that may be outside the viewport.
- Pointer actions also fail fast with `not_actionable` and reason `covered`. This happens when
  another egui layer sits over the target's action point, such as a floating card or a modal
  backdrop. `eguidev` computes `covered` from `ctx.layer_id_at` at record time and publishes it on
  `WidgetState`. A click never silently reaches the covering layer instead of the intended widget.
  `scroll_into_view()` ignores coverage, because scrolling cannot uncover a widget under a fixed
  overlay. `{ actionable = true }` and pointer admission both require the widget to be uncovered.

6. Fixture reset contract and boundary cleanup
- Fixture apply boundaries clear transient DevMCP state (queued input/commands, queued widget
  value updates, value-override consumer tracking, scroll overrides, and overlay debug artifacts)
  to avoid cross-run leakage.
- The same cleanup closes egui popups/menus on captured contexts. It stops text input
  in each context's active viewport; shared contexts can retain focus in other
  viewports. `Viewport:dismiss_popups()` clears queued input, overrides, and visual aids
  only in its viewport, then sends Escape there to dismiss popups and release text
  focus. App code can also receive this Escape, for example to close a modal or
  cancel a drag. Fixture-wide cleanup does not inject Escape.
- Fixtures without preconditions are baseline-reset by contract: they are independently invokable,
  isolated from prior app state, and safe to apply in any order. Fixtures with preconditions are
  transitions and require the caller to establish their declared entry state first.

7. Runtime-owned repaint and visual determinism
- `DevMcp::finish_frame` owns runtime keep-alive when hooks are attached and `keep_alive`
  is enabled.
- Automation options default to disabling egui animations while the runtime is attached;
  scripts can override this with `eguidev.configure({ animations = true })`.
- `Viewport:sample_pixels(...)` and widget-relative `Widget:sample_pixels(...)` sample exact
  `ColorImage` RGBA data before JPEG encoding. `Widget:sample_grid(nx, ny)` samples a clipped
  visible widget area from one capture, and the `painted` expectation catches flat painter-only
  regions published with `publish_rect_meta`. Use `hex` for exact color equality;
  use `rgba` channel arithmetic only for threshold checks.

## Intentional strict semantics

- Shared conditions cover ordinary readiness; predicates remain available for app-specific logic.
- Targeted key delivery fails fast instead of silently dropping delivery.
- Actions auto-settle; callers must explicitly opt out when needed.
- Custom settable widgets that publish values but never call `take_widget_value_override()` surface
  `override_not_consumed` instead of silently ignoring script-driven `set_value()`.
