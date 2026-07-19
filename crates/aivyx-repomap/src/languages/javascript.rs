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
