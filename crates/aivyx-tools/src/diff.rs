use similar::TextDiff;

/// Renders a unified diff between `old` and `new` for display in a
/// permission-confirmation prompt. `aivyx-sandbox`/`aivyx-tui` treat the
/// result as an opaque string — only this crate needs a diff library.
pub(crate) fn unified_diff(label: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(label, label)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_additions_and_removals() {
        let rendered = unified_diff("a.rs", "line1\nline2\n", "line1\nline2 changed\n");
        assert!(rendered.contains("-line2"));
        assert!(rendered.contains("+line2 changed"));
    }

    #[test]
    fn new_file_shows_all_additions() {
        let rendered = unified_diff("new.rs", "", "hello\nworld\n");
        assert!(rendered.contains("+hello"));
        assert!(rendered.contains("+world"));
    }
}
