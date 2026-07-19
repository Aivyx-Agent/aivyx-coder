//! TypeScript's tree-sitter grammar and queries. `tree-sitter-typescript`
//! exposes two distinct grammars from one crate — `LANGUAGE_TYPESCRIPT`
//! for `.ts` and `LANGUAGE_TSX` for `.tsx` — both of which accept this
//! same query text (TSX is a strict superset of TS's own node kinds, it
//! only adds JSX-specific ones on top).

pub(crate) const DEF_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @item
(class_declaration name: (type_identifier) @name) @item
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
