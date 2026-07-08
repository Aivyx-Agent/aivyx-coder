use std::path::Path;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::diff::unified_diff;
use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct EditFileArgs {
    /// Path to the file to edit, absolute or relative to the working directory.
    path: String,
    /// Exact text to find. Must match exactly once in the file unless replace_all is set.
    old_string: String,
    /// Text to replace old_string with.
    new_string: String,
    /// If true, replace every occurrence of old_string instead of requiring exactly one match.
    #[serde(default)]
    replace_all: bool,
}

/// Pure exact-match search/replace, shared by `permission_request` (a dry
/// run, to build the diff) and `execute` (the real run). 0 matches, or
/// more than 1 without `replace_all`, is a usage error — the model needs
/// to supply more context or opt into `replace_all`, not something a
/// human should be asked to approve/deny.
fn apply_edit(
    old: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<String, ToolError> {
    if old_string.is_empty() {
        return Err(ToolError::InvalidArguments(
            "old_string must not be empty".to_string(),
        ));
    }

    let match_count = old.matches(old_string).count();
    if match_count == 0 {
        return Err(ToolError::InvalidArguments(
            "old_string not found in file".to_string(),
        ));
    }
    if match_count > 1 && !replace_all {
        return Err(ToolError::InvalidArguments(format!(
            "old_string matches {match_count} places; include more surrounding context to make it unique, \
             or set replace_all to true"
        )));
    }

    Ok(if replace_all {
        old.replace(old_string, new_string)
    } else {
        old.replacen(old_string, new_string, 1)
    })
}

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Replace an exact substring in an existing file with new text. old_string must \
                match exactly once in the file (include enough surrounding context to make it unique), \
                unless replace_all is set. Fails if the file doesn't exist or old_string isn't found — \
                use write_file to create a new file."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(EditFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: EditFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(cwd, &args.path);

        let old_content = std::fs::read_to_string(&resolved).map_err(|err| {
            ToolError::ExecutionFailed(format!("cannot edit {}: {err}", resolved.display()))
        })?;
        let new_content = apply_edit(
            &old_content,
            &args.old_string,
            &args.new_string,
            args.replace_all,
        )?;
        let preview = Some(unified_diff(
            &resolved.display().to_string(),
            &old_content,
            &new_content,
        ));

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Write,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path, "replace_all": args.replace_all }),
            preview,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: EditFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(&ctx.cwd, &args.path);

        let old_content = tokio::fs::read_to_string(&resolved).await.map_err(|err| {
            ToolError::ExecutionFailed(format!("cannot edit {}: {err}", resolved.display()))
        })?;
        let new_content = apply_edit(
            &old_content,
            &args.old_string,
            &args.new_string,
            args.replace_all,
        )?;
        tokio::fs::write(&resolved, &new_content).await?;

        Ok(ToolOutput::Ok(format!("edited {}", resolved.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_single_match() {
        let result =
            apply_edit("fn foo() {}\nfn bar() {}\n", "fn foo()", "fn baz()", false).unwrap();
        assert_eq!(result, "fn baz() {}\nfn bar() {}\n");
    }

    #[test]
    fn zero_matches_is_an_error() {
        let err = apply_edit("fn foo() {}\n", "fn missing()", "x", false).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn ambiguous_match_without_replace_all_is_an_error() {
        let err = apply_edit("a\na\n", "a", "b", false).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn replace_all_replaces_every_match() {
        let result = apply_edit("a\na\n", "a", "b", true).unwrap();
        assert_eq!(result, "b\nb\n");
    }

    #[test]
    fn empty_old_string_is_rejected() {
        let err = apply_edit("anything", "", "x", false).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
