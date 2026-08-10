# Cleanup API Notes

These notes capture the public API snapshot (via ruskel) and the intended adjustments tracked
in the cleanup plan.

## Current public surface

- `eguidev` default build: inert `DevMcp`, `FrameGuard`, the recording
  functions (`track_widget`, `track_widget_with_meta`, `track_response`,
  `publish_rect_meta`, `publish_rect_container`, `container`,
  `begin_container`), `DevUiExt`, fixture types, and widget metadata/types.
- `eguidev` with `devtools`: adds `runtime::attach()`, the embedded MCP
  server, script evaluation/types, screenshots, and smoke helpers.
- `edev`: `run()` and `EdevError` remain the only public items.

## Intentions under cleanup

- Keep helper functions and `DevUiExt` consistent with the inert-by-default
  `DevMcp` model.
- Keep runtime attachment explicit and localized to one bootstrap boundary.
- Tool semantics updated: `widget_get` now errors on missing widgets, and `widget_list`
  includes all visibility states unless the `visible` filter is set.
- Duplicate explicit widget ids now block automation until instrumentation is fixed.
- Widget registry entries now include optional `value` data for stateful widgets to aid
  verification without screenshots.
- Widget registry entries now include optional `layout` data (clip rect, available rect,
  visible fraction) when captured via `DevUiExt` helpers.
- Script-facing widget discovery now returns `Widget` handles; live frame fields are read through
  `Widget:state()` as `WidgetState`.
- Widget registry entries now expose one canonical `id`. Explicit ids are
  preferred; generated ids may be opaque hex strings and are session-stable only.
- Widget role taxonomy now includes `Label` for text-only widgets.
- Tool errors now use `not_found`, `ambiguous`, `invalid_ref`, and `internal` codes with
  structured details when available.
