//! Aider-style repository map: tree-sitter symbol extraction, PageRank over
//! the cross-file reference graph, and a token-budgeted rendering that gets
//! appended to the agent's system prompt each turn.
//!
//! v1 parses Rust only (per the Phase 6 design); files in other languages
//! simply contribute no symbols and the map degrades gracefully — an empty
//! map renders as `None` so non-Rust projects pay no prompt cost at all.
//!
//! Deliberately dependency-free of the rest of the workspace: pure
//! filesystem-in, string-out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

mod languages;

use languages::{LanguageConfig, LANGUAGES};

/// Files larger than this are skipped — generated monsters would dominate
/// parse time while contributing noise.
const MAX_FILE_BYTES: u64 = 512 * 1024;

/// Signatures shown per file before "..." — one huge module must not hog
/// the whole budget.
const MAX_SIGNATURES_PER_FILE: usize = 20;

/// Same chars-per-token convention as the agent's estimator.
const CHARS_PER_TOKEN: usize = 4;

const PAGERANK_DAMPING: f64 = 0.85;
const PAGERANK_ITERATIONS: usize = 30;

/// Where generated wiki pages live, relative to `root` — duplicated from
/// `aivyx-core::wiki::WIKI_DIR` rather than shared: this crate is
/// deliberately dependency-free of the rest of the workspace (see the
/// module doc comment at the top of this file). Keep both literals in sync
/// if this path ever changes.
const WIKI_DIR: &str = "docs/wiki";


#[derive(Debug, Clone)]
struct Def {
    name: String,
    /// First line of the item, e.g. `pub fn run_turn(&mut self,`.
    signature: String,
    is_pub: bool,
}

#[derive(Debug, Clone, Default)]
struct FileTags {
    defs: Vec<Def>,
    /// Referenced name -> occurrence count.
    refs: HashMap<String, u32>,
}

struct CacheEntry {
    mtime: SystemTime,
    size: u64,
    tags: FileTags,
}

pub struct RepoMap {
    root: PathBuf,
    deny_paths: Vec<PathBuf>,
    cache: Mutex<HashMap<PathBuf, CacheEntry>>,
}

impl RepoMap {
    pub fn new(root: PathBuf, deny_paths: Vec<PathBuf>) -> Self {
        Self {
            root,
            deny_paths,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Builds the budgeted map, or `None` when the repository yields no
    /// symbols (not a Rust project, or nothing parseable) — the caller then
    /// adds nothing to the prompt. Synchronous and CPU-bound: call from
    /// `spawn_blocking`.
    pub fn render(&self, budget_tokens: u32) -> Option<String> {
        if budget_tokens == 0 {
            return None;
        }
        let files = self.collect_tags();
        if files.iter().all(|(_, tags)| tags.defs.is_empty()) {
            return None;
        }

        let ranks = pagerank(&files);
        let mut order: Vec<usize> = (0..files.len()).collect();
        order.sort_by(|&a, &b| {
            ranks[b]
                .partial_cmp(&ranks[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let budget_chars = budget_tokens as usize * CHARS_PER_TOKEN;
        let mut out = String::from("Repository map (top files by internal references):\n");
        for &i in &order {
            let (path, tags) = &files[i];
            if tags.defs.is_empty() {
                continue;
            }
            let relative = path.strip_prefix(&self.root).unwrap_or(path);
            let entry = render_file(relative, tags);
            if out.len() + entry.len() > budget_chars {
                // Budget spent; whatever ranked below simply isn't shown.
                break;
            }
            out.push_str(&entry);
        }

        for line in self.wiki_pointer_lines() {
            if out.len() + line.len() > budget_chars {
                break;
            }
            out.push_str(&line);
        }

        // Only the header fit — the budget is too small to say anything.
        (out.lines().count() > 1).then_some(out)
    }

    /// Lightweight pointer lines for existing wiki pages (path + one-line
    /// summary, if the page has one) — cheap enough to include every turn;
    /// the model reads a page's full content via `read_file` only if the
    /// pointer looks relevant. See ROADMAP.md Phase 11b.
    fn wiki_pointer_lines(&self) -> Vec<String> {
        let wiki_dir = self.root.join(WIKI_DIR);
        let Ok(entries) = std::fs::read_dir(&wiki_dir) else {
            return Vec::new();
        };

        let mut pages: Vec<(String, Option<String>)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .map(|e| {
                let path = e.path();
                let relative = path.strip_prefix(&self.root).unwrap_or(&path).to_path_buf();
                let summary = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|content| wiki_summary(&content));
                (relative.display().to_string(), summary)
            })
            .collect();
        pages.sort();

        if pages.is_empty() {
            return Vec::new();
        }
        let mut lines = vec!["\nWiki pages (read via read_file for full detail):\n".to_string()];
        lines.extend(pages.into_iter().map(|(path, summary)| match summary {
            Some(s) => format!("  {path}: {s}\n"),
            None => format!("  {path}\n"),
        }));
        lines
    }

    /// Walks the repo and returns tags for every parseable file, re-parsing
    /// only files whose `(mtime, size)` changed since the cached entry.
    fn collect_tags(&self) -> Vec<(PathBuf, FileTags)> {
        let mut extractor = Extractor::new();
        let mut cache = self.cache.lock().unwrap();
        let mut results = Vec::new();
        let mut seen: Vec<PathBuf> = Vec::new();

        for entry in ignore::WalkBuilder::new(&self.root).build() {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
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
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

            let path = path.to_path_buf();
            seen.push(path.clone());
            let fresh = cache
                .get(&path)
                .is_some_and(|c| c.mtime == mtime && c.size == meta.len());
            if !fresh {
                let Ok(source) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let tags = extractor.extract(&source, ext);
                cache.insert(
                    path.clone(),
                    CacheEntry {
                        mtime,
                        size: meta.len(),
                        tags,
                    },
                );
            }
            if let Some(entry) = cache.get(&path) {
                results.push((path, entry.tags.clone()));
            }
        }

        // Deleted files must leave the cache (and the map).
        cache.retain(|path, _| seen.contains(path));
        results
    }
}

fn is_denied(path: &Path, deny_paths: &[PathBuf]) -> bool {
    // Denied entries are canonicalized at config load; canonicalize the
    // candidate too so a symlinked spelling can't slip past the comparison
    // (same convention as the search tools).
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    deny_paths
        .iter()
        .any(|denied| canonical.starts_with(denied) || path.starts_with(denied))
}

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

    fn extract(&mut self, source: &str, ext: &str) -> FileTags {
        // Find the language config and extract what we need before using self.parser
        let lang_idx = self
            .languages
            .iter()
            .position(|l| l.config.extensions.contains(&ext));
        let Some(lang_idx) = lang_idx else {
            return FileTags::default();
        };

        let lang = &self.languages[lang_idx];
        let def_query = &lang.def_query;
        let ref_query = &lang.ref_query;
        let config = lang.config;

        self.parser
            .set_language(&lang.language)
            .expect("bundled grammar must load");
        let Some(tree) = self.parser.parse(source, None) else {
            return FileTags::default();
        };
        let bytes = source.as_bytes();
        let mut tags = FileTags::default();

        let name_index = def_query
            .capture_index_for_name("name")
            .expect("@name exists");
        let item_index = def_query
            .capture_index_for_name("item")
            .expect("@item exists");

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(def_query, tree.root_node(), bytes);
        while let Some(m) = matches.next() {
            let name = m
                .captures
                .iter()
                .find(|c| c.index == name_index)
                .and_then(|c| c.node.utf8_text(bytes).ok());
            let item = m.captures.iter().find(|c| c.index == item_index);
            if let (Some(name), Some(item)) = (name, item) {
                let sig_node = (config.signature_node)(item.node);
                let signature = signature_line(sig_node.utf8_text(bytes).unwrap_or(""));
                let is_pub = (config.is_pub)(name, &signature);
                tags.defs.push(Def {
                    name: name.to_string(),
                    is_pub,
                    signature,
                });
            }
        }

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(ref_query, tree.root_node(), bytes);
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

/// First line of an item, cleaned for display: cut at the body's opening
/// `{` (which also drops a single-line item's entire body), then strip a
/// trailing `;`.
fn signature_line(item_text: &str) -> String {
    let first = item_text.lines().next().unwrap_or("");
    let head = first.split_once('{').map_or(first, |(head, _)| head);
    head.trim_end().trim_end_matches(';').trim_end().to_string()
}

/// PageRank over the cross-file reference graph: an edge from the
/// referencing file to each *other* file defining that name, weighted by
/// reference count and split across multiple definers. Plain power
/// iteration — the graphs here are a few hundred nodes, not the web.
fn pagerank(files: &[(PathBuf, FileTags)]) -> Vec<f64> {
    let n = files.len();
    if n == 0 {
        return Vec::new();
    }

    // name -> indices of files defining it
    let mut definers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, (_, tags)) in files.iter().enumerate() {
        for def in &tags.defs {
            definers.entry(def.name.as_str()).or_default().push(i);
        }
    }

    // out_edges[src] = (dst, weight)
    let mut out_edges: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    for (src, (_, tags)) in files.iter().enumerate() {
        for (name, count) in &tags.refs {
            let Some(dsts) = definers.get(name.as_str()) else {
                continue;
            };
            let external: Vec<usize> = dsts.iter().copied().filter(|&d| d != src).collect();
            if external.is_empty() {
                continue;
            }
            let weight = f64::from(*count) / external.len() as f64;
            for dst in external {
                out_edges[src].push((dst, weight));
            }
        }
    }
    let out_totals: Vec<f64> = out_edges
        .iter()
        .map(|edges| edges.iter().map(|(_, w)| w).sum::<f64>())
        .collect();

    let mut ranks = vec![1.0 / n as f64; n];
    for _ in 0..PAGERANK_ITERATIONS {
        let mut next = vec![(1.0 - PAGERANK_DAMPING) / n as f64; n];
        for src in 0..n {
            if out_totals[src] == 0.0 {
                // Dangling node: its rank redistributes uniformly.
                for rank in next.iter_mut() {
                    *rank += PAGERANK_DAMPING * ranks[src] / n as f64;
                }
                continue;
            }
            for &(dst, weight) in &out_edges[src] {
                next[dst] += PAGERANK_DAMPING * ranks[src] * weight / out_totals[src];
            }
        }
        ranks = next;
    }
    ranks
}

fn render_file(path: &Path, tags: &FileTags) -> String {
    let mut out = format!("\n{}:\n", path.display());
    // Public items first (they're what other files can actually use), each
    // group in source order.
    let (pubs, privs): (Vec<&Def>, Vec<&Def>) = tags.defs.iter().partition(|d| d.is_pub);
    for (shown, def) in pubs.iter().chain(privs.iter()).enumerate() {
        if shown == MAX_SIGNATURES_PER_FILE {
            out.push_str("  ...\n");
            break;
        }
        out.push_str("  ");
        out.push_str(&def.signature);
        out.push('\n');
    }
    out
}

/// Extracts just the `summary:` line from a `---`-delimited frontmatter
/// block, if present. Deliberately lenient — not a real YAML parser, just
/// enough structure-scanning for the one field this crate ever needs (see
/// `aivyx_tools::wiki`'s independent, more complete parser for the format
/// this reads; duplicated here rather than shared, per this crate's
/// dependency-free constraint).
fn wiki_summary(content: &str) -> Option<String> {
    let after_open = content.strip_prefix("---\n")?;
    let block_end = after_open.find("\n---")?;
    let block = &after_open[..block_end];
    block.lines().find_map(|line| {
        line.strip_prefix("summary: ")
            .map(|v| v.trim().trim_matches('"').to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn extracts_definition_kinds_with_signatures() {
        let mut extractor = Extractor::new();
        let tags = extractor.extract(
            r#"
pub struct Widget { size: u32 }
pub enum Mode { A, B }
pub trait Render { fn draw(&self); }
pub fn make_widget(size: u32) -> Widget { Widget { size } }
const LIMIT: usize = 10;
mod helpers;
type Alias = Vec<u8>;
"#,
            "rs",
        );

        let names: Vec<&str> = tags.defs.iter().map(|d| d.name.as_str()).collect();
        for expected in [
            "Widget",
            "Mode",
            "Render",
            "draw",
            "make_widget",
            "LIMIT",
            "helpers",
            "Alias",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        let make = tags.defs.iter().find(|d| d.name == "make_widget").unwrap();
        assert_eq!(make.signature, "pub fn make_widget(size: u32) -> Widget");
        assert!(make.is_pub);
        let limit = tags.defs.iter().find(|d| d.name == "LIMIT").unwrap();
        assert!(!limit.is_pub);
    }

    #[test]
    fn extracts_call_type_and_macro_references() {
        let mut extractor = Extractor::new();
        let tags = extractor.extract(
            r#"
fn caller() {
    let w: Widget = make_widget(3);
    helper::assist();
    w.render();
    println!("done");
}
"#,
            "rs",
        );
        for expected in ["Widget", "make_widget", "assist", "render", "println"] {
            assert!(
                tags.refs.contains_key(expected),
                "missing ref {expected}: {:?}",
                tags.refs.keys()
            );
        }
    }

    #[test]
    fn heavily_referenced_files_rank_first() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "core.rs",
            "pub struct Engine;\npub fn start(e: Engine) {}\n",
        );
        write(
            dir.path(),
            "a.rs",
            "fn a() { let e: Engine = todo!(); start(e); }\n",
        );
        write(
            dir.path(),
            "b.rs",
            "fn b() { let e: Engine = todo!(); start(e); }\n",
        );
        write(dir.path(), "lonely.rs", "pub fn unused_helper() {}\n");

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).expect("map should render");

        let core_pos = rendered.find("core.rs").expect("core.rs in map");
        let lonely_pos = rendered.find("lonely.rs").expect("lonely.rs in map");
        assert!(
            core_pos < lonely_pos,
            "referenced file should outrank unreferenced one:\n{rendered}"
        );
        assert!(rendered.contains("pub struct Engine"));
    }

    #[test]
    fn budget_limits_how_many_files_appear() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..30 {
            write(
                dir.path(),
                &format!("file{i:02}.rs"),
                &format!("pub fn function_number_{i:02}_with_a_long_name() {{}}\n"),
            );
        }
        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);

        let big = map.render(10_000).unwrap();
        let small = map.render(60).unwrap();
        assert!(small.len() < big.len());
        assert!(small.matches(".rs:").count() < big.matches(".rs:").count());
    }

    #[test]
    fn no_rust_files_means_no_map() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "readme.md", "# nothing to parse\n");
        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        assert!(map.render(1000).is_none());
    }

    #[test]
    fn denied_and_gitignored_files_stay_out_of_the_map() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        write(dir.path(), ".gitignore", "generated.rs\n");
        write(dir.path(), "visible.rs", "pub fn visible_fn() {}\n");
        write(dir.path(), "generated.rs", "pub fn generated_fn() {}\n");
        write(dir.path(), "secret/hidden.rs", "pub fn secret_fn() {}\n");

        let deny = vec![dir.path().canonicalize().unwrap().join("secret")];
        let map = RepoMap::new(dir.path().to_path_buf(), deny);
        let rendered = map.render(10_000).unwrap();

        assert!(rendered.contains("visible_fn"));
        assert!(
            !rendered.contains("generated_fn"),
            "gitignored leaked:\n{rendered}"
        );
        assert!(
            !rendered.contains("secret_fn"),
            "denied leaked:\n{rendered}"
        );
    }

    #[test]
    fn cache_invalidates_when_a_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "lib.rs", "pub fn before_edit() {}\n");
        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        assert!(map.render(1000).unwrap().contains("before_edit"));

        // A same-length content change with a bumped mtime must re-parse.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(dir.path(), "lib.rs", "pub fn aafter_edit() {}\n");
        let rendered = map.render(1000).unwrap();
        assert!(rendered.contains("aafter_edit"), "stale cache:\n{rendered}");
        assert!(!rendered.contains("before_edit"));
    }

    #[test]
    fn deleted_files_leave_the_map() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "keep.rs", "pub fn keep_fn() {}\n");
        write(dir.path(), "gone.rs", "pub fn gone_fn() {}\n");
        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        assert!(map.render(1000).unwrap().contains("gone_fn"));

        std::fs::remove_file(dir.path().join("gone.rs")).unwrap();
        let rendered = map.render(1000).unwrap();
        assert!(!rendered.contains("gone_fn"));
        assert!(rendered.contains("keep_fn"));
    }

    #[test]
    fn zero_budget_renders_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "lib.rs", "pub fn some_fn() {}\n");
        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        assert!(map.render(0).is_none());
    }

    #[test]
    fn render_includes_a_wiki_pointer_section_with_summaries() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src.rs", "pub fn something() {}\n");
        write(
            dir.path(),
            "docs/wiki/aivyx-core.md",
            "---\ngenerated_at_commit: abc\nsummary: \"Turn loop and orchestration.\"\n---\nBody.\n",
        );

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).unwrap();

        assert!(rendered.contains("docs/wiki/aivyx-core.md"));
        assert!(rendered.contains("Turn loop and orchestration."));
    }

    #[test]
    fn render_wiki_pointer_falls_back_when_a_page_has_no_summary() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src.rs", "pub fn something() {}\n");
        write(dir.path(), "docs/wiki/plain.md", "no frontmatter here\n");

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).unwrap();

        assert!(rendered.contains("docs/wiki/plain.md"));
    }

    #[test]
    fn render_wiki_pointer_section_is_absent_with_no_wiki_pages() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src.rs", "pub fn something() {}\n");

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        let rendered = map.render(10_000).unwrap();

        assert!(!rendered.contains("Wiki pages"));
    }

    #[test]
    fn render_does_not_panic_with_a_tiny_budget_and_wiki_pages_present() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src.rs", "pub fn something() {}\n");
        write(
            dir.path(),
            "docs/wiki/aivyx-core.md",
            "---\nsummary: \"Should not fit in a tiny budget.\"\n---\nBody.\n",
        );

        let map = RepoMap::new(dir.path().to_path_buf(), vec![]);
        // A 1-token budget (4 chars) is smaller than the header alone, so
        // `render` returns `None` here (matches existing behavior for the
        // file-listing section — not asserted as a hardcoded byte count,
        // just confirmed not to panic now that the wiki section shares the
        // same budget check). `Some(...)` would also be an acceptable
        // outcome if the budget math ever changes; the only real assertion
        // is "doesn't panic and stays within budget if it returns Some."
        if let Some(rendered) = map.render(1) {
            assert!(rendered.chars().count() < 200);
        }
    }

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
}
