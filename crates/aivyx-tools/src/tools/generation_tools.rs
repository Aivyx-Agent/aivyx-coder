//! `generate_image` and `generate_3d` -- both backed by the same
//! `Arc<dyn GenerationProvider>` (concretely `aivyx-vision-mold`'s
//! `MoldProvider` today; see `agent_builder.rs`). `generate_3d` always
//! fails today with `VisionError::Unsupported` -- mold's Pass B (async 3D
//! generation) isn't built yet -- but the tool ships now so it's
//! discoverable, and it starts working with zero changes here once a real
//! backend lands. See `docs/superpowers/specs/
//! 2026-09-18-vision-image-3d-adoption-design.md`.
//!
//! Unlike `generate_svg` (`ActionKind::Network`, an auto-allow tier here),
//! both tools in this file are `ActionKind::Write`: they write a real file
//! into the project (`assets/generated/<uuid>.<ext>`), which is the thing
//! that actually needs operator confirmation, not the network call that
//! produces its content. The permission target is the `assets/generated/`
//! directory itself, not the specific (backend-minted, never
//! operator-chosen) filename -- see this crate's own design doc for why
//! that's the correct Always-Allow cache key here, not a shortcut.

use std::path::Path;
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use aivyx_vision_core::{GeneratedAsset, GenerationProvider, ImageRequest, ThreeDRequest};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct GenerateImageArgs {
    /// Description of the image to generate, e.g. "a small red circle icon".
    prompt: String,
    width: Option<u32>,
    height: Option<u32>,
    seed: Option<u64>,
    /// A free-text style nudge, e.g. "watercolor", "pixel art".
    style_hint: Option<String>,
    /// Path to a reference image for image-to-image generation, absolute or relative to the working directory.
    reference_image: Option<String>,
}

pub struct GenerateImageTool {
    provider: Arc<dyn GenerationProvider>,
}

impl GenerateImageTool {
    pub fn new(provider: Arc<dyn GenerationProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for GenerateImageTool {
    fn name(&self) -> &str {
        "generate_image"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Generate an image from a text prompt via a local image-generation \
                backend and save it under assets/generated/. Returns the saved file's path. \
                `reference_image`, if given, enables image-to-image generation."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GenerateImageArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GenerateImageArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd.join("assets/generated")),
            arguments_preview: json!({ "prompt": args.prompt }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GenerateImageArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let reference_image = args
            .reference_image
            .as_ref()
            .map(|path| resolve(&ctx.cwd, path));
        let req = ImageRequest {
            prompt: args.prompt,
            reference_image,
            width: args.width,
            height: args.height,
            seed: args.seed,
            style_hint: args.style_hint,
        };
        match self.provider.generate_image(req).await {
            Ok(asset) => Ok(ToolOutput::Ok(success_message("image", &asset))),
            Err(e) => Ok(ToolOutput::Error(format!("generate_image: {e}"))),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct GenerateThreeDArgs {
    /// Description of the 3D model to generate.
    prompt: String,
}

pub struct GenerateThreeDTool {
    provider: Arc<dyn GenerationProvider>,
}

impl GenerateThreeDTool {
    pub fn new(provider: Arc<dyn GenerationProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for GenerateThreeDTool {
    fn name(&self) -> &str {
        "generate_3d"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Generate a 3D model from a text prompt and save it under \
                assets/generated/. **Not yet implemented** -- every call currently fails \
                with a clear error; the tool exists now so it's discoverable ahead of a \
                future backend that implements it."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(GenerateThreeDArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: GenerateThreeDArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(cwd.join("assets/generated")),
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
        let args: GenerateThreeDArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let req = ThreeDRequest {
            prompt: args.prompt,
            reference_image: None,
            style_hint: None,
        };
        match self.provider.generate_3d(req).await {
            Ok(asset) => Ok(ToolOutput::Ok(success_message("3D model", &asset))),
            Err(e) => Ok(ToolOutput::Error(format!("generate_3d: {e}"))),
        }
    }
}

fn success_message(kind: &str, asset: &GeneratedAsset) -> String {
    let seed_display = asset
        .seed_used
        .map(|s| s.to_string())
        .unwrap_or_else(|| "none".to_string());
    format!(
        "generated {kind} saved to {} (seed {seed_display})",
        asset.path.display()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aivyx_types::ToolOutput;
    use aivyx_vision_core::{FakeGenerationProvider, GeneratedAsset, VisionError};
    use serde_json::json;

    use super::*;

    fn sample_asset(path: &str) -> GeneratedAsset {
        GeneratedAsset {
            path: path.into(),
            backend: "mold".to_string(),
            seed_used: Some(42),
            generated_at: std::time::SystemTime::now(),
        }
    }

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn generate_image_metadata_is_sound() {
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let tool = GenerateImageTool::new(Arc::new(provider));
        assert_eq!(tool.name(), "generate_image");
        let request = tool
            .permission_request(&json!({"prompt": "a red circle"}), &ctx().cwd)
            .unwrap();
        assert_eq!(request.action, ActionKind::Write);
        assert_eq!(
            request.target,
            PermissionTarget::Path(ctx().cwd.join("assets/generated"))
        );
    }

    #[test]
    fn generate_image_needs_checkpoint_and_mutates_outside_session_default_true() {
        // Unlike generate_svg's deliberate false override, generate_image
        // has a real filesystem effect and must keep both defaults.
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let tool = GenerateImageTool::new(Arc::new(provider));
        assert!(tool.mutates_outside_session());
        assert!(tool.needs_checkpoint());
    }

    #[tokio::test]
    async fn generate_image_returns_the_generated_asset_on_success() {
        let provider =
            FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/out/abc.png")));
        let tool = GenerateImageTool::new(Arc::new(provider));
        let output = tool
            .execute(json!({"prompt": "a red circle"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Ok(msg) = output else {
            panic!("expected Ok output, got {output:?}")
        };
        assert!(msg.contains("/tmp/out/abc.png"));
        assert!(msg.contains("42"));
    }

    #[tokio::test]
    async fn generate_image_passes_width_height_seed_style_hint_through() {
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let provider = Arc::new(provider);
        let tool = GenerateImageTool::new(provider.clone());
        tool.execute(
            json!({
                "prompt": "a mountain",
                "width": 512,
                "height": 768,
                "seed": 7,
                "style_hint": "watercolor"
            }),
            &ctx(),
        )
        .await
        .unwrap();
        let captured = provider
            .captured_image_request()
            .expect("must capture the request");
        assert_eq!(captured.prompt, "a mountain");
        assert_eq!(captured.width, Some(512));
        assert_eq!(captured.height, Some(768));
        assert_eq!(captured.seed, Some(7));
        assert_eq!(captured.style_hint.as_deref(), Some("watercolor"));
    }

    #[tokio::test]
    async fn generate_image_fails_clearly_on_a_missing_prompt_field() {
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let tool = GenerateImageTool::new(Arc::new(provider));
        let result = tool.execute(json!({}), &ctx()).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }

    #[tokio::test]
    async fn generate_image_fails_clearly_when_generation_errors() {
        let provider = FakeGenerationProvider::with_image_result(Err(VisionError::GpuLockTimeout));
        let tool = GenerateImageTool::new(Arc::new(provider));
        let output = tool
            .execute(json!({"prompt": "anything"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Error(msg) = output else {
            panic!("expected Error output, got {output:?}")
        };
        assert!(msg.contains("generate_image"));
    }

    #[tokio::test]
    async fn generate_image_resolves_a_reference_image_relative_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ref.png"), b"fake png bytes").unwrap();
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let provider = Arc::new(provider);
        let tool = GenerateImageTool::new(provider.clone());
        let ctx = ToolExecutionContext {
            cwd: dir.path().to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };
        tool.execute(
            json!({"prompt": "edit this", "reference_image": "ref.png"}),
            &ctx,
        )
        .await
        .unwrap();
        let captured = provider.captured_image_request().unwrap();
        assert_eq!(
            captured.reference_image,
            Some(crate::path_resolve::resolve(dir.path(), "ref.png"))
        );
    }

    #[test]
    fn generate_3d_metadata_is_sound() {
        let provider = FakeGenerationProvider::with_3d_result(Ok(sample_asset("/tmp/x.glb")));
        let tool = GenerateThreeDTool::new(Arc::new(provider));
        assert_eq!(tool.name(), "generate_3d");
        let request = tool
            .permission_request(&json!({"prompt": "a small statue"}), &ctx().cwd)
            .unwrap();
        assert_eq!(request.action, ActionKind::Write);
    }

    #[tokio::test]
    async fn generate_3d_returns_the_generated_asset_on_success() {
        let provider = FakeGenerationProvider::with_3d_result(Ok(sample_asset("/tmp/out/x.glb")));
        let tool = GenerateThreeDTool::new(Arc::new(provider));
        let output = tool
            .execute(json!({"prompt": "a small statue"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Ok(msg) = output else {
            panic!("expected Ok output, got {output:?}")
        };
        assert!(msg.contains("/tmp/out/x.glb"));
    }

    #[tokio::test]
    async fn generate_3d_fails_clearly_when_unsupported() {
        let provider = FakeGenerationProvider::with_3d_result(Err(VisionError::Unsupported(
            "3D generation via mold (Pass B is not yet implemented)",
        )));
        let tool = GenerateThreeDTool::new(Arc::new(provider));
        let output = tool
            .execute(json!({"prompt": "a small statue"}), &ctx())
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Error(_)));
    }
}
