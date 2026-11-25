// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! Recognitor: a local-first memory engine for LLMs.
//!
//! Plain Markdown files in the vault are the source of truth. The SQLite database is a disposable projection of
//! them. The design lives in `docs/`; doc 07 lists every interface this crate exposes.

pub mod bootstrap;
pub mod bootstrap_write;
pub mod config;
pub mod context;
pub mod db;
pub mod delete_note;
pub mod doctor;
pub mod edit_note;
pub mod error;
pub mod ghosts;
pub mod harness;
pub mod hash;
pub mod index;
pub mod lint;
pub mod mcp;
pub mod move_cli;
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
pub mod trash;
pub mod walk;
mod write;
pub mod write_note;

pub use error::{Error, Result};
pub use index::{Index, SweepOptions, SweepReport};
