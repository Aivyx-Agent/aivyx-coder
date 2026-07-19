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
