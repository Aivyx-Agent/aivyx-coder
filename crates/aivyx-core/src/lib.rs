pub mod agent;
pub mod architect;
pub mod changes;
pub mod commands;
pub mod council;
pub mod delegate;
pub mod delegate_to_specialist;
pub mod edit_blocks;
pub mod editor_context;
mod failing_tests;
pub mod mission_tools;
pub mod routing_commands;
pub mod sandbox_failures;
pub mod session;
pub mod session_list;
pub mod specialist_enforcement;
pub mod specialist_sessions;
pub mod test_detect;
pub mod undo;
pub mod wiki;

pub use agent::{AUTONOMOUS_HIDDEN_TOOLS, Agent, AgentConfig, AgentError, AgentEvent, EditFormat};

/// Part of every system prompt: the main agent's, a `delegate_task`
/// sub-agent's and a specialist's. In the build tests a model edited a
/// brief's data, and another a test's expected value taken from the brief,
/// to make its own wrong tests pass (`read_only_paths` blocks the edit
/// itself, when configured).
pub const FIXED_INPUTS_RULE: &str = "The task's spec (a brief, requirements, a README you were \
pointed to) and the data it gives you are fixed inputs: don't edit them unless the user asks you \
to. When a test disagrees with them, the test or the code is wrong. Fix that, and never change \
an expected value that came from the spec to match what your code returns.";
pub use architect::{Architect, ArchitectSeat};
pub use council::{Council, CouncilSeat};
pub use delegate::{DelegateTaskConfig, DelegateTaskTool};
pub use delegate_to_specialist::{DelegateToSpecialistConfig, DelegateToSpecialistTool};
pub use sandbox_failures::SandboxFailures;
pub use mission_tools::{
    DecomposeTaskTool, MissionToolsConfig, SynthesizeResultsTool, VerifyOutputTool,
};
pub use session::{SessionOwner, SessionState, Task, TaskStatus};
pub use specialist_sessions::{
    CloseSpecialistTool, QuerySpecialistTool, SpawnSpecialistTool, SpecialistSessionPool,
    SpecialistSessionSummary, SpecialistSessionsConfig,
};
