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

Report it privately on GitHub: [Report a vulnerability](https://github.com/Aivyx-Agent/aivyx-coder/security/advisories/new)
(this repo's **Security** tab). Only the maintainer sees the report
until a fix is published. Please don't open a public issue for a
vulnerability.

This is a small, solo-maintained project: reports will be
read and acknowledged, and we aim to resolve or provide a remediation
plan for a confirmed vulnerability within 90 days of the report, or
coordinate a later disclosure date directly with the reporter if a fix
genuinely needs longer. Credit is offered in release notes at the
reporter's preference.
