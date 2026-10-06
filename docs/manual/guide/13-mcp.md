# MCP

aivyx-coder speaks the [Model Context Protocol](https://modelcontextprotocol.io)
both ways: it can **use** MCP servers' tools, and it can **be** an MCP
server that another tool hands coding work to.

## Using MCP servers

Add each server to your config:

```toml
[[mcp.servers]]
name = "github"
command = "/usr/local/bin/github-mcp-server"
args = ["stdio"]
env = { GITHUB_TOKEN = "…" }
timeout_secs = 30
```

At start-up every server is launched and asked what it offers, in
parallel; one that fails or doesn't answer within `timeout_secs` is skipped
with a warning. A server that dies mid-session is restarted on its next
use.

- **Tools** appear to the model as `mcp__<server>__<tool>`. Every call asks
  for your approval — an MCP tool is someone else's code, so it's never
  trusted to be read-only, whatever it claims. They're never available in
  `--auto`.
- **Resources and prompts** are read-only by the protocol, so the model
  reads them freely through four tools: `list_mcp_resources`,
  `read_mcp_resource`, `list_mcp_prompts` and `get_mcp_prompt`.

On Linux, MCP servers run inside the sandbox like any command. A server
that talks to a local daemon (docker, a database socket) needs
`[sandbox] allow_unix_sockets = true`. `npx`-based servers often need
`npx`'s cache directory in `[sandbox] extra_read_paths`, or use the
server's installed binary directly.

## Being an MCP server

```sh
aivyx-coder --mcp-server
```

runs aivyx-coder over standard input and output as an MCP server with two
tools, `code` and `code_reply`, so another local MCP client — Aivyx PA, for
example — can hand it a bounded coding task and follow up. Each task gets a
fresh, isolated session that lives until it has been idle for
`session_ttl_secs`.

It won't start until you set the highest access level callers may ask for:

```toml
[mcp_server]
max_access_level = "edit"     # "plan" | "edit" | "execute" — required, no default
session_ttl_secs = 1800
max_concurrent_sessions = 8
max_iterations = 10           # round trips per code / code_reply call
```

| Level | The session can |
|---|---|
| `plan` | read and search the project, and plan |
| `edit` | also write, edit, delete and move files |
| `execute` | also use the web, and — once pre-approval exists — commands and git (see below) |

A caller asking for more than `max_access_level` is refused, never quietly
downgraded. No level gets interactive processes or your own MCP servers.

**There is nobody to ask**, so sessions run under the same rules as
`--auto`: file changes stay inside the session's folder, a prompt-injection
hit stops further changes, and command tools — even at `execute` — are
refused unless pre-approved, which the MCP-server mode can't do yet, and
saving memory is always refused. So in practice `execute` adds web access
but not commands. Set the level no higher
than the caller needs.
