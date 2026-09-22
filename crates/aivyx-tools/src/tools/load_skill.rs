use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_skills::SkillLoader;
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct LoadSkillArgs {
    /// The name of the skill to load -- must match one of the names
    /// listed in this tool's own description or the system prompt's
    /// skill listing.
    skill: String,
}

/// Returns one skill's full body verbatim, by name. The set of valid
/// names comes from `SkillLoader::list()` -- both this tool's own
/// `definition()` and the system-prompt skill listing (`Agent::set_skills`,
/// built in `agent_builder.rs`) advertise the same names, from the same
/// loader.
pub struct LoadSkillTool {
    loader: Arc<SkillLoader>,
}

impl LoadSkillTool {
    pub fn new(loader: Arc<SkillLoader>) -> Self {
        Self { loader }
    }
}

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }

    // A skill load touches no project state -- stays visible in plan
    // mode, no checkpoint.
    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        let names: Vec<String> = self.loader.list().into_iter().map(|s| s.name).collect();
        ToolDefinition {
            name: self.name().to_string(),
            description: format!(
                "Load the full body of one default or project/user skill by name, for \
                step-by-step process guidance (e.g. systematic debugging, writing a plan, \
                brainstorming and scoping). Available skills: {}.",
                names.join(", ")
            ),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(LoadSkillArgs)),
        }
    }

    fn permission_request(
        &self,
        _arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Internal,
            target: PermissionTarget::Other(self.name().to_string()),
            arguments_preview: serde_json::json!({}),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: LoadSkillArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        match self.loader.get(&args.skill) {
            Some(skill) => Ok(ToolOutput::Ok(skill.body)),
            None => {
                let names: Vec<String> =
                    self.loader.list().into_iter().map(|s| s.name).collect();
                Ok(ToolOutput::Error(format!(
                    "unknown skill: {:?} -- valid skills: {}",
                    args.skill,
                    names.join(", ")
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: std::path::PathBuf::from("."),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn loading_a_known_bundled_skill_returns_its_real_body() {
        let tool = LoadSkillTool::new(Arc::new(SkillLoader::new()));

        let output = tool
            .execute(serde_json::json!({ "skill": "systematic-debugging" }), &ctx())
            .await
            .unwrap();

        let ToolOutput::Ok(body) = output else {
            panic!("expected Ok output, got {output:?}")
        };
        assert!(body.contains("Reproduce"));
    }

    #[tokio::test]
    async fn loading_an_unknown_skill_returns_a_clear_error_listing_valid_names() {
        let tool = LoadSkillTool::new(Arc::new(SkillLoader::new()));

        let output = tool
            .execute(serde_json::json!({ "skill": "does-not-exist" }), &ctx())
            .await
            .unwrap();

        let ToolOutput::Error(message) = output else {
            panic!("expected Error output, got {output:?}")
        };
        assert!(message.contains("does-not-exist"));
        assert!(message.contains("systematic-debugging"));
    }

    #[test]
    fn definition_interpolates_the_real_bundled_skill_names() {
        let tool = LoadSkillTool::new(Arc::new(SkillLoader::new()));
        let definition = tool.definition();
        assert!(definition.description.contains("systematic-debugging"));
        assert!(definition.description.contains("writing-plans"));
    }

    #[test]
    fn permission_request_is_internal_and_never_touches_a_path_or_command() {
        let tool = LoadSkillTool::new(Arc::new(SkillLoader::new()));
        let request = tool
            .permission_request(&serde_json::json!({ "skill": "x" }), Path::new("."))
            .unwrap();

        assert_eq!(request.action, ActionKind::Internal);
        assert!(matches!(request.target, PermissionTarget::Other(_)));
    }

    #[test]
    fn mutates_outside_session_is_false() {
        let tool = LoadSkillTool::new(Arc::new(SkillLoader::new()));
        assert!(!tool.mutates_outside_session());
    }
}
