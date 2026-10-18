//! Desktop commands adapt the existing engine and edit sessions to Tauri.

pub mod api_admin;
pub mod authentication;
pub mod choices;
pub mod collections;
pub mod commands;
pub mod configuration;
pub mod directories;
pub mod editor;
pub mod events;
pub mod history;
pub mod host;
pub mod hub;
pub mod input;
pub mod paths;

#[cfg(feature = "native")]
mod application;
#[cfg(feature = "native")]
pub use application::run;
