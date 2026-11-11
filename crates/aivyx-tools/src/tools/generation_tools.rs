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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget, path_is_denied};
use aivyx_types::{ImageData, ToolDefinition, ToolOutput};
use aivyx_vision_core::{
    GeneratedAsset, GenerationProvider, ImageRequest, ThreeDRequest, UpscaleRequest,
    generate_variations,
};
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
    /// Path to a reference image for image-to-image generation, absolute or relative to the working directory. Without width/height the result keeps its size.
    reference_image: Option<String>,
    /// Image-to-image only: how far the result may move from reference_image, from 0 (keep it) to 1 (ignore it). Default 0.75.
    strength: Option<f32>,
    /// Inpainting: path to a mask the same size as reference_image (white = repaint, black = keep). Needs reference_image.
    mask_image: Option<String>,
    /// How many variations to make (1-4), with consecutive seeds. Default 1.
    count: Option<usize>,
}

/// `deny_paths` is required separately from the usual
/// `permission_request`-level check for exactly the reason `GrepTool`'s own
/// doc comment explains: that check only sees the declared
/// `PermissionTarget` (here, `assets/generated/`, the *output* directory),
/// never `reference_image` — an entirely different, caller-supplied path
/// this tool also reads (for image-to-image generation) and hands to the
/// generation backend. Without its own check, `reference_image` could read
/// and transmit a `deny_paths`-protected file (e.g. `~/.ssh/id_rsa`) with
/// zero gate involvement, and — because the Always-Allow cache key is the
/// declared target alone — every call after the first approval would
/// silently auto-allow it too, regardless of `reference_image`.
pub struct GenerateImageTool {
    provider: Arc<dyn GenerationProvider>,
    deny_paths: Vec<PathBuf>,
    previews: bool,
}

impl GenerateImageTool {
    pub fn new(provider: Arc<dyn GenerationProvider>, deny_paths: Vec<PathBuf>) -> Self {
        Self {
            provider,
            deny_paths,
            previews: true,
        }
    }

    /// `[vision] show_model_previews` (default on): give the agent a small
    /// preview of each image, shown to a model that can take images.
    pub fn with_model_previews(mut self, on: bool) -> Self {
        self.previews = on;
        self
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
            target: PermissionTarget::Path(resolve(cwd, "assets/generated")),
            arguments_preview: json!({
                "prompt": args.prompt,
                "reference_image": args.reference_image,
                "mask_image": args.mask_image,
                "count": args.count,
            }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(self.execute_with_images(arguments, ctx).await?.0)
    }

    async fn execute_with_images(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<(ToolOutput, Vec<ImageData>), ToolError> {
        let args: GenerateImageArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let reference_image = args
            .reference_image
            .as_ref()
            .map(|path| resolve(&ctx.cwd, path));
        let mask_image = args.mask_image.as_ref().map(|path| resolve(&ctx.cwd, path));
        // `reference_image` and `mask_image` are never the declared
        // `PermissionTarget` above (that's `assets/generated/`, the output
        // directory) — so `ConfirmationGate::is_denied` never sees these
        // paths at all. Check them against `deny_paths` here, before they're
        // read and transmitted to the generation backend, the same way
        // `GrepTool`/`MoveFileTool` check every path they touch beyond their
        // own declared target.
        for (field, path) in [
            ("reference_image", &reference_image),
            ("mask_image", &mask_image),
        ] {
            if let Some(path) = path
                && path_is_denied(path, &self.deny_paths)
            {
                return Ok((
                    ToolOutput::Denied(format!(
                        "{field} `{}` is under a configured deny_paths entry (hard-blocked)",
                        path.display()
                    )),
                    Vec::new(),
                ));
            }
        }
        let req = ImageRequest {
            prompt: args.prompt,
            reference_image,
            mask_image,
            width: args.width,
            height: args.height,
            seed: args.seed,
            style_hint: args.style_hint,
            strength: args.strength,
        };
        let count = args.count.unwrap_or(1);
        if count != 1 {
            let results = match generate_variations(self.provider.as_ref(), req, count).await {
                Ok(results) => results,
                Err(e) => {
                    return Ok((ToolOutput::Error(format!("generate_image: {e}")), Vec::new()));
                }
            };
            let lines: Vec<String> = results
                .iter()
                .enumerate()
                .map(|(i, r)| match r {
                    Ok(asset) => format!("{}. {}", i + 1, success_message("image", asset)),
                    Err(e) => format!("{}. failed: {e}", i + 1),
                })
                .collect();
            if results.iter().all(|r| r.is_err()) {
                return Ok((
                    ToolOutput::Error(format!(
                        "generate_image: every variation failed\n{}",
                        lines.join("\n")
                    )),
                    Vec::new(),
                ));
            }
            let paths = results.iter().flatten().map(|asset| asset.path.clone());
            let images = previews(self.previews, paths).await;
            return Ok((
                ToolOutput::Ok(format!("{count} variations:\n{}", lines.join("\n"))),
                images,
            ));
        }
        match self.provider.generate_image(req).await {
            Ok(asset) => Ok((
                ToolOutput::Ok(success_message("image", &asset)),
                previews(self.previews, [asset.path.clone()]).await,
            )),
            Err(e) => Ok((ToolOutput::Error(format!("generate_image: {e}")), Vec::new())),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct UpscaleImageArgs {
    /// Path to a PNG or JPEG of up to 1 megapixel (for example 1024x1024), absolute or relative to the working directory.
    image: String,
}

/// Same `deny_paths` reasoning as `GenerateImageTool`: `image` is read and
/// sent to the backend but is not the declared permission target.
pub struct UpscaleImageTool {
    provider: Arc<dyn GenerationProvider>,
    deny_paths: Vec<PathBuf>,
    previews: bool,
}

impl UpscaleImageTool {
    pub fn new(provider: Arc<dyn GenerationProvider>, deny_paths: Vec<PathBuf>) -> Self {
        Self {
            provider,
            deny_paths,
            previews: true,
        }
    }

    /// `[vision] show_model_previews` (default on): give the agent a small
    /// preview of each image, shown to a model that can take images.
    pub fn with_model_previews(mut self, on: bool) -> Self {
        self.previews = on;
        self
    }
}

#[async_trait]
impl Tool for UpscaleImageTool {
    fn name(&self) -> &str {
        "upscale_image"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Enlarge an image four times with the local image-generation \
                backend's upscaler and save the result under assets/generated/. Returns the \
                saved file's path."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(UpscaleImageArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: UpscaleImageArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(resolve(cwd, "assets/generated")),
            arguments_preview: json!({ "image": args.image }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(self.execute_with_images(arguments, ctx).await?.0)
    }

    async fn execute_with_images(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<(ToolOutput, Vec<ImageData>), ToolError> {
        let args: UpscaleImageArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let image = resolve(&ctx.cwd, &args.image);
        if path_is_denied(&image, &self.deny_paths) {
            return Ok((
                ToolOutput::Denied(format!(
                    "image `{}` is under a configured deny_paths entry (hard-blocked)",
                    image.display()
                )),
                Vec::new(),
            ));
        }
        match self.provider.upscale_image(UpscaleRequest { image }).await {
            Ok(asset) => Ok((
                ToolOutput::Ok(success_message("image", &asset)),
                previews(self.previews, [asset.path.clone()]).await,
            )),
            Err(e) => Ok((ToolOutput::Error(format!("upscale_image: {e}")), Vec::new())),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct GenerateThreeDArgs {
    /// Description of the object to model.
    prompt: String,
    /// Path to a PNG or JPEG of one whole object on a plain background, absolute or relative to the working directory. Without it, an image of the object is generated first and kept.
    reference_image: Option<String>,
    /// A style nudge for the generated image, e.g. "low poly".
    style_hint: Option<String>,
    seed: Option<u64>,
}

/// Same `deny_paths` reasoning as `GenerateImageTool`: `reference_image`
/// is read and sent to the backend but is not the declared permission
/// target.
pub struct GenerateThreeDTool {
    provider: Arc<dyn GenerationProvider>,
    deny_paths: Vec<PathBuf>,
    previews: bool,
}

impl GenerateThreeDTool {
    pub fn new(provider: Arc<dyn GenerationProvider>, deny_paths: Vec<PathBuf>) -> Self {
        Self {
            provider,
            deny_paths,
            previews: true,
        }
    }

    /// `[vision] show_model_previews` (default on): give the agent a render
    /// of the mesh, shown to a model that can take images.
    pub fn with_model_previews(mut self, on: bool) -> Self {
        self.previews = on;
        self
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
            description: "Generate a 3D model of one object (a .glb mesh, untextured) and \
                save it under assets/generated/; takes about half a minute. Returns the saved \
                file's path. Without `reference_image`, an image of the object is generated \
                first and kept."
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
            target: PermissionTarget::Path(resolve(cwd, "assets/generated")),
            arguments_preview: json!({
                "prompt": args.prompt,
                "reference_image": args.reference_image,
            }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(self.execute_with_images(arguments, ctx).await?.0)
    }

    async fn execute_with_images(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<(ToolOutput, Vec<ImageData>), ToolError> {
        let args: GenerateThreeDArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let reference_image = args
            .reference_image
            .as_ref()
            .map(|path| resolve(&ctx.cwd, path));
        if let Some(path) = &reference_image
            && path_is_denied(path, &self.deny_paths)
        {
            return Ok((
                ToolOutput::Denied(format!(
                    "reference_image `{}` is under a configured deny_paths entry (hard-blocked)",
                    path.display()
                )),
                Vec::new(),
            ));
        }
        let req = ThreeDRequest {
            prompt: args.prompt,
            reference_image,
            style_hint: args.style_hint,
            seed: args.seed,
        };
        match self.provider.generate_3d(req).await {
            Ok(asset) => Ok((
                ToolOutput::Ok(success_message("3D model", &asset)),
                previews(self.previews, [asset.path.clone()]).await,
            )),
            Err(e) => Ok((ToolOutput::Error(format!("generate_3d: {e}")), Vec::new())),
        }
    }
}

/// A small JPEG of each image or `.glb` mesh (at most 768 px; a mesh is
/// rendered from two angles; `aivyx_vision_mold::preview`) for the model
/// to look at, or none when `on` is false. An image that
/// can't be previewed is left out; the result text still names its file.
async fn previews(on: bool, paths: impl IntoIterator<Item = PathBuf>) -> Vec<ImageData> {
    use base64::Engine;
    if !on {
        return Vec::new();
    }
    let paths: Vec<PathBuf> = paths.into_iter().take(crate::MAX_TOOL_IMAGES).collect();
    tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .filter_map(|path| aivyx_vision_mold::preview::model_preview(path).ok())
            .map(|(media_type, jpeg)| ImageData {
                media_type,
                data: base64::engine::general_purpose::STANDARD.encode(jpeg),
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn success_message(kind: &str, asset: &GeneratedAsset) -> String {
    let seed_display = asset
        .seed_used
        .map(|s| s.to_string())
        .unwrap_or_else(|| "none".to_string());
    let mut message = format!(
        "generated {kind} saved to {} (seed {seed_display})",
        asset.path.display()
    );
    // The backend's advisories (for example a size it adjusted), so the
    // model reports what it got rather than what it asked for.
    for warning in &asset.warnings {
        message.push_str(&format!("; backend note: {warning}"));
    }
    message
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aivyx_types::ToolOutput;
    use aivyx_vision_core::{FakeGenerationProvider, GeneratedAsset, MAX_VARIATIONS, VisionError};
    use serde_json::json;

    use super::*;

    fn sample_asset(path: &str) -> GeneratedAsset {
        GeneratedAsset {
            path: path.into(),
            backend: "mold".to_string(),
            seed_used: Some(42),
            generated_at: std::time::SystemTime::now(),
            warnings: Vec::new(),
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
        let tool = GenerateImageTool::new(Arc::new(provider), vec![]);
        assert_eq!(tool.name(), "generate_image");
        let request = tool
            .permission_request(&json!({"prompt": "a red circle"}), &ctx().cwd)
            .unwrap();
        assert_eq!(request.action, ActionKind::Write);
        assert_eq!(
            request.target,
            PermissionTarget::Path(resolve(&ctx().cwd, "assets/generated"))
        );
    }

    /// Answers every call with the seed it was asked for, failing the
    /// call numbered `fail`.
    struct Seeds {
        calls: std::sync::Mutex<u64>,
        fail: Option<u64>,
    }

    #[async_trait]
    impl GenerationProvider for Seeds {
        async fn generate_image(&self, req: ImageRequest) -> Result<GeneratedAsset, VisionError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if self.fail == Some(*calls) {
                return Err(VisionError::GenerationTimeout);
            }
            Ok(GeneratedAsset {
                seed_used: req.seed,
                ..sample_asset(&format!("assets/generated/{calls}.png"))
            })
        }
        async fn generate_3d(&self, _: ThreeDRequest) -> Result<GeneratedAsset, VisionError> {
            Err(VisionError::Unsupported("3D"))
        }
    }

    #[tokio::test]
    async fn generate_image_with_count_lists_every_variation_and_each_failure() {
        let provider = Arc::new(Seeds {
            calls: std::sync::Mutex::new(0),
            fail: Some(2),
        });
        let tool = GenerateImageTool::new(provider, vec![]);
        let output = tool
            .execute(json!({"prompt": "a cat", "seed": 5, "count": 3}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Ok(msg) = output else {
            panic!("expected Ok output, got {output:?}")
        };
        assert!(
            msg.contains("1. generated image saved to assets/generated/1.png (seed 5)"),
            "{msg}"
        );
        assert!(msg.contains("2. failed: generation timed out"), "{msg}");
        assert!(msg.contains("(seed 7)"), "{msg}");
    }

    #[tokio::test]
    async fn generated_images_come_with_a_preview_unless_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        image::RgbImage::from_pixel(1024, 512, image::Rgb([10, 20, 30])).save(&png).unwrap();
        let asset = || sample_asset(png.to_str().unwrap());

        let tool = GenerateImageTool::new(
            Arc::new(FakeGenerationProvider::with_image_result(Ok(asset()))),
            vec![],
        );
        let (output, images) = tool
            .execute_with_images(json!({"prompt": "a band"}), &ctx())
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(_)), "{output:?}");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/jpeg");
        use base64::Engine;
        let jpeg = base64::engine::general_purpose::STANDARD.decode(&images[0].data).unwrap();
        let preview = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((preview.width(), preview.height()), (768, 384));

        let off = GenerateImageTool::new(
            Arc::new(FakeGenerationProvider::with_image_result(Ok(asset()))),
            vec![],
        )
        .with_model_previews(false);
        let (_, images) = off
            .execute_with_images(json!({"prompt": "a band"}), &ctx())
            .await
            .unwrap();
        assert!(images.is_empty());
    }

    #[tokio::test]
    async fn generate_image_refuses_a_count_over_the_limit() {
        let provider = Arc::new(Seeds {
            calls: std::sync::Mutex::new(0),
            fail: None,
        });
        let tool = GenerateImageTool::new(provider, vec![]);
        let output = tool
            .execute(
                json!({"prompt": "a cat", "count": MAX_VARIATIONS + 1}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Error(_)), "{output:?}");
    }

    #[tokio::test]
    async fn generate_image_denies_a_mask_under_deny_paths() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let tool = GenerateImageTool::new(Arc::new(provider), vec![secret.clone()]);
        let output = tool
            .execute(
                json!({
                    "prompt": "repaint",
                    "reference_image": dir.path().join("ok.png"),
                    "mask_image": secret.join("mask.png"),
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let ToolOutput::Denied(msg) = output else {
            panic!("expected Denied, got {output:?}")
        };
        assert!(msg.contains("mask_image"), "{msg}");
    }

    #[tokio::test]
    async fn upscale_image_passes_the_path_and_honours_deny_paths() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeGenerationProvider::with_upscale_result(Ok(
            sample_asset("assets/generated/big.png"),
        )));
        let tool = UpscaleImageTool::new(provider.clone(), vec![dir.path().join("secret")]);
        assert_eq!(tool.name(), "upscale_image");
        let request = tool
            .permission_request(&json!({"image": "a.png"}), &ctx().cwd)
            .unwrap();
        assert_eq!(request.action, ActionKind::Write);

        let output = tool
            .execute(json!({"image": dir.path().join("a.png")}), &ctx())
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(_)), "{output:?}");
        assert_eq!(
            provider.captured_upscale_request().unwrap().image,
            dir.path().join("a.png")
        );

        let output = tool
            .execute(json!({"image": dir.path().join("secret/a.png")}), &ctx())
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Denied(_)), "{output:?}");
    }

    #[tokio::test]
    async fn generate_3d_passes_its_reference_and_honours_deny_paths() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeGenerationProvider::with_3d_result(Ok(sample_asset(
            "assets/generated/x.glb",
        ))));
        let tool = GenerateThreeDTool::new(provider.clone(), vec![dir.path().join("secret")]);
        let output = tool
            .execute(
                json!({"prompt": "a lantern", "reference_image": dir.path().join("l.png"), "seed": 3}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Ok(_)), "{output:?}");
        let captured = provider.captured_3d_request().unwrap();
        assert_eq!(captured.reference_image, Some(dir.path().join("l.png")));
        assert_eq!(captured.seed, Some(3));

        let output = tool
            .execute(
                json!({"prompt": "a lantern", "reference_image": dir.path().join("secret/l.png")}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Denied(_)), "{output:?}");
    }

    #[test]
    fn generate_image_needs_checkpoint_and_mutates_outside_session_default_true() {
        // Unlike generate_svg's deliberate false override, generate_image
        // has a real filesystem effect and must keep both defaults.
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let tool = GenerateImageTool::new(Arc::new(provider), vec![]);
        assert!(tool.mutates_outside_session());
        assert!(tool.needs_checkpoint());
    }

    /// mold's advisories reach the model (aivyx-vision audit 2026-11-11 L6).
    #[test]
    fn success_message_passes_on_the_backend_warnings() {
        let mut asset = sample_asset("assets/generated/a.png");
        asset.warnings = vec!["dimensions adjusted from 1000x1000 to 1024x1024".to_string()];
        let message = success_message("image", &asset);
        assert!(
            message.contains("backend note: dimensions adjusted from 1000x1000 to 1024x1024"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn generate_image_returns_the_generated_asset_on_success() {
        let provider =
            FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/out/abc.png")));
        let tool = GenerateImageTool::new(Arc::new(provider), vec![]);
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
    async fn generate_image_passes_its_optional_fields_through() {
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let provider = Arc::new(provider);
        let tool = GenerateImageTool::new(provider.clone(), vec![]);
        tool.execute(
            json!({
                "prompt": "a mountain",
                "width": 512,
                "height": 768,
                "seed": 7,
                "style_hint": "watercolor",
                "strength": 0.5
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
        assert_eq!(captured.strength, Some(0.5));
    }

    #[tokio::test]
    async fn generate_image_fails_clearly_on_a_missing_prompt_field() {
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let tool = GenerateImageTool::new(Arc::new(provider), vec![]);
        let result = tool.execute(json!({}), &ctx()).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
    }

    #[tokio::test]
    async fn generate_image_fails_clearly_when_generation_errors() {
        let provider = FakeGenerationProvider::with_image_result(Err(VisionError::GpuLockTimeout));
        let tool = GenerateImageTool::new(Arc::new(provider), vec![]);
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
        let tool = GenerateImageTool::new(provider.clone(), vec![]);
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

    /// Regression test for the whole-branch review finding this fix closes:
    /// `reference_image` is never the declared `PermissionTarget`
    /// (`assets/generated/`, the output directory), so
    /// `ConfirmationGate::is_denied` never checked it against `deny_paths`
    /// at all. `execute` must now refuse on its own, before the provider is
    /// ever called — asserted here via `captured_image_request()`, which
    /// stays `None` if `generate_image` was never invoked on the fake.
    #[tokio::test]
    async fn generate_image_refuses_a_reference_image_under_a_deny_paths_entry() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("id_rsa");
        std::fs::write(&secret, b"private key bytes").unwrap();
        let provider = FakeGenerationProvider::with_image_result(Ok(sample_asset("/tmp/x.png")));
        let provider = Arc::new(provider);
        let tool = GenerateImageTool::new(provider.clone(), vec![secret.canonicalize().unwrap()]);
        let ctx = ToolExecutionContext {
            cwd: dir.path().to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };

        let output = tool
            .execute(json!({"prompt": "x", "reference_image": "id_rsa"}), &ctx)
            .await
            .unwrap();

        assert!(
            matches!(output, ToolOutput::Denied(_)),
            "expected Denied, got {output:?}"
        );
        assert!(
            provider.captured_image_request().is_none(),
            "the provider must never be called when reference_image is denied"
        );
    }

    #[test]
    fn generate_3d_metadata_is_sound() {
        let provider = FakeGenerationProvider::with_3d_result(Ok(sample_asset("/tmp/x.glb")));
        let tool = GenerateThreeDTool::new(Arc::new(provider), vec![]);
        assert_eq!(tool.name(), "generate_3d");
        let request = tool
            .permission_request(&json!({"prompt": "a small statue"}), &ctx().cwd)
            .unwrap();
        assert_eq!(request.action, ActionKind::Write);
    }

    #[tokio::test]
    async fn generate_3d_returns_the_generated_asset_on_success() {
        let provider = FakeGenerationProvider::with_3d_result(Ok(sample_asset("/tmp/out/x.glb")));
        let tool = GenerateThreeDTool::new(Arc::new(provider), vec![]);
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
        let tool = GenerateThreeDTool::new(Arc::new(provider), vec![]);
        let output = tool
            .execute(json!({"prompt": "a small statue"}), &ctx())
            .await
            .unwrap();
        assert!(matches!(output, ToolOutput::Error(_)));
    }
}
