//! MCP-server frontend for aivyx-coder's `Agent` core. See
//! `docs/superpowers/specs/2026-08-20-mcp-server-frontend-design.md`.

mod server;
mod session;
mod tiers;

pub use server::{run, McpServerRunConfig};
pub use session::{build_session_agent, run_bounded_turn, SessionConfig};
pub use tiers::AccessLevel;
