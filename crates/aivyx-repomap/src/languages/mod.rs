//! One `LanguageConfig` per supported language, dispatched by file
//! extension. See `docs/superpowers/specs/
//! 2026-07-19-repo-map-multi-language-design.md` (Decision 3) for why
//! this is a plain data table rather than a trait.

mod javascript;
mod python;
mod rust;
mod typescript;

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
    if let Some(parent) = candidate.parent()
        && (parent.kind() == "lexical_declaration" || parent.kind() == "variable_declaration")
    {
        candidate = parent;
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
    LanguageConfig {
        extensions: &["js", "jsx"],
        grammar: javascript::language,
        def_query: javascript::DEF_QUERY,
        ref_query: javascript::REF_QUERY,
        signature_node: js_signature_node,
        is_pub: export_is_pub,
    },
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
];
