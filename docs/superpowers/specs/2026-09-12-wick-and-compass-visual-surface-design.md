# "Wick & Compass" — aivyx-coder Visual Surface (Design)

## Context

Sub-project 4 of a 5-part ecosystem rebrand (1: identity definition —
done; 2: `aivyx-brand` deliverables — done; 3: `aivyx-pa` Studio UI —
done; **4: this one**; 5: `aivyx-website` redesign), itself carved out
of sub-project 4 of the larger pre-release go-to-market push.

Source of truth: `aivyx-brand/docs/superpowers/specs/2026-09-11-wick-and-compass-identity-design.md`
and `.../2026-09-11-wick-and-compass-tokens-design.md`.

## Grounding — why this sub-project is small

`aivyx-coder` is a terminal (TUI) coding agent, built with `ratatui`
(`crates/aivyx-tui`) — a fundamentally different visual surface from
`aivyx-pa`'s Dioxus/web Studio UI. Confirmed by reading the real code
before proposing anything:

- **No theme system exists.** `crates/aivyx-tui/src/app.rs` uses
  `ratatui::style::Color`'s built-in named ANSI colors
  (`Color::Yellow`/`Green`/`Red`/`Cyan`/`Magenta`/`DarkGray`/`Blue`/
  `LightYellow`) purely semantically — yellow for in-progress/tool-calls,
  green for success/tool-results, red for errors, cyan for user speech,
  magenta for council mode, etc. None of this traces to Neon
  Cartographer or any other brand palette; it's idiomatic terminal
  convention, already correct, with nothing to retire or migrate.
- **No logo, icon, or ASCII-art asset exists anywhere in the repo**
  (confirmed via a repo-wide search for `.svg`/`.png`/`.ico` and for
  banner/splash/ASCII-art code).
- **The README has only the 2 badges** (CI, license) added during the
  earlier GitHub-hygiene sub-project — no logo image.
- **No sibling repo has a README logo either**, including `aivyx-pa`'s
  own — confirmed by reading it. There's no existing ecosystem
  convention this repo would be breaking by skipping one, or matching
  by adding one.

Given this, the scope decision (confirmed with the user, who opted for
the fuller option each time) is: **add two new, small, additive pieces
of identity that don't exist today**, rather than migrate anything —
a terminal startup banner, and a README logo. Both are genuinely new
work, not retranscription.

## 1. Terminal startup banner

**Direction confirmed:** "Instrument Panel" — a bordered box (brass
rule), wordmark line, one status line with a rust-colored dot:

```
┌──────────────────────────────┐
│  aivyx-coder  v0.1.0          │
│  ● local models only          │
└──────────────────────────────┘
```

**Colors** (24-bit true-color ANSI escapes, via `crossterm`'s
`Color::Rgb`, matching the same real hex values used everywhere else
in this rebrand — no new palette invented):
- Border rule: brass `#c9a24b` (`Color::Rgb(0xc9, 0xa2, 0x4b)`)
- Wordmark ("aivyx-coder"): default terminal foreground, bold
- Version: slate `#8a95a1`
- Status dot (`●`): rust `#b5432b`
- Status text: slate `#8a95a1`

**Placement in the real code**: printed to plain stdout as the very
first statement inside `aivyx_tui::run()` (`crates/aivyx-tui/src/app.rs:136`),
*before* `TerminalGuard::init()` is ever constructed — i.e. before
`enable_raw_mode()`/`EnterAlternateScreen` run
(`crates/aivyx-tui/src/terminal.rs:22,41`). This means the banner
appears as normal scrolled terminal output for a brief moment, then the
TUI takes over the screen via the alternate-screen buffer — no timing/
animation logic needed, no risk of a keypress "skipping" it awkwardly,
and it naturally stays visible in the user's scrollback after the TUI
exits (a real, small UX bonus: `scrollback` retains a startup record).

This is TUI-frontend-specific — only `aivyx_tui::run()`'s call path
prints it. The ACP (`aivyx-acp`) and MCP-server (`aivyx-mcp-server`)
frontends are non-interactive (no human watching a terminal), and
neither one calls into `aivyx_tui::run()`, so they're naturally
unaffected without any extra gating logic.

**24-bit color support**: `crossterm`'s `Color::Rgb` degrades
gracefully on terminals that don't support true-color (24-bit) —
`crossterm` itself doesn't attempt fallback-downsampling to the
16-color palette, but terminal emulators without true-color support
generally either ignore the unsupported SGR sequence or approximate it
visually; this is standard, widely-shipped behavior across CLI tools
and not something this task needs to special-case.

## 2. README logo

**Real problem found during planning, not assumed**: `aivyx-brand`'s
existing `logos/aivyx-lockup-horizontal.svg` uses dark-theme token
values (wordmark fill `#e8e2d0`, a light off-white) — correct for the
app's own dark surfaces, but **nearly invisible dropped directly into
a README on GitHub's light theme** (a white background).

**Fix (confirmed with the user)**: use GitHub's supported `<picture>` +
`prefers-color-scheme` pattern — two real SVG variants, swapped
automatically by the viewer's OS theme:

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/aivyx-lockup-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/aivyx-lockup-light.svg">
  <img alt="aivyx-coder" src="docs/aivyx-lockup-light.svg" width="280">
</picture>
```

**A new asset is needed to make this work**: `aivyx-brand` currently
only has one lockup variant (the dark-theme one). A light-theme variant
doesn't exist yet anywhere in the ecosystem. Rather than create a
one-off light-theme lockup only inside `aivyx-coder`, this spec's
recommendation (matching `aivyx-brand`'s own established pattern of
owning every mark variant — it already has separate
`app-icon.svg`/`app-icon-dark.svg`/`app-icon-light.svg` for exactly
this same light/dark problem) is: **add a new
`logos/aivyx-lockup-horizontal-light.svg` to `aivyx-brand` first**,
using the identity's own already-defined light-theme token values
(ring `#a9822f`, flame `#963823`, wordmark `#1f3b4d`) applied to the
exact same geometry the dark lockup already uses — not new design
work, just the existing light-theme column of values already defined
in `design-tokens.md`, applied to an already-existing shape. This keeps
`aivyx-brand` as the single source of truth for every mark variant
(reusable later by `aivyx-website`'s own README/marketing needs in
sub-project 5), rather than letting a one-off light variant live only
in `aivyx-coder`.

Both variants (the existing dark one, copied in; the new light one,
created upstream and then copied in) are copied into `aivyx-coder`
under `docs/` (a new directory — this repo doesn't have one yet;
confirm during implementation whether a plain `docs/` root is the right
home or whether an `assets/` or `docs/images/` subdirectory reads more
naturally against this repo's real existing layout).

**Placement in `README.md`**: centered, directly below the H1 title and
above the two existing badges — matches the natural reading order (name
→ mark → status badges → description) without disrupting the badges'
own existing position relative to the opening paragraph.

## What this spec does not decide

- The exact `width` attribute for the README `<img>` tag — a reasonable
  value in the 240–320px range, refined visually during implementation
  once it's actually rendering in a real GitHub preview.
- Whether `docs/`, `docs/images/`, or `assets/` is the right directory
  name for the 2 copied SVG files — decide during implementation against
  this repo's real current layout.
- Exact terminal-width handling for the banner box if a user's terminal
  is narrower than the box's fixed width — the box is a fixed 34
  characters wide (matching the mockup exactly); if this wraps
  awkwardly on very narrow terminals, that's an acceptable, low-stakes
  edge case for a one-time startup banner, not something this spec
  requires solving.

## Downstream

Sub-project 5 (`aivyx-website` redesign) is next — the final piece of
the ecosystem rebrand, and can now use `aivyx-pa`'s real (rebranded)
Studio UI screenshots and the newly-created light-theme lockup variant
from this sub-project, rather than the old Neon Cartographer
`command-center.png` and dark-only lockup.
