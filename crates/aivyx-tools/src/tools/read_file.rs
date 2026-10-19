use std::path::Path;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::bounded_read;
use crate::path_resolve::resolve;
use crate::{Tool, ToolError, ToolExecutionContext};

/// Lines `read_file` returns when no `limit` is given.
pub const DEFAULT_READ_LINES: usize = 400;

/// The most text one `read_file` call returns, whatever the line count: a
/// handful of whole-file reads used to fill a 32k-token window inside one
/// turn (live test on a 20k-line Rust project, 2026-10-19), where nothing
/// can be compacted away.
pub const MAX_READ_CHARS: usize = 20_000;

#[derive(Deserialize, JsonSchema)]
struct ReadFileArgs {
    /// Path to the file to read, absolute or relative to the working directory.
    path: String,
    /// First line to return, counting from 1. Defaults to 1.
    #[serde(default)]
    offset: Option<usize>,
    /// How many lines to return. Defaults to 400.
    #[serde(default)]
    limit: Option<usize>,
}

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Read a text file: up to 400 lines (and 20,000 characters) from `offset` \
                (default line 1), or `limit` lines. A longer file ends with a note giving the \
                line range shown and the total, so read on with a later `offset`, or use grep to \
                find the part you need instead of reading a large file whole."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(ReadFileArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: ReadFileArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(cwd, &args.path);

        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Path(resolved),
            arguments_preview: json!({ "path": args.path }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadFileArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let resolved = resolve(&ctx.cwd, &args.path);

        let capped = tokio::task::spawn_blocking(move || {
            bounded_read::read_capped(&resolved, bounded_read::READ_FILE_LIMIT)
        })
        .await
        .map_err(|err| ToolError::ExecutionFailed(format!("read_file task failed: {err}")))??;
        let offset = args.offset.unwrap_or(1).max(1);
        let limit = args.limit.unwrap_or(DEFAULT_READ_LINES).max(1);
        Ok(ToolOutput::Ok(select_lines(&capped, offset, limit)))
    }
}

/// Lines `offset..offset + limit` of the file, cut at `MAX_READ_CHARS`, with
/// a closing note whenever that isn't the whole file. A whole file that fits
/// is returned exactly as before, with no note.
fn select_lines(capped: &bounded_read::Capped, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = capped.text.split_inclusive('\n').collect();
    let total = lines.len();
    // "of N" is only certain when the byte cap didn't cut the file.
    let total_text = if capped.truncated {
        format!(
            "at least {total} (only the first {} of this {} file can be read)",
            bounded_read::human_size(bounded_read::READ_FILE_LIMIT),
            bounded_read::human_size(capped.size)
        )
    } else {
        total.to_string()
    };
    if offset > total {
        return format!("[read_file: the file has {total_text} lines — nothing at line {offset}]");
    }
    let start = offset - 1;
    let mut out = String::new();
    let mut end = start;
    for line in lines.iter().skip(start).take(limit) {
        if !out.is_empty() && out.len() + line.len() > MAX_READ_CHARS {
            break;
        }
        out.push_str(line);
        end += 1;
    }
    // A single line longer than the cap.
    if out.len() > MAX_READ_CHARS {
        let mut cut = MAX_READ_CHARS;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    if start == 0 && end == total && !capped.truncated {
        return out;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let next = if end < total || capped.truncated {
        format!(" — pass offset={} to read on, or grep for the part you need", end + 1)
    } else {
        String::new()
    };
    out.push_str(&format!(
        "\n[read_file: lines {}-{end} of {total_text}{next}]",
        start + 1
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ctx(dir: &Path) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: dir.to_path_buf(),
            confiner: Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn a_device_is_refused_rather_than_read_forever() {
        let dir = tempfile::tempdir().unwrap();
        let out = ReadFileTool.execute(json!({"path": "/dev/zero"}), &ctx(dir.path())).await;
        let err = out.expect_err("/dev/zero must be refused");
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    async fn read(dir: &Path, args: serde_json::Value) -> String {
        let ToolOutput::Ok(text) = ReadFileTool.execute(args, &ctx(dir)).await.unwrap() else {
            panic!("expected Ok");
        };
        text
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[tokio::test]
    async fn a_small_file_comes_back_whole_with_no_note() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), numbered(10)).unwrap();
        assert_eq!(read(dir.path(), json!({"path": "a.rs"})).await, numbered(10));
    }

    #[tokio::test]
    async fn a_long_file_stops_at_the_default_lines_and_says_where_to_go_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), numbered(1000)).unwrap();
        let text = read(dir.path(), json!({"path": "a.rs"})).await;
        assert!(text.starts_with("line 1\n"));
        assert!(text.contains("line 400\n"));
        assert!(!text.contains("line 401\n"));
        assert!(text.ends_with("[read_file: lines 1-400 of 1000 — pass offset=401 to read on, or grep for the part you need]"), "{text}");
    }

    #[tokio::test]
    async fn offset_and_limit_select_a_range() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), numbered(1000)).unwrap();
        let text = read(dir.path(), json!({"path": "a.rs", "offset": 995, "limit": 50})).await;
        assert!(text.starts_with("line 995\n"));
        assert!(text.contains("line 1000\n"));
        assert!(text.ends_with("[read_file: lines 995-1000 of 1000]"), "{text}");
        let past = read(dir.path(), json!({"path": "a.rs", "offset": 2000})).await;
        assert!(past.contains("the file has 1000 lines"), "{past}");
    }

    #[tokio::test]
    async fn a_read_is_cut_at_the_character_cap() {
        let dir = tempfile::tempdir().unwrap();
        let wide: String = (0..300).map(|i| format!("{i:0>199}\n")).collect();
        std::fs::write(dir.path().join("w.txt"), &wide).unwrap();
        let text = read(dir.path(), json!({"path": "w.txt"})).await;
        // 100 lines of 200 chars fill the 20,000-char cap exactly.
        assert!(text.contains("[read_file: lines 1-100 of 300 — pass offset=101"), "{}", &text[text.len() - 120..]);
        assert!(text.len() < MAX_READ_CHARS + 200);
    }

    #[tokio::test]
    async fn a_large_file_is_cut_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(crate::bounded_read::READ_FILE_LIMIT as usize * 2);
        std::fs::write(dir.path().join("big.log"), &big).unwrap();
        let ToolOutput::Ok(text) =
            ReadFileTool.execute(json!({"path": "big.log"}), &ctx(dir.path())).await.unwrap()
        else {
            panic!("expected Ok");
        };
        assert!(text.len() < MAX_READ_CHARS + 300);
        assert!(text.contains("only the first 1.0 MiB of this 2.0 MiB file"), "{}", &text[text.len() - 200..]);
        assert!(text.contains("grep"));
    }
}
