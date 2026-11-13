//! Local-first, deterministic retrieval primitives for AETHER Midnight.

pub mod auth;
pub mod benchmark;
pub mod cloud;
pub mod code;
pub mod config;
pub mod connect;
pub mod context;
pub mod db;
pub mod embed;
pub mod mcp;
pub mod mcp_http;
pub mod memory;
pub mod progress;
pub mod project;
pub mod runtime;
mod text_index;
pub mod uninstall;
pub mod update;
pub mod ux;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const INDEX_FORMAT_VERSION: u32 = 2;
pub const MAX_FILE_BYTES: u64 = 1_048_576;

#[cfg(test)]
mod retrieval_tests;
#[cfg(test)]
mod tests;

#[cfg(feature = "retrieval-experiments")]
mod kernel_diagnostic;

#[cfg(feature = "allocation-diagnostics")]
mod allocation_diagnostic;
