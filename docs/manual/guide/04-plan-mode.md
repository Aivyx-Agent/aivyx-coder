# Plan mode

Plan mode lets the model look around and propose a plan before it touches
anything. Turn it on with **Ctrl+P**, or start with `aivyx-coder --plan`. A
magenta **PLAN** badge in the status line shows it's on.

## What changes

- The model can **read, search and build a task list** — the task panel
  becomes the plan you review.
- Tools that write files or run commands are **withheld** from the model,
  and if it invents a call anyway, the permission gate refuses it. This is
  enforced, not a suggestion: approvals you gave earlier don't apply while
  plan mode is on, and the model has no way to switch it off.
- Every task in the plan must stay *pending*, so the model can't report
  work as done when nothing was changed. Each reply ends with a reminder:
  "Plan mode — nothing was changed."

`/test` still works in plan mode — you typed it, so it isn't the model
acting.

## Approving the plan

Press **Ctrl+P** again. Plan mode turns off, and if there's a pending task
list and nothing is running, aivyx-coder sends "The plan is approved. Carry
it out now, task by task." for you, so work starts straight away. From then
on every change asks for approval as usual.

## Tips

- Plan mode is a good default for unfamiliar code or a vague request: ask
  for an investigation and a plan, read it, edit the request, then approve.
- A resumed conversation (`--resume`) remembers if it was in plan mode.
- In an editor over ACP, plan mode is a session mode you pick in the
  editor — see [Editor integration](12-editor-integration.md).
- Can't be combined with `--auto`, which is the opposite: acting without
  asking.
