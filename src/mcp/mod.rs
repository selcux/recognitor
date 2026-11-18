//! The read-only MCP server (doc 07 §1, "Transport", §2.1). `recognitor mcp` serves five tools over stdio: the
//! tools of [`tools`] are the CLI's read commands, and [`serve`] is the wiring to the protocol.
//!
//! Shadow mode exposes no write tool.

mod serve;
pub mod tools;

pub use serve::serve_stdio;
