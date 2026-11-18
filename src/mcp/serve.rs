//! The protocol side of `recognitor mcp`: one server per client session over stdio, no HTTP, no daemon (doc 07 §1,
//! "Transport"). stdout carries protocol messages only; nothing in this crate prints to it while the server runs.
//!
//! `ServerHandler` is implemented by hand, not with the SDK's `#[tool]` macros, because the macros turn arguments
//! that do not parse into JSON-RPC errors. Doc 07 §2.1 wants those as a tool result with `isError: true`, so the
//! arguments reach [`tools::call`] as raw JSON and it checks them itself.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};

use super::tools::{self, INSTRUCTIONS};
use crate::{Error, Index, Result};

/// The shared state of one server: the index, which every call sweeps and reads, and the config file that
/// `read.max_tokens` and `bootstrap.max_tokens` come from.
#[derive(Clone)]
struct Recognitor {
    index: Arc<Mutex<Index>>,
    config_file: Arc<PathBuf>,
}

fn list() -> Vec<Tool> {
    tools::TOOLS
        .iter()
        .map(|t| Tool::new(t.name, t.description, t.input_schema()))
        .collect()
}

impl ServerHandler for Recognitor {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("recognitor", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        let arguments = request.arguments.unwrap_or_default();
        let index = Arc::clone(&self.index);
        let config_file = Arc::clone(&self.config_file);
        // SQLite and the sweep block, so they run off the async threads. One call at a time per server: the
        // connection is not shared between threads, and a session is one agent.
        let outcome = tokio::task::spawn_blocking({
            let name = name.clone();
            move || {
                // A call that panicked poisoned the lock, and left no half-written state: a sweep's transaction
                // rolls back when its guard drops during the unwind. The next call may use the index.
                let mut index = index.lock().unwrap_or_else(PoisonError::into_inner);
                tools::call(&mut index, &config_file, &name, &arguments)
            }
        })
        .await
        .map_err(|e| McpError::internal_error(format!("the tool call failed: {e}"), None))?;
        let Some(outcome) = outcome else {
            // Doc 07 §2.1, errors table, "Unknown tool": a JSON-RPC error (-32602), as the MCP spec prescribes.
            return Err(McpError::invalid_params(format!("unknown tool: {name}"), None));
        };
        let content = vec![ContentBlock::text(outcome.text)];
        Ok(CallToolResponse::from(if outcome.is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }))
    }
}

/// Serves the tools over stdio until the client closes it. Blocks the calling thread.
pub fn serve_stdio(index: Index, config_file: PathBuf) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Mcp(e.to_string()))?;
    let handler = Recognitor {
        index: Arc::new(Mutex::new(index)),
        config_file: Arc::new(config_file),
    };
    let result = runtime.block_on(async {
        let service = handler.serve(stdio()).await.map_err(|e| Error::Mcp(e.to_string()))?;
        service.waiting().await.map_err(|e| Error::Mcp(e.to_string()))?;
        Ok(())
    });
    // A read of stdin that is still pending cannot be cancelled; do not wait for it.
    runtime.shutdown_background();
    result
}
