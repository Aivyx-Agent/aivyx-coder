# Security Policy

aivyx-coder is a local-only tool — no server, no multi-tenant deployment.
The security boundary that matters is the sandbox/permission model:
`ActionKind` permission tiers, the `ConfirmationGate`, Landlock/seccomp
process confinement, and `deny_paths`. Vulnerabilities in scope are things
like sandbox escapes, permission-gate bypasses, or confinement gaps — not
"the LLM produced a bad answer" or similar model-quality issues.

Known, already-documented, accepted risk surface — not new findings — is
listed in `README.md`'s "Known limitations" section (indirect prompt
injection, unrestricted network access for approved commands, inherited
environment variables, TOCTOU windows on path resolution,
`AIVYX_DEBUG_LOG` plaintext logging, the `Tool::execute`
convention-not-type-system boundary, and git-specific caveats). Read that
section before reporting — if your finding is already listed there, it's
known and accepted, not a new report.

This is not a bug bounty program.

## Reporting a vulnerability

Email **jccorbett67@gmail.com** with details. This repo is currently
private, so GitHub Security Advisories' private vulnerability
reporting isn't available yet (GitHub only offers it on public
repositories) — it will be added as a second channel if this repo
goes public. This is a small, solo-maintained project: reports will be
read and acknowledged, and we aim to resolve or provide a remediation
plan for a confirmed vulnerability within 90 days of the report, or
coordinate a later disclosure date directly with the reporter if a fix
genuinely needs longer. Credit is offered in release notes at the
reporter's preference.
