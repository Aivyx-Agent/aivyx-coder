# "Wick & Compass" — aivyx-coder Visual Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two new, small, additive pieces of Wick & Compass identity to `aivyx-coder` that don't exist today — a terminal startup banner and a theme-adaptive README logo — plus the one upstream asset the README logo needs.

**Architecture:** Spans two repos, no unified branch — each repo's work happens on its own branch, merged independently. `aivyx-brand` gets one new SVG file (a light-theme lockup variant). `aivyx-coder` gets a new banner function in `aivyx-tui`, two copied SVG files, and a `README.md` edit.

**Tech Stack:** Rust/`crossterm` (the banner, using real ANSI 24-bit color — `aivyx-tui` already depends on `crossterm = "0.28.1"`), plain SVG/Markdown (the logo work).

## Global Constraints

- Banner colors: brass `#c9a24b` (border/title accent), rust `#b5432b` (status dot), slate `#8a95a1` (version/status text) — no new palette values.
- **Real, verified type collision to avoid**: `crates/aivyx-tui/src/app.rs` already has `use ratatui::style::{Color, Modifier, Style};` at file scope. `ratatui::style::Color::Rgb` is a **tuple variant** (`Rgb(u8, u8, u8)`); `crossterm::style::Color::Rgb` is a **struct variant** (`Rgb { r: u8, g: u8, b: u8 }`) — different shapes, same enum name. A bare `Color::Rgb { r, g, b }` inside `app.rs` resolves to the file-scope `ratatui::style::Color` import and **fails to compile**. Every reference in this plan's code uses the fully-qualified `crossterm::style::Color::Rgb { r, g, b }` for exactly this reason — do not simplify it back to a bare `Color::Rgb` reference.
- The banner is TUI-frontend-specific — only the call path through `aivyx_tui::run()` prints it; `aivyx-acp`/`aivyx-mcp-server` never call `run()`, so no extra gating logic is needed or wanted.
- The README `<picture>` element's light variant must be genuinely visible on a white background (dark text/marks) and the dark variant genuinely visible on a dark background (light text/marks) — verify the `media` query maps to the correct file, not swapped.
- `aivyx-brand`'s new light lockup file reuses the exact same wordmark path data as the existing dark file (real Fraunces-extracted glyph paths) — copy the `<path d="...">` value verbatim, change only fill/stroke colors.
- No scope beyond what's specified here — no new ASCII art, no additional badges, no unrelated refactoring.

---

### Task 1 (repo: `aivyx-brand`): light-theme lockup variant

**Files:**
- Create: `logos/aivyx-lockup-horizontal-light.svg`

**Interfaces:** Produces the file Task 3 (in `aivyx-coder`) copies in. No
dependency on anything else in this plan.

- [ ] **Step 1: Create the branch**

```bash
cd /home/julian/Projects/Rust/aivyx-brand
git checkout -b wick-and-compass-coder-assets
```

- [ ] **Step 2: Create the light-theme variant**

Same geometry and wordmark path data as the existing
`logos/aivyx-lockup-horizontal.svg`, with 3 color substitutions applied
(ring, flame outer, flame inner-highlight, wordmark fill — all real
light-theme values already defined in `design-tokens.md`, not new
design work):

```bash
python3 << 'PYEOF'
with open('logos/aivyx-lockup-horizontal.svg') as f:
    content = f.read()

replacements = [
    ('<title>Aivyx Horizontal Lockup</title>', '<title>Aivyx Horizontal Lockup — Light</title>'),
    ('stroke="#c9a24b"', 'stroke="#a9822f"'),   # ring: brass (dark) -> brass (light)
    ('fill="#b5432b"', 'fill="#963823"'),       # flame outer: rust (dark) -> rust (light)
    ('fill="#c95a3f"', 'fill="#a84428"'),       # flame inner highlight: secondary-hover (dark) -> (light)
    ('fill="#e8e2d0"', 'fill="#1f3b4d"'),       # wordmark: text-primary (dark) -> text-primary (light)
]

for old, new in replacements:
    assert old in content, f"pattern not found verbatim, stop and check the source file manually:\n{old}"
    content = content.replace(old, new)

with open('logos/aivyx-lockup-horizontal-light.svg', 'w') as f:
    f.write(content)

print("wrote logos/aivyx-lockup-horizontal-light.svg")
PYEOF
```

- [ ] **Step 3: Verify**

```bash
python3 -c "import xml.etree.ElementTree as ET; ET.parse('logos/aivyx-lockup-horizontal-light.svg'); print('valid XML')"
diff logos/aivyx-lockup-horizontal.svg logos/aivyx-lockup-horizontal-light.svg
```

Expected: `valid XML`, then a diff showing exactly the 5 lines changed
above (title + 4 color values) and nothing else — the wordmark's long
`<path d="...">` value itself must show as unchanged/identical between
the two files (only its `fill` attribute value differs).

If a browser/image viewer or `rsvg-convert` is available, render both
files and visually confirm: the light variant reads as dark ink on
whatever background it's placed against (won't self-evidently prove the
"visible on white" claim without a real white background behind it —
if you can, composite it against a white rect to confirm, e.g. wrap it
in a temporary `<svg><rect width="100%" height="100%" fill="white"/>...`
test file, not committed).

- [ ] **Step 4: Commit**

```bash
git add logos/aivyx-lockup-horizontal-light.svg
git commit -m "feat: add light-theme lockup variant for cross-theme README use

aivyx-coder needs a version of the lockup visible on GitHub's light
README theme -- the existing lockup uses dark-theme token values
(wordmark fill #e8e2d0, near-white) and is nearly invisible on a white
background. Same geometry and real Fraunces wordmark path data as the
existing dark file; only ring/flame/wordmark colors change, to the
identity's own already-defined light-theme token values."
```

- [ ] **Step 5: Merge to main**

```bash
git checkout main
git pull
git merge wick-and-compass-coder-assets --no-edit
git branch -d wick-and-compass-coder-assets
```

---

### Task 2 (repo: `aivyx-coder`): terminal startup banner

**Files:**
- Modify: `crates/aivyx-tui/src/app.rs` (new `startup_banner()` function + one call site + one test)

**Interfaces:**
- Produces: `fn startup_banner() -> String` — a private, free function in
  `app.rs`, callable from `run()`. Returns the full multi-line, ANSI-
  colored banner as a single `String` (dynamic-width box, computed from
  the real content so a longer future version string can't misalign the
  border).
- Consumes: nothing from Task 1 (independent, different repo).

- [ ] **Step 1: Create the branch**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
git checkout -b wick-and-compass-coder-assets
```

- [ ] **Step 2: Add `startup_banner()` — read the real current top of `crates/aivyx-tui/src/app.rs` first to confirm the import list at lines 1-18 still matches what this plan assumes (specifically that `ratatui::style::Color` is still imported at file scope, and `crossterm::style::Stylize`/`crossterm::style::Color` are not)**

If the imports differ from what's described in the Global Constraints
section above, stop and re-derive the fully-qualified-vs-bare-`Color`
approach against the real current state before proceeding — don't apply
this step's code blindly if the collision risk it's designed around has
changed.

Insert this function into `crates/aivyx-tui/src/app.rs`, directly above
the `pub async fn run(` function definition (currently at line 136):

```rust
/// A brief startup banner, printed to plain stdout before the TUI takes
/// over the screen via the alternate-screen buffer (see
/// `TerminalGuard::init` in `terminal.rs`). Colors match the Wick &
/// Compass identity's real brass/rust/slate token values exactly
/// (`aivyx-brand/design-tokens.md`) -- no new palette invented. Box
/// width is computed from the real content, not hardcoded, so a longer
/// future version string can't misalign the border.
///
/// Uses `crossterm::style::Color` fully-qualified throughout, not the
/// bare `Color` this file already imports from `ratatui::style` at file
/// scope -- the two types share a name but not a shape
/// (`ratatui::style::Color::Rgb` is a tuple variant, `Rgb(u8, u8, u8)`;
/// `crossterm::style::Color::Rgb` is a struct variant, `Rgb { r, g, b }`)
/// and a bare reference here would silently resolve to the wrong one and
/// fail to compile.
fn startup_banner() -> String {
    use crossterm::style::Stylize;

    let brass = crossterm::style::Color::Rgb {
        r: 0xc9,
        g: 0xa2,
        b: 0x4b,
    };
    let rust = crossterm::style::Color::Rgb {
        r: 0xb5,
        g: 0x43,
        b: 0x2b,
    };
    let slate = crossterm::style::Color::Rgb {
        r: 0x8a,
        g: 0x95,
        b: 0xa1,
    };

    let title = "aivyx-coder";
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let line1_plain = format!("{title}  {version}");
    let line2_plain = "\u{25cf} local models only".to_string();

    let interior_width = line1_plain
        .chars()
        .count()
        .max(line2_plain.chars().count());
    let line1_pad = " ".repeat(interior_width - line1_plain.chars().count());
    let line2_pad = " ".repeat(interior_width - line2_plain.chars().count());
    let border = "\u{2500}".repeat(interior_width + 4);

    format!(
        "{tl}{border_top}{tr}\n\
         {v1}  {title_c}  {version_c}{p1}  {v2}\n\
         {v3}  {dot_c} {tagline_c}{p2}  {v4}\n\
         {bl}{border_bottom}{br}\n",
        tl = "\u{250c}".with(brass),
        border_top = border.clone().with(brass),
        tr = "\u{2510}".with(brass),
        v1 = "\u{2502}".with(brass),
        title_c = title.bold(),
        version_c = version.with(slate),
        p1 = line1_pad,
        v2 = "\u{2502}".with(brass),
        v3 = "\u{2502}".with(brass),
        dot_c = "\u{25cf}".with(rust),
        tagline_c = "local models only".with(slate),
        p2 = line2_pad,
        v4 = "\u{2502}".with(brass),
        bl = "\u{2514}".with(brass),
        border_bottom = border.with(brass),
        br = "\u{2518}".with(brass),
    )
}
```

- [ ] **Step 3: Call it from `run()`, right before `TerminalGuard::init()`**

The real call site is currently at line 211
(`let mut guard = TerminalGuard::init()?;`) — confirm this is still
accurate against the real current file before editing (it may have
shifted if other work has landed on `main` since this plan was written).

```bash
cd /home/julian/Projects/Rust/aivyx-coder
python3 << 'PYEOF'
with open('crates/aivyx-tui/src/app.rs') as f:
    content = f.read()

old = "    let mut guard = TerminalGuard::init()?;"
new = (
    "    print!(\"{}\", startup_banner());\n"
    "    use std::io::Write as _;\n"
    "    std::io::stdout().flush().ok();\n\n"
    "    let mut guard = TerminalGuard::init()?;"
)

assert old in content, "TerminalGuard::init() call site not found verbatim -- stop and check app.rs manually, the line may have shifted"
content = content.replace(old, new, 1)

with open('crates/aivyx-tui/src/app.rs', 'w') as f:
    f.write(content)
PYEOF
```

- [ ] **Step 4: Add a unit test in the existing `#[cfg(test)] mod tests` block**

The block starts at line 947 in the pre-edit file (will have shifted by
a few lines after Step 3's insertion — find it fresh via
`grep -n "mod tests" crates/aivyx-tui/src/app.rs` before editing). Add
this test inside that module, alongside the existing `#[test]` functions:

```rust
#[test]
fn startup_banner_contains_real_content_and_is_structurally_balanced() {
    let banner = startup_banner();

    // Real content present (plain substrings survive being wrapped in
    // ANSI color codes -- crossterm's Stylize wraps content, doesn't
    // transform it).
    assert!(banner.contains("aivyx-coder"));
    assert!(banner.contains(env!("CARGO_PKG_VERSION")));
    assert!(banner.contains("local models only"));
    assert!(banner.contains('\u{25cf}')); // the status dot

    // Box-drawing structure: exactly one top-left/top-right/bottom-left/
    // bottom-right corner each, and exactly 4 vertical-bar glyphs (2 per
    // content line).
    assert_eq!(banner.matches('\u{250c}').count(), 1); // ┌
    assert_eq!(banner.matches('\u{2510}').count(), 1); // ┐
    assert_eq!(banner.matches('\u{2514}').count(), 1); // └
    assert_eq!(banner.matches('\u{2518}').count(), 1); // ┘
    assert_eq!(banner.matches('\u{2502}').count(), 4); // │

    // Exactly 4 printed lines (top border, 2 content lines, bottom
    // border), each terminated by \n.
    assert_eq!(banner.matches('\n').count(), 4);
}
```

- [ ] **Step 5: Run the new test, then the full crate test suite**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo test -p aivyx-tui startup_banner_contains_real_content_and_is_structurally_balanced -- --nocapture
```

Expected: `test result: ok. 1 passed`.

```bash
cargo test -p aivyx-tui
```

Expected: all tests pass, no regressions.

- [ ] **Step 6: Confirm the whole workspace still builds and lints clean**

```bash
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-tui/src/app.rs
git commit -m "feat: add Wick & Compass startup banner to the TUI

Printed to plain stdout as the first thing run() does, before
TerminalGuard::init() enters raw mode / the alternate screen -- appears
as normal scrolled terminal output, then the TUI takes over; stays in
scrollback after the TUI exits. Real brass/rust/slate hex values via
crossterm::style::Color (fully-qualified throughout to avoid a real,
verified collision with this file's existing ratatui::style::Color
import -- the two Color::Rgb variants have different shapes). Box width
computed from the real content so a longer future version string can't
misalign the border. TUI-frontend-specific by construction: aivyx-acp
and aivyx-mcp-server never call aivyx_tui::run(), so no gating logic
is needed."
```

---

### Task 3 (repo: `aivyx-coder`): README logo + copied assets

**Files:**
- Create: `docs/logos/aivyx-lockup-horizontal-dark.svg`, `docs/logos/aivyx-lockup-horizontal-light.svg`
- Modify: `README.md:1-5` (add the `<picture>` element)

**Interfaces:** Consumes Task 1's new file (`aivyx-brand/logos/aivyx-lockup-horizontal-light.svg`,
merged to that repo's `main` by the time this task runs) and the
existing `aivyx-brand/logos/aivyx-lockup-horizontal.svg`. Independent of
Task 2 (different files, no shared state) — could run in either order,
but both must land before Task 4's final verification.

- [ ] **Step 1: Create `docs/logos/` and copy both variants**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
mkdir -p docs/logos
cp /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-lockup-horizontal.svg docs/logos/aivyx-lockup-horizontal-dark.svg
cp /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-lockup-horizontal-light.svg docs/logos/aivyx-lockup-horizontal-light.svg
```

If the second `cp` fails because the source file doesn't exist, Task 1
hasn't actually merged to `aivyx-brand`'s `main` yet — stop and confirm
Task 1's real completion state before proceeding, don't skip this file.

- [ ] **Step 2: Verify both copies are genuine, byte-identical copies**

```bash
diff docs/logos/aivyx-lockup-horizontal-dark.svg /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-lockup-horizontal.svg && echo "dark: identical"
diff docs/logos/aivyx-lockup-horizontal-light.svg /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-lockup-horizontal-light.svg && echo "light: identical"
```

- [ ] **Step 3: Add the `<picture>` element to `README.md`**

Read the real current top of `README.md` first — the plan assumes it's
still exactly this (confirmed at spec-writing time):

```markdown
# aivyx-coder

[![CI](https://github.com/Aivyx-Agent/aivyx-coder/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Aivyx-Agent/aivyx-coder/actions/workflows/ci.yml)
[![License: Apache-2.0 OR MIT](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](LICENSE)
```

If it differs (e.g. something else already landed between the title and
badges), adapt the insertion point to sit directly below the `# aivyx-coder`
H1 and above the two badge lines, rather than forcing this exact match.

```bash
python3 << 'PYEOF'
with open('README.md') as f:
    content = f.read()

old = '''# aivyx-coder

[![CI](https://github.com/Aivyx-Agent/aivyx-coder/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Aivyx-Agent/aivyx-coder/actions/workflows/ci.yml)'''

new = '''# aivyx-coder

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/logos/aivyx-lockup-horizontal-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="docs/logos/aivyx-lockup-horizontal-light.svg">
  <img alt="aivyx-coder" src="docs/logos/aivyx-lockup-horizontal-light.svg" width="280">
</picture>

[![CI](https://github.com/Aivyx-Agent/aivyx-coder/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Aivyx-Agent/aivyx-coder/actions/workflows/ci.yml)'''

assert old in content, "README.md's top doesn't match what this plan assumed -- stop and adapt the insertion point to the real current file instead of forcing this exact string"
content = content.replace(old, new, 1)

with open('README.md', 'w') as f:
    f.write(content)
PYEOF
```

Note the `<img>` fallback (`src="docs/logos/aivyx-lockup-horizontal-light.svg"`)
deliberately uses the **light** variant, not the dark one — this is the
fallback shown by clients that don't support `<picture>`/`prefers-color-scheme`
at all (some Markdown renderers, non-browser contexts), and GitHub's own
default rendering context is more often a light background than dark,
so the light variant is the safer default fallback.

- [ ] **Step 4: Verify the `<picture>` element's `media` queries map to the correct file — this is the actual bug being fixed, don't just trust the edit landed textually correct**

```bash
grep -A4 "<picture>" README.md
```

Expected output shows exactly:
- `(prefers-color-scheme: dark)` → `aivyx-lockup-horizontal-dark.svg`
- `(prefers-color-scheme: light)` → `aivyx-lockup-horizontal-light.svg`

If these are swapped, the exact bug this task exists to fix (dark
wordmark text on a light background, or vice versa) recurs — fix it and
re-verify before proceeding.

- [ ] **Step 5: Confirm the referenced paths actually resolve to real files (GitHub renders README images via repo-relative paths — a typo here fails silently, showing a broken-image icon, not a build error)**

```bash
test -f docs/logos/aivyx-lockup-horizontal-dark.svg && echo "dark path OK"
test -f docs/logos/aivyx-lockup-horizontal-light.svg && echo "light path OK"
grep -oE 'srcset="[^"]+"' README.md
```

Expected: both `test -f` checks print their OK line; the `srcset` values
printed exactly match the two real file paths that exist.

- [ ] **Step 6: Confirm both SVG files are valid XML**

```bash
python3 -c "import xml.etree.ElementTree as ET; ET.parse('docs/logos/aivyx-lockup-horizontal-dark.svg'); print('dark: valid XML')"
python3 -c "import xml.etree.ElementTree as ET; ET.parse('docs/logos/aivyx-lockup-horizontal-light.svg'); print('light: valid XML')"
```

- [ ] **Step 7: Commit**

```bash
git add docs/logos/ README.md
git commit -m "feat: add theme-adaptive README logo

Copies of aivyx-brand's dark and light lockup variants under
docs/logos/, referenced via a <picture>/prefers-color-scheme element so
the correct variant renders on both GitHub's light and dark README
themes -- fixes the real bug where a single dark-theme-only lockup
would have been nearly invisible on a white background. The <img>
fallback (for clients without <picture> support) uses the light variant
as the safer default, since GitHub's own default rendering context is
more often light than dark."
```

---

### Task 4 (repo: `aivyx-coder`): final verification

**Files:** none created/modified — pure verification.

**Interfaces:** Consumes the complete branch diff from Tasks 2-3
(Task 1 already merged and verified independently in `aivyx-brand`).

- [ ] **Step 1: Full workspace build, test, and lint**

```bash
cd /home/julian/Projects/Rust/aivyx-coder
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all three clean, no failures, no warnings.

- [ ] **Step 2: Run the real binary and capture the actual banner output**

```bash
cargo build -p aivyx --release 2>&1 | tail -5
```

If this environment can run the built binary interactively enough to
capture just the startup banner before it enters the alternate screen
(e.g. by piping stdin from `/dev/null` or a value that makes it exit
immediately after printing, or by checking whether `aivyx-coder --help`
takes a path that also prints the banner) — do so and paste the real
captured output (including a note on whether raw ANSI escape codes are
visible, expected when not piped through a terminal, or rendered as
real colors, expected when run in a true TTY) into this task's report.

If the binary genuinely cannot be run without a live TTY/model backend
in this environment, that's an accepted limitation — rely on Task 2's
own unit test (already verified passing) as the structural correctness
check, and note in the report that live visual confirmation wasn't
possible here, rather than silently skipping this step without
comment.

- [ ] **Step 3: Repo-wide sweep for any stray Neon-Cartographer-era reference in the newly-touched files**

```bash
grep -n "space grotesk\|neon cartographer\|candle" -i crates/aivyx-tui/src/app.rs README.md docs/logos/*.svg
grep -oE '#[0-9a-fA-F]{6}' docs/logos/*.svg | sort -u
```

Expected: the first grep returns no output. The second grep's output
should be exactly the 8 real Wick & Compass hex values used across the
two SVG files (4 dark-theme + 4 light-theme: ring, flame outer, flame
inner, wordmark fill, per theme) — cross-check each against
`aivyx-brand/design-tokens.md`'s real current values if anything looks
unfamiliar.

- [ ] **Step 4: Report**

Summarize: build/test/clippy results, whether live banner output was
actually captured or reported as an accepted limitation, and the
sweep's real output. No commit for this task — pure verification.
