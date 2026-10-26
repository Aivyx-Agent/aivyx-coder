# Autonomous and advanced modes

## Autonomous mode — `--auto`

```sh
aivyx-coder --auto "make the CSV parser handle quoted newlines"
```

aivyx-coder works toward the goal without asking you. The terminal UI stays
open so you can watch, and **Ctrl+C** stops it at any point. The run ends
when every task in its list is done, when its budget runs out, when three
turns in a row fail (the model server is down, say, or rejects every
request), or when you stop it. If the model works without a task list, it's asked to write one —
with the steps already done marked done — since that is how the run ends.
The model marks its own tasks done, so a run can end "goal achieved" with the
tests still failing (when it decides the rest is out of its hands, for
example a failure the sandbox causes); the notice then adds that the last
verification run failed.

**It needs a way to check its work.** Autonomous mode only edits without
asking because every batch of edits is tested. It uses your
`[verification] command`, or else the test command `/test` would detect,
and says which at start-up. It refuses to start if there's neither, or if
`[verification] command` names no real `allowed_commands` entry. A test
run that exits cleanly without running any tests (`cargo test` reporting
`0 passed`, `go test` with `[no test files]`, Jest or Vitest with
`--passWithNoTests`) doesn't count as a pass: the model is told it verified
nothing and must write tests. When a batch of edits keeps failing verification, it **rewinds** to the checkpoint
from before that batch, throws the failed attempt away, and carries on with
the remaining budget.

A batch that made things better is kept, though. Before rewinding, it runs
the test command on the edited files and again on the checkpoint, then
puts the edits back and compares the failing tests. If the edits fixed at
least one failing test and broke none, they stay, and the notice lists what
they fixed and what still fails — a failure that was already there, such as
a test that [only fails inside the sandbox](15-troubleshooting.md), says
nothing about the edits. This needs output it can read (`cargo test`,
pytest, `go test`, Vitest or Jest); with anything else, or when part of the suite didn't
report or fewer tests ran, it rewinds as before.

Tests you know fail only inside the sandbox can be listed in
`[verification] sandbox_failures` ([details](15-troubleshooting.md)): the
model is told they aren't its task, and a verification run whose only
failures they are counts as passed, so the run can end with the goal
achieved.

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

The time limit is exact: a turn still running when it passes is stopped,
the same way Ctrl+C stops it, and the run reports the budget exhausted.

**A last review (experimental).** With `final_review = true` under
`[autonomous]`, the first time the goal is reached the model gets one more
turn. It is asked to check the code against the task's spec field by field
(names, types, status codes, `null` versus left out) and fix any mismatch,
with a test. That turn counts against `max_iterations`, and it is skipped
on the last one. It is off by default while we measure whether it helps.

**What the run reports.** The model writes its own final answer, and it can
be wrong. In the build tests, models said fields were `null` when they
weren't, and that clippy was clean after it had printed three warnings. So
the stop notice adds what aivyx-coder checked itself, one line each:

```
autonomous run stopped: goal achieved after 5 iteration(s)
  verification: `test` passed
  final check `clippy`: passed, 3 warnings
  changed: 14 files (+2310 −12) — src/main.rs, src/db.rs, … and 9 more
  read-only files changed: data/airports.csv
```

- **Final checks:** `[verification] final_checks` names allowed commands
  (a linter, say) that aivyx-coder runs itself when the goal is reached.
- **Changed:** compares the project with a snapshot taken when the run
  started, leaving out build output.
- **Read-only files changed:** any of those files that are in
  `read_only_paths`.
- **`--report run.json`:** writes all of this as JSON when the run stops,
  for scripts and for looking back.

**Running without the TUI.** When stdout isn't a terminal, or with
`--headless`, `--auto` runs without the TUI: progress goes to stderr, the
stop notice to stdout, and the exit code says how the run ended (see
[`--headless`](../reference/01-command-line.md#--headless)). `SIGINT` or
`SIGTERM` stops it cleanly, with the report written. This is how the
benchmark and CI scripts run aivyx-coder.

A command that succeeds but prints compiler or linter warnings shows them in
its result too (`exit status: 0 (success), 3 warnings`), so the model sees
them.

**Keep the task's inputs fixed.** When a run builds from a brief and its
data, list them in `[permissions] read_only_paths`. The model can still read
them, but it can't change them. In a build test, a model whose own tests
disagreed with the brief's data changed the data. Every agent's
instructions already say the spec and its data are fixed. `read_only_paths`
enforces that for file edits. Commands are not covered: an approved command
can still write those files.

```toml
[permissions]
read_only_paths = ["BRIEF.md", "data"]

[verification]
command = "test"
final_checks = ["clippy"]
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
