use ruau::{
    module::{self, Binding},
    vm::ModuleBinding,
};

pub const DECLARATION: &str = include_str!("../../luau/eguidev.d.luau");
pub const SOURCE: &[u8] = include_bytes!("../../luau/eguidev.luau");
pub const PRIVATE_INPUTS: [&str; 7] = [
    "eguidev.query",
    "eguidev.action",
    "eguidev.wait",
    "eguidev.capture",
    "eguidev.fixture",
    "eguidev.diagnostic",
    "eguidev.record",
];

pub fn register(builder: &mut module::Builder) {
    builder.source_value_with(
        "eguidev",
        Binding::declared_global(),
        SOURCE,
        PRIVATE_INPUTS,
    );
}

pub fn declared_binding(binding: ModuleBinding) -> Binding {
    match binding {
        ModuleBinding::Global => Binding::declared_global(),
        ModuleBinding::GlobalOverride => Binding::declared_global_override(),
        ModuleBinding::Library(name) => Binding::declared_library(name),
        // FreeDF 포크: 비공개 ruau의 `LibraryOverride`는 공개 ruau(0.4.0)에 아직
        // 없습니다. 이 리비전의 eguidev가 실제로 이 변형을 생성하지 않으므로
        // 선언 표면은 `Library` 하나로 충분합니다.
        ModuleBinding::Hidden(name) => Binding::hidden(name),
    }
}
