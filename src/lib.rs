extern crate self as snenk_bot;

pub mod app;
pub mod choices;
pub mod editor;
pub mod engine;
pub mod execution;
pub mod history;
pub mod integration;
pub mod lua;
pub mod migration;
pub mod obs;
pub mod paths;
pub mod runtime;
pub mod schema;
pub mod storage;
pub mod twitch;
#[cfg(feature = "slint-ui")]
pub mod ui;
pub mod value_sources;
pub mod vtube;
pub mod workflows;

pub use snenkbot_macros::{ConfigSchema, WorkflowValues};
