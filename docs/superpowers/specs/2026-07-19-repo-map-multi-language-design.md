# Repo-Map Multi-Language Support — Design

**Status:** Approved by user 2026-07-19. Fourth and final of 4 sub-projects
closing the capability gaps identified in a fresh audit of aivyx-coder's
actual code-writing ability (the first three — multi-file edit atomicity,
reasoning visibility, structured verification memory — are merged).

## Context

**Problem this spec solves:** the repo map (aider-style symbol extraction +
PageRank over the cross-file reference graph, token-budgeted and appended
to the system prompt each turn) only understands Rust today. Any other
language's files contribute zero symbols and silently produce no map at
all — the agent has zero structural awareness of any non-Rust codebase,
and a user working in one wouldn't necessarily notice the feature isn't
helping them. This matters directly for the eventual bare-metal test-rig
trial (the motivating context for closing all 4 gaps), which may well
involve non-Rust code.

Facts confirmed against the current codebase before this design was written:

- `crates/aivyx-repomap/src/lib.rs` (the entire crate — one file, no
  submodules) is deliberately dependency-free of the rest of the
  workspace: pure filesystem-in, string-out (`Cargo.toml` depends only on
  `ignore`, `streaming-iterator`, `tree-sitter`, `tree-sitter-rust`).
- `Extractor::new()` (lib.rs:257-271) hardcodes exactly one grammar
  (`tree_sitter_rust::LANGUAGE`) and two `const &str` queries (`DEF_QUERY`,
  `REF_QUERY`, lib.rs:43-67) compiled once against that grammar. This is
  the entire Rust-specific surface for extraction.
- `collect_tags` (lib.rs:190-238) filters files by
  `path.extension().is_none_or(|e| e != "rs")` (lib.rs:200) — the only
  place a file's language is "detected," and it's a single hardcoded
  extension check.
- Everything downstream of extraction — `FileTags`/`Def` (lib.rs:69-82,
  plain `name`/`signature`/`is_pub` strings and a name→count ref map),
  `pagerank` (lib.rts:335-389, a name-string keyed graph over `defs`/`refs`
  with no language awareness at all), `render_file` (lib.rs:391-406,
  pub-first-in-source-order text formatting), and the whole-map budget
  loop in `render` (lib.rs:109-151) — is already 100% language-agnostic.
  This means a mixed-language repo (e.g. a Rust backend + TS frontend)
  gets one unified, correctly-ranked map for free the moment more than one
  language's extractor exists — no special-casing needed anywhere in this
  downstream code.
- `signature_line` (lib.rs:325-329) takes an item's first source line,
  cuts at the body's opening `{` (dropping everything after), then strips
  a trailing `;`. This happens to already work correctly for Python
  (whose function/class headers end in `:` before a newline-delimited
  body, no `{` to strip) with zero changes needed.
- The existing musl cross-compile gotcha for `tree-sitter-rust`'s bundled
  C source (see this project's own
  `reference_musl_cross_compile_tree_sitter_gcc` memory: needs
  `CC_x86_64_unknown_linux_musl=musl-gcc` + the `musl`/`musl-tools`
  package) will need re-confirming for each new grammar crate — flagged
  as the implementation plan's first verification step, not resolved
  here.
- Compatible grammar crates confirmed to exist on crates.io as of this
  writing: `tree-sitter-python = "0.25.0"`, `tree-sitter-javascript =
  "0.25.0"`, `tree-sitter-typescript = "0.23.2"` (the latter exposes two
  distinct grammars from one crate — `LANGUAGE_TYPESCRIPT` and
  `LANGUAGE_TSX` — for `.ts` and `.tsx` respectively). Exact ABI
  compatibility with the workspace's pinned `tree-sitter = "0.26.10"` core
  crate is to be confirmed by the plan's first `cargo build`, not assumed
  here.

## Decisions (confirmed with user via one-at-a-time questions before this doc was written)

1. **Language scope: Python, JavaScript, and TypeScript, all in this one
   sub-project.** Not staged into separate phases — one plan, with
   per-language tasks. Rejected staging into N separate sub-projects since
   the architecture (Decision 3) is shared infrastructure work that's
   wasteful to split; each language's own extraction logic is still an
   independently testable, separately reviewable task within one plan.
2. **JS/TS file extensions: `.js`/`.jsx` and `.ts`/`.tsx`, all four.**
   `.js`/`.jsx` use the `tree-sitter-javascript` grammar; `.ts` uses
   `tree-sitter-typescript`'s `LANGUAGE_TYPESCRIPT`; `.tsx` uses the same
   crate's distinct `LANGUAGE_TSX` — JSX/TSX syntax needs a grammar that
   understands embedded markup, which plain JS/TS grammars don't parse.
3. **Architecture: a data-only `LanguageConfig` table, not a trait.** One
   plain struct per language (extensions it claims, its grammar, its two
   query strings, and two small per-language hooks — see Change 2) held in
   a `const`/static list, looked up by file extension. No trait objects,
   no `dyn` dispatch — matches this crate's existing non-trait, very
   direct style, and this project's own stated preference against
   introducing abstraction machinery a plain data table can express just
   as well.
4. **Public-ness is language-appropriate, not uniform.** Rust already
   checks `signature.starts_with("pub ")`. For JS/TS: an item wrapped in
   an `export_statement` is public — and the *signature shown* is the
   outer `export ...` text (not just the inner declaration), which means
   the exact same `starts_with(prefix)` check Rust already uses works
   unchanged, just with `"export "` as the prefix. For Python (no formal
   public/private keyword): the common convention — a leading-underscore
   name (`_helper`) is private, everything else is public. This is the
   one language whose "pub" check is name-based rather than
   signature-text-based.
5. **Reference-query depth is honest about each language's actual
   grammar, not padded to look uniform.** Python's grammar has no
   distinct "this identifier names a type" node the way Rust and
   TypeScript do — capturing every bare `identifier` to compensate would
   flood the graph with ordinary variable reads, defeating the point of a
   reference graph. So: Python gets call-site references only (function
   calls, method/attribute calls) — a type used only in an annotation and
   never called won't create a graph edge, a real, accepted, documented
   limitation, not an oversight. JavaScript gets call-sites plus `new
   Foo()` construction (its closest analogue to a type reference).
   TypeScript, having a real `type_identifier` grammar node, gets it
   captured directly — the same reference quality Rust already has.

## Changes

### 1. New workspace dependencies

`crates/aivyx-repomap/Cargo.toml`, added to `[dependencies]`:

```toml
tree-sitter-python = "0.25.0"
tree-sitter-javascript = "0.25.0"
tree-sitter-typescript = "0.23.2"
```

Exact versions to be confirmed compiling (including under the musl
release-build target — see the Context section's flagged risk) as the
implementation plan's very first step, before any extraction code is
written.

### 2. `LanguageConfig` — the per-language data table

New type, replacing `Extractor`'s hardcoded single grammar/query pair:

```rust
/// Everything the extractor needs to know about one language: which files
/// it claims, its tree-sitter grammar and def/ref queries, and two small
/// hooks for the two things that genuinely differ per language (which
/// node's text becomes the shown signature, and what counts as "public").
struct LanguageConfig {
    /// File extensions this language claims (checked case-sensitively,
    /// matching this crate's existing convention).
    extensions: &'static [&'static str],
    grammar: fn() -> tree_sitter::Language,
    def_query: &'static str,
    ref_query: &'static str,
    /// Given the captured `@item` node, returns the node whose text should
    /// become the rendered signature. Identity for Rust/Python (the
    /// captured item already is the right node); for JS/TS, substitutes
    /// the enclosing `export_statement` when present, so an exported
    /// item's shown signature literally starts with "export " and
    /// `is_pub` (below) can reuse the same prefix-check shape Rust uses.
    signature_node: fn(item: tree_sitter::Node) -> tree_sitter::Node,
    /// Given the extracted name and the (already `signature_node`-adjusted)
    /// signature text, is this definition part of the file's public
    /// surface? Rust/JS/TS: a text-prefix check ("pub "/"export ").
    /// Python: name-based (no leading underscore).
    is_pub: fn(name: &str, signature: &str) -> bool,
}

fn js_signature_node(item: tree_sitter::Node) -> tree_sitter::Node {
    item.parent()
        .filter(|p| p.kind() == "export_statement")
        .unwrap_or(item)
}

fn prefix_is_pub(prefix: &'static str) -> fn(&str, &str) -> bool {
    // Returned as a plain fn pointer via a per-prefix top-level fn below
    // (Rust doesn't allow capturing closures in a `fn` field) — see
    // `rust_is_pub`/`export_is_pub` for the two concrete instances used.
    unreachable!()
}

fn rust_is_pub(_name: &str, signature: &str) -> bool {
    signature.starts_with("pub ")
}

fn export_is_pub(_name: &str, signature: &str) -> bool {
    signature.starts_with("export ")
}

fn python_is_pub(name: &str, _signature: &str) -> bool {
    !name.starts_with('_')
}

fn identity_node(item: tree_sitter::Node) -> tree_sitter::Node {
    item
}
```

(The `prefix_is_pub` sketch above is illustrative only — the plan should
land on concrete top-level `fn`s like `rust_is_pub`/`export_is_pub`, since
Rust function pointers in a `const`/static table can't close over a
runtime prefix string. This spec fixes the *behavior*; the plan may choose
its own tidy way to express "two nearly-identical prefix checks" — e.g.
two named functions, as sketched, or one function taking the prefix as a
`const` generic — whichever compiles cleanest.)

Definition queries per language:

```rust
const PYTHON_DEF_QUERY: &str = r#"
(function_definition name: (identifier) @name) @item
(class_definition name: (identifier) @name) @item
"#;

const PYTHON_REF_QUERY: &str = r#"
(call function: (identifier) @ref)
(call function: (attribute attribute: (identifier) @ref))
"#;

const JS_DEF_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @item
(class_declaration name: (identifier) @name) @item
(method_definition name: (property_identifier) @name) @item
(variable_declarator name: (identifier) @name value: (arrow_function)) @item
(variable_declarator name: (identifier) @name value: (function_expression)) @item
"#;

const JS_REF_QUERY: &str = r#"
(call_expression function: (identifier) @ref)
(call_expression function: (member_expression property: (property_identifier) @ref))
(new_expression constructor: (identifier) @ref)
"#;

// TypeScript (and TSX) reuse every JS pattern above, plus TS-only
// declaration kinds and real type references:
const TS_DEF_QUERY_EXTRA: &str = r#"
(interface_declaration name: (type_identifier) @name) @item
(type_alias_declaration name: (type_identifier) @name) @item
(enum_declaration name: (identifier) @name) @item
"#;

const TS_REF_QUERY_EXTRA: &str = r#"
(type_identifier) @ref
"#;
```

The plan should concatenate `JS_DEF_QUERY` + `TS_DEF_QUERY_EXTRA` (and
`JS_REF_QUERY` + `TS_REF_QUERY_EXTRA`) into the actual TypeScript/TSX
`LanguageConfig` entries' query strings, rather than duplicating the JS
patterns verbatim a second time.

The registry:

```rust
const LANGUAGES: &[LanguageConfig] = &[
    LanguageConfig {
        extensions: &["rs"],
        grammar: || tree_sitter::Language::from(tree_sitter_rust::LANGUAGE),
        def_query: DEF_QUERY, // existing Rust query, unrenamed
        ref_query: REF_QUERY, // existing Rust query, unrenamed
        signature_node: identity_node,
        is_pub: rust_is_pub,
    },
    LanguageConfig {
        extensions: &["py"],
        grammar: || tree_sitter::Language::from(tree_sitter_python::LANGUAGE),
        def_query: PYTHON_DEF_QUERY,
        ref_query: PYTHON_REF_QUERY,
        signature_node: identity_node,
        is_pub: python_is_pub,
    },
    LanguageConfig {
        extensions: &["js", "jsx"],
        grammar: || tree_sitter::Language::from(tree_sitter_javascript::LANGUAGE),
        def_query: JS_DEF_QUERY,
        ref_query: JS_REF_QUERY,
        signature_node: js_signature_node,
        is_pub: export_is_pub,
    },
    LanguageConfig {
        extensions: &["ts"],
        grammar: || tree_sitter::Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
        def_query: /* JS_DEF_QUERY + TS_DEF_QUERY_EXTRA, concatenated */ "",
        ref_query: /* JS_REF_QUERY + TS_REF_QUERY_EXTRA, concatenated */ "",
        signature_node: js_signature_node,
        is_pub: export_is_pub,
    },
    LanguageConfig {
        extensions: &["tsx"],
        grammar: || tree_sitter::Language::from(tree_sitter_typescript::LANGUAGE_TSX),
        def_query: /* same concatenation as .ts */ "",
        ref_query: /* same concatenation as .ts */ "",
        signature_node: js_signature_node,
        is_pub: export_is_pub,
    },
];
```

(Query-string concatenation placeholders above are literal — the plan
must actually build these, e.g. via a `const fn` string concat helper, a
`once_cell`/`std::sync::LazyLock` computed constant, or by writing out
`JS_DEF_QUERY`'s patterns a second time inline in a combined
`TS_DEF_QUERY` const. Whichever reads cleanest; not a design-level
decision.)

### 3. `Extractor` becomes multi-language

Replace the single hardcoded `Parser`/`Query`/`Query` in `Extractor` with
one compiled `(Query, Query)` pair per `LanguageConfig` entry (built once
at construction, same as today) plus a single reusable `tree_sitter::
Parser` whose language is switched via `set_language()` per file
immediately before parsing (cheap, and tree-sitter explicitly supports
re-targeting a `Parser` between calls). `collect_tags`'s file filter
changes from the single `path.extension().is_none_or(|e| e != "rs")`
check to: does this file's extension match *any* `LanguageConfig` entry's
`extensions`? If not, skip the file exactly as today. The matched
`LanguageConfig` determines which compiled query pair, `signature_node`,
and `is_pub` function `Extractor::extract` uses for that file.

`Extractor::extract`'s existing capture-loop logic (lib.rs:273-319) stays
structurally the same; the only changes are: (a) select the right
`LanguageConfig`/compiled queries by extension before parsing, (b) after
capturing an `@item`/`@name` pair, call that language's `signature_node`
on the captured item node before extracting signature text (Rust/Python:
no-op; JS/TS: substitutes the `export_statement` parent when present),
and (c) compute `is_pub` via that language's `is_pub(name, signature)`
instead of the current hardcoded `signature.starts_with("pub ")`.

## Out of scope for this spec

- Any language beyond Python/JavaScript/TypeScript (Go, C/C++, Java,
  etc.) — future work, not blocked by this design's shape (adding one is
  "add one more `LanguageConfig` entry + its two query consts").
- Python type-annotation-only references (Decision 5) — accepted, honest
  limitation, not a bug to chase.
- JS/TS named re-exports (`export { foo, bar }`, `export * from './x'`)
  and `export default` for anonymous expressions — only direct-declaration
  exports (`export function foo() {}`, `export const foo = () => {}`,
  `export class Foo {}`, `export interface Foo {}`, etc.) are recognized
  as public. A named re-export list doesn't wrap a *definition* the query
  can anchor to the same way, and is a real but narrower gap than the
  core feature this spec ships.
- Any change to the crate's dependency-free-of-workspace constraint, its
  caching strategy (mtime+size, unchanged), its budget/rendering logic, or
  its wiki-pointer feature — none of these are language-specific and none
  need to change.
- Adding new tree-sitter query *fields* beyond what's listed above (e.g.
  JSDoc/docstring extraction, decorator/annotation capture) — the map
  stays a signature-only structural index, matching the existing Rust
  behavior exactly.

## Testing / verification

- Per new language (Python, JS, TS — TSX reuses TS's queries so gets
  lighter, targeted coverage rather than a full duplicate suite):
  definition-extraction test (function/class/method/arrow-function forms
  as applicable), reference-extraction test (call-sites, plus
  `new`/type-identifier where applicable), an `is_pub`/export-detection
  test, and a signature-rendering test confirming `signature_node`'s
  export-substitution shows "export " when expected and doesn't
  when not.
- A PageRank-ranking test in a non-Rust language (mirrors the existing
  `heavily_referenced_files_rank_first` Rust test), proving the
  already-language-agnostic ranking logic works unchanged for new
  extractors too.
- A **mixed-language repo test**: a small tree with e.g. one `.rs` file
  and one `.ts` file that references something the other doesn't (no
  actual cross-language linking expected — names don't collide across
  unrelated languages in a toy fixture — the assertion is simply that
  *both* files' definitions appear in one rendered map, proving the
  walk/render loop truly is language-blind).
- Existing Rust-only tests (all of `crates/aivyx-repomap/src/lib.rs`'s
  current `#[cfg(test)] mod tests`) must continue passing unchanged —
  confirms this refactor doesn't regress the one language already shipped.
- Live E2E (through the real binary, this project's established PTY +
  session-JSON method): a toy Python (or TS) project, confirm via the
  persisted session JSON that the system prompt actually included repo-map
  content for that language (not just unit-level proof) — the first live
  proof this feature helps a non-Rust project at all.

## Sequencing

Written now, at the user's request, as the fourth and final gap-closing
sub-project (multi-file edit atomicity, reasoning visibility, and
structured verification memory already shipped). Once merged, the
originally-motivating bare-metal test-rig trial becomes unblocked from
this audit's perspective — not something this spec itself designs.
