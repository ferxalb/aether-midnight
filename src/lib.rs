//! Local-first, deterministic retrieval primitives for AETHER Midnight.

pub mod benchmark;
pub mod code;
pub mod db;
pub mod mcp;
pub mod memory;
mod text_index;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const INDEX_FORMAT_VERSION: u32 = 2;
pub const MAX_FILE_BYTES: u64 = 1_048_576;

#[cfg(test)]
mod retrieval_tests;
#[cfg(test)]
mod tests;
