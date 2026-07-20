//! ACP server frontend for aivyx-coder's `Agent` core. See
//! `docs/superpowers/specs/2026-07-20-acp-editor-integration-design.md`.

mod prompter;
mod session;
mod translate;

pub use prompter::{AcpPrompter, DeferredPrompter, PrompterInstaller, deferred_prompter};
pub use session::{run, AcpSessionConfig};
