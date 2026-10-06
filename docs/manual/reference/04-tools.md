# Tools

Every tool aivyx-coder can give the model, with the description the model
itself reads. Generated from the code by `scripts/gen-tools-reference.py` —
edit the tool, not this page.

**Asks first?** Reading, searching and fetching from the web don't ask.
Anything that writes, deletes, moves or runs something, calls an MCP
server, or saves memory asks you (`y` once, `a` always for that exact file,
command or topic, `n` no) — unless you pre-approved it. In plan mode the
tools that change things are hidden from the model and refused if it tries.
Files on your deny list are refused outright. Delegated sub-agents and
specialists ask for their own actions the same way. See the
[security model](05-security-model.md).


## Files

Read, write and change files in the project.

| Tool | Asks first? | What it does |
|---|---|---|
| `read_file` | no | Read the full contents of a text file. |
| `write_file` | yes | Create a file or overwrite it entirely with new content. Creates parent directories as needed. |
| `edit_file` | yes | Replace an exact substring in an existing file with new text. old_string must match exactly once in the file (include enough surrounding context to make it unique), unless replace_all is set. |
| `patch_file` | yes | Apply a unified-diff patch to an existing file. |
| `move_file` | yes | Move or rename a file or directory. Refuses if the destination already exists (no overwrite) or if source and destination are on different filesystems (no copy+delete fallback). |
| `delete_file` | yes | Delete a single file. Refuses to delete directories — use run_shell for that. |

## Search

Find things in the project.

| Tool | Asks first? | What it does |
|---|---|---|
| `grep` | no | Search file contents for a regex pattern (Rust regex syntax, not PCRE) under a directory, defaulting to the working directory. Respects .gitignore and does not follow symlinks. |
| `glob` | no | Find file paths matching a glob pattern (e.g. "**/*.rs") under a directory, defaulting to the working directory. The pattern is matched against paths relative to that directory. |

## Commands

Run programs. `run_command` only runs commands you listed in `[[permissions.allowed_commands]]`; `run_shell` runs anything, and always asks.

| Tool | Asks first? | What it does |
|---|---|---|
| `run_command` | yes | Run a pre-configured project command (e.g. build/test/lint) and get its exit status and output back. Commands are fixed by user configuration — you select one by name and cannot supply a program or arbitrary arguments. |
| `run_shell` | yes | Run an arbitrary shell command (via `sh -c`) and get its exit status and output back. |

## Interactive processes

Drive a long-running program such as a REPL or debugger (`[repl]`).

| Tool | Asks first? | What it does |
|---|---|---|
| `repl_start` | yes | Start a persistent, interactive process (e.g. a language REPL like `python3 -i`, a database CLI like `psql mydb`, or a long-running dev server like `npm run dev`) and get any output it produces immediately (e.g. a startup banner) back. |
| `repl_send` | no | Send input to the process started by repl_start and get its output back. Omit input (or send an empty string) to just check for new output without sending anything — useful for polling a long-running process. |
| `repl_stop` | no | Stop the process started by repl_start. Errors if no session is running. |

## Git

Reading history is free; writing asks.

| Tool | Asks first? | What it does |
|---|---|---|
| `git_read` | no | Inspect the git repository in the working directory (read-only): mode "status" shows branch and changed files, mode "diff" shows unstaged changes (set staged=true for staged ones, path to limit to one file/directory), mode "log" shows recent commits (count, default 10), mode "branches" shows local branches with upstream tracking info. |
| `git_commit` | yes | Stage and commit changes to the git repository in the working directory, with the given commit message. Commits all current changes unless specific paths are given. |
| `git_branch` | yes | Create a new branch (and switch to it) or switch to an existing branch. "create" takes an optional base ref to branch from (defaults to the current HEAD); "switch" moves to an already-existing branch. |
| `git_push` | yes | Push the current branch to a remote (default "origin"), setting upstream tracking if not already configured. Never force-pushes. |
| `git_pr` | yes | Open a pull request for the current branch via the gh CLI. The branch must already be pushed (use git_push first) — this returns a clear error naming that fix if there's no upstream configured yet. |

## Planning

The model's own task list, shown in the TUI.

| Tool | Asks first? | What it does |
|---|---|---|
| `set_tasks` | no | Replace your task list with a new one. Use this to plan multi-step work and track progress: write the full list of steps up front, then rewrite the list as statuses change (pending, in_progress, done). |

## Web

Only when `[web] enabled = true`. Inference stays local; these reach the network.

| Tool | Asks first? | What it does |
|---|---|---|
| `web_fetch` | no | Fetch a URL and return its readable text content (HTML is converted to plain text, scripts/styles stripped). Use this to read documentation, error message explanations, or API references. |
| `web_search` | no | Search the web via a configured SearXNG instance and return ranked results (title, URL, snippet). Use web_fetch on a result's URL to read the full page. |

## Language server

Uses rust-analyzer (`[lsp]`).

| Tool | Asks first? | What it does |
|---|---|---|
| `go_to_definition` | no | Resolve the symbol at a file position (1-indexed line/column) to its actual definition site via rust-analyzer. Use this when the repo map's symbol list isn't enough to tell which exact definition a call site resolves to. |
| `find_references` | no | Find every reference to the symbol at a file position (1-indexed line/column) across the whole workspace via rust-analyzer — unlike grep, this distinguishes the symbol from unrelated identifiers that merely share its name. |

## Memory and learning

See [Memory and learning](../guide/07-memory-and-learning.md).

| Tool | Asks first? | What it does |
|---|---|---|
| `memory_read` | no | Recall entries previously saved with memory_write under an exact topic, newest first. Returns an empty list if nothing has been saved under that topic. |
| `memory_write` | yes | Persist a small fact or note for future recall via memory_read — not shown to you automatically. |
| `memory_forget` | yes | Permanently delete every entry saved with memory_write under an exact topic. Returns how many entries were deleted (0 if the topic was never written). |
| `remember_preference` | yes | Propose an update to your own long-term memory of the user's preferences and working style — stored in a file that's automatically included in every future project, not just this one. |
| `load_skill` | no | Load the full body of one default or project/user skill by name, for step-by-step process guidance (e.g. systematic debugging, writing a plan, brainstorming and scoping). Available skills: {}., |

## Delegation and teams

Sub-agents and specialist teams. See [Specialist teams](../guide/11-specialist-teams.md).

| Tool | Asks first? | What it does |
|---|---|---|
| `delegate_task` | no | Delegate a bounded task to a fresh sub-agent with its own isolated conversation history and full tool access (same permissions as you). |
| `delegate_to_specialist` | no | Delegate a bounded task to a named team specialist -- a fresh sub-agent scoped to that specialist's own role and tool access (narrower than yours), with its own isolated conversation history. |
| `spawn_specialist` | no | Start a resumable session with one team specialist -- unlike delegate_to_specialist (a single exchange), the specialist stays alive so you can send it follow-ups with query_specialist, then end it with close_specialist when done. |
| `query_specialist` | no | Send a follow-up message to a specialist session opened with spawn_specialist -- the specialist remembers everything from earlier exchanges in this same session. Returns the specialist's response. |
| `close_specialist` | no | End a specialist session opened with spawn_specialist, freeing it up so a new session can be opened within the concurrent-session limit. Errors if session_id is unknown or already closed. |
| `decompose_task` | no | Decompose a mission into steps, each delegated to one team specialist. Call this once at the start of a team mission, before delegating anything. |
| `verify_output` | no | Record your verification judgment (pass or fail, with notes) for one step of the current mission plan, after reviewing a specialist's delegated output. Use the step number from decompose_task's own response. |
| `synthesize_results` | no | Record the final synthesized deliverable for the current mission, once every step has been delegated and verified. |

## MCP servers

Your `[[mcp.servers]]`' resources and prompts. Their own tools appear as `mcp__<server>__<tool>`. See [MCP](../guide/13-mcp.md).

| Tool | Asks first? | What it does |
|---|---|---|
| `list_mcp_resources` | no | List resources exposed by connected MCP servers (uri, name, description). Pass `server` to restrict to one server, or omit to aggregate across all of them. |
| `read_mcp_resource` | no | Fetch one MCP resource's content by server name and URI (see list_mcp_resources). Binary resources render as a placeholder note, not decoded. |
| `list_mcp_prompts` | no | List prompt templates exposed by connected MCP servers (name, description, declared arguments). Pass `server` to restrict to one server, or omit to aggregate across all of them. |
| `get_mcp_prompt` | no | Fetch one MCP prompt template's expanded content by server and name (see list_mcp_prompts), with optional arguments. Returns the resolved messages as text for you to read and use directly. |

## Images

`generate_svg` uses your model; `generate_image` needs `[vision] enabled = true`.

| Tool | Asks first? | What it does |
|---|---|---|
| `generate_svg` | no | Generate a sanitized SVG image from a text prompt. Returns the SVG markup as a string -- use write_file separately if you want to save it. |
| `generate_image` | yes | Generate an image from a text prompt via a local image-generation backend and save it under assets/generated/. Returns the saved file's path. |
| `generate_3d` | yes | Generate a 3D model from a text prompt and save it under assets/generated/. **Not yet implemented** -- every call currently fails with a clear error; the tool exists now so it's discoverable ahead of a future backend that implements it. |
