// Task 4 fills this in.
//
// Minimal placeholders below so `lib.rs`'s `pub use server::{run,
// McpServerRunConfig};` compiles ahead of Task 4's real implementation --
// the task-3 brief's own stub (a bare comment, no symbols) can't satisfy
// that re-export on its own. Not part of Task 3's own deliverable; do not
// treat the shape below as designed, only as a placeholder that type-checks.

/// Placeholder for Task 4's real MCP server run-configuration.
pub struct McpServerRunConfig;

/// Placeholder for Task 4's real MCP server entry point.
pub async fn run(_config: McpServerRunConfig) -> anyhow::Result<()> {
    unimplemented!("Task 4 implements the MCP server run loop")
}
