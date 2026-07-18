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

    fn mutates_outside_session(&self) -> bool {
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
            action: ActionKind::Read,
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

        if !self.allow_private_targets {
            resolve_and_check(&url).await?;
        }

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
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
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
            let body = response.text().await.map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to read response body: {err}"))
            })?;
            Ok(body)
        };

        let body = tokio::select! {
            result = fetch => result?,
            _ = ctx.cancellation.cancelled() => {
                return Err(ToolError::ExecutionFailed("fetch was cancelled".to_string()));
            }
        };

        let text = html_to_text(&body);
        Ok(ToolOutput::Ok(head_truncate(&text)))
    }
}

fn html_to_text(html: &str) -> String {
    // html2text 0.6.0's `from_read` returns `String` directly (not
    // `Result<String, _>` as the task brief's starting draft assumed —
    // see task-3-report.md for the verified signature), so there's no
    // fallible path here to `unwrap_or_else` against.
    html2text::from_read(html.as_bytes(), 100)
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
    fn permission_request_is_read_tier_with_no_confirmation_needed() {
        let tool = WebFetchTool::new(5, false);
        let request = tool
            .permission_request(&json!({"url": "https://example.com"}), Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Read);
        assert_eq!(
            request.target,
            PermissionTarget::Other("https://example.com".to_string())
        );
    }
}
