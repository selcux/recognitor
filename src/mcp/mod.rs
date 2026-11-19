// SPDX-License-Identifier: CPAL-1.0
// Copyright (c) 2026 Selçuk Öztürk.
// This file is subject to the Common Public Attribution License Version 1.0 (CPAL-1.0).
// See the LICENSE file in the repository root for full terms, including Exhibit A and Exhibit B.

//! The MCP server (doc 07 §1, "Transport", §2.1). `recognitor mcp` serves the tools of [`tools`] over stdio: the
//! CLI's read commands and `relink`, the first write tool. [`serve`] is the wiring to the protocol.

mod serve;
pub mod tools;

pub use serve::serve_stdio;
