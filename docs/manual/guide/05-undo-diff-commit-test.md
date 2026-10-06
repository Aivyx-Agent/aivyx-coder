# Undo, diff, commit and test

In a git repository, aivyx-coder keeps track of everything the assistant
changes, so you can review it, take it back, commit it, and check it.

## Checkpoints

Before every change the model makes — a file write or edit, a command — and
when each of your messages starts and its reply ends, aivyx-coder
snapshots the whole worktree into a hidden git ref
(`refs/aivyx/checkpoints/<time>`). It never touches your branch, index or
files, respects `.gitignore`, skips identical states, and keeps the newest
50. Turn it off with `checkpoints = false` under `[git]`.

## See what changed

After every turn that changed files, a dim line sums it up:
`Changed: stats.py (+3 −1) · test_stats.py (+12, new)`.

- **`/diff`** — every uncommitted change in a scrollable view (↑ ↓,
  PgUp/PgDn, Home/End; Esc closes).
- **`/diff turn`** — just the last turn's changes.

## Take it back

- **`/undo`** — rewinds the assistant's last turn as a whole: every edit,
  every command effect, and the work of any sub-agent it delegated to. It
  shows which files will change, flags any you edited afterwards, and asks
  first. Run it again to go back further.
- **`/redo`** — puts back what the last `/undo` removed.
- **`/checkpoints`** — lists the turns you can take back.

The model is told about an undo in your next message. Git-ignored files are
never touched. `/undo` after a `/commit` rewinds your files but leaves the
commit in place, and its preview says so.

The checkpoints are plain git refs, if you prefer git itself:

```
git for-each-ref refs/aivyx/checkpoints/     # list them
git diff <ref>                               # what changed since one
git checkout <ref> -- <path>                 # restore one file
```

## Commit

**`/commit`** commits what you've staged — or, if nothing is staged, every
uncommitted change — with a message the model drafts from the diff. Approve
it, press **e** to edit the message, or cancel; your staging is left exactly
as it was. `/commit -m "message"` commits with your own message straight
away. It uses your git identity and runs your hooks (inside the sandbox, so
a hook that writes outside the project may fail).

`/commit` never stages files on your deny list, and their contents are
never sent to the model. If the repository's `.git` is outside the folder
you started in (a subfolder, or a linked worktree), the sandbox can't write
it and `/commit` says so — start at the repository root, or commit with git.

**Generated files** such as `__pycache__/`, `node_modules/` and
`.pytest_cache/` are kept out of the change line, `/diff`, the undo
previews and what `/commit` stages — as long as git doesn't track them. The
list is `ignore` under `[git]`, in `.gitignore` syntax.

## Test

**`/test`** runs the project's tests: your `[verification] command` if you
set one, otherwise a command detected from the project:

| Found | Runs |
|---|---|
| `Cargo.toml` | `cargo test` |
| `go.mod` | `go test ./...` |
| `package.json` | `pnpm test`, `yarn test` or `npm test`, by lockfile |
| pytest markers (`pytest.ini`, `[tool.pytest]`, `conftest.py`) | `python3 -m pytest` |
| Python test files or a `tests/` folder | `python3 -m unittest` |
| a `Makefile` with a `test:` target | `make test` |

The start-up hint and `/help` show which one it found. `/test` needs no
approval (you typed it), runs inside the sandbox, works in plan mode, and
stops after 10 minutes or on Ctrl+C. The result and the last 80 lines of
output are added to your next message, marked as program output, so the
model can act on failures.

## Make the model check its own work

Set a verification command and the model can't end a turn that edited
files without running it:

```toml
[[permissions.allowed_commands]]
name = "test"
program = "cargo"
args = ["test"]

[verification]
command = "test"
max_auto_verify_retries = 3
```

After edits, aivyx-coder runs the command (labelled `auto-verify:` in the
transcript); on failure the model gets the output and tries to fix it, up
to `max_auto_verify_retries` times. If it still fails, the turn ends with a
loud notice pointing at the checkpoints. Each failure also says which
failing lines are new since the previous attempt.

For speed, `scoped_command` can name a second command that runs only the
tests for the files touched, with `{touched_paths}` in its arguments; a full
run is still required before edits count as verified. Not every test runner
can select tests by file — `cargo test <path>` silently matches nothing, so
Rust projects need a small wrapper script. A broken `scoped_command` makes
verification fail for that batch rather than quietly using the full one.

Because its arguments change on every retry, the scoped command doesn't use
the per-exact-command approval cache: it runs directly, inside the sandbox.
That's a deliberate, narrow exception to the rule that the model never
shapes a command's arguments — the only varying input is files it already
had approval to edit.

## The model's own git tools

Besides `git_commit`, the model can create and switch branches
(`git_branch`), push (`git_push`) and open a pull request through the `gh`
CLI (`git_pr`). Each asks first, like a command. `git_push` can never
force-push, branch and remote names starting with `-` are refused (git would
read them as options), and `git_pr` checks that the branch is pushed and
`gh` is signed in before trying.
