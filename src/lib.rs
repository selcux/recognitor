//! Recognitor: a local-first memory engine for LLMs.
//!
//! Plain Markdown files in the vault are the source of truth. The SQLite database is a disposable projection of
//! them. The design lives in `docs/`; doc 07 lists every interface this crate exposes.

pub mod bootstrap;
pub mod config;
pub mod context;
pub mod db;
pub mod delete_note;
pub mod edit_note;
pub mod error;
pub mod hash;
pub mod index;
pub mod mcp;
pub mod move_note;
pub mod namespace;
pub mod outline;
pub mod parse;
pub mod paths;
pub mod read;
pub mod relink;
pub mod report;
pub mod resolve;
pub mod search;
pub mod walk;
mod write;
pub mod write_note;

pub use error::{Error, Result};
pub use index::{Index, SweepOptions, SweepReport};
