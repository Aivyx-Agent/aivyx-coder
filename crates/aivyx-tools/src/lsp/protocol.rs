//! Minimal hand-rolled LSP response types — just enough to deserialize
//! `textDocument/definition`/`textDocument/references` results. Request
//! params are built inline via `serde_json::json!` at the call sites
//! (matching this project's existing convention, e.g. `council.rs`'s and
//! `architect.rs`'s `ChatRequest`/prompt construction), so there's no
//! parallel typed-params surface to maintain. Deliberately not the
//! `lsp-types` crate — this workspace hand-rolls small protocol/format
//! surfaces rather than pulling in a heavy dependency for a handful of
//! JSON shapes (the same reasoning behind the hand-rolled frontmatter
//! parser having no YAML dependency).

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct Position {
    pub line: u32,
    #[allow(dead_code)]
    pub character: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Range {
    pub start: Position,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Location {
    pub uri: String,
    pub range: Range,
}
