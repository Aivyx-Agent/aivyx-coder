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
