mod delete_file;
mod edit_file;
mod find_references;
mod generate_svg;
mod generate_svg_completer;
mod generation_tools;
mod git_branch;
mod git_commit;
mod git_pr;
mod git_push;
mod git_read;
mod glob;
mod go_to_definition;
mod grep;
mod load_skill;
mod mcp_meta;
mod mcp_tool;
mod memory_forget;
mod memory_read;
mod memory_write;
mod move_file;
mod patch_file;
mod read_file;
mod remember_preference;
mod repl;
mod run_command;
mod run_shell;
mod set_tasks;
mod web_fetch;
mod web_search;
mod write_file;

pub use delete_file::DeleteFileTool;
pub use edit_file::EditFileTool;
pub use find_references::FindReferencesTool;
pub use generate_svg::GenerateSvgTool;
pub use generate_svg_completer::CoderTextCompleter;
pub use generation_tools::{GenerateImageTool, GenerateThreeDTool};
pub use git_branch::GitBranchTool;
pub use git_commit::{GitCommitTool, confined_git};
pub use git_pr::GitPrTool;
pub use git_push::GitPushTool;
pub use git_read::GitReadTool;
pub use glob::GlobTool;
pub use go_to_definition::GoToDefinitionTool;
pub use grep::GrepTool;
pub use load_skill::LoadSkillTool;
pub use mcp_meta::{GetMcpPromptTool, ListMcpPromptsTool, ListMcpResourcesTool, ReadMcpResourceTool};
pub use mcp_tool::McpToolAdapter;
pub use memory_forget::MemoryForgetTool;
pub use memory_read::MemoryReadTool;
pub use memory_write::MemoryWriteTool;
pub use move_file::MoveFileTool;
pub use patch_file::PatchFileTool;
pub use read_file::ReadFileTool;
pub use remember_preference::RememberPreferenceTool;
pub use repl::{
    ReplResizeTarget, ReplSendTool, ReplStartTool, ReplStopTool, SharedReplSession,
    new_shared_repl_session,
};
pub use run_command::RunCommandTool;
pub use run_shell::RunShellTool;
pub use set_tasks::SetTasksTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;
pub use write_file::WriteFileTool;

/// A bounded, *unconfined* git read for a permission preview or a branch
/// lookup — run before the user approves anything, so it must never run a
/// program the repository's own config names (a confined command can write
/// `.git/config`): fsmonitor, hooks and the repository's filter drivers are
/// switched off. Diffs should still add `--no-ext-diff --no-textconv`.
/// `None` on any failure.
pub(crate) fn unconfined_git_capture(cwd: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(aivyx_checkpoint::unconfined_git_args_blocking(cwd))
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
