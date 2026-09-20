//! `DelegateToSpecialistTool`: an attenuated sibling to `delegate_task`
//! (`delegate.rs`) -- spins up a specialist `Agent` scoped to one
//! `aivyx_team::TeamMember`'s `effective_tool_allowlist` and `persona`,
//! reusing the exact same shared gate/confiner/checkpointer,
//! fresh-history, bounded-iteration mechanism `delegate_task` already
//! has. Deliberately NOT registered onto the default agent's tool list
//! (see `docs/superpowers/plans/2026-09-20-nonagon-team-delegation.md`'s
//! Global Constraints) -- that's a later phase's job, once there's a
//! real way to load a `TeamConfig` and opt into team mode at all.
//!
//! Deny-paths attenuation is explicitly out of scope here: a specialist
//! shares the lead's exact `deny_paths` (baked into each tool instance
//! at construction time in `agent_builder.rs`, which this function
//! never touches) -- only which tools are *visible* is attenuated.

use std::path::Path;
use std::sync::Arc;

use aivyx_llm::LlmBackend;
use aivyx_repomap::RepoMap;
use aivyx_sandbox::{
    ActionKind, AutonomousMode, ExecutionConfiner, InjectionTaint, PermissionGate,
    PermissionRequest, PermissionTarget, PlanMode,
};
use aivyx_team::TeamConfig;
use aivyx_tools::{GitCheckpointer, Tool, ToolError, ToolExecutionContext, ToolExecutor, ToolRegistry};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{AgentEvent, EditFormat};

#[derive(Deserialize, JsonSchema)]
struct DelegateToSpecialistArgs {
    /// The name of a `TeamConfig` member to delegate to -- must match
    /// one of `team.members`' own `name` fields.
    member: String,
    /// A complete, self-contained description of the task for the
    /// specialist -- it starts with no context beyond this text and the
    /// specialist's own persona.
    task: String,
}

/// Turns `member`'s `tool_allowlist` into an attenuated `ToolRegistry`:
/// every tool in `parent_registry` whose name is in
/// `aivyx_team::effective_tool_allowlist(member, ...)`, nothing else.
/// Does not re-validate `member`'s `tool_allowlist` against
/// `parent_registry` -- an allowlist entry naming a tool the parent
/// doesn't actually have is silently absent from the result (that's
/// `TeamConfig::validate`'s job, at config-load time, not this
/// function's).
pub fn compute_specialist_registry(
    member: &aivyx_team::TeamMember,
    parent_registry: &ToolRegistry,
) -> ToolRegistry {
    // Bound once, owned, so the borrowed &str names below have
    // somewhere to live for the rest of this function.
    let definitions = parent_registry.definitions();
    let parent_names: Vec<&str> = definitions.iter().map(|d| d.name.as_str()).collect();
    let effective = aivyx_team::effective_tool_allowlist(member, &parent_names);
    let effective_set: std::collections::HashSet<&str> =
        effective.iter().map(|s| s.as_str()).collect();
    let exclude_names: Vec<&str> = parent_names
        .into_iter()
        .filter(|name| !effective_set.contains(name))
        .collect();
    let mut attenuated = parent_registry.clone();
    attenuated.exclude(&exclude_names);
    attenuated
}

pub struct DelegateToSpecialistConfig {
    pub llm: Arc<dyn LlmBackend>,
    pub gate: Arc<dyn PermissionGate>,
    pub confiner: Arc<dyn ExecutionConfiner>,
    pub checkpointer: Option<Arc<GitCheckpointer>>,
    pub repo_map: Option<(Arc<RepoMap>, u32)>,
    pub events_tx: UnboundedSender<AgentEvent>,
    /// The parent's full tool registry -- `compute_specialist_registry`
    /// attenuates it per-member at call time, so (unlike
    /// `delegate_task`'s pre-cloned, pre-excluded `sub_agent_registry`)
    /// this is the *unfiltered* parent registry, cloned once here at
    /// construction time (before this tool itself would ever be
    /// registered onto it, mirroring `delegate_task`'s own recursion-
    /// prevention structure -- though this tool is not registered onto
    /// the default agent at all this phase, see the module doc comment).
    pub parent_registry: ToolRegistry,
    pub team: TeamConfig,
    pub plan_mode: PlanMode,
    pub autonomous_mode: AutonomousMode,
    pub injection_taint: InjectionTaint,
    pub context_tokens: u32,
    pub edit_format: EditFormat,
    pub verification: Option<(String, u32)>,
    pub max_iterations: u32,
    pub broker_mode: bool,
}

pub struct DelegateToSpecialistTool {
    config: DelegateToSpecialistConfig,
}

impl DelegateToSpecialistTool {
    pub fn new(config: DelegateToSpecialistConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for DelegateToSpecialistTool {
    fn name(&self) -> &str {
        "delegate_to_specialist"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Delegate a bounded task to a named team specialist -- a fresh \
                sub-agent scoped to that specialist's own role and tool access (narrower than \
                yours), with its own isolated conversation history. Write a complete, \
                self-contained task description: the specialist starts with no context beyond \
                what you write here plus its own persona. Returns the specialist's final answer \
                as text."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(
                DelegateToSpecialistArgs
            )),
        }
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other("delegate_to_specialist".to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        todo!("Task 2 implements this")
    }
}

#[cfg(test)]
mod registry_attenuation_tests {
    use super::*;
    use aivyx_team::TeamMember;

    fn member(tool_allowlist: &[&str]) -> TeamMember {
        TeamMember {
            name: "implementer".to_string(),
            role: "Implementer".to_string(),
            persona: "You write code.".to_string(),
            tool_allowlist: tool_allowlist.iter().map(|s| s.to_string()).collect(),
            extra_deny_paths: vec![],
        }
    }

    // A minimal Tool impl for building a test registry -- named tools
    // with no real behavior, matching this crate's existing test
    // conventions of lightweight stand-ins rather than the real
    // aivyx-tools types (which this crate cannot depend on -- see the
    // parent plan's Global Constraints on dependency direction).
    struct NamedTool(&'static str);
    #[async_trait::async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn definition(&self) -> aivyx_types::ToolDefinition {
            aivyx_types::ToolDefinition {
                name: self.0.to_string(),
                description: String::new(),
                parameters_schema: serde_json::json!({}),
            }
        }
        fn permission_request(
            &self,
            _arguments: &serde_json::Value,
            _cwd: &std::path::Path,
        ) -> Result<aivyx_sandbox::PermissionRequest, aivyx_tools::ToolError> {
            unreachable!("not exercised by these tests")
        }
        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _ctx: &aivyx_tools::ToolExecutionContext,
        ) -> Result<aivyx_types::ToolOutput, aivyx_tools::ToolError> {
            unreachable!("not exercised by these tests")
        }
    }

    fn registry_with(names: &[&'static str]) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        for name in names {
            registry.register(Arc::new(NamedTool(name)));
        }
        registry
    }

    #[test]
    fn attenuated_registry_keeps_only_the_members_allowed_tools() {
        let parent = registry_with(&["read_file", "write_file", "grep", "run_command"]);
        let m = member(&["read_file", "grep"]);
        let attenuated = compute_specialist_registry(&m, &parent);
        let mut names: Vec<String> = attenuated
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["grep".to_string(), "read_file".to_string()]);
    }

    #[test]
    fn attenuated_registry_is_empty_for_a_member_with_no_tool_allowlist() {
        let parent = registry_with(&["read_file", "write_file"]);
        let m = member(&[]);
        let attenuated = compute_specialist_registry(&m, &parent);
        assert!(attenuated.definitions().is_empty());
    }

    #[test]
    fn attenuated_registry_silently_drops_an_allowlist_entry_not_in_the_parent() {
        // A member's tool_allowlist naming a tool the parent doesn't
        // actually have registered (e.g. a stale/misconfigured
        // TeamConfig) must not panic or error here -- it's simply not
        // in the parent's registry to begin with, so it's absent from
        // the attenuated result too. (Load-time TeamConfig::validate is
        // where this should have been caught already; this function
        // doesn't re-validate.)
        let parent = registry_with(&["read_file"]);
        let m = member(&["read_file", "nonexistent_tool"]);
        let attenuated = compute_specialist_registry(&m, &parent);
        let names: Vec<String> = attenuated.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["read_file".to_string()]);
    }
}
