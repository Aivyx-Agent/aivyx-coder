//! Parser for prompted SEARCH/REPLACE edit blocks (Aider's format) in
//! assistant text:
//!
//! ```text
//! path/to/file.rs
//! <<<<<<< SEARCH
//! old lines
//! =======
//! new lines
//! >>>>>>> REPLACE
//! ```
//!
//! Deliberately tolerant of the decoration small models wrap around the
//! format — code fences before/after, backticks or bold markers on the
//! path line, 5–9 marker characters — while every *detected* block that
//! can't be fully parsed is reported as `Malformed` rather than silently
//! dropped, so the model gets corrective feedback in-history.

/// One well-formed block. An empty `search` means "create this file with
/// `replace` as its content".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditBlock {
    pub path: String,
    pub search: String,
    pub replace: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockParse {
    Ok(EditBlock),
    /// A `<<<<<<< SEARCH` marker was seen but the block couldn't be
    /// completed; the string says what was missing.
    Malformed(String),
}

fn is_search_marker(line: &str) -> bool {
    marker_matches(line, '<', "SEARCH")
}

fn is_divider(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed.len() >= 5
        && trimmed.len() <= 9
        && trimmed.chars().all(|c| c == '=')
}

fn is_replace_marker(line: &str) -> bool {
    marker_matches(line, '>', "REPLACE")
}

fn marker_matches(line: &str, ch: char, word: &str) -> bool {
    let trimmed = line.trim();
    let run = trimmed.chars().take_while(|&c| c == ch).count();
    if !(5..=9).contains(&run) {
        return false;
    }
    trimmed[run..].trim() == word
}

/// The path is the nearest preceding non-blank line that isn't a code
/// fence, stripped of the decorations models like to add (`backticks`,
/// **bold**, `#` headers, a trailing `:`).
fn extract_path(preceding: &[&str]) -> Option<String> {
    for line in preceding.iter().rev() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        let cleaned = line
            .trim_matches(|c| c == '`' || c == '*' || c == '#' || c == ':' || c == ' ')
            .trim();
        if cleaned.is_empty() || cleaned.contains(' ') {
            // A sentence, not a path — keep looking upward would only
            // find prose; give up on this block instead of guessing.
            return None;
        }
        return Some(cleaned.to_string());
    }
    None
}

pub fn parse_edit_blocks(text: &str) -> Vec<BlockParse> {
    // Detected once, up front, from the raw text -- `str::lines()` below
    // strips the `\r` off every CRLF line, so by the time we're looking at
    // individual lines there's no way to tell CRLF and LF input apart
    // per-line. Rejoining unconditionally with a bare `\n` (as this used
    // to) would silently produce a SEARCH/REPLACE string that never
    // matches a CRLF file's real on-disk content.
    let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if !is_search_marker(lines[i]) {
            i += 1;
            continue;
        }

        let Some(path) = extract_path(&lines[..i]) else {
            blocks.push(BlockParse::Malformed(
                "SEARCH/REPLACE block has no file path on the line above `<<<<<<< SEARCH`"
                    .to_string(),
            ));
            i += 1;
            continue;
        };

        // Collect the SEARCH side up to the ======= divider.
        let mut j = i + 1;
        let mut search_lines: Vec<&str> = Vec::new();
        while j < lines.len() && !is_divider(lines[j]) {
            if is_search_marker(lines[j]) || is_replace_marker(lines[j]) {
                break;
            }
            search_lines.push(lines[j]);
            j += 1;
        }
        if j >= lines.len() || !is_divider(lines[j]) {
            blocks.push(BlockParse::Malformed(format!(
                "block for `{path}` is missing the `=======` divider"
            )));
            i = j;
            continue;
        }

        // Collect the REPLACE side up to the >>>>>>> REPLACE terminator.
        let mut k = j + 1;
        let mut replace_lines: Vec<&str> = Vec::new();
        while k < lines.len() && !is_replace_marker(lines[k]) {
            if is_search_marker(lines[k]) {
                break;
            }
            replace_lines.push(lines[k]);
            k += 1;
        }
        if k >= lines.len() || !is_replace_marker(lines[k]) {
            blocks.push(BlockParse::Malformed(format!(
                "block for `{path}` is missing the `>>>>>>> REPLACE` terminator"
            )));
            i = k;
            continue;
        }

        blocks.push(BlockParse::Ok(EditBlock {
            path,
            search: join_block(&search_lines, line_ending),
            replace: join_block(&replace_lines, line_ending),
        }));
        i = k + 1;
    }

    blocks
}

/// Block content keeps interior lines verbatim; a single trailing line
/// ending is appended when non-empty so whole-line replacements splice
/// cleanly. `line_ending` is `"\r\n"` or `"\n"`, detected once per input
/// text by the caller, so a CRLF-line-ended file's content round-trips
/// through SEARCH/REPLACE unchanged rather than silently degrading to LF.
fn join_block(lines: &[&str], line_ending: &str) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join(line_ending);
    out.push_str(line_ending);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_blocks(text: &str) -> Vec<EditBlock> {
        parse_edit_blocks(text)
            .into_iter()
            .filter_map(|b| match b {
                BlockParse::Ok(block) => Some(block),
                BlockParse::Malformed(_) => None,
            })
            .collect()
    }

    #[test]
    fn parses_a_plain_block() {
        let blocks = ok_blocks(
            "src/lib.rs\n<<<<<<< SEARCH\nfn old() {}\n=======\nfn new() {}\n>>>>>>> REPLACE\n",
        );
        assert_eq!(
            blocks,
            vec![EditBlock {
                path: "src/lib.rs".to_string(),
                search: "fn old() {}\n".to_string(),
                replace: "fn new() {}\n".to_string(),
            }]
        );
    }

    #[test]
    fn tolerates_fences_and_path_decorations() {
        let text = "I'll update the file:\n\n**`src/main.rs`**\n```rust\n<<<<<<< SEARCH\nlet a = 1;\n=======\nlet a = 2;\n>>>>>>> REPLACE\n```\ndone.";
        let blocks = ok_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].path, "src/main.rs");
        assert_eq!(blocks[0].search, "let a = 1;\n");
    }

    #[test]
    fn parses_multiple_blocks_in_one_response() {
        let text = "a.rs\n<<<<<<< SEARCH\none\n=======\nuno\n>>>>>>> REPLACE\n\nb.rs\n<<<<<<< SEARCH\ntwo\n=======\ndos\n>>>>>>> REPLACE\n";
        let blocks = ok_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].path, "a.rs");
        assert_eq!(blocks[1].path, "b.rs");
    }

    #[test]
    fn empty_search_means_new_file() {
        let blocks = ok_blocks(
            "new_module.rs\n<<<<<<< SEARCH\n=======\npub fn hello() {}\n>>>>>>> REPLACE\n",
        );
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].search, "");
        assert_eq!(blocks[0].replace, "pub fn hello() {}\n");
    }

    #[test]
    fn multiline_content_with_equals_signs_inside_survives() {
        // `let x == comparison` lines must not be mistaken for the divider —
        // only a line that is entirely `=` characters divides.
        let text = "m.rs\n<<<<<<< SEARCH\nif a == b {\n    c = d;\n}\n=======\nif a == b {\n    c = e;\n}\n>>>>>>> REPLACE\n";
        let blocks = ok_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].search.contains("a == b"));
        assert!(blocks[0].replace.contains("c = e;"));
    }

    #[test]
    fn marker_length_is_flexible_but_bounded() {
        let five = "f.rs\n<<<<< SEARCH\nx\n=====\ny\n>>>>> REPLACE\n";
        assert_eq!(ok_blocks(five).len(), 1);
        // A 4-char run is ordinary text (e.g. quoted diff noise), not a marker.
        let four = "f.rs\n<<<< SEARCH\nx\n====\ny\n>>>> REPLACE\n";
        assert!(ok_blocks(four).is_empty());
    }

    #[test]
    fn missing_terminator_is_reported_not_dropped() {
        let parsed = parse_edit_blocks("f.rs\n<<<<<<< SEARCH\nx\n=======\ny\n");
        assert_eq!(parsed.len(), 1);
        let BlockParse::Malformed(msg) = &parsed[0] else {
            panic!("expected Malformed, got {parsed:?}");
        };
        assert!(msg.contains("REPLACE"));
    }

    #[test]
    fn missing_divider_is_reported() {
        let parsed = parse_edit_blocks("f.rs\n<<<<<<< SEARCH\nx\n>>>>>>> REPLACE\n");
        assert_eq!(parsed.len(), 1);
        assert!(matches!(&parsed[0], BlockParse::Malformed(m) if m.contains("=======")));
    }

    #[test]
    fn prose_above_the_marker_is_not_a_path() {
        let parsed = parse_edit_blocks(
            "Here is the change we discussed earlier today\n<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE\n",
        );
        assert_eq!(parsed.len(), 1);
        assert!(matches!(&parsed[0], BlockParse::Malformed(m) if m.contains("path")));
    }

    #[test]
    fn text_without_markers_yields_nothing() {
        assert!(
            parse_edit_blocks("just a normal answer with code:\n```rust\nfn main() {}\n```")
                .is_empty()
        );
    }

    #[test]
    fn search_replace_matches_against_crlf_line_endings() {
        // A CRLF-line-ended file's content, quoted verbatim by the model
        // into its SEARCH section, must round-trip with `\r\n` preserved --
        // `str::lines()` strips `\r` from each line, so naively rejoining
        // with a bare `\n` would silently produce a SEARCH string that
        // never matches the real (CRLF) file content on disk.
        let file_content = "line one\r\nline two\r\nline three\r\n";
        let text =
            "f.txt\r\n<<<<<<< SEARCH\r\nline two\r\n=======\r\nline TWO\r\n>>>>>>> REPLACE\r\n";
        let blocks = ok_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].search, "line two\r\n");
        assert_eq!(blocks[0].replace, "line TWO\r\n");
        assert!(
            file_content.contains(&blocks[0].search),
            "SEARCH text {:?} does not match the CRLF file content",
            blocks[0].search
        );
    }
}
