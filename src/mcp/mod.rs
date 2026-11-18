//! The MCP server (doc 07 §1, "Transport", §2.1). `recognitor mcp` serves the tools of [`tools`] over stdio: the
//! CLI's read commands and `relink`, the first write tool. [`serve`] is the wiring to the protocol.

mod serve;
pub mod tools;

pub use serve::serve_stdio;
