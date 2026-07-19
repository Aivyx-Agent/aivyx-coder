# Repo-Map Multi-Language Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the repo map's tree-sitter symbol extraction from Rust-only
to also cover Python, JavaScript, and TypeScript/TSX, so a non-Rust project
gets real structural awareness in the system prompt instead of silently no
map at all.

**Architecture:** A data-only `LanguageConfig` table (one entry per
extension-set, holding its grammar, its two tree-sitter queries, and two
small per-language hooks) replaces `Extractor`'s current hardcoded single
grammar/query pair. Everything downstream — `FileTags`/`Def`, `pagerank`,
`render_file`, the budget loop in `render` — is already language-agnostic
and untouched.

**Tech Stack:** Rust, `tree-sitter` 0.26 (already pinned), new grammar
crates `tree-sitter-python` 0.25.0, `tree-sitter-javascript` 0.25.0,
`tree-sitter-typescript` 0.23.2 — all confirmed to compile cleanly against
the workspace's pinned `tree-sitter` core before this plan was written.

## Global Constraints

- Entirely within `crates/aivyx-repomap` — no other crate touched, no
  change to the crate's dependency-free-of-workspace property (only
  `tree-sitter-*` grammar crates are added, nothing from this workspace).
- Never simplify Decision 2/4/5 from the spec (coarse, framework-agnostic
  scope; per-language public-ness; honest accepted limitations on
  reference depth) — implement exactly as specified, verbatim query text
  below.
- **Correction to the spec's own illustrative `js_signature_node` sketch,
  found while grounding this plan in the real JS/TS grammar's
  `node-types.json`** (not a re-litigation of any Decision — this is a
  structural fact about the grammar the spec's own sketch didn't account
  for): the spec's sketch checked only *one* parent hop
  (`item.parent().filter(|p| p.kind() == "export_statement")`). This is
  correct for `function_declaration`/`class_declaration`/
  `method_definition`/TS's `interface_declaration`/`type_alias_declaration`/
  `enum_declaration` — for these, `export_statement`'s `declaration` field
  can literally BE one of these nodes directly, so their own parent, when
  exported, really is `export_statement`. But **arrow-function/
  function-expression `const`/`let` bindings need an extra hop**: the
  `@item` capture for these is the `variable_declarator` node
  (confirmed via `tree-sitter-javascript`'s own `node-types.json`), whose
  immediate parent is always the `lexical_declaration`/
  `variable_declaration` wrapper (the `const`/`let` keyword) — and
  `export_statement`'s `declaration` field can ALSO directly be a
  `lexical_declaration`/`variable_declaration` node (confirmed: these are
  both listed as concrete subtypes of the grammar's abstract
  `declaration` supertype). So for `export const foo = () => {}`, the
  true ancestor chain is `variable_declarator` → `lexical_declaration` →
  `export_statement` — **two** hops, not one. A 1-hop-only check would
  silently fail to mark exported arrow-function consts as public. Task 3's
  `js_signature_node` below implements the corrected 2-hop-aware version;
  do not simplify it back to a single `.parent()` check.
- `tree-sitter::Node`'s lifetime parameter means the `signature_node` hook
  field on `LanguageConfig` must be written as a higher-ranked function
  pointer type: `for<'a> fn(tree_sitter::Node<'a>) -> tree_sitter::Node<'a>`
  — confirmed to compile in this exact shape (verified independently
  before this plan was written, not assumed).
- Full test suite (`cargo test --workspace`) and `cargo clippy --workspace
  --all-targets` must stay clean (0 failures, 0 warnings) after every
  task.
- `signature_line`'s existing behavior (strip at the body's opening `{`,
  strip a trailing `;`) is unchanged and reused as-is for every language —
  do not modify it. This means a rendered signature never includes a
  trailing `{`, in any language (e.g. `export function makeWidget(size)`,
  not `export function makeWidget(size) {`) — write test assertions
  accordingly.

---

### Task 1: `LanguageConfig` architecture + Rust migration (behavior-preserving)

**Files:**
- Create: `crates/aivyx-repomap/src/languages/mod.rs`
- Create: `crates/aivyx-repomap/src/languages/rust.rs`
- Modify: `crates/aivyx-repomap/src/lib.rs` (add `mod languages;`, replace
  `Extractor`'s hardcoded grammar/queries with the new dispatch, update
  `collect_tags`'s extension check, remove the old top-level `DEF_QUERY`/
  `REF_QUERY` consts — moved into `languages/rust.rs` — and update the 2
  existing tests' `extract()` call sites for the new signature)

**Interfaces:**
- Produces: `languages::LanguageConfig { extensions: &'static [&'static
  str], grammar: fn() -> tree_sitter::Language, def_query: &'static str,
  ref_query: &'static str, signature_node: for<'a> fn(tree_sitter::Node<'a>)
  -> tree_sitter::Node<'a>, is_pub: fn(&str, &str) -> bool }`,
  `languages::LANGUAGES: &'static [LanguageConfig]` (one entry, Rust,
  after this task), `languages::identity_node`, `languages::rust::is_pub`.
  `Extractor::extract(&mut self, source: &str, ext: &str) -> FileTags`
  (signature changed from the current single-argument form — every
  caller, including tests, must pass the file extension now).

This task is a **behavior-preserving refactor**: Rust's own extraction
results must be byte-for-byte identical before and after. No new test
scenarios are added in this task — the existing Rust tests, updated only
for the new `extract()` signature, are the proof.

- [ ] **Step 1: Create `languages/rust.rs`, moving the existing queries verbatim**

```rust
//! Rust's tree-sitter grammar, def/ref queries, and language-specific
//! hooks. Moved here unchanged from the crate's old top-level consts —
//! see `docs/superpowers/specs/2026-07-19-repo-map-multi-language-design.md`
//! for why this crate now has one file per language.

pub(crate) const DEF_QUERY: &str = r#"
(function_item name: (identifier) @name) @item
(function_signature_item name: (identifier) @name) @item
(struct_item name: (type_identifier) @name) @item
(enum_item name: (type_identifier) @name) @item
(union_item name: (type_identifier) @name) @item
(trait_item name: (type_identifier) @name) @item
(mod_item name: (identifier) @name) @item
(const_item name: (identifier) @name) @item
(static_item name: (identifier) @name) @item
(type_item name: (type_identifier) @name) @item
(macro_definition name: (identifier) @name) @item
"#;

pub(crate) const REF_QUERY: &str = r#"
(call_expression function: (identifier) @ref)
(call_expression function: (scoped_identifier name: (identifier) @ref))
(call_expression function: (field_expression field: (field_identifier) @ref))
(type_identifier) @ref
(macro_invocation macro: (identifier) @ref)
"#;

pub(crate) fn language() -> tree_sitter::Language {
    tree_sitter::Language::from(tree_sitter_rust::LANGUAGE)
}

pub(crate) fn is_pub(_name: &str, signature: &str) -> bool {
    signature.starts_with("pub ")
}
```

- [ ] **Step 2: Create `languages/mod.rs` with `LanguageConfig` and the registry**

```rust
//! One `LanguageConfig` per supported language, dispatched by file
//! extension. See `docs/superpowers/specs/
//! 2026-07-19-repo-map-multi-language-design.md` (Decision 3) for why
//! this is a plain data table rather than a trait.

mod rust;

/// Everything the extractor needs to know about one language: which
/// files it claims, its tree-sitter grammar and def/ref queries, and two
/// small hooks for the two things that genuinely differ per language.
pub(crate) struct LanguageConfig {
    /// File extensions this language claims (checked case-sensitively).
    pub(crate) extensions: &'static [&'static str],
    pub(crate) grammar: fn() -> tree_sitter::Language,
    pub(crate) def_query: &'static str,
    pub(crate) ref_query: &'static str,
    /// Given the captured `@item` node, returns the node whose text
    /// should become the rendered signature. Identity for Rust/Python
    /// (the captured item already is the right node); JS/TS substitute
    /// the enclosing `export_statement` when present (see Task 3).
    pub(crate) signature_node: for<'a> fn(tree_sitter::Node<'a>) -> tree_sitter::Node<'a>,
    /// Given the extracted name and the (already `signature_node`
    /// -adjusted) signature text, is this definition part of the file's
    /// public surface? Rust/JS/TS: a text-prefix check ("pub "/"export ").
    /// Python: name-based (no leading underscore).
    pub(crate) is_pub: fn(&str, &str) -> bool,
}

/// Identity `signature_node` hook — the captured item is already the
/// right node to render (Rust, Python).
pub(crate) fn identity_node(item: tree_sitter::Node) -> tree_sitter::Node {
    item
}

pub(crate) const LANGUAGES: &[LanguageConfig] = &[LanguageConfig {
    extensions: &["rs"],
    grammar: rust::language,
    def_query: rust::DEF_QUERY,
    ref_query: rust::REF_QUERY,
    signature_node: identity_node,
    is_pub: rust::is_pub,
}];
```

- [ ] **Step 3: Update `lib.rs`'s existing tests for the new `extract()` signature**

In `crates/aivyx-repomap/src/lib.rs`, find the two existing calls
`extractor.extract(r#"..."#)` (in `extracts_definition_kinds_with_signatures`
and `extracts_call_type_and_macro_references`) and add `"rs"` as a second
argument to each, e.g. change:
```rust
let tags = extractor.extract(
    r#"
pub struct Widget { size: u32 }
...
"#,
);
```
to:
```rust
let tags = extractor.extract(
    r#"
pub struct Widget { size: u32 }
...
"#,
    "rs",
);
```
(Same one-line change to the other test's call site.)

- [ ] **Step 4: Run the tests to verify they fail to compile**

Run: `cargo test -p aivyx-repomap 2>&1 | tail -30`
Expected: compile errors — `extract` still takes one argument, `DEF_QUERY`/
`REF_QUERY` still exist at the top of `lib.rs` (not yet removed), and
`languages` module doesn't exist yet in `lib.rs`'s own view (it's a new
file but not yet declared via `mod languages;`).

- [ ] **Step 5: Refactor `lib.rs`'s `Extractor` and `collect_tags`**

Add near the top of `crates/aivyx-repomap/src/lib.rs` (alongside the
existing `use` statements):
```rust
mod languages;

use languages::{LanguageConfig, LANGUAGES};
```

Remove the old top-level `const DEF_QUERY: &str = ...` and `const
REF_QUERY: &str = ...` blocks entirely (now living in
`languages/rust.rs`).

Replace the existing `struct Extractor { parser: Parser, def_query: Query,
ref_query: Query }` and its `impl Extractor` block with:

```rust
struct CompiledLanguage {
    config: &'static LanguageConfig,
    language: tree_sitter::Language,
    def_query: Query,
    ref_query: Query,
}

struct Extractor {
    parser: Parser,
    languages: Vec<CompiledLanguage>,
}

impl Extractor {
    fn new() -> Self {
        let languages = LANGUAGES
            .iter()
            .map(|config| {
                let language = (config.grammar)();
                let def_query = Query::new(&language, config.def_query).unwrap_or_else(|e| {
                    panic!("{:?} DEF_QUERY must compile: {e}", config.extensions)
                });
                let ref_query = Query::new(&language, config.ref_query).unwrap_or_else(|e| {
                    panic!("{:?} REF_QUERY must compile: {e}", config.extensions)
                });
                CompiledLanguage {
                    config,
                    language,
                    def_query,
                    ref_query,
                }
            })
            .collect();
        Self {
            parser: Parser::new(),
            languages,
        }
    }

    fn for_extension(&self, ext: &str) -> Option<&CompiledLanguage> {
        self.languages
            .iter()
            .find(|l| l.config.extensions.contains(&ext))
    }

    fn extract(&mut self, source: &str, ext: &str) -> FileTags {
        let Some(lang) = self.for_extension(ext) else {
            return FileTags::default();
        };
        self.parser
            .set_language(&lang.language)
            .expect("bundled grammar must load");
        let Some(tree) = self.parser.parse(source, None) else {
            return FileTags::default();
        };
        let bytes = source.as_bytes();
        let mut tags = FileTags::default();

        let name_index = lang
            .def_query
            .capture_index_for_name("name")
            .expect("@name exists");
        let item_index = lang
            .def_query
            .capture_index_for_name("item")
            .expect("@item exists");

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&lang.def_query, tree.root_node(), bytes);
        while let Some(m) = matches.next() {
            let name = m
                .captures
                .iter()
                .find(|c| c.index == name_index)
                .and_then(|c| c.node.utf8_text(bytes).ok());
            let item = m.captures.iter().find(|c| c.index == item_index);
            if let (Some(name), Some(item)) = (name, item) {
                let sig_node = (lang.config.signature_node)(item.node);
                let signature = signature_line(sig_node.utf8_text(bytes).unwrap_or(""));
                let is_pub = (lang.config.is_pub)(name, &signature);
                tags.defs.push(Def {
                    name: name.to_string(),
                    is_pub,
                    signature,
                });
            }
        }

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&lang.ref_query, tree.root_node(), bytes);
        while let Some(m) = matches.next() {
            for capture in m.captures {
                if let Ok(name) = capture.node.utf8_text(bytes) {
                    *tags.refs.entry(name.to_string()).or_insert(0) += 1;
                }
            }
        }

        tags
    }
}

fn is_supported_extension(ext: &str) -> bool {
    LANGUAGES
        .iter()
        .any(|config| config.extensions.contains(&ext))
}
```

Note `Def`'s own `is_pub: bool` field no longer needs to be set inline via
`signature.starts_with("pub ")` at the call site — that logic now lives in
`languages::rust::is_pub` and is invoked generically above.

In `collect_tags`, replace:
```rust
            if !entry.file_type().is_some_and(|ft| ft.is_file())
                || path.extension().is_none_or(|e| e != "rs")
                || is_denied(path, &self.deny_paths)
            {
                continue;
            }
```
with:
```rust
            if !entry.file_type().is_some_and(|ft| ft.is_file())
                || is_denied(path, &self.deny_paths)
            {
                continue;
            }
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if !is_supported_extension(ext) {
                continue;
            }
```
and replace the later `let tags = extractor.extract(&source);` with `let
tags = extractor.extract(&source, ext);`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-repomap 2>&1 | tail -40`
Expected: every existing test (all Rust-only, unchanged assertions) passes
— this is the proof the refactor is behavior-preserving.

- [ ] **Step 7: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-repomap/src/lib.rs crates/aivyx-repomap/src/languages/
git commit -m "Refactor repo-map extraction to a per-language config table"
```

---

### Task 2: Python support

**Files:**
- Create: `crates/aivyx-repomap/src/languages/python.rs`
- Modify: `crates/aivyx-repomap/src/languages/mod.rs` (declare the module,
  add Python's `LanguageConfig` entry)
- Modify: `crates/aivyx-repomap/Cargo.toml` (add `tree-sitter-python`)
- Modify: `crates/aivyx-repomap/src/lib.rs` (2 new tests)

**Interfaces:**
- Consumes: Task 1's `LanguageConfig`, `identity_node`.
- Produces: `languages::python::{DEF_QUERY, REF_QUERY, language, is_pub}`,
  a second `LANGUAGES` entry (extensions `&["py"]`).

- [ ] **Step 1: Add the dependency**

In `crates/aivyx-repomap/Cargo.toml`, add to `[dependencies]`:
```toml
tree-sitter-python = "0.25.0"
```

- [ ] **Step 2: Write the failing tests**

In `crates/aivyx-repomap/src/lib.rs`'s `#[cfg(test)] mod tests` block, add:

```rust
    #[test]
    fn extracts_python_definitions_with_signatures() {
        let mut extractor = Extractor::new();
        let tags = extractor.extract(
            r#"
class Widget:
    def draw(self):
        pass

def make_widget(size: int) -> "Widget":
    return Widget()

def _private_helper():
    pass
"#,
            "py",
        );

        let names: Vec<&str> = tags.defs.iter().map(|d| d.name.as_str()).collect();
        for expected in ["Widget", "draw", "make_widget", "_private_helper"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        let make = tags.defs.iter().find(|d| d.name == "make_widget").unwrap();
        assert_eq!(make.signature, "def make_widget(size: int) -> \"Widget\":");
        assert!(make.is_pub);
        let helper = tags
            .defs
            .iter()
            .find(|d| d.name == "_private_helper")
            .unwrap();
        assert!(!helper.is_pub);
    }

    #[test]
    fn extracts_python_call_and_attribute_references() {
        let mut extractor = Extractor::new();
        let tags = extractor.extract(
            r#"
def caller():
    make_widget(3)
    widget.draw()
    helper.assist()
"#,
            "py",
        );
        for expected in ["make_widget", "draw", "assist"] {
            assert!(
                tags.refs.contains_key(expected),
                "missing ref {expected}: {:?}",
                tags.refs.keys()
            );
        }
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p aivyx-repomap python -- --nocapture 2>&1 | tail -30`
Expected: FAIL — `.py` isn't a supported extension yet, so `extract`
returns an empty `FileTags::default()` for both tests.

- [ ] **Step 4: Create `languages/python.rs`**

```rust
//! Python's tree-sitter grammar, def/ref queries, and language-specific
//! hooks. See `docs/superpowers/specs/
//! 2026-07-19-repo-map-multi-language-design.md` (Decision 5) for why
//! Python's reference query is call-sites only — Python has no distinct
//! "this identifier names a type" grammar node the way Rust/TypeScript
//! do, so a type used only in an annotation and never called won't
//! create a graph edge. Accepted, documented limitation, not a bug.

pub(crate) const DEF_QUERY: &str = r#"
(function_definition name: (identifier) @name) @item
(class_definition name: (identifier) @name) @item
"#;

pub(crate) const REF_QUERY: &str = r#"
(call function: (identifier) @ref)
(call function: (attribute attribute: (identifier) @ref))
"#;

pub(crate) fn language() -> tree_sitter::Language {
    tree_sitter::Language::from(tree_sitter_python::LANGUAGE)
}

/// Python has no formal public/private keyword — the common convention
/// (a leading underscore marks an intentionally private name) is used
/// instead of a signature-text check.
pub(crate) fn is_pub(name: &str, _signature: &str) -> bool {
    !name.starts_with('_')
}
```

- [ ] **Step 5: Register Python in `languages/mod.rs`**

Add `mod python;` alongside `mod rust;`, and add a second entry to
`LANGUAGES`:

```rust
pub(crate) const LANGUAGES: &[LanguageConfig] = &[
    LanguageConfig {
        extensions: &["rs"],
        grammar: rust::language,
        def_query: rust::DEF_QUERY,
        ref_query: rust::REF_QUERY,
        signature_node: identity_node,
        is_pub: rust::is_pub,
    },
    LanguageConfig {
        extensions: &["py"],
        grammar: python::language,
        def_query: python::DEF_QUERY,
        ref_query: python::REF_QUERY,
        signature_node: identity_node,
        is_pub: python::is_pub,
    },
];
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-repomap python -- --nocapture 2>&1 | tail -30`
Expected: both new tests pass.

- [ ] **Step 7: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-repomap/Cargo.toml crates/aivyx-repomap/Cargo.lock \
        crates/aivyx-repomap/src/languages/ crates/aivyx-repomap/src/lib.rs
git commit -m "Add Python support to the repo map"
```

---

### Task 3: JavaScript support

**Files:**
- Create: `crates/aivyx-repomap/src/languages/javascript.rs`
- Modify: `crates/aivyx-repomap/src/languages/mod.rs` (declare the module,
  add the `js_signature_node`/`export_is_pub` hooks — shared with Task 4's
  TypeScript entries — and JS's `LanguageConfig` entry)
- Modify: `crates/aivyx-repomap/Cargo.toml` (add `tree-sitter-javascript`)
- Modify: `crates/aivyx-repomap/src/lib.rs` (2 new tests)

**Interfaces:**
- Consumes: Task 1's `LanguageConfig`.
- Produces: `languages::javascript::{DEF_QUERY, REF_QUERY, language}`,
  `languages::{js_signature_node, export_is_pub}` (placed in `mod.rs`
  since Task 4 reuses both), a `LANGUAGES` entry (extensions
  `&["js", "jsx"]`).

- [ ] **Step 1: Add the dependency**

In `crates/aivyx-repomap/Cargo.toml`, add to `[dependencies]`:
```toml
tree-sitter-javascript = "0.25.0"
```

- [ ] **Step 2: Write the failing tests**

In `crates/aivyx-repomap/src/lib.rs`'s test module, add:

```rust
    #[test]
    fn extracts_js_definitions_with_export_and_signatures() {
        let mut extractor = Extractor::new();
        let tags = extractor.extract(
            r#"
export function makeWidget(size) {
    return { size };
}

function helper() {
    return 1;
}

export class Widget {
    draw() {
        return true;
    }
}

export const arrowFn = (x) => {
    return x + 1;
};
"#,
            "js",
        );

        let names: Vec<&str> = tags.defs.iter().map(|d| d.name.as_str()).collect();
        for expected in ["makeWidget", "helper", "Widget", "draw", "arrowFn"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }

        let make = tags.defs.iter().find(|d| d.name == "makeWidget").unwrap();
        assert_eq!(make.signature, "export function makeWidget(size)");
        assert!(make.is_pub);

        let helper = tags.defs.iter().find(|d| d.name == "helper").unwrap();
        assert_eq!(helper.signature, "function helper()");
        assert!(!helper.is_pub);

        let widget = tags.defs.iter().find(|d| d.name == "Widget").unwrap();
        assert!(widget.is_pub);

        // Arrow-function const bindings need the 2-hop export check (see
        // this plan's Global Constraints) — this is the case that would
        // silently fail to register as public under a naive 1-hop check.
        let arrow = tags.defs.iter().find(|d| d.name == "arrowFn").unwrap();
        assert!(
            arrow.signature.starts_with("export "),
            "exported arrow-function const must show the export keyword: {}",
            arrow.signature
        );
        assert!(arrow.is_pub);
    }

    #[test]
    fn extracts_js_call_and_new_references() {
        let mut extractor = Extractor::new();
        let tags = extractor.extract(
            r#"
function caller() {
    makeWidget(3);
    widget.draw();
    const w = new Widget();
}
"#,
            "js",
        );
        for expected in ["makeWidget", "draw", "Widget"] {
            assert!(
                tags.refs.contains_key(expected),
                "missing ref {expected}: {:?}",
                tags.refs.keys()
            );
        }
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p aivyx-repomap js -- --nocapture 2>&1 | tail -40`
Expected: FAIL — `.js` isn't supported yet.

- [ ] **Step 4: Create `languages/javascript.rs`**

```rust
//! JavaScript's tree-sitter grammar and def/ref queries. `.jsx` shares
//! this same grammar and query set (JSX syntax is a superset the plain
//! JavaScript grammar already understands) — see `languages::mod`'s
//! registry, which lists both extensions against this one config.

pub(crate) const DEF_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @item
(class_declaration name: (identifier) @name) @item
(method_definition name: (property_identifier) @name) @item
(variable_declarator name: (identifier) @name value: (arrow_function)) @item
(variable_declarator name: (identifier) @name value: (function_expression)) @item
"#;

pub(crate) const REF_QUERY: &str = r#"
(call_expression function: (identifier) @ref)
(call_expression function: (member_expression property: (property_identifier) @ref))
(new_expression constructor: (identifier) @ref)
"#;

pub(crate) fn language() -> tree_sitter::Language {
    tree_sitter::Language::from(tree_sitter_javascript::LANGUAGE)
}
```

- [ ] **Step 5: Add the shared JS/TS hooks to `languages/mod.rs`**

Add these two functions to `crates/aivyx-repomap/src/languages/mod.rs`,
alongside `identity_node`:

```rust
/// Given the captured `@item` node, returns the node whose text should
/// become the rendered signature: the enclosing `export_statement` when
/// this item is exported, or the item itself otherwise.
///
/// Arrow-function/function-expression `const`/`let` bindings need an
/// extra hop: their `@item` capture is the `variable_declarator` node,
/// whose own immediate parent is always the `lexical_declaration`/
/// `variable_declaration` wrapper (the `const`/`let` keyword) — and
/// `export_statement`'s `declaration` field can itself directly be one of
/// *those* wrapper nodes (confirmed against `tree-sitter-javascript`'s
/// own `node-types.json`: both are listed as concrete subtypes of the
/// grammar's `declaration` supertype). So for `export const foo = () =>
/// {}` the true ancestor chain is `variable_declarator` ->
/// `lexical_declaration` -> `export_statement`: two hops, not one.
/// `function_declaration`/`class_declaration`/`method_definition`/TS's
/// `interface_declaration`/`type_alias_declaration`/`enum_declaration`
/// don't have this extra wrapper (their own parent, when exported,
/// already IS `export_statement`), so checking one extra potential hop
/// is always safe — it's a no-op for those forms, since
/// `lexical_declaration`/`variable_declaration` never appears as their
/// parent.
pub(crate) fn js_signature_node(item: tree_sitter::Node) -> tree_sitter::Node {
    let mut candidate = item;
    if let Some(parent) = candidate.parent() {
        if parent.kind() == "lexical_declaration" || parent.kind() == "variable_declaration" {
            candidate = parent;
        }
    }
    candidate
        .parent()
        .filter(|p| p.kind() == "export_statement")
        .unwrap_or(item)
}

/// JS/TS's "public" signal: is this definition wrapped in an `export`?
/// Once `js_signature_node` has (possibly) substituted in the enclosing
/// `export_statement`, an exported item's signature text literally
/// starts with "export " — the same shape as Rust's own "pub " check.
pub(crate) fn export_is_pub(_name: &str, signature: &str) -> bool {
    signature.starts_with("export ")
}
```

- [ ] **Step 6: Register JavaScript in `languages/mod.rs`**

Add `mod javascript;` and a third `LANGUAGES` entry:

```rust
    LanguageConfig {
        extensions: &["js", "jsx"],
        grammar: javascript::language,
        def_query: javascript::DEF_QUERY,
        ref_query: javascript::REF_QUERY,
        signature_node: js_signature_node,
        is_pub: export_is_pub,
    },
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p aivyx-repomap js -- --nocapture 2>&1 | tail -40`
Expected: both new tests pass, including the exported-arrow-function
assertion.

- [ ] **Step 8: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/aivyx-repomap/Cargo.toml crates/aivyx-repomap/Cargo.lock \
        crates/aivyx-repomap/src/languages/ crates/aivyx-repomap/src/lib.rs
git commit -m "Add JavaScript support to the repo map"
```

---

### Task 4: TypeScript/TSX support

**Files:**
- Create: `crates/aivyx-repomap/src/languages/typescript.rs`
- Modify: `crates/aivyx-repomap/src/languages/mod.rs` (declare the module,
  add two `LANGUAGES` entries — `.ts` and `.tsx` — sharing the same
  queries but different grammar functions)
- Modify: `crates/aivyx-repomap/Cargo.toml` (add `tree-sitter-typescript`)
- Modify: `crates/aivyx-repomap/src/lib.rs` (2 new tests)

**Interfaces:**
- Consumes: Task 3's `js_signature_node`, `export_is_pub`.
- Produces: `languages::typescript::{DEF_QUERY, REF_QUERY, language_ts,
  language_tsx}`, two new `LANGUAGES` entries.

- [ ] **Step 1: Add the dependency**

In `crates/aivyx-repomap/Cargo.toml`, add to `[dependencies]`:
```toml
tree-sitter-typescript = "0.23.2"
```

- [ ] **Step 2: Write the failing tests**

In `crates/aivyx-repomap/src/lib.rs`'s test module, add:

```rust
    #[test]
    fn extracts_ts_only_definitions_and_type_references() {
        let mut extractor = Extractor::new();
        let tags = extractor.extract(
            r#"
export interface Shape {
    area(): number;
}

type Point = { x: number; y: number };

enum Color {
    Red,
    Green,
}

function useShape(s: Shape): Point {
    return { x: 0, y: 0 };
}
"#,
            "ts",
        );

        let names: Vec<&str> = tags.defs.iter().map(|d| d.name.as_str()).collect();
        for expected in ["Shape", "Point", "Color", "useShape"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        let shape = tags.defs.iter().find(|d| d.name == "Shape").unwrap();
        assert!(shape.is_pub);

        // TypeScript has a real `type_identifier` grammar node, so type
        // annotations alone (not just calls) create reference edges —
        // the one place TS gets strictly richer references than JS.
        for expected in ["Shape", "Point"] {
            assert!(
                tags.refs.contains_key(expected),
                "missing type ref {expected}: {:?}",
                tags.refs.keys()
            );
        }
    }

    #[test]
    fn tsx_shares_typescripts_queries() {
        let mut extractor = Extractor::new();
        let tags = extractor.extract(
            r#"
export function Widget(props: { label: string }) {
    return <div>{props.label}</div>;
}
"#,
            "tsx",
        );
        let names: Vec<&str> = tags.defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Widget"), "missing Widget: {names:?}");
        let widget = tags.defs.iter().find(|d| d.name == "Widget").unwrap();
        assert!(widget.is_pub);
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p aivyx-repomap ts_only tsx_shares -- --nocapture 2>&1 | tail -40`
Expected: FAIL — `.ts`/`.tsx` aren't supported yet.

- [ ] **Step 4: Create `languages/typescript.rs`**

TypeScript reuses every JavaScript definition/reference pattern (function/
class/method/arrow-const forms, call-sites, `new` construction) plus its
own type-only declarations and a real `type_identifier` reference. Since
tree-sitter query strings are plain compiled text (not shared Rust code),
this const simply repeats JS's own patterns alongside the TS-only ones,
rather than trying to concatenate two separate consts at compile time —
the simpler, more direct choice for two literal strings:

```rust
//! TypeScript's tree-sitter grammar and queries. `tree-sitter-typescript`
//! exposes two distinct grammars from one crate — `LANGUAGE_TYPESCRIPT`
//! for `.ts` and `LANGUAGE_TSX` for `.tsx` — both of which accept this
//! same query text (TSX is a strict superset of TS's own node kinds, it
//! only adds JSX-specific ones on top).

pub(crate) const DEF_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @item
(class_declaration name: (identifier) @name) @item
(method_definition name: (property_identifier) @name) @item
(variable_declarator name: (identifier) @name value: (arrow_function)) @item
(variable_declarator name: (identifier) @name value: (function_expression)) @item
(interface_declaration name: (type_identifier) @name) @item
(type_alias_declaration name: (type_identifier) @name) @item
(enum_declaration name: (identifier) @name) @item
"#;

pub(crate) const REF_QUERY: &str = r#"
(call_expression function: (identifier) @ref)
(call_expression function: (member_expression property: (property_identifier) @ref))
(new_expression constructor: (identifier) @ref)
(type_identifier) @ref
"#;

pub(crate) fn language_ts() -> tree_sitter::Language {
    tree_sitter::Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT)
}

pub(crate) fn language_tsx() -> tree_sitter::Language {
    tree_sitter::Language::from(tree_sitter_typescript::LANGUAGE_TSX)
}
```

- [ ] **Step 5: Register TypeScript and TSX in `languages/mod.rs`**

Add `mod typescript;` and two more `LANGUAGES` entries (reusing Task 3's
`js_signature_node`/`export_is_pub` — TS's exported-declaration shapes are
a superset of JS's, so the same 2-hop-aware hook and prefix check apply
unchanged):

```rust
    LanguageConfig {
        extensions: &["ts"],
        grammar: typescript::language_ts,
        def_query: typescript::DEF_QUERY,
        ref_query: typescript::REF_QUERY,
        signature_node: js_signature_node,
        is_pub: export_is_pub,
    },
    LanguageConfig {
        extensions: &["tsx"],
        grammar: typescript::language_tsx,
        def_query: typescript::DEF_QUERY,
        ref_query: typescript::REF_QUERY,
        signature_node: js_signature_node,
        is_pub: export_is_pub,
    },
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p aivyx-repomap ts_only tsx_shares -- --nocapture 2>&1 | tail -40`
Expected: both new tests pass.

- [ ] **Step 7: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/aivyx-repomap/Cargo.toml crates/aivyx-repomap/Cargo.lock \
        crates/aivyx-repomap/src/languages/ crates/aivyx-repomap/src/lib.rs
git commit -m "Add TypeScript/TSX support to the repo map"
```

---

### Task 5: Mixed-language repo test + non-Rust PageRank test

**Files:**
- Modify: `crates/aivyx-repomap/src/lib.rs` (2 new tests, in the existing
  test module)

**Interfaces:**
- Consumes: Tasks 1-4's fully-wired 5-language registry (`rs`, `py`, `js`/
  `jsx`, `ts`, `tsx`).

These two tests need at least two shipped languages to be meaningful,
which is why they're sequenced after Tasks 2-4 rather than folded into
any single language's own task.

- [ ] **Step 1: Write the failing tests**

In `crates/aivyx-repomap/src/lib.rs`'s test module, add (this file's
existing `write` test helper, used by several tests already in this
module, is reused here):

```rust
    #[test]
    fn a_python_reference_graph_ranks_a_heavily_called_file_first() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "core.py",
            "def start():\n    pass\n",
        );
        write(
            dir.path(),
            "a.py",
            "from core import start\n\ndef a():\n    start()\n",
        );
        write(
            dir.path(),
            "b.py",
            "from core import start\n\ndef b():\n    start()\n",
        );
        write(dir.path(), "lonely.py", "def unused_helper():\n    pass\n");

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).expect("map should render");

        let core_pos = rendered.find("core.py").expect("core.py in map");
        let lonely_pos = rendered.find("lonely.py").expect("lonely.py in map");
        assert!(
            core_pos < lonely_pos,
            "referenced file should outrank unreferenced one:\n{rendered}"
        );
    }

    #[test]
    fn a_mixed_language_repo_gets_one_unified_map() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "engine.rs", "pub fn run_engine() {}\n");
        write(
            dir.path(),
            "widget.ts",
            "export function makeWidget(): void {}\n",
        );

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).expect("map should render");

        assert!(rendered.contains("engine.rs"), "missing Rust file:\n{rendered}");
        assert!(rendered.contains("run_engine"), "missing Rust def:\n{rendered}");
        assert!(rendered.contains("widget.ts"), "missing TS file:\n{rendered}");
        assert!(
            rendered.contains("makeWidget"),
            "missing TS def:\n{rendered}"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p aivyx-repomap python_reference_graph mixed_language -- --nocapture 2>&1 | tail -30`
Expected: both should actually already pass once Tasks 1-4 are complete
(the underlying machinery is already fully language-agnostic, per the
spec's own Context section) — if either fails, that indicates a real gap
in Tasks 1-4's wiring, not something to fix in this task; investigate and
fix the actual root cause in the relevant earlier task's code before
proceeding.

- [ ] **Step 3: Run the full workspace suite and clippy**

Run: `cargo test --workspace 2>&1 | grep -E "^test result|FAILED"`
Expected: every crate `ok`, 0 failed.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: 0 warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/aivyx-repomap/src/lib.rs
git commit -m "Prove the repo map handles a mixed-language repo as one unified map"
```

---

### Task 6: Live E2E through the real binary

**Files:**
- None modified — verification only, via this project's established
  live-E2E method (PTY + `python-pyte`, graded via the persisted session
  JSON).

**Interfaces:**
- Consumes: Tasks 1-5's fully-wired feature.

- [ ] **Step 1: Build the release binary**

```bash
cargo build --release -p aivyx 2>&1 | tail -10
```

Expected: succeeds.

- [ ] **Step 2: Set up a scratch Python project with a real cross-file dependency**

```bash
SCRATCH_PROJECT=$(mktemp -d)
cd "$SCRATCH_PROJECT"
git init -q -b main
git config user.name test
git config user.email test@test.invalid

cat > calc.py << 'EOF'
def add(a, b):
    return a + b

def subtract(a, b):
    return a - b
EOF

cat > main.py << 'EOF'
from calc import add, subtract

print(add(2, 2))
print(subtract(5, 2))
EOF

git add -A
git commit -q -m initial
```

- [ ] **Step 3: Drive the real binary, ask a question the repo map alone should answer**

Follow this project's established live-E2E harness pattern (`python-pyte`
for rendering, explicit `TIOCSWINSZ` window sizing, paced keystrokes
~15ms apart, wait for the "Type a message..." readiness marker, wait for
both the ready-status text and the input-placeholder text before
considering a turn complete). Ensure `[repo_map] enabled = true` (the
project's existing default) and a reasonable `budget_tokens` (e.g. the
existing default of 1024) in `~/.config/aivyx-coder/config.toml` — back up
the file first and restore it afterward, per this project's established
practice for live-E2E tasks that touch the real global config (see prior
phases' live-E2E tasks for this exact backup/restore pattern).

Send a message that only the repo map (not a `read_file` call) could
plausibly answer without the model first reading any file, e.g.:

`"Without reading any files, what functions are defined in calc.py?"`

- [ ] **Step 4: Grade from the persisted session JSON**

Confirm via `~/.local/state/aivyx-coder/sessions/<hash>.json` (not screen
text):
- No `read_file`/`grep`/`glob` tool call appears before the model's answer
  (proving the injected repo-map content, not a tool call, informed the
  response — mirrors how `editor_context`'s own live E2E proved its
  injected note alone informed an answer).
- The model's answer names `add` and `subtract` (the two real functions in
  `calc.py`), confirming the map's Python extraction genuinely reached the
  system prompt, not just unit-tested in isolation.

If the small local model doesn't reliably skip tool calls even when it
has the answer already (a real risk with local models, not something to
force), a weaker but still valid proof is: the model's answer is correct
whether or not it also called a tool — the key evidence is *correctness*,
with "no tool call needed" as a bonus signal, not a hard requirement to
re-prompt endlessly for.

- [ ] **Step 5: Clean up**

```bash
rm -f ~/.local/state/aivyx-coder/sessions/*"$(basename "$SCRATCH_PROJECT")"*.json 2>/dev/null || true
rm -rf "$SCRATCH_PROJECT"
```

(Adjust the session-file glob after inspecting what file actually got
created, per `session::session_file_path`'s own naming convention. Also
restore `~/.config/aivyx-coder/config.toml` from the Step 3 backup and
verify the restoration byte-for-byte, per this project's established
practice.)

- [ ] **Step 6: Report**

No commit for this task (verification only, no files modified). Report
the exact session JSON excerpt showing the model's answer and confirming
no `read_file`/`grep`/`glob` call preceded it (or, if the model did call a
tool anyway, report that honestly alongside the correctness evidence,
rather than treating it as a failed test).
