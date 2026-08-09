# Luau guide

This guide covers Luau syntax and common Eguidev idioms. The complete API reference is
`crates/eguidev_runtime/luau/eguidev.d.luau`; `script_api` and `edev docs` return those exact
bytes.

Every submitted script is checked in strict mode before it runs. The sandbox has no filesystem,
network, or module-import access. Eguidev adds one global, `eguidev`.

## Values, tables, and functions

```luau
local ok = true
local count = 3
local ratio = 0.25
local name = "Alice"
local items = { 1, 2, 3 }
local options = { timeout_ms = 2000, poll_interval_ms = 16 }

local function add(left: number, right: number): number
    return left + right
end

for _, item in ipairs(items) do
    eguidev.log(add(item, count))
end
```

Tables use dot access for named fields and bracket access for dynamic keys. Strings support the
standard library and `..` concatenation.

```luau
local payload = { name = "Alice", nested = { count = 2 } }
assert(payload.nested.count == 2)
assert(string.find(payload.name, "lic", 1, true) ~= nil)
```

## Live references and optional state

`eguidev.widget(id)` and `eguidev.viewport(id)` create immutable live references without looking
up current state. Their `state()` methods return `nil` when the target is absent, so strict scripts
must narrow the optional value.

```luau
local submit = eguidev.widget("submit")
local state = submit:state()
assert(state ~= nil, "submit should exist")
eguidev.log(state.rect.min.x)

if state.label ~= nil then
    eguidev.log(state.label)
end
```

References returned by viewport discovery keep their viewport scope. State snapshots carry both
`viewport_id` and `id`, which form canonical widget identity.

## Conditions, waits, and assertions

Use a typed condition for ordinary state waits and a predicate for app-specific logic. The same
condition model is used by fixture targets.

```luau
local status = eguidev.widget("status")
local ready = status:wait({ visible = true, value_text_contains = "Ready" })
assert(ready ~= nil)

local complete = status:wait(function(current)
    return current ~= nil and current.data ~= nil and current.data.complete == true
end, { timeout_ms = 2000 })
assert(complete ~= nil)
```

`Widget:expect(...)` adds geometry, hierarchy, text-fit, and paint expectations. Failed Eguidev
expectations are returned in the structured script outcome.

```luau
local panel = eguidev.widget("panel")
eguidev.widget("submit"):expect({
    role = "button",
    enabled = true,
    within = panel,
    text_fits = true,
    painted = { min_colors = 2 },
})
```

## Actions and raw input

High-level actions wait for an actionable target and auto-settle by default. Set `settle = false`
only when the script intentionally observes an intermediate state.

```luau
local input = eguidev.widget("name")
input:type_text("Sky", { clear = true, enter = true })
eguidev.widget("submit"):click()

local vp = eguidev.root
vp:key("escape")
vp:key("enter", { target = input })
```

`Viewport:input(...)` queues exactly one validated raw event. It does not wait or settle.

```luau
local vp = eguidev.root
vp:input({ type = "pointer_move", position = { x = 20, y = 30 } })
vp:input({ type = "text", text = "raw text" })
vp:settle()
```

## Fixtures

Fixtures establish a known baseline. Preconditions always run. Unless `wait = false`, Eguidev
then waits for one fresh frame and every static or handler-returned ready target.

```luau
local result = eguidev.fixture("basic.default", { count = 3 })
eguidev.log(result.values)

for _, observation in ipairs(result.observations) do
    eguidev.log({ phase = observation.phase, widget = observation.widget.id })
end
```

## Capture, diagnostics, and results

Prefer state, dumps, pixel samples, and layout checks for programmatic inspection. Return an
`ImageRef` when the MCP response should include an image block.

```luau
local before = eguidev.capture()
eguidev.widget("submit"):click()
local changed = before:diff({ id_prefix = "status" })

local vp = eguidev.root
local issues = vp:layout_issues()
local image = vp:screenshot()
return { changes = changed.changes, issues = issues, image = image }
```

Each script result also includes logs, assertions, fixture applications, timing, and undismissed
egui identity diagnostics. Script failures use `success = false` with structured error data; they
are not MCP protocol errors.
