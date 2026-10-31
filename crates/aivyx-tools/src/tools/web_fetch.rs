use std::path::Path;
use std::time::Duration;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::web::resolve_and_check;
use crate::{Tool, ToolError, ToolExecutionContext};

/// Head-truncated (not tail) — an article's useful content is at the top,
/// unlike command output, matching `grep`/`glob`'s own truncation
/// convention rather than `run_command`/`run_shell`'s tail-truncation.
const MAX_FETCH_OUTPUT_BYTES: usize = 50 * 1024;

/// How much of a response body is read at all. The model sees 50 KiB of the
/// converted text, so a few MiB of markup is plenty; the rest of a larger
/// body is never pulled into memory.
const MAX_FETCH_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The longest run of non-whitespace characters handed to the HTML
/// converter. `html2text` is quadratic in an unbreakable run (a base64
/// blob, a minified bundle), so longer runs are broken up first.
const MAX_UNBROKEN_RUN: usize = 4096;

/// Deepest element nesting handed to the HTML converter. `html2text` recurses
/// per level (a few thousand `<ul><li>` overflow a thread's stack) and slows
/// down on deep trees; a page nested deeper than this is reduced to its text
/// with the tags stripped instead.
const MAX_NESTING_DEPTH: usize = 200;

#[derive(Deserialize, JsonSchema)]
struct WebFetchArgs {
    /// The URL to fetch, including scheme (e.g. "https://example.com/page").
    url: String,
}

/// Fetches a URL and returns its readable text content. The agent's first
/// network-reaching tool — see the 2026-07-15 web-tools design doc for why
/// "local-only" describes the LLM backend, not network isolation.
pub struct WebFetchTool {
    timeout: Duration,
    allow_private_targets: bool,
}

impl WebFetchTool {
    pub fn new(timeout_secs: u64, allow_private_targets: bool) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
            allow_private_targets,
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    // No `mutates_outside_session` override: the trait's fail-closed
    // default (`true`) is correct here. Before the 2026-09-16 security
    // audit fix (Task 1), this returned `false`, offering web_fetch to the
    // model even in plan mode -- the same "network isn't a session-local
    // read" bug that `ActionKind::Network` fixed on the `ConfirmationGate`
    // side. This is the type-level ("static") counterpart the trait's own
    // doc comment names: "filesystem, processes, network."

    // `needs_checkpoint` IS overridden, though, to `false`: a network read
    // cannot mutate the worktree, so there is nothing here for a
    // checkpoint to protect. Left at the trait default (which just mirrors
    // `mutates_outside_session`), the Task 1 fix above would silently opt
    // every web_fetch call into taking a git checkpoint too — wasted work,
    // and worse, a web_fetch that runs before a real edit in the same
    // batch could "mint" that edit's checkpoint (GitCheckpointer dedups by
    // tree hash) and get misattributed as the edit in a later
    // batch-rollback notice. See `Tool::needs_checkpoint`'s doc comment.
    fn needs_checkpoint(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Fetch a URL and return its readable text content (HTML is converted \
                to plain text, scripts/styles stripped). Use this to read documentation, error \
                message explanations, or API references. Output is capped and head-truncated \
                for very long pages."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(WebFetchArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: WebFetchArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Network,
            target: PermissionTarget::Other(args.url.clone()),
            arguments_preview: json!({ "url": args.url }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WebFetchArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        let url = reqwest::Url::parse(&args.url)
            .map_err(|err| ToolError::InvalidArguments(format!("invalid URL: {err}")))?;

        // The checked addresses, pinned for the connection below so a
        // second DNS lookup can't hand back a different (private) one.
        let pinned = if self.allow_private_targets {
            None
        } else {
            Some(resolve_and_check(&url).await?)
        };

        // Redirects must not be auto-followed: `resolve_and_check` only
        // validated the *initial* URL above, and reqwest's default client
        // follows up to 10 redirects with no re-check — a public URL that
        // 302s to e.g. http://169.254.169.254/ or http://127.0.0.1/ would
        // sail through untouched, defeating the entire SSRF pre-flight
        // check for a tool with no human confirmation gate. Instead, a 3xx
        // response is surfaced as an error naming the redirect target so
        // the caller can issue a fresh top-level `web_fetch` call for it —
        // which naturally re-runs `resolve_and_check` against that exact
        // URL, keeping "every URL ever connected to has gone through
        // resolve_and_check exactly once" trivially true.
        let mut builder = reqwest::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none());
        if let (Some(addrs), Some(host)) = (&pinned, url.host_str()) {
            builder = builder.resolve_to_addrs(host, addrs);
        }
        let client = builder
            .build()
            .map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to build HTTP client: {err}"))
            })?;

        let fetch = async {
            let response = client
                .get(url)
                .send()
                .await
                .map_err(|err| ToolError::ExecutionFailed(format!("request failed: {err}")))?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("<no Location header>");
                return Err(ToolError::ExecutionFailed(format!(
                    "request to {} redirected ({}) to {location} — call web_fetch again with \
                     that URL if you want to follow it",
                    response.url(),
                    response.status()
                )));
            }
            if !response.status().is_success() {
                return Err(ToolError::ExecutionFailed(format!(
                    "request failed with status {}",
                    response.status()
                )));
            }
            // Read chunk by chunk and stop at the cap: dropping the response
            // closes the connection, so a huge body is not pulled in.
            let mut response = response;
            let mut bytes: Vec<u8> = Vec::new();
            let mut cut = false;
            while let Some(chunk) = response.chunk().await.map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to read response body: {err}"))
            })? {
                let room = MAX_FETCH_BODY_BYTES - bytes.len();
                if chunk.len() > room {
                    bytes.extend_from_slice(&chunk[..room]);
                    cut = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok((String::from_utf8_lossy(&bytes).into_owned(), cut))
        };

        let (body, cut) = tokio::select! {
            result = fetch => result?,
            _ = ctx.cancellation.cancelled() => {
                return Err(ToolError::ExecutionFailed("fetch was cancelled".to_string()));
            }
        };

        // The conversion is CPU work: off the async worker, and abandoned
        // (not waited for) when the call is cancelled.
        let text = tokio::select! {
            text = convert_html(body) => text,
            _ = ctx.cancellation.cancelled() => {
                return Err(ToolError::ExecutionFailed("fetch was cancelled".to_string()));
            }
        };
        let mut out = head_truncate(&text);
        if cut {
            out.push_str(&format!(
                "\n\n[... the page is larger than {} MiB; the rest was not read ...]",
                MAX_FETCH_BODY_BYTES / (1024 * 1024)
            ));
        }
        Ok(ToolOutput::Ok(out))
    }
}

fn html_to_text(html: &str) -> String {
    // html2text 0.6.0's `from_read` returns `String` directly (not
    // `Result<String, _>` as the task brief's starting draft assumed —
    // see task-3-report.md for the verified signature), so there's no
    // fallible path here to `unwrap_or_else` against.
    let html = break_long_runs(html);
    if nesting_depth(&html) > MAX_NESTING_DEPTH {
        return strip_tags(&html);
    }
    html2text::from_read(html.as_bytes(), 100)
}

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source",
    "track", "wbr",
];

/// Calls `f(tag_text)` for each `<...>` tag in `html`, in order.
fn for_each_tag(html: &str, mut f: impl FnMut(&str)) {
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else { break };
        f(&after[..end]);
        rest = &after[end + 1..];
    }
}

/// The deepest element nesting in `html`, counted from its tags with a
/// stack of open element names: an end tag closes the nearest open element of
/// that name (and what is inside it), and an end tag that matches nothing
/// closes nothing, as the parser ignores it. Void and self-closing elements
/// and comments do not nest. It may overestimate, never underestimate.
fn nesting_depth(html: &str) -> usize {
    let mut open: Vec<String> = Vec::new();
    let mut deepest = 0usize;
    for_each_tag(html, |tag| {
        if tag.starts_with('!') || tag.starts_with('?') || tag.ends_with('/') {
            return;
        }
        if let Some(closing) = tag.strip_prefix('/') {
            let name = closing.trim().to_ascii_lowercase();
            if let Some(at) = open.iter().rposition(|open| *open == name) {
                open.truncate(at);
            }
            return;
        }
        let name = tag
            .split(|c: char| c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if VOID_ELEMENTS.contains(&name.as_str()) {
            return;
        }
        open.push(name);
        deepest = deepest.max(open.len());
    });
    deepest
}

/// How long the HTML conversion may run before the plain-text fallback is
/// used instead.
const CONVERT_DEADLINE: Duration = Duration::from_secs(10);

/// Stack for the conversion thread: html2text recurses per nesting level.
const CONVERT_STACK_BYTES: usize = 64 * 1024 * 1024;

/// `html_to_text` on a thread of its own with a large stack and a deadline.
/// A panic, a failure to start the thread or a conversion that runs past the
/// deadline gives the page's text with the tags stripped, plus a note.
async fn convert_html(body: String) -> String {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let fallback_source = body.clone();
    let spawned = std::thread::Builder::new()
        .name("web-fetch-html".to_string())
        .stack_size(CONVERT_STACK_BYTES)
        .spawn(move || {
            let _ = tx.send(html_to_text(&body));
        });
    let converted = match spawned {
        Ok(_) => tokio::time::timeout(CONVERT_DEADLINE, rx).await.ok().and_then(Result::ok),
        Err(_) => None,
    };
    match converted {
        Some(text) => text,
        None => format!(
            "[the page could not be converted to readable text; showing its text with the \
             tags stripped]\n\n{}",
            strip_tags(&fallback_source)
        ),
    }
}

/// `html` with every tag removed and runs of blank space folded.
fn strip_tags(html: &str) -> String {
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

/// Inserts a space into any run of non-whitespace longer than
/// `MAX_UNBROKEN_RUN` characters, so the converter's cost stays linear in
/// the page size.
fn break_long_runs(html: &str) -> std::borrow::Cow<'_, str> {
    let mut run = 0usize;
    let mut longest = 0usize;
    for c in html.chars() {
        if c.is_whitespace() {
            run = 0;
        } else {
            run += 1;
            longest = longest.max(run);
        }
    }
    if longest <= MAX_UNBROKEN_RUN {
        return std::borrow::Cow::Borrowed(html);
    }
    let mut out = String::with_capacity(html.len() + html.len() / MAX_UNBROKEN_RUN);
    run = 0;
    for c in html.chars() {
        if c.is_whitespace() {
            run = 0;
        } else {
            if run == MAX_UNBROKEN_RUN {
                out.push(' ');
                run = 0;
            }
            run += 1;
        }
        out.push(c);
    }
    std::borrow::Cow::Owned(out)
}

fn head_truncate(text: &str) -> String {
    if text.len() <= MAX_FETCH_OUTPUT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_FETCH_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n[... {} more bytes truncated ...]",
        &text[..end],
        text.len() - end
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::test_support::spawn_mock_http_server;

    fn ctx(cancellation: tokio_util::sync::CancellationToken) -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation,
        }
    }

    #[test]
    fn html_to_text_strips_tags() {
        let html = "<html><body><h1>Title</h1><p>Some content here.</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Some content here."));
        assert!(!text.contains("<h1>"));
        assert!(!text.contains("<p>"));
    }

    #[test]
    fn head_truncate_leaves_short_text_untouched() {
        let text = "short text";
        assert_eq!(head_truncate(text), text);
    }

    #[test]
    fn head_truncate_cuts_long_text_and_notes_how_much() {
        let text = "x".repeat(MAX_FETCH_OUTPUT_BYTES + 500);
        let result = head_truncate(&text);
        assert!(result.contains("truncated"));
        assert!(result.contains("500 more bytes"));
        assert!(result.starts_with(&"x".repeat(100)));
    }

    #[test]
    fn head_truncate_does_not_split_a_multibyte_character_at_the_boundary() {
        // Pad with ASCII up to one byte short of the cap, then place a
        // 3-byte UTF-8 character (€, U+20AC) straddling the boundary — a
        // naive `&text[..MAX_FETCH_OUTPUT_BYTES]` byte-slice would panic
        // here (not a char boundary); the char-boundary-safe version must
        // back up instead.
        let mut text = "a".repeat(MAX_FETCH_OUTPUT_BYTES - 1);
        text.push('€');
        text.push_str("more text after");
        let result = head_truncate(&text); // must not panic
        assert!(result.contains("truncated"));
    }

    #[tokio::test]
    async fn execute_returns_the_fetched_pages_text() {
        let body = "<html><body><p>hello from the mock server</p></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let addr = spawn_mock_http_server(Box::leak(response.into_boxed_str())).await;

        // allow_private_targets: true — the mock server binds to
        // 127.0.0.1, which the SSRF check would otherwise correctly
        // refuse; this test is about execute()'s fetch/parse/truncate
        // behavior, not the SSRF check itself (Task 2 already covers that
        // in isolation).
        let tool = WebFetchTool::new(5, true);
        let args = json!({ "url": format!("http://{addr}/") });
        let output = tool
            .execute(args, &ctx(tokio_util::sync::CancellationToken::new()))
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("hello from the mock server"));
    }

    #[tokio::test]
    async fn execute_refuses_a_private_target_when_not_allowed() {
        let tool = WebFetchTool::new(5, false);
        let args = json!({ "url": "http://127.0.0.1:9/" });
        let err = tool
            .execute(args, &ctx(tokio_util::sync::CancellationToken::new()))
            .await
            .unwrap_err();
        let ToolError::ExecutionFailed(msg) = err else {
            panic!("expected ExecutionFailed")
        };
        assert!(msg.contains("private"));
    }

    #[tokio::test]
    async fn execute_refuses_to_follow_a_redirect_and_names_the_target() {
        // resolve_and_check only validates the *initial* URL; if the
        // client auto-followed redirects, a public-looking URL that 302s
        // to a private/local target would bypass the SSRF check entirely.
        // Assert the redirect is surfaced as an error naming the target
        // instead of being silently followed.
        let target = "http://169.254.169.254/latest/meta-data/";
        let response = format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\r\n");
        let addr = spawn_mock_http_server(Box::leak(response.into_boxed_str())).await;

        let tool = WebFetchTool::new(5, true);
        let args = json!({ "url": format!("http://{addr}/") });
        let err = tool
            .execute(args, &ctx(tokio_util::sync::CancellationToken::new()))
            .await
            .unwrap_err();
        let ToolError::ExecutionFailed(msg) = err else {
            panic!("expected ExecutionFailed")
        };
        assert!(
            msg.contains(target),
            "expected error to name the redirect target {target:?}, got: {msg}"
        );
    }

    #[test]
    fn needs_checkpoint_is_false_while_mutates_outside_session_stays_true() {
        // The split from Task 1's review fix: plan-mode filtering
        // (`mutates_outside_session`) must still treat web_fetch as
        // mutating (network isn't session-local), but checkpointing
        // (`needs_checkpoint`) must not, since a network read cannot
        // change the worktree.
        let tool = WebFetchTool::new(5, false);
        assert!(
            tool.mutates_outside_session(),
            "web_fetch must stay hidden from plan mode"
        );
        assert!(
            !tool.needs_checkpoint(),
            "web_fetch must not trigger a git checkpoint"
        );
    }

    #[test]
    fn permission_request_is_network_tier_with_no_confirmation_needed() {
        let tool = WebFetchTool::new(5, false);
        let request = tool
            .permission_request(&json!({"url": "https://example.com"}), Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Network);
        assert_eq!(
            request.target,
            PermissionTarget::Other("https://example.com".to_string())
        );
    }
}

#[cfg(test)]
mod audit_probes {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A body of exactly the cap is complete: no "rest was not read" note.
    #[tokio::test]
    async fn a_body_of_exactly_the_cap_is_not_marked_as_cut() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n")
                .await;
            let body = "word ".repeat(MAX_FETCH_BODY_BYTES / 5 + 1).into_bytes();
            let _ = stream.write_all(&body[..MAX_FETCH_BODY_BYTES]).await;
            let _ = stream.shutdown().await;
        });
        let tool = WebFetchTool::new(30, true);
        let ctx = ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };
        let out = tool
            .execute(serde_json::json!({ "url": format!("http://{addr}/") }), &ctx)
            .await
            .unwrap();
        let aivyx_types::ToolOutput::Ok(text) = out else { panic!("not ok") };
        assert!(!text.contains("the rest was not read"), "{}", &text[text.len().saturating_sub(200)..]);
    }

    /// C4: the body is read whole (`response.text()`) before the output is
    /// truncated, so a page of any size is held in memory. A well-behaved
    /// reader stops after a bounded prefix; this server counts what it got to
    /// write before the client let go.
    #[tokio::test]
    async fn audit_c4_web_fetch_stops_reading_an_enormous_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sent_in_server = sent.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n")
                .await;
            let chunk = "word ".repeat((1 << 20) / 5).into_bytes();
            for _ in 0..64 {
                if stream.write_all(&chunk).await.is_err() {
                    break;
                }
                sent_in_server.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let _ = stream.shutdown().await;
        });
        let tool = WebFetchTool::new(30, true);
        let ctx = ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };
        let _ = tool
            .execute(serde_json::json!({ "url": format!("http://{addr}/") }), &ctx)
            .await;
        let mib = sent.load(std::sync::atomic::Ordering::SeqCst);
        // Loopback socket buffers let the server write a few MiB past what the
        // client has read, so the bound is generous: far below the 64 MiB body.
        assert!(mib < 16, "web_fetch pulled {mib} MiB of a 64 MiB body into memory");
    }
}

#[cfg(test)]
mod audit_probes_cpu {
    /// C4b: `html_to_text` is quadratic in the length of an unbreakable run
    /// (a base64 blob, minified JSON): 64 KiB takes 0.2 s, 512 KiB 12 s (debug
    /// build). It runs inline in the async `execute`, so neither the fetch
    /// timeout nor Ctrl+C can interrupt it.
    #[test]
    fn audit_c4c_deeply_nested_markup_is_flattened_not_recursed() {
        for unit in ["<div>", "<ul><li>", "<ul><li></b></b>", "<div></x>"] {
            let page = format!("{}deep text", unit.repeat(3000));
            let start = std::time::Instant::now();
            let text = super::html_to_text(&page);
            assert!(text.contains("deep text"), "{text:?}");
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
        }
        // Ordinary nesting still goes through the converter.
        let text = super::html_to_text("<ul><li>one</li><li>two</li></ul>");
        assert!(text.contains("one") && text.contains("two"));
    }

    #[test]
    fn audit_c4b_html_to_text_is_not_quadratic_on_a_long_unbroken_run() {
        let page = "a".repeat(256 << 10);
        let start = std::time::Instant::now();
        let _ = super::html_to_text(&page);
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(1), "256 KiB took {took:?}");
    }
}
