#!/usr/bin/env python3
"""Generate docs/manual/reference/03-configuration.md from the config structs.

The reference is built from the code itself — `Settings` in
crates/aivyx-config/src/lib.rs (the shape of config.toml) and the structs it
points to — so it lists exactly the sections and keys the loader accepts.
Each key's meaning comes from, in order: an exact `MEANINGS` entry below, its
doc comment, a `*.` fallback in `MEANINGS`, and the trailing comment in
docs/manual/reference/config-example.toml (which also fills the Example
column). Adapted from aivyx-pa's script of the same name.

Re-run after changing the config schema:

    python3 scripts/gen-config-reference.py > docs/manual/reference/03-configuration.md

A new section, or a key with no meaning from any source, fails the run until
it is placed in `GROUPS`/`INTROS` or given a doc comment.
"""
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXAMPLE = ROOT / "docs" / "manual" / "reference" / "config-example.toml"
GUIDE = "../guide"
ROOT_STRUCT = "Settings"


def dependency_sources(names):
    """`src/**/*.rs` of the named Cargo dependencies (e.g. aivyx-route)."""
    try:
        meta = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--offline"],
            cwd=ROOT, capture_output=True, text=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return []
    out = []
    for pkg in json.loads(meta)["packages"]:
        if pkg["name"] in names:
            out += sorted(pathlib.Path(pkg["manifest_path"]).parent.glob("src/**/*.rs"))
    return out


SOURCES = sorted((ROOT / "crates").glob("*/src/**/*.rs")) + dependency_sources({"aivyx-route"})
TEXTS = {p: p.read_text(encoding="utf-8", errors="replace") for p in SOURCES}
PRIMITIVES = {
    "String", "bool", "u8", "u16", "u32", "u64", "usize", "i32", "i64",
    "f32", "f64", "PathBuf", "SecretString", "Value",
}

# ---------------------------------------------------------------------------
# Hand-written parts: section grouping, intros, and plain-words meanings.
# ---------------------------------------------------------------------------

GROUPS = [
    ("The model", ["backend", "routing"]),
    ("Safety", ["permissions", "sandbox", "git"]),
    ("Context the model gets", ["repo_map", "agents_file", "editor_context", "skills"]),
    ("Ways of working", ["verification", "autonomous", "architect", "council", "sub_agent", "team"]),
    ("Tools", ["web", "lsp", "repl", "vision", "mcp", "persona"]),
    ("Running inside other tools", ["mcp_server", "editor_approval"]),
]

INTROS = {
    "backend": f"The local model server and model aivyx-coder talks to. `aivyx-coder --setup` writes this for you. See [Install and first run]({GUIDE}/02-install-and-first-run.md) and [Local model servers]({GUIDE}/09-local-model-servers.md).",
    "routing": f"Per-call model routing across several local models. Off by default. See [Models and routing]({GUIDE}/08-models-and-routing.md).",
    "permissions": "What the model may touch and run: the deny list, pre-approved commands, and per-turn limits. See the [security model](05-security-model.md).",
    "sandbox": "Kernel confinement (Landlock + seccomp) for the commands the model runs, and its opt-outs. See the [security model](05-security-model.md).",
    "git": f"Worktree checkpoints (what `/undo` uses) and the generated-file ignore list. See [Undo, diff, commit and test]({GUIDE}/05-undo-diff-commit-test.md).",
    "repo_map": "The token-budgeted map of the repository's symbols added to every turn.",
    "agents_file": f"Project and personal instructions (`AGENTS.md`) loaded into every turn. See [Memory and learning]({GUIDE}/07-memory-and-learning.md).",
    "editor_context": f"Your editor's open file, cursor and selection, read from a small JSON file. See [Editor integration]({GUIDE}/12-editor-integration.md).",
    "skills": "The shared library of `SKILL.md` capability packages the model can load.",
    "verification": f"Check the work after edits by running a test command before a turn may end. See [Undo, diff, commit and test]({GUIDE}/05-undo-diff-commit-test.md).",
    "autonomous": f"Limits for `--auto` runs. See [Autonomous and advanced modes]({GUIDE}/10-autonomous-and-advanced-modes.md).",
    "architect": "The model that plans for `/architect <task>`.",
    "council": "The models `/council` consults, and the chairman that sums up.",
    "sub_agent": "Limits for `delegate_task`, which hands a sub-task to a fresh agent.",
    "team": f"Specialist teams (`delegate_to_specialist`). Off by default. See [Specialist teams]({GUIDE}/11-specialist-teams.md).",
    "web": "The `web_fetch` and `web_search` tools. Inference stays local; these reach the network.",
    "lsp": "Language-server tools (`go_to_definition`, `find_references`).",
    "repl": "Interactive processes the model can drive (`repl_start`, `repl_send`, `repl_stop`).",
    "vision": "Image generation tools, which need an image backend.",
    "mcp": f"MCP servers whose tools the model can use, one `[[mcp.servers]]` block each. See [MCP]({GUIDE}/13-mcp.md).",
    "persona": f"Whether the model may propose additions to your personal `AGENTS.md`. See [Memory and learning]({GUIDE}/07-memory-and-learning.md).",
    "mcp_server": f"Running aivyx-coder as an MCP server (`--mcp-server`). Refuses to start until `max_access_level` is set. See [MCP]({GUIDE}/13-mcp.md).",
    "editor_approval": f"Answering approval prompts from your editor instead of the terminal. See [Editor integration]({GUIDE}/12-editor-integration.md).",
}

# Plain-words meanings. An exact "<section path>.<key>" entry wins over the
# key's doc comment; a "*.<tail>" entry is only a fallback for keys with no
# doc comment anywhere.
MEANINGS = {
    "*.enabled": "Turn this on or off.",
    "*.base_url": "The server's OpenAI-compatible address, e.g. `http://localhost:11434/v1`.",
    "*.model": "The model id, as the server names it.",
    "*.api_key": "Key for an auth-protected local server. Usually unset.",
    "backend.base_url": "The model server's OpenAI-compatible address, e.g. `http://localhost:11434/v1` (Ollama) or `http://localhost:8080/v1` (llama.cpp). `--base-url` overrides it.",
    "backend.model": "The model id, as the server names it. `--model` overrides it.",
    "backend.tool_calling_mode": "Reserved for a text-fallback tool-call parser; not used yet.",
    "routing.discover": "Ask each endpoint what models it serves at startup. Off means use only `[[routing.models]]`.",
    "routing.endpoints.kind": "What kind of server this is.",
    "routing.endpoints.base_url": "The server's URL. Unset uses the kind's usual local address.",
    "routing.models.id": "The model id, as its endpoint names it.",
    "routing.models.tier": "Its size class, for matching tasks to models.",
    "routing.models.strengths": "What it is good at.",
    "routing.models.context_window": "The context window you actually serve it with, in tokens.",
    "routing.tasks.tier": "The tier this task prefers. Keyed by task name (`chat`, `code_edit`, `plan`, `judge`, `summarize`, `compact`, `classify`, `embed`).",
    "routing.tasks.strengths": "Strengths this task prefers in a model.",
    "permissions.mode": "How permission is decided. `confirm` is the only mode: anything that isn't read-only asks you.",
    "permissions.max_tool_iterations_per_turn": "Most tool-call round trips in one turn before it stops.",
    "permissions.allowed_commands.name": "The name the model uses to ask for this command.",
    "permissions.allowed_commands.program": "The program to run.",
    "permissions.allowed_commands.args": "Its fixed arguments. The model can't add to them.",
    "web.search_base_url": "Your SearXNG instance. Without it, `web_search` explains that it isn't set up.",
    "lsp.timeout_secs": "Longest wait for one language-server request, in seconds. Default 60 — the first call in a big project may index for a while.",
    "vision.broker_url": "The aivyx-broker address. Default `http://127.0.0.1:8899`.",
    "vision.mold_url": "The `mold serve` image server address. Default `http://127.0.0.1:7680`.",
    "vision.api_key": "Key for the image backend, if it needs one.",
    "mcp.servers.args": "Arguments for the server's command.",
    "routing.enabled": "Turn routing on. Off (the default) means every call uses `[backend] model`.",
    "routing.models.endpoint": "Which `[routing.endpoints.<name>]` serves it. Unset means `[backend]`.",
    "backend.kind": "Which server this is, for server-specific features: `generic` (default), `llama_server` (KV-cache persistence), `llama_server_broker` (GPU sharing through aivyx-broker; needs `broker_base_url`), `mistral_rs` (the embedded engine, if built in).",
    "backend.kvcache_store_path": "Where saved KV-cache slots go (default under `~/.local/share/aivyx-coder/kvcache`). Point it at the same directory as Aivyx PA's `[kvcache] store_path` when both share one llama.cpp server.",
    "backend.mistralrs_model_path": "With `kind = \"mistral_rs\"`: a GGUF file, or a directory of them. Required for that kind.",
    "backend.mistralrs_constrain_tool_calls": "Reserved for grammar-constrained tool calls in the embedded engine; not used yet.",
    "verification.command": "Name of a `[[permissions.allowed_commands]]` entry to run after edits, before a turn may end. Setting it turns verification on.",
    "skills.project_dir": "An extra skills directory for this project, holding one `<skill-name>/SKILL.md` per skill.",
    "skills.user_dir": "An extra personal skills directory, same layout.",
    "team.roster_path": "A team-config TOML to use instead of the built-in coding team.",
    "web.allow_private_targets": "Let `web_fetch` reach loopback, private and link-local addresses. Off by default.",
}

# ---------------------------------------------------------------------------


def crate_of(path):
    """The `src` directory a source file belongs to."""
    parts = path.parts
    return pathlib.Path(*parts[: len(parts) - 1 - parts[::-1].index("src") + 1]) if "src" in parts else path


DEF = re.compile(
    r"^[ \t]*(?:pub(?:\([a-z]+\))?\s+)?struct\s+(?P<struct>\w+)\s*\{"
    r"|^[ \t]*pub\s+type\s+(?P<alias>\w+)\s*=\s*(?P<target>[^;]+);"
    r"|^[ \t]*pub\s+struct\s+(?P<newtype>\w+)\s*\(\s*pub\s+(?P<inner>[^;]+)\)\s*;"
    r"|^[ \t]*pub\s+enum\s+(?P<enum>\w+)\s*\{",
    re.M,
)


def index_definitions():
    """{name: [(file, kind, payload)]} for every struct, alias, newtype and enum."""
    defs = {}
    for path, text in TEXTS.items():
        for m in DEF.finditer(text):
            if m["struct"]:
                i, depth = m.end(), 1
                while depth and i < len(text):
                    depth += {"{": 1, "}": -1}.get(text[i], 0)
                    i += 1
                entry = (m["struct"], "struct", text[m.end(): i - 1])
            elif m["alias"] or m["newtype"]:
                target = (m["target"] or m["inner"]).strip()
                entry = (m["alias"] or m["newtype"], "alias", re.sub(r"\bBTreeSet<", "Vec<", target))
            else:
                attrs = []
                for line in reversed(text[: m.start()].splitlines()):
                    if not line.strip().startswith(("#[", "///")):
                        break
                    attrs.append(line)
                body = text[m.end(): text.index("\n}", m.end())]
                entry = (m["enum"], "enum", (" ".join(attrs), body))
            defs.setdefault(entry[0], []).append((path, entry[1], entry[2]))
    return defs


DEFS = index_definitions()


def lookup(name: str, kinds, near):
    """The best definition of `name` among `kinds`: `near`'s file, then its crate, then any."""
    found = [d for d in DEFS.get(name.split("::")[-1], []) if d[1] in kinds]
    if not found:
        return None
    if near is not None:
        for test in (lambda p: p == near, lambda p: crate_of(p) == crate_of(near)):
            for d in found:
                if test(d[0]):
                    return d
    return found[0]


def find_struct(name: str, near=None):
    """(fields, file) of `struct <name> { … }`, preferring `near`'s file and crate.

    A `type` alias or a newtype (`struct X(pub BTreeMap<String, T>)`) comes back
    as `({"alias": "<type>"}, file)`."""
    d = lookup(name, ("struct", "alias"), near)
    if d is None:
        return None, None
    path, kind, payload = d
    return (parse_fields(payload) if kind == "struct" else {"alias": payload}), path


def resolve(type_name: str, near):
    """Follow aliases: (final type, file it was found in)."""
    for _ in range(5):
        found, path = find_struct(unwrap(type_name)[1], near)
        if not isinstance(found, dict):
            return type_name, near
        kind, _ = unwrap(type_name)
        inner = found["alias"]
        type_name = {"array": f"Vec<{inner}>", "map": f"BTreeMap<String, {inner}>"}.get(kind, inner)
        near = path
    return type_name, near


def find_enum(name: str, near=None):
    """Serde spellings of `enum <name>`'s variants, or None."""
    d = lookup(name, ("enum",), near)
    if d is None:
        return None
    attrs, body = d[2]
    rule = re.search(r'rename_all\s*=\s*"([^"]+)"', attrs)
    out, rename = [], None
    for line in body.splitlines():
        s = line.strip()
        r = re.search(r'(?<![_a-z])rename\s*=\s*"([^"]+)"', s)
        if s.startswith("#[") and r:
            rename = r.group(1)
        v = re.match(r"^([A-Z][A-Za-z0-9]*)\s*(,|\{|\(|$)", s)
        if v:
            out.append(rename or spell(v.group(1), rule.group(1) if rule else None))
            rename = None
    return out


def spell(variant: str, rule):
    words = re.findall(r"[A-Z][a-z0-9]*", variant)
    if rule == "lowercase":
        return variant.lower()
    if rule == "snake_case":
        return "_".join(w.lower() for w in words)
    if rule == "kebab-case":
        return "-".join(w.lower() for w in words)
    return variant


FIELD = re.compile(r"^    (pub(\([a-z]+\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*(.+?),?\s*$")


def parse_fields(body: str):
    """Top-level fields of a struct body (4-space indent), with docs and attributes."""
    fields, docs, attrs = [], [], []
    for line in body.splitlines():
        s = line.strip()
        if s.startswith("///"):
            docs.append(s[3:].strip())
            continue
        if s.startswith("#["):
            attrs.append(s)
            continue
        if s.startswith("//") or not s:
            continue
        m = FIELD.match(line)
        if m:
            fields.append({"name": m.group(3), "type": m.group(4).rstrip(","),
                           "docs": docs, "attrs": " ".join(attrs)})
        docs, attrs = [], []
    return fields


def serde_name(field):
    m = re.search(r'(?<![_a-z])rename\s*=\s*"([^"]+)"', field["attrs"])
    return m.group(1) if m else field["name"]


def skipped(field):
    return bool(re.search(r"\bskip\b(?!_)", field["attrs"]))


def flattened(field):
    return "flatten" in field["attrs"]


def unwrap(t: str):
    """('array'|'map'|'single', inner type) for a field type."""
    t = t.strip()
    m = re.fullmatch(r"Option<(.+)>", t)
    if m:
        t = m.group(1).strip()
    m = re.fullmatch(r"(?:Vec|BTreeSet|HashSet)<(.+)>", t)
    if m:
        return "array", m.group(1).strip()
    m = re.fullmatch(r"(?:[\w:]+::)?(?:BTreeMap|HashMap)<\s*String\s*,\s*(.+)>", t)
    if m:
        return "map", m.group(1).strip()
    return "single", t


def short_type(t: str, near=None) -> str:
    kind, inner = unwrap(t)
    base = inner.split("::")[-1]
    names = {"String": "string", "SecretString": "string", "bool": "bool", "PathBuf": "path",
             "f32": "number", "f64": "number", "Value": "any"}
    for n in ("u8", "u16", "u32", "u64", "usize", "i32", "i64"):
        names[n] = "integer"
    if base in names:
        shown = names[base]
    elif unwrap(inner) != ("single", inner):
        shown = short_type(inner, near)
    elif "<" in inner:
        shown = f"`{inner}`"
    else:
        variants = find_enum(base, near)
        shown = " \\| ".join(f"`{v}`" for v in variants) if variants else base
    if kind == "array":
        return f"list of {shown}"
    if kind == "map":
        return f"table of {shown}"
    return shown


PROVENANCE = re.compile(
    r"^(?:(?:Phase|Chapter|Piece|Security-audit|Team-Command|Model routing|Aivyx-Skills|Q\d|A\d+\b"
    r"|Task \d+|[A-Z][a-z]+ §\d+)[^—]{0,80}—\s*)+"
)
CONST_REF = re.compile(r"\[`([A-Z][A-Z0-9_]+)`\]")
# Developer-history asides that mean nothing to an operator.
NOISE = [
    (r"^`\[[a-z_.]+\] [a-z_]+`(?: \(default [^)]*\))?[.:]\s*", ""),
    (r"^(?:[Oo]ptional )?`\[\[?[a-z_.]+\]\]?`(?: (?:table-array|sub-table|nested block|section))?[.:]\s*", ""),
    (r"\s*[,;(—–-]*\s*(?:and is |is |are |stays |which is |keeps it )?byte-\s?identical(?: to)?[^.;)]*\)?", ""),
    (r"\s*\((?:[^()]*\bQ\d+[a-z]?\b[^()]*|[^()]*\bPhase \d+[^()]*|pre-[A-Z][a-z]+[^()]*)\)", ""),
    (r"\s*,?\s*pre-Phase-\d+ behaviou?r", ""),
    (r"\bper Q\d+[a-z]?\b\s*", ""),
    (r"\bQ\d+[a-z]?(?:'s)?\s+", ""),
    (r"(?:^|(?<=[.!?]) )(?:Q-block|Phase \d+(?: Task \d+)?)[^.]*\.", ""),
    (r"\bPhase \d+(?:'s)? ", ""),
    (r"\s*\((?:today's|unchanged|Step \d)[^)]*\)", ""),
    (r"Reuses `Raw\w+`[^.]*\.", ""),
    (r"`None`/absent|`None`|\bAbsent\b", "Unset"),
    (r"\babsent\b", "unset"),
    (r"`Some\(([^)]*)\)`", r"`\1`"),
    (r"Empty `Vec`", "An empty list"),
    (r"`(\S+)` \(`\1`\)", r"`\1`"),
    (r"`Self::([a-z_]+)`", r"`\1`"),
    (r"`Option` because ([^.]*)\.", ""),
    (r"\s{2,}", " "),
]


def const_value(name: str):
    pat = re.compile(r"const\s+" + re.escape(name) + r"\s*:\s*[^=]+=\s*([^;]+);")
    for text in TEXTS.values():
        m = pat.search(text)
        if m and len(m.group(1).strip()) <= 40:
            return m.group(1).strip()
    return None


def clean(text: str) -> str:
    text = PROVENANCE.sub("", text).strip()
    text = CONST_REF.sub(lambda m: f"`{const_value(m.group(1)) or m.group(1)}`", text)
    text = re.sub(r"\[`([^`]+)`\]", r"`\1`", text)
    for pat, rep in NOISE:
        text = re.sub(pat, rep, text)
    text = re.sub(r"\s+([.,;])", r"\1", text).strip(" ,;—")
    if re.match(r"^see `", text, re.I) or text.lower().strip(".") in {"default", "optional", ""}:
        return ""
    # Two sentences are plenty for a table cell; one if they're long.
    parts = [p for p in re.split(r"(?<=[.!?])\s+(?=[A-Z`(*])", text) if p]
    text = " ".join(parts[:2])
    if len(text) > 320:
        text = parts[0]
    text = text[:1].upper() + text[1:]
    return text.replace("|", "\\|")


def first_paragraph(docs):
    out = []
    for d in docs:
        if not d:
            if out:
                break
            continue
        if d.startswith("```"):
            break
        if out and out[-1].endswith("-") and not out[-1].endswith(" -") and d[:1].islower():
            out[-1] += d
        else:
            out.append(d)
    return clean(" ".join(out))


# --- examples/aivyx-pa.toml ------------------------------------------------

HEADER = re.compile(r"^#?\s*\[\[?([a-z0-9_.]+)\]\]?\s*(#.*)?$")
KEYLINE = re.compile(r"^#?\s*([a-z][a-z0-9_]*)\s*=\s*(.*)$")
CONT = re.compile(r"^#\s{2,}#\s?(.*)$")


def split_value(rest: str):
    """('value', 'trailing comment') — a `#` outside quotes starts the comment."""
    in_str, quote = False, ""
    for i, ch in enumerate(rest):
        if ch in "\"'" and (not in_str or ch == quote):
            in_str, quote = (not in_str), ch
        elif ch == "#" and not in_str:
            return rest[:i].strip(), rest[i + 1:].strip()
    return rest.strip(), ""


def example_keys():
    """{(section, key): (example value, comment)} from examples/aivyx-pa.toml."""
    out, section, last = {}, "", None
    for line in EXAMPLE.read_text(encoding="utf-8").splitlines():
        s = line.strip()
        m = HEADER.match(s)
        if m and not KEYLINE.match(s):
            section, last = m.group(1), None
            continue
        c = CONT.match(s)
        if c and last:
            v, com = out[last]
            out[last] = (v, (com + " " + c.group(1).strip()).strip())
            continue
        last = None
        m = KEYLINE.match(s)
        if m and section:
            value, comment = split_value(m.group(2))
            if len(value) > 40 or value.count("[") != value.count("]") or value.count("{") != value.count("}"):
                value = ""
            key = (section, m.group(1))
            if key not in out:
                out[key] = (value, comment)
                last = key
    return out


EXAMPLES = example_keys()

# --- rendering --------------------------------------------------------------

UNDOCUMENTED = []


def twin_docs(struct_name: str, near):
    """Field docs from the resolved struct a `RawX` loads into (X, XConfig, XSettings)."""
    base = struct_name.split("::")[-1]
    if not base.startswith("Raw"):
        return {}
    stem = base[3:]
    for cand in (stem, stem + "Config", stem + "Settings"):
        found, _ = find_struct(cand, near)
        if isinstance(found, list) and found:
            return {f["name"]: f["docs"] for f in found}
    return {}


def plain(header: str) -> str:
    """`[[a.b]]` / `[a.<name>.c]` → `a.b` / `a.c` (the lookup path)."""
    return re.sub(r"\.<[a-z]+>", "", header.strip("[]"))


def meaning_for(path: str, key: str):
    full = f"{path}.{key}"
    if full in MEANINGS:
        return MEANINGS[full]
    parts = full.split(".")
    for i in range(1, len(parts)):
        wild = "*." + ".".join(parts[i:])
        if wild in MEANINGS:
            return MEANINGS[wild]
    return ""


def row(header, key, f, twins, type_name, near):
    path = plain(header)
    value, comment = EXAMPLES.get((path, key), ("", ""))
    meaning = (
        MEANINGS.get(f"{path}.{key}")
        or first_paragraph(f["docs"])
        or first_paragraph(twins.get(f["name"], []))
        or meaning_for(path, key)
        or clean(comment)
    )
    if not meaning:
        UNDOCUMENTED.append(f"{path}.{key}")
    shown = f"`{value}`".replace("|", "\\|") if value else ""
    return f"| `{key}` | {short_type(type_name, near)} | {shown} | {meaning} |"


def is_struct(type_name: str, near) -> bool:
    base = type_name.split("::")[-1]
    if base in PRIMITIVES or not base[:1].isupper() or "<" in base:
        return False
    found, _ = find_struct(base, near)
    return isinstance(found, list) and bool(found)


def collect(struct_name, near):
    """(flat fields, nested tables) for a struct, expanding `#[serde(flatten)]`."""
    fields, here = find_struct(struct_name, near)
    if not isinstance(fields, list):
        return [], []
    twins = twin_docs(struct_name, here)
    flat, nested = [], []
    for f in fields:
        if skipped(f) or f["name"].startswith("legacy"):
            continue
        type_name, where = resolve(f["type"], here)
        kind, inner = unwrap(type_name)
        if flattened(f):
            sub_flat, sub_nested = collect(inner, where)
            flat += sub_flat
            nested += sub_nested
        elif is_struct(inner, where):
            nested.append((kind, serde_name(f), inner, f["docs"], where))
        else:
            flat.append((f, twins, type_name, where))
    return flat, nested


def emit_section(out, header, struct_name, docs, near=None, depth=0, seen=frozenset(), intro=None):
    if struct_name in seen:
        return
    seen = seen | {struct_name}
    flat, nested = collect(struct_name, near)
    out.append(f"\n{'#' * min(3 + depth, 5)} `{header}`\n")
    text = intro or first_paragraph(docs)
    if text:
        out.append(text + "\n")
    if flat:
        out.append("| Key | Type | Example | Meaning |\n|---|---|---|---|")
        out.extend(row(header, serde_name(f), f, twins, t, w) for f, twins, t, w in flat)
    prefix = header.strip("[]")
    # Several sibling sub-tables of one type (one per Persona category, say)
    # are documented once.
    by_type = {}
    for kind, key, inner, d, where in nested:
        by_type.setdefault((kind, inner, where), []).append((key, d))
    for (kind, inner, where), entries in by_type.items():
        if len(entries) > 2 and kind == "single":
            keys = ", ".join(f"`{k}`" for k, _ in entries)
            emit_section(out, f"[{prefix}.<category>]", inner, [], where, depth + 1, seen,
                         intro=f"One sub-table per category: {keys}.")
            continue
        for key, d in entries:
            if kind == "array":
                h = f"[[{prefix}.{key}]]"
            elif kind == "map":
                h = f"[{prefix}.{key}.<name>]"
            else:
                h = f"[{prefix}.{key}]"
            emit_section(out, h, inner, d, where, depth + 1, seen)


PREAMBLE = """# Configuration reference

Every section and key `config.toml` accepts. This page is generated from
the config structs in `crates/aivyx-config` by
`scripts/gen-config-reference.py` — edit the code or the script, not this
page. A commented example is in [`config-example.toml`](config-example.toml);
the **Example** column below comes from it.

## Where the config lives

`~/.config/aivyx-coder/config.toml` on Linux (`$XDG_CONFIG_HOME` is
honoured), `~/Library/Application Support/aivyx-coder/config.toml` on macOS.
`aivyx-coder --setup` writes it, with `0600` permissions since it may hold
an `api_key`; on a first run without it, defaults are written. Every
section is optional. For one session, `--base-url`, `--model` and
`--edit-format` override the file — see the [command line](01-command-line.md).

## Reading the tables

- **Type**: `string`, `integer`, `number`, `bool`, `path`, a list, or the
  allowed words (`` `a` | `b` ``).
- `[[name]]` is an array of tables: repeat the block once per entry.
- `<name>` in a heading is a name you choose.
"""


def main():
    sections = {}
    fields, root_file = find_struct(ROOT_STRUCT)
    for f in fields:
        if not skipped(f) and not f["name"].startswith("legacy"):
            sections[serde_name(f)] = f
    placed = [s for _, names in GROUPS for s in names]
    missing = sorted(set(sections) - set(placed))
    unknown = sorted(set(placed) - set(sections))
    no_intro = sorted(set(sections) - set(INTROS))
    if missing or unknown or no_intro:
        sys.exit(f"update GROUPS/INTROS: unplaced {missing}, unknown {unknown}, no intro {no_intro}")

    out = [PREAMBLE]
    for title, names in GROUPS:
        out.append(f"\n## {title}")
        for key in names:
            type_name, where = resolve(sections[key]["type"], root_file)
            kind, inner = unwrap(type_name)
            h = {"array": f"[[{key}]]", "map": f"[{key}.<name>]"}.get(kind, f"[{key}]")
            emit_section(out, h, inner, sections[key]["docs"], where, intro=INTROS[key])
    if UNDOCUMENTED:
        sys.exit("no meaning for: " + ", ".join(UNDOCUMENTED)
                 + " — add a doc comment or a MEANINGS entry in this script")
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
