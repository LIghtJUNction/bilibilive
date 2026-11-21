//! One streaming application exposed through native, terminal and agent interfaces.
pub mod app;
pub mod auth;
pub mod bili;
pub mod bundle;
pub mod config;
pub mod convert;
pub mod danmaku;
#[cfg(feature = "gui")]
pub mod desktop;
#[cfg(feature = "gui")]
pub mod gui;
pub mod http;
pub mod logging;
pub mod mcp;
pub mod media;
pub mod obs;
pub mod obs_plugin;
pub mod pk_session;
pub mod protocol_profile;
pub mod schema;
pub mod updater;
pub mod zuse_host;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
