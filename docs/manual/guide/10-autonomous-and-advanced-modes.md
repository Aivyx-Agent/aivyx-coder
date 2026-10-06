# Autonomous and advanced modes

## Autonomous mode — `--auto`

```sh
aivyx-coder --auto "make the CSV parser handle quoted newlines"
```

aivyx-coder works toward the goal without asking you. The terminal UI stays
open so you can watch, and **Ctrl+C** stops it at any point. The run ends
when every task in its list is done, when its budget runs out, or when you
stop it.

**It needs a way to check its work.** Autonomous mode only edits without
asking because every batch of edits is tested. It uses your
`[verification] command`, or else the test command `/test` would detect,
and says which at start-up. It refuses to start if there's neither, or if
`[verification] command` names no real `allowed_commands` entry. When a
batch of edits keeps failing verification, it **rewinds** to the checkpoint
from before that batch, throws the failed attempt away, and carries on with
the remaining budget.

**What it may do on its own:**

- edit, write, delete and move files — but only inside the project folder,
  and never on your deny list;
- run commands you pre-approved in `[[permissions.allowed_commands]]` (and
  the detected test command);
- read, search and fetch from the web.

**What it never does unattended:** `run_shell`, `git_commit`, interactive
processes, MCP tool calls, saving memory or preferences. Those are hidden
from the model and refused if it tries.

**Prompt-injection guard.** Everything that comes back into the model's
context (file contents, command output, web pages) is scanned for text that
tries to give the model new instructions. A strong match **pauses the run**
with a notice and blocks any further change or command for the rest of the
session, so a person has to look. (In interactive use the same scan shows a
notice after the turn.) It's a pattern scan, not a guarantee.

**Budget**, under `[autonomous]`:

```toml
[autonomous]
max_iterations = 20       # "continue" round trips for the whole run
max_duration_secs = 3600  # wall-clock limit
```

Not with `--plan` or `--resume`, and not over ACP yet.

## Plan with a bigger model — `/architect`

```
/architect add pagination to the users endpoint
```

A separately configured model — usually a stronger one — writes an
implementation plan, which your main model then carries out straight away.
The architect only plans; it never calls tools. Set it up under
`[architect]` with `base_url` and `model`; unconfigured, `/architect`
explains what it needs (or, with routing on, routes the plan to a model
suited to planning). If planning fails, nothing runs.

## Ask a panel — `/council`

```
/council should this cache be per-request or per-process?
```

For hard design questions: several models answer independently, rank each
other's anonymised answers, and a chairman model writes one
recommendation. Bare `/council` reviews the assistant's last answer.

- **Read-only.** Council members get no tools and don't see your files —
  just your question and a short digest of the recent conversation.
- **Slow on purpose.** On one GPU the members run one after another; use it
  where a second opinion is worth minutes.
- **Only the conclusion stays.** The full discussion shows in the
  transcript; only the chairman's recommendation joins the conversation.

```toml
[council]
members = [
  { base_url = "http://localhost:11434/v1", model = "qwen3.5:9b" },
  { base_url = "http://localhost:11434/v1", model = "ornith:9b" },
]
chairman = { base_url = "http://localhost:11434/v1", model = "qwen3.6:27b" }
```

It needs at least two members and a chairman.

## Hand off a side task — `delegate_task`

The model can hand a bounded piece of work to a fresh sub-agent with its
own, separate conversation — useful for exploring something without
cluttering the main context. The sub-agent works under the same approvals,
checkpoints and plan mode as you; its steps show in the transcript prefixed
`sub-agent>`, and only its final answer comes back. It has its own budget
(`[sub_agent] max_iterations`, default 10), can't delegate further, and
can't use interactive processes.

For a standing team of specialists instead, see
[Specialist teams](11-specialist-teams.md).
