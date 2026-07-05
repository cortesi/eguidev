--!nonstrict

local function __eguidev_repr(value, depth)
    depth = depth or 0
    local kind = type(value)
    if kind == "string" then
        return string.format("%q", value)
    end
    if kind ~= "table" then
        return tostring(value)
    end
    if depth >= 3 then
        return "{...}"
    end
    local parts = {}
    for key, child in pairs(value) do
        table.insert(parts, tostring(key) .. " = " .. __eguidev_repr(child, depth + 1))
    end
    table.sort(parts)
    return "{ " .. table.concat(parts, ", ") .. " }"
end

local function __eguidev_rects_overlap(a, b)
    return a.min.x < b.max.x and a.max.x > b.min.x and a.min.y < b.max.y and a.max.y > b.min.y
end

local function __eguidev_rect_within(inner, outer)
    return inner.min.x >= outer.min.x
        and inner.min.y >= outer.min.y
        and inner.max.x <= outer.max.x
        and inner.max.y <= outer.max.y
end

local function __eguidev_find_options(expected)
    if expected ~= nil and expected.viewport ~= nil then
        return { viewport = expected.viewport }
    end
    return nil
end

local function __eguidev_mismatches(state, expected)
    local mismatches = {}
    if expected == nil then
        return mismatches
    end
    local function add(field, got)
        table.insert(
            mismatches,
            field
                .. ": expected "
                .. __eguidev_repr(expected[field])
                .. ", got "
                .. __eguidev_repr(got)
        )
    end
    for _, field in ipairs({ "visible", "focused", "enabled", "selected", "role", "label", "value" }) do
        if expected[field] ~= nil and state[field] ~= expected[field] then
            add(field, state[field])
        end
    end
    if expected.label_contains ~= nil then
        if state.label == nil or string.find(state.label, expected.label_contains, 1, true) == nil then
            table.insert(
                mismatches,
                "label_contains: expected "
                    .. __eguidev_repr(expected.label_contains)
                    .. ", got "
                    .. __eguidev_repr(state.label)
            )
        end
    end
    if expected.value_text_contains ~= nil then
        if string.find(state.value_text, expected.value_text_contains, 1, true) == nil then
            table.insert(
                mismatches,
                "value_text_contains: expected "
                    .. __eguidev_repr(expected.value_text_contains)
                    .. ", got "
                    .. __eguidev_repr(state.value_text)
            )
        end
    end
    return mismatches
end

local function __eguidev_viewport_mismatch(widget, expected)
    if expected == nil or expected.viewport == nil then
        return nil
    end
    if widget.viewport_id == expected.viewport then
        return nil
    end
    local viewport_state = widget:viewport():state()
    if viewport_state.name == expected.viewport then
        return nil
    end
    local got = widget.viewport_id
    if viewport_state.name ~= nil then
        got = got .. " (" .. viewport_state.name .. ")"
    end
    return "viewport: expected "
        .. __eguidev_repr(expected.viewport)
        .. ", got "
        .. __eguidev_repr(got)
end

local function __eguidev_wait_for(predicate, failure)
    local ok, err = pcall(function()
        wait_until(predicate)
    end)
    if not ok then
        error(failure() .. "\nwait error: " .. tostring(err), 3)
    end
end

function expect(id, expected)
    local options = __eguidev_find_options(expected)
    local final_state = nil
    local final_mismatches = nil
    __eguidev_wait_for(function()
        local current = try_widget(id, options)
        if current == nil then
            final_state = nil
            final_mismatches = { "widget is absent" }
            return false
        end
        local state = current:state()
        local mismatches = __eguidev_mismatches(state, expected)
        local viewport_mismatch = __eguidev_viewport_mismatch(current, expected)
        if viewport_mismatch ~= nil then
            table.insert(mismatches, viewport_mismatch)
        end
        final_state = state
        final_mismatches = mismatches
        return #mismatches == 0
    end, function()
        return "expect(" .. __eguidev_repr(id) .. ") failed: "
            .. table.concat(final_mismatches or { "widget is absent" }, "; ")
            .. "\nstate: "
            .. __eguidev_repr(final_state)
    end)
    return final_state
end

function expect_absent(id)
    local final_state = nil
    __eguidev_wait_for(function()
        local current = try_widget(id)
        if current == nil then
            final_state = nil
            return true
        end
        final_state = current:state()
        return false
    end, function()
        return "expect_absent(" .. __eguidev_repr(id) .. ") failed; state: "
            .. __eguidev_repr(final_state)
    end)
end

function expect_left_of(a, b, min_gap)
    min_gap = min_gap or 0
    local left = widget(a):state()
    local right = widget(b):state()
    if left.rect.max.x + min_gap > right.rect.min.x then
        error(
            "expect_left_of failed: "
                .. __eguidev_repr(a)
                .. " rect "
                .. __eguidev_repr(left.rect)
                .. " is not left of "
                .. __eguidev_repr(b)
                .. " rect "
                .. __eguidev_repr(right.rect),
            2
        )
    end
end

function expect_above(a, b, min_gap)
    min_gap = min_gap or 0
    local upper = widget(a):state()
    local lower = widget(b):state()
    if upper.rect.max.y + min_gap > lower.rect.min.y then
        error(
            "expect_above failed: "
                .. __eguidev_repr(a)
                .. " rect "
                .. __eguidev_repr(upper.rect)
                .. " is not above "
                .. __eguidev_repr(b)
                .. " rect "
                .. __eguidev_repr(lower.rect),
            2
        )
    end
end

function expect_no_overlap(a, b)
    local first = widget(a):state()
    local second = widget(b):state()
    if __eguidev_rects_overlap(first.rect, second.rect) then
        error(
            "expect_no_overlap failed: "
                .. __eguidev_repr(a)
                .. " rect "
                .. __eguidev_repr(first.rect)
                .. " overlaps "
                .. __eguidev_repr(b)
                .. " rect "
                .. __eguidev_repr(second.rect),
            2
        )
    end
end

function expect_within(inner, outer)
    local inner_state = widget(inner):state()
    local outer_state = widget(outer):state()
    if not __eguidev_rect_within(inner_state.rect, outer_state.rect) then
        error(
            "expect_within failed: "
                .. __eguidev_repr(inner)
                .. " rect "
                .. __eguidev_repr(inner_state.rect)
                .. " is not within "
                .. __eguidev_repr(outer)
                .. " rect "
                .. __eguidev_repr(outer_state.rect),
            2
        )
    end
end

function expect_text_fits(id)
    local handle = widget(id)
    local state = handle:state()
    local measure = handle:text_measure()
    local full_text = state.label or state.value_text or measure.text
    local layout_ok = state.layout == nil
        or (state.layout.visible_fraction >= 1 and state.layout.clipped == false)
    if not state.visible or not layout_ok or measure.ellipsis or measure.visible_text ~= full_text then
        error(
            "expect_text_fits failed for "
                .. __eguidev_repr(id)
                .. "; state: "
                .. __eguidev_repr(state)
                .. "; text_measure: "
                .. __eguidev_repr(measure),
            2
        )
    end
end

function expect_tree(parent, children)
    local parent_handle = widget(parent)
    for _, child_id in ipairs(children) do
        local child = widget(child_id)
        local found = false
        local ancestor = child:parent()
        while ancestor ~= nil do
            if ancestor.id == parent_handle.id and ancestor.viewport_id == parent_handle.viewport_id then
                found = true
                break
            end
            ancestor = ancestor:parent()
        end
        if not found then
            error(
                "expect_tree failed: "
                    .. __eguidev_repr(child_id)
                    .. " is not a descendant of "
                    .. __eguidev_repr(parent)
                    .. "; child state: "
                    .. __eguidev_repr(child:state())
                    .. "; parent state: "
                    .. __eguidev_repr(parent_handle:state()),
                2
            )
        end
    end
end
