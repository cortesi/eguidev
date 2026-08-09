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
        ModuleBinding::Hidden(name) => Binding::hidden(name),
    }
}
