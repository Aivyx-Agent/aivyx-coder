# Editor integration

aivyx-coder can run inside your editor's agent panel instead of the
terminal, using the [Agent Client Protocol](https://agentclientprotocol.com)
(ACP). It's the same agent with the same safety: every edit and command
still needs your approval, shown in the editor's own permission UI.

## Zed

Add to your `settings.json`:

```json
{
  "agent_servers": {
    "aivyx-coder": {
      "command": "/path/to/aivyx-coder",
      "args": ["--acp"]
    }
  }
}
```

## VS Code

Install the [ACP Client](https://marketplace.visualstudio.com/items?itemName=formulahendry.acp-client)
extension and point it at `aivyx-coder --acp`. No aivyx-specific extension
is needed.

## First run from an editor

If aivyx-coder isn't set up yet, the editor offers a **terminal sign-in**
step: it runs `aivyx-coder --setup` in a terminal so you can pick your
server and model, then reconnects.

## What works there

- Slash commands appear in the editor's command picker — all of them
  except `/resume` and `/quit`.
- Plan mode is a session mode you choose in the editor.
- The editor's stop button stops the reply (including a `/test` run); an
  approval it cancels counts as denied.
- Conversations are saved with the terminal's, per project.

Not supported over ACP yet: `--auto`, `--resume`, and images or other
non-text content in prompts.

## Editor context

Any editor plugin can tell aivyx-coder which file is open, where the cursor
is, and what's selected, by writing a small JSON file to
`~/.local/state/aivyx-coder/editor-context/<hash>.json` (`<hash>` is the
same per-project key sessions use). aivyx-coder re-reads it every turn and
adds one line to the model's instructions — "Currently open in editor:
src/foo.rs, cursor at line 42." — never the file's contents. A file that's
missing, malformed, more than 5 minutes old, for a different project, or
pointing at a denied path is ignored. No plugin ships with aivyx-coder;
this is the contract a plugin writes to. `[editor_context] enabled`
(default on) turns it off.

The JSON schema (`schema_version: 1`):

```json
{
  "schema_version": 1,
  "workspace_root": "/abs/path/to/project",
  "file": "src/foo.rs",
  "cursor": { "line": 42, "column": 8 },
  "selection": { "start_line": 40, "end_line": 45 },
  "updated_at": "2026-07-18T12:00:00Z"
}
```

`workspace_root` is absolute and must canonicalize to aivyx-coder's own
`cwd`. `file` is relative to `workspace_root`. `cursor` is required,
1-indexed. `selection` is optional — omit the `selection` field (or set it
to `null`) when there's no active selection; 1-indexed, inclusive line
range, no column granularity in this version. `updated_at` is an RFC 3339
timestamp.

## Answering approvals from the editor

A plugin can also answer approval prompts. When a decision is pending,
aivyx-coder writes a request file to
`~/.local/state/aivyx-coder/editor-approval/<hash>-request.json` (mode
`0600`) and waits for either your answer in the terminal or a matching
`<hash>-response.json` from the plugin — whichever comes first wins. Both
files are deleted as soon as it's decided. `[editor_approval] enabled`
(default on, inert without a plugin) turns it off.

Request file schema:

```json
{
  "schema_version": 1,
  "request_id": "6a6e...-uuid",
  "target": "/home/user/project/src/foo.rs",
  "action_kind": "write",
  "old_content": "fn foo() {}\n",
  "new_content": "fn foo() -> i32 { 42 }\n"
}
```

`target` is an absolute, resolved path for `write`/`delete` (contrast this
with editor context's `file` field, which is relative to `workspace_root`);
for `execute` it's the command string (e.g. `"cargo test"`), and for
`mcp_tool` a description string (e.g. `"search (server: filesystem)"`) —
neither of those is a path. `action_kind`
is one of `write`, `delete`, `execute`, `mcp_tool`, each with different content
fields: `write` carries `old_content`/`new_content` (old empty for a brand-new
file); `delete` carries `old_content` plus `will_delete: true` (no
`new_content` key at all); `execute` carries `command`/`args`; `mcp_tool`
carries a `description` string. A `write` or `delete` request whose underlying
file can't be read as text (a binary file) never generates a request file at
all — the terminal remains the sole surface for that one decision, same as when
no editor integration is running.

Response file schema (written by the editor integration):

```json
{
  "schema_version": 1,
  "request_id": "6a6e...-uuid",
  "decision": "allow"
}
```

`decision` is one of `allow`, `deny`, `always_allow` — `always_allow`
feeds the exact same Always-Allow cache a terminal Always-Allow does,
keyed on the same exact target. A response whose `request_id` doesn't
match the currently pending request is ignored.

## Language-server tools

`go_to_definition` and `find_references` give the model exact symbol
lookups through `rust-analyzer`: which definition a call really resolves
to, and every place a symbol is used. They're always available; if
`rust-analyzer` isn't on your `PATH`, the first call says so. It starts on
first use, runs inside the sandbox, and each request is limited by
`[lsp] timeout_secs` (default 60, since the first index of a big project
takes a while). Rust only for now.
