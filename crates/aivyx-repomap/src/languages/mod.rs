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
