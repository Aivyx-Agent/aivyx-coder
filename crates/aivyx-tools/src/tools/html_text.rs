//! Readable text from an HTML page, safe against hostile nesting.
//!
//! `html2text` recurses once per nesting level (a few thousand levels
//! overflow a thread's stack) and slows down on deep trees, and a text-level
//! guess at the depth is fooled by tags inside comments and attributes. So
//! the page is parsed here, with html5ever, into a flat arena (no recursion
//! anywhere, including drop), fed in small chunks so a page that turns out
//! to be hostile is abandoned after a bounded amount of work. A page within
//! `MAX_RENDER_DEPTH` goes to `html2text`; a deeper one is rendered by the
//! iterative walker below; whatever was not parsed comes out with its tags
//! stripped.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, ExpandedName, QualName, ParseOpts, parse_document};

/// Deepest element nesting handed to `html2text`.
const MAX_RENDER_DEPTH: usize = 200;
/// Parsing stops once the tree is this deep: the rest is not parsed.
const ABORT_DEPTH: usize = 1000;
/// Parsing stops after this long (stray end tags make the tree builder scan
/// its whole stack, so work can grow faster than the input).
const PARSE_BUDGET: Duration = Duration::from_millis(500);
/// How much input is fed to the parser between checks.
const CHUNK_BYTES: usize = 8 * 1024;
/// The most input looked at; the rest is dropped with a note.
pub(crate) const MAX_INPUT_BYTES: usize = 2 * 1024 * 1024;

const NONE: usize = usize::MAX;

enum Kind {
    Document,
    Element(String),
    Text(String),
    Other,
}

struct Rec {
    parent: usize,
    kind: Kind,
    children: Vec<usize>,
    depth: usize,
}

/// A node handle: its arena index, and its name for elements (the parser asks
/// for it by reference).
#[derive(Clone)]
struct Handle {
    id: usize,
    name: Option<Rc<QualName>>,
}

struct Arena {
    nodes: Vec<Rec>,
    max_depth: usize,
}

struct Sink(Rc<RefCell<Arena>>);

impl Sink {
    fn new() -> Self {
        let document = Rec { parent: NONE, kind: Kind::Document, children: Vec::new(), depth: 0 };
        Sink(Rc::new(RefCell::new(Arena { nodes: vec![document], max_depth: 0 })))
    }

    fn new_rec(&self, kind: Kind) -> usize {
        let mut arena = self.0.borrow_mut();
        arena.nodes.push(Rec { parent: NONE, kind, children: Vec::new(), depth: 0 });
        arena.nodes.len() - 1
    }

    fn attach(&self, parent: usize, child: usize, at: Option<usize>) {
        let mut arena = self.0.borrow_mut();
        let depth = arena.nodes[parent].depth + 1;
        arena.nodes[child].parent = parent;
        arena.nodes[child].depth = depth;
        arena.max_depth = arena.max_depth.max(depth);
        match at {
            Some(index) => arena.nodes[parent].children.insert(index, child),
            None => arena.nodes[parent].children.push(child),
        }
    }

    fn detach(&self, id: usize) {
        let mut arena = self.0.borrow_mut();
        let parent = arena.nodes[id].parent;
        if parent != NONE {
            arena.nodes[parent].children.retain(|&c| c != id);
            arena.nodes[id].parent = NONE;
        }
    }

    /// Adds `text` at `index` of `parent` (the end when `None`), merging with
    /// an adjacent text node.
    fn add_text(&self, parent: usize, index: Option<usize>, text: &str) {
        {
            let mut arena = self.0.borrow_mut();
            let at = index.unwrap_or(arena.nodes[parent].children.len());
            if at > 0 {
                let previous = arena.nodes[parent].children[at - 1];
                if let Kind::Text(existing) = &mut arena.nodes[previous].kind {
                    existing.push_str(text);
                    return;
                }
            }
        }
        let id = self.new_rec(Kind::Text(text.to_string()));
        self.attach(parent, id, index);
    }

    fn add(&self, parent: usize, index: Option<usize>, node: NodeOrText<Handle>) {
        match node {
            NodeOrText::AppendNode(handle) => {
                self.detach(handle.id);
                self.attach(parent, handle.id, index);
            }
            NodeOrText::AppendText(text) => self.add_text(parent, index, &text),
        }
    }

    fn index_in_parent(&self, id: usize) -> Option<(usize, usize)> {
        let arena = self.0.borrow();
        let parent = arena.nodes[id].parent;
        if parent == NONE {
            return None;
        }
        let index = arena.nodes[parent].children.iter().position(|&c| c == id)?;
        Some((parent, index))
    }
}

impl TreeSink for Sink {
    type Handle = Handle;
    type Output = Sink;

    fn finish(self) -> Sink {
        self
    }
    fn parse_error(&mut self, _msg: std::borrow::Cow<'static, str>) {}
    fn get_document(&mut self) -> Handle {
        Handle { id: 0, name: None }
    }
    fn elem_name<'a>(&'a self, target: &'a Handle) -> ExpandedName<'a> {
        target.name.as_ref().expect("elem_name of an element").expanded()
    }
    fn create_element(&mut self, name: QualName, _attrs: Vec<Attribute>, _flags: ElementFlags) -> Handle {
        let id = self.new_rec(Kind::Element(name.local.to_string().to_ascii_lowercase()));
        Handle { id, name: Some(Rc::new(name)) }
    }
    fn create_comment(&mut self, _text: StrTendril) -> Handle {
        Handle { id: self.new_rec(Kind::Other), name: None }
    }
    fn create_pi(&mut self, _target: StrTendril, _data: StrTendril) -> Handle {
        Handle { id: self.new_rec(Kind::Other), name: None }
    }
    fn append(&mut self, parent: &Handle, child: NodeOrText<Handle>) {
        self.add(parent.id, None, child);
    }
    fn append_based_on_parent_node(
        &mut self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        if self.index_in_parent(element.id).is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }
    fn append_doctype_to_document(&mut self, _name: StrTendril, _public: StrTendril, _system: StrTendril) {}
    fn get_template_contents(&mut self, target: &Handle) -> Handle {
        target.clone()
    }
    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        x.id == y.id
    }
    fn set_quirks_mode(&mut self, _mode: QuirksMode) {}
    fn append_before_sibling(&mut self, sibling: &Handle, new_node: NodeOrText<Handle>) {
        if let Some((parent, index)) = self.index_in_parent(sibling.id) {
            self.add(parent, Some(index), new_node);
        }
    }
    fn add_attrs_if_missing(&mut self, _target: &Handle, _attrs: Vec<Attribute>) {}
    fn remove_from_parent(&mut self, target: &Handle) {
        self.detach(target.id);
    }
    fn reparent_children(&mut self, node: &Handle, new_parent: &Handle) {
        let moved = std::mem::take(&mut self.0.borrow_mut().nodes[node.id].children);
        for child in moved {
            self.0.borrow_mut().nodes[child].parent = NONE;
            self.attach(new_parent.id, child, None);
        }
    }
}

/// The page parsed so far, and the input that was not given to the parser.
struct Parsed<'a> {
    arena: Arena,
    rest: &'a str,
}

fn parse_bounded(html: &str) -> Parsed<'_> {
    let started = Instant::now();
    let sink = Sink::new();
    let shared = Rc::clone(&sink.0);
    let mut parser = parse_document(sink, ParseOpts::default());
    let mut offset = 0;
    while offset < html.len() {
        let mut end = (offset + CHUNK_BYTES).min(html.len());
        while !html.is_char_boundary(end) {
            end += 1;
        }
        parser.process(StrTendril::from_slice(&html[offset..end]));
        offset = end;
        let too_deep = shared.borrow().max_depth > ABORT_DEPTH;
        if too_deep || started.elapsed() > PARSE_BUDGET {
            break;
        }
    }
    drop(parser.finish());
    let arena = Rc::try_unwrap(shared).ok().expect("the parser is finished").into_inner();
    Parsed { arena, rest: &html[offset..] }
}

/// The deepest element nesting of the tree, computed without recursion.
fn real_depth(arena: &Arena) -> usize {
    let mut deepest = 0;
    let mut stack = vec![(0usize, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        deepest = deepest.max(depth);
        for &child in &arena.nodes[id].children {
            stack.push((child, depth + 1));
        }
    }
    deepest
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "address" | "article" | "aside" | "blockquote" | "body" | "br" | "dd" | "details" | "div"
            | "dl" | "dt" | "fieldset" | "figcaption" | "figure" | "footer" | "form" | "h1" | "h2"
            | "h3" | "h4" | "h5" | "h6" | "header" | "hr" | "html" | "li" | "main" | "nav" | "ol"
            | "p" | "pre" | "section" | "table" | "tr" | "ul"
    )
}

struct TextOut {
    buf: String,
}

impl TextOut {
    fn newline(&mut self) {
        while self.buf.ends_with(' ') {
            self.buf.pop();
        }
        if !self.buf.is_empty() && !self.buf.ends_with('\n') {
            self.buf.push('\n');
        }
    }

    fn text(&mut self, text: &str) {
        let leading = text.starts_with(char::is_whitespace);
        let mut words = text.split_whitespace().peekable();
        if words.peek().is_none() {
            if leading && !self.buf.is_empty() && !self.buf.ends_with([' ', '\n']) {
                self.buf.push(' ');
            }
            return;
        }
        if leading && !self.buf.is_empty() && !self.buf.ends_with([' ', '\n']) {
            self.buf.push(' ');
        }
        let mut first = true;
        for word in words {
            if !first {
                self.buf.push(' ');
            }
            self.buf.push_str(word);
            first = false;
        }
        if text.ends_with(char::is_whitespace) {
            self.buf.push(' ');
        }
    }
}

/// The text of the tree, block elements on lines of their own, walked with an
/// explicit stack.
fn render_iteratively(arena: &Arena) -> String {
    enum Step {
        Enter(usize),
        Exit(usize),
    }
    let mut out = TextOut { buf: String::new() };
    let mut stack = vec![Step::Enter(0)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(id) => match &arena.nodes[id].kind {
                Kind::Text(text) => out.text(text),
                Kind::Other => {}
                Kind::Document => {
                    stack.extend(arena.nodes[id].children.iter().rev().map(|&c| Step::Enter(c)))
                }
                Kind::Element(name) => {
                    if matches!(name.as_str(), "script" | "style" | "head" | "template" | "noscript") {
                        continue;
                    }
                    if is_block(name) {
                        out.newline();
                    }
                    if name == "li" {
                        out.buf.push_str("* ");
                    }
                    stack.push(Step::Exit(id));
                    stack.extend(arena.nodes[id].children.iter().rev().map(|&c| Step::Enter(c)));
                }
            },
            Step::Exit(id) => {
                if let Kind::Element(name) = &arena.nodes[id].kind
                    && is_block(name)
                {
                    out.newline();
                }
            }
        }
    }
    out.newline();
    out.buf
}

/// `html` with every tag removed and runs of blank space folded. Linear.
pub(crate) fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        out.push(' ');
        let after = &rest[start + 1..];
        match after.find('>') {
            Some(end) => rest = &after[end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Readable text for `html`, in time bounded by the input size whatever the
/// nesting; `convert` is `html2text` for pages shallow enough for it.
pub(crate) fn page_text(html: &str, convert: impl FnOnce(&str) -> String) -> String {
    let mut cut = false;
    let mut html = html;
    if html.len() > MAX_INPUT_BYTES {
        let mut end = MAX_INPUT_BYTES;
        while !html.is_char_boundary(end) {
            end -= 1;
        }
        html = &html[..end];
        cut = true;
    }
    let parsed = parse_bounded(html);
    let mut text = if parsed.rest.is_empty() && real_depth(&parsed.arena) <= MAX_RENDER_DEPTH {
        drop(parsed);
        convert(html)
    } else {
        let mut text = render_iteratively(&parsed.arena);
        if !parsed.rest.is_empty() {
            text.push_str(&strip_tags(parsed.rest));
            text.push_str(
                "\n\n[... the page is nested too deeply or is too complex to convert; the rest \
                 is shown with its tags stripped ...]",
            );
        }
        text
    };
    if cut {
        text.push_str("\n\n[... the page is larger than 2 MiB; the rest was not converted ...]");
    }
    text
}
