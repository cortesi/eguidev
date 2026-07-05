--!nonstrict

function demo.status_label()
    local state = widget("basic.status"):state()
    return state.label or state.value_text
end

function demo.expect_status_contains(text)
    return expect("basic.status", { value_text_contains = text })
end
