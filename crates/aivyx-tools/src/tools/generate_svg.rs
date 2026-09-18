use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use aivyx_vision_svg::TextCompleter;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct GenerateSvgArgs {
    /// Description of the SVG image to generate, e.g. "a small red circle icon".
    prompt: String,
}

/// Generates a sanitized SVG image from a text prompt via the standalone
/// `aivyx-vision-svg` crate, using the agent's own already-configured
/// LLM backend (see `generate_svg_completer::CoderTextCompleter`). The
/// agent's third network-reaching tool alongside `web_search`/
/// `web_fetch` — see those tools' own doc comments for the shared
/// "local-only means the LLM backend, not network isolation" reasoning.
pub struct GenerateSvgTool {
    completer: Arc<dyn TextCompleter>,
}

impl GenerateSvgTool {
    pub fn new(completer: Arc<dyn TextCompleter>) -> Self {
        Self { completer }
    }
}

#[async_trait]
impl Tool for GenerateSvgTool {
    fn name(&self) -> &str {
        "generate_svg"
    }

    // No `mutates_outside_session` override: the trait's fail-closed
    // default (`true`) is correct, matching `web_search`/`web_fetch` --
    // a network call is not session-local, regardless of whether it
    // mutates anything.

    // `needs_checkpoint` IS overridden, though, to `false`: generating an
    // SVG never touches the worktree, so there is nothing here for a
    // checkpoint to protect — see `WebSearchTool`'s identical override and
    // `Tool::needs_checkpoint`'s doc comment for the full rationale
    // (misattribution in the batch-rollback notice).
    fn needs_checkpoint(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Generate a sanitized SVG image from a text prompt. Returns the SVG \
                markup as a string -- use write_file separately if you want to save it. \
                Uses the same LLM backend as this conversation."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GenerateSvgArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GenerateSvgArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Network,
            target: PermissionTarget::Other(args.prompt.clone()),
            arguments_preview: json!({ "prompt": args.prompt }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GenerateSvgArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        match aivyx_vision_svg::generate_svg(self.completer.as_ref(), &args.prompt).await {
            Ok(svg) => Ok(ToolOutput::Ok(svg)),
            Err(e) => Ok(ToolOutput::Error(format!("generate_svg: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_vision_svg::TextCompleterError;
    use std::sync::Mutex;

    struct FakeCompleter {
        response: Mutex<Option<Result<String, TextCompleterError>>>,
    }

    impl FakeCompleter {
        fn returning(response: Result<String, TextCompleterError>) -> Self {
            Self {
                response: Mutex::new(Some(response)),
            }
        }
    }

    #[async_trait]
    impl TextCompleter for FakeCompleter {
        async fn complete(&self, _prompt: &str) -> Result<String, TextCompleterError> {
            self.response
                .lock()
                .unwrap()
                .take()
                .expect("FakeCompleter.complete called more than once")
        }
    }

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn mutates_outside_session_true_but_needs_checkpoint_false() {
        // Same split as web_search/web_fetch -- see those tests' own
        // comments.
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        assert!(
            tool.mutates_outside_session(),
            "generate_svg must stay hidden from plan mode"
        );
        assert!(
            !tool.needs_checkpoint(),
            "generate_svg must not trigger a git checkpoint"
        );
    }

    #[test]
    fn permission_request_is_network_tier() {
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        let request = tool
            .permission_request(&json!({"prompt": "a circle"}), Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Network);
    }

    #[tokio::test]
    async fn execute_returns_the_sanitized_svg_on_success() {
        let completer = FakeCompleter::returning(Ok(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>".to_string(),
        ));
        let tool = GenerateSvgTool::new(Arc::new(completer));
        let output = tool
            .execute(json!({"prompt": "a small circle"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Ok(svg) = output else {
            panic!("expected Ok output, got {output:?}")
        };
        assert!(svg.contains("<circle"));
    }

    #[tokio::test]
    async fn execute_reports_a_clear_error_when_generation_fails() {
        let completer = FakeCompleter::returning(Err(TextCompleterError("backend down".into())));
        let tool = GenerateSvgTool::new(Arc::new(completer));
        let output = tool
            .execute(json!({"prompt": "anything"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Error(msg) = output else {
            panic!("expected Error output, got {output:?}")
        };
        assert!(msg.contains("backend down"));
    }

    #[tokio::test]
    async fn execute_rejects_a_missing_prompt_field() {
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        let result = tool.execute(json!({}), &ctx()).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }
}
