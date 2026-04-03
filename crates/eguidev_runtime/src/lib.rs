//! Native embedded runtime for `eguidev`.

#![allow(clippy::missing_docs_in_private_items)]

#[cfg(target_arch = "wasm32")]
compile_error!("eguidev_runtime is native-only and is not supported on wasm32 targets");

mod error;
mod runtime;
mod screenshots;
mod script_docs;
mod server;
pub mod smoke;
mod tools;

pub(crate) mod actions {
    pub use eguidev::internal::actions::*;
}

pub(crate) mod overlay {
    pub use eguidev::internal::overlay::*;
}

pub(crate) mod registry {
    pub use eguidev::internal::registry::*;
}

pub(crate) mod tree {
    pub use eguidev::internal::tree::*;
}

pub(crate) mod types {
    pub use eguidev::internal::types::*;
}

pub(crate) mod ui_ext {
    pub use eguidev::internal::ui_ext::*;
}

pub(crate) mod viewports {
    pub use eguidev::internal::viewports::*;
}

pub use eguidev::{DevMcp, ScrollAreaMeta};

pub use crate::{
    runtime::{attach, eval_script},
    script_docs::{render_script_docs_markdown, script_definitions},
    tools::{
        ScriptArgValue, ScriptArgs, ScriptAssertion, ScriptErrorInfo, ScriptEvalOptions,
        ScriptEvalOutcome, ScriptEvalRequest, ScriptImageInfo, ScriptLocation, ScriptTiming,
    },
};

#[cfg(test)]
pub(crate) mod widget_registry {
    pub use eguidev::internal::widget_registry::*;
}
