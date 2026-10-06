#!/usr/bin/env python3
"""Generate docs/manual/reference/04-tools.md from the code.

Every `impl Tool for X` outside test code contributes its `name()`, the start
of its `definition()` description (what the model reads), and the
`ActionKind` its permission request uses — which decides whether it asks
first. Re-run after adding or renaming a tool:

    python3 scripts/gen-tools-reference.py > docs/manual/reference/04-tools.md

A tool missing from `GROUPS`, or a group with no tools, fails the run.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
GUIDE = "../guide"

# Section → (intro, tool names). Order is the page order.
GROUPS = {
    "Files": ("Read, write and change files in the project.",
              ["read_file", "write_file", "edit_file", "patch_file", "move_file", "delete_file"]),
    "Search": ("Find things in the project.", ["grep", "glob"]),
    "Commands": ("Run programs. `run_command` only runs commands you listed in "
                 "`[[permissions.allowed_commands]]`; `run_shell` runs anything, and always asks.",
                 ["run_command", "run_shell"]),
    "Interactive processes": ("Drive a long-running program such as a REPL or debugger "
                              "(`[repl]`).", ["repl_start", "repl_send", "repl_stop"]),
    "Git": ("Reading history is free; writing asks.",
            ["git_read", "git_commit", "git_branch", "git_push", "git_pr"]),
    "Planning": ("The model's own task list, shown in the TUI.", ["set_tasks"]),
    "Web": ("Only when `[web] enabled = true`. Inference stays local; these reach the network.",
            ["web_fetch", "web_search"]),
    "Language server": ("Uses rust-analyzer (`[lsp]`).", ["go_to_definition", "find_references"]),
    "Memory and learning": (f"See [Memory and learning]({GUIDE}/07-memory-and-learning.md).",
                            ["memory_read", "memory_write", "memory_forget", "remember_preference",
                             "load_skill"]),
    "Delegation and teams": (f"Sub-agents and specialist teams. See [Specialist teams]({GUIDE}/11-specialist-teams.md).",
                             ["delegate_task", "delegate_to_specialist", "spawn_specialist",
                              "query_specialist", "close_specialist", "decompose_task",
                              "verify_output", "synthesize_results"]),
    "MCP servers": (f"Your `[[mcp.servers]]`' resources and prompts. Their own tools appear as "
                    f"`mcp__<server>__<tool>`. See [MCP]({GUIDE}/13-mcp.md).",
                    ["list_mcp_resources", "read_mcp_resource", "list_mcp_prompts", "get_mcp_prompt"]),
    "Images": ("`generate_svg` uses your model; `generate_image` needs `[vision] enabled = true`.",
               ["generate_svg", "generate_image", "generate_3d"]),
}
# Test doubles and fixtures, never registered.
IGNORED = {"cancel_tool", "counting_tool", "counting_read_tool", "fake_network_tool",
           "injection_echo_tool", "sneaky_write"}

IMPL = re.compile(r"^\s*impl(?:<[^>]*>)?\s+(?:[\w:]+::)?Tool\s+for\s+([\w:<>, ]+?)\s*\{", re.M)
TEST_MOD = re.compile(r"^#\[cfg\(test\)\]\s*\n(?:#\[[^\]]*\]\s*\n)*mod\s+\w+\s*\{", re.M)


def block(text: str, start: int) -> str:
    i, depth = start, 1
    while depth and i < len(text):
        c = text[i]
        if c == '"':
            i += 1
            while i < len(text) and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
        depth += {"{": 1, "}": -1}.get(c, 0)
        i += 1
    return text[start: i - 1]


def literal(src: str, text: str):
    src = src.strip()
    if re.fullmatch(r"[A-Z][A-Z0-9_]*", src):
        c = re.search(r"const\s+" + src + r"\s*:\s*&(?:'static\s+)?str\s*=\s*(.+?);\s*$", text, re.S | re.M)
        return literal(c.group(1), text) if c else None
    parts = re.findall(r'r#"(.*?)"#|"((?:[^"\\]|\\.)*)"', src, re.S)
    if not parts:
        return None
    out = ""
    for raw, cooked in parts:
        if raw:
            out += raw
        else:
            out += re.sub(r"\\\n\s*", "", cooked).replace('\\"', '"').replace("\\n", " ").replace("\\\\", "\\")
    return re.sub(r"\s+", " ", out).strip()


def summary(desc: str) -> str:
    parts = re.split(r"(?<=[.!?])\s+(?=[A-Z`(])", desc)
    text = parts[0]
    if len(parts) > 1 and len(text) + len(parts[1]) < 220:
        text += " " + parts[1]
    return text.replace("|", "\\|")


# How the gate treats each ActionKind (aivyx-sandbox/src/lib.rs): auto-allowed
# (still denied in plan mode where they mutate) or asks.
ASKS = {"Read": "no", "Internal": "no", "Network": "no", "Interact": "no",
        "Write": "yes", "Delete": "yes", "Execute": "yes", "Move": "yes",
        "McpTool": "yes", "Memory": "yes", "PersistentMemory": "yes"}
# Tools whose PermissionRequest is built by a helper outside the impl block.
ASK_OVERRIDES = {"spawn_specialist": "no", "query_specialist": "no", "close_specialist": "no"}


def collect():
    tools = {}
    for path in sorted((ROOT / "crates").glob("*/src/**/*.rs")):
        if "tests" in path.parts or path.name in ("tests.rs", "test_support.rs"):
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        t = TEST_MOD.search(text)
        live = text[: t.start()] if t else text
        for m in IMPL.finditer(live):
            impl = block(live, m.end())
            n = re.search(r"fn\s+name\s*\(\s*&self\s*\)\s*->\s*&(?:'static\s+)?str\s*\{\s*(.+?)\s*\}", impl, re.S)
            if not n:
                continue
            name = literal(n.group(1), text)
            if not name or name in IGNORED:
                continue
            d = re.search(r"description:\s*(.+?)\s*\.(?:to_string|into|to_owned)\(\)", impl, re.S)
            desc = literal(d.group(1), text) if d else None
            kinds = sorted(set(re.findall(r"ActionKind::(\w+)", impl)))
            unknown = [k for k in kinds if k not in ASKS]
            if unknown:
                sys.exit(f"{name}: unknown ActionKind {unknown} — add it to ASKS")
            asks = "yes" if any(ASKS[k] == "yes" for k in kinds) else ("no" if kinds else None)
            asks = ASK_OVERRIDES.get(name, asks)
            if asks is None:
                sys.exit(f"{name}: no ActionKind found — add it to ASK_OVERRIDES")
            tools[name] = (summary(desc) if desc else "", asks)
    return tools


PREAMBLE = f"""# Tools

Every tool aivyx-coder can give the model, with the description the model
itself reads. Generated from the code by `scripts/gen-tools-reference.py` —
edit the tool, not this page.

**Asks first?** Reading, searching and fetching from the web don't ask.
Anything that writes, deletes, moves or runs something, calls an MCP
server, or saves memory asks you (`y` once, `a` always for that exact file,
command or topic, `n` no) — unless you pre-approved it. In plan mode the
tools that change things are hidden from the model and refused if it tries.
Files on your deny list are refused outright. Delegated sub-agents and
specialists ask for their own actions the same way. See the
[security model](05-security-model.md).
"""


def main():
    tools = collect()
    placed = {t for _, names in GROUPS.values() for t in names}
    unplaced = sorted(set(tools) - placed)
    missing = sorted(placed - set(tools))
    if unplaced or missing:
        sys.exit(f"update GROUPS: unplaced {unplaced}, not found {missing}")
    out = [PREAMBLE]
    for title, (intro, names) in GROUPS.items():
        out.append(f"\n## {title}\n\n{intro}\n")
        out.append("| Tool | Asks first? | What it does |\n|---|---|---|")
        for name in names:
            desc, asks = tools[name]
            out.append(f"| `{name}` | {asks} | {desc} |")
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
