# Memory and learning

aivyx-coder carries knowledge between sessions in four ways, from the most
deliberate to the most automatic.

## `AGENTS.md` — instructions you write

Put conventions, architecture notes, build and test commands and "don't
touch X" rules in:

- **`AGENTS.md`** at the project root — for this project;
- **`~/.config/aivyx-coder/AGENTS.md`** — for every project.

Both are included in every turn and re-read each turn, so edits apply
immediately. Your personal file comes first; where they conflict, the
project's wins. Each file has a token budget (`[agents_file]
budget_tokens`, default 1024); a longer file is still included in full,
with a one-time notice.

`AGENTS.md` is trusted: it goes straight into the model's instructions. A
project you've just cloned can therefore steer the model from the first
turn — read its `AGENTS.md` like any other instructions before relying on
it. It can't get around the approval prompts or the sandbox.

## Learned preferences

The model can propose additions to your personal `AGENTS.md` with its
`remember_preference` tool — when you ask it to remember something, or when
it notices a clear pattern. You see the diff and approve or refuse **every
time**; this file never gets "always allow". Turn it off with
`enabled = false` under `[persona]`. Not available in `--auto`.

## Notes it keeps

For smaller facts, the model has `memory_write`, `memory_read` and
`memory_forget`: many small notes under topics, either for this project
(`project:…`) or global (`global:…`). Notes are only recalled when the model
asks for them, never added automatically. Saving and forgetting ask for
approval like any change (and "always allow" covers one exact topic).
Each topic keeps at most 1,000 notes. Not available in `--auto`.

## The project wiki

**`/wiki`** has the model write and maintain `docs/wiki/` — one page per
part of the workspace plus an architecture overview — with every page write
approved as usual. Later, bare `/wiki` regenerates only the pages whose
source changed since they were written; `/wiki <page>` regenerates one. The
repository map lists the pages so the model can read the right one when it
needs it.

## Skills

aivyx-coder ships a small library of skills — step-by-step guidance for
things like systematic debugging, writing a plan, or scoping a task. The
model loads one with `load_skill` when it's useful. Add your own in
`<skill-name>/SKILL.md` folders under `[skills] project_dir` or `user_dir`;
turn the library off with `enabled = false` under `[skills]`.
