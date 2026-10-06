# Specialist teams

With a team switched on, the model you talk to becomes a **lead** that can
hand pieces of work to **specialists** — each with a role, a personality,
and a narrower set of tools than the lead. A reviewer that can only read
can't edit; a tester can run commands but not write files.

Teams are **off by default**:

```toml
[team]
enabled = true
```

## The built-in team

| Member | Can use |
|---|---|
| **coordinator** | the lead's entry — the agent you talk to, which keeps its own tools |
| **implementer** | read, write and edit files, search |
| **reviewer** | read, search, git history |
| **tester** | read, search, run commands |

## How the lead uses it

- **One request** — `delegate_to_specialist` gives a specialist a bounded
  task; it works in its own conversation and reports back.
- **A conversation** — `spawn_specialist` opens a session with one
  specialist that remembers earlier exchanges; `query_specialist` sends
  follow-ups; `close_specialist` ends it. At most
  `max_concurrent_specialist_sessions` (default 3) are open at once, and a
  session idle for `specialist_session_idle_timeout_secs` (default 600) is
  dropped. Specialists can consult each other directly, one hop deep.
- **A mission** — `decompose_task` records a plan of steps, each assigned
  to a specialist; `verify_output` records whether each step's result
  passed; `synthesize_results` records the final deliverable.

Every specialist action goes through the same approvals as yours: you'll
see prompts for a specialist's edits and commands just as for the lead's.
Each specialist gets its own private temporary directory, and its steps
show in the transcript.

## Your own roster

Point `roster_path` at a TOML file describing your team:

```toml
lead = "lead"

[[members]]
name = "lead"
role = "Plans the work and delegates"
persona = "Calm and methodical"
tool_allowlist = ["set_tasks"]

[[members]]
name = "docs-writer"
role = "Writes and updates documentation"
persona = "Clear and concise"
tool_allowlist = ["read_file", "write_file", "edit_file", "grep", "glob"]
extra_deny_paths = ["src/"]     # this member may not touch src/
task = "summarize"              # optional: routing task for this member
```

```toml
[team]
enabled = true
roster_path = "~/my-team.toml"
```

It's checked at start-up: an unknown lead, a duplicate member, or a tool
the lead itself doesn't have stops aivyx-coder with an error rather than
running with a broken team. `extra_deny_paths` adds to the global deny
list for that member only. With [model routing](08-models-and-routing.md)
on, `task` lets a member use a different model from the lead.
