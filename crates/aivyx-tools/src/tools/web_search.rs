use std::path::Path;
use std::time::Duration;

use aivyx_sandbox::{ActionKind, PermissionRequest, PermissionTarget};
use aivyx_types::{ToolDefinition, ToolOutput};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{Tool, ToolError, ToolExecutionContext};

#[derive(Deserialize, JsonSchema)]
struct WebSearchArgs {
    /// The search query.
    query: String,
}

#[derive(Debug, Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Debug, Deserialize)]
struct SearxngResult {
    title: String,
    url: String,
    #[serde(default)]
    content: String,
}

/// Queries a configured SearXNG instance. The agent's second
/// network-reaching tool — see `WebFetchTool`'s doc comment and the
/// 2026-07-15 web-tools design doc for the "local-only means the LLM
/// backend, not network isolation" reasoning this and `web_fetch` share.
pub struct WebSearchTool {
    search_base_url: Option<String>,
    max_results: u32,
    timeout: Duration,
}

impl WebSearchTool {
    pub fn new(search_base_url: Option<String>, max_results: u32, timeout_secs: u64) -> Self {
        Self {
            search_base_url,
            max_results,
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn mutates_outside_session(&self) -> bool {
        false
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: "Search the web via a configured SearXNG instance and return ranked \
                results (title, URL, snippet). Use web_fetch on a result's URL to read the full \
                page. Returns a message explaining how to configure it if no SearXNG instance is \
                set."
                .to_string(),
            parameters_schema: serde_json::Value::from(schemars::schema_for!(WebSearchArgs)),
        }
    }

    fn permission_request(
        &self,
        arguments: &serde_json::Value,
        _cwd: &Path,
    ) -> Result<PermissionRequest, ToolError> {
        let args: WebSearchArgs = serde_json::from_value(arguments.clone())
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;
        Ok(PermissionRequest {
            tool_name: self.name().to_string(),
            action: ActionKind::Read,
            target: PermissionTarget::Other(args.query.clone()),
            arguments_preview: json!({ "query": args.query }),
            preview: None,
            diff: None,
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WebSearchArgs = serde_json::from_value(arguments)
            .map_err(|err| ToolError::InvalidArguments(err.to_string()))?;

        let Some(base_url) = &self.search_base_url else {
            return Ok(ToolOutput::Ok(
                "web_search is not configured — add [web] search_base_url pointing at a \
                 SearXNG instance (e.g. a public instance, or one you self-host) to config.toml \
                 to enable it."
                    .to_string(),
            ));
        };

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to build HTTP client: {err}"))
            })?;
        let url = format!("{base_url}/search");

        let search = async {
            let response = client
                .get(&url)
                .query(&[("q", args.query.as_str()), ("format", "json")])
                .send()
                .await
                .map_err(|err| {
                    ToolError::ExecutionFailed(format!("search request failed: {err}"))
                })?;
            if !response.status().is_success() {
                return Err(ToolError::ExecutionFailed(format!(
                    "search request failed with status {}",
                    response.status()
                )));
            }
            response.json::<SearxngResponse>().await.map_err(|err| {
                ToolError::ExecutionFailed(format!("failed to parse search response: {err}"))
            })
        };

        let parsed = tokio::select! {
            result = search => result?,
            _ = ctx.cancellation.cancelled() => {
                return Err(ToolError::ExecutionFailed("search was cancelled".to_string()));
            }
        };

        Ok(ToolOutput::Ok(format_results(
            &parsed.results,
            self.max_results,
        )))
    }
}

fn format_results(results: &[SearxngResult], max_results: u32) -> String {
    if results.is_empty() {
        return "no results found".to_string();
    }
    results
        .iter()
        .take(max_results as usize)
        .map(|r| format!("{} | {} | {}", r.title, r.url, r.content))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::test_support::spawn_mock_http_server;

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            cwd: std::env::temp_dir(),
            confiner: std::sync::Arc::new(aivyx_sandbox::NoopConfiner),
            cancellation: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn format_results_joins_multiple_results_one_per_line() {
        let results = vec![
            SearxngResult {
                title: "First".to_string(),
                url: "https://a.example".to_string(),
                content: "first snippet".to_string(),
            },
            SearxngResult {
                title: "Second".to_string(),
                url: "https://b.example".to_string(),
                content: "second snippet".to_string(),
            },
        ];
        let formatted = format_results(&results, 10);
        assert_eq!(
            formatted,
            "First | https://a.example | first snippet\nSecond | https://b.example | second snippet"
        );
    }

    #[test]
    fn format_results_reports_no_results_found_instead_of_empty_string() {
        assert_eq!(format_results(&[], 10), "no results found");
    }

    #[test]
    fn format_results_respects_max_results() {
        let results = vec![
            SearxngResult {
                title: "A".to_string(),
                url: "u1".to_string(),
                content: String::new(),
            },
            SearxngResult {
                title: "B".to_string(),
                url: "u2".to_string(),
                content: String::new(),
            },
            SearxngResult {
                title: "C".to_string(),
                url: "u3".to_string(),
                content: String::new(),
            },
        ];
        let formatted = format_results(&results, 2);
        assert_eq!(formatted.lines().count(), 2);
        assert!(formatted.contains('A'));
        assert!(formatted.contains('B'));
        assert!(!formatted.contains('C'));
    }

    #[tokio::test]
    async fn execute_without_search_base_url_explains_itself() {
        let tool = WebSearchTool::new(None, 10, 5);
        let output = tool
            .execute(json!({"query": "rust async traits"}), &ctx())
            .await
            .unwrap();
        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("not configured"));
        assert!(text.contains("search_base_url"));
    }

    #[tokio::test]
    async fn execute_parses_a_real_searxng_style_response() {
        let body = r#"{"results":[{"title":"Rust async book","url":"https://rust-lang.github.io/async-book/","content":"An introduction to asynchronous programming in Rust."}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let addr = spawn_mock_http_server(Box::leak(response.into_boxed_str())).await;

        let tool = WebSearchTool::new(Some(format!("http://{addr}")), 10, 5);
        let output = tool
            .execute(json!({"query": "rust async"}), &ctx())
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert!(text.contains("Rust async book"));
        assert!(text.contains("https://rust-lang.github.io/async-book/"));
    }

    #[tokio::test]
    async fn execute_handles_a_zero_results_response() {
        let body = r#"{"results":[]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let addr = spawn_mock_http_server(Box::leak(response.into_boxed_str())).await;

        let tool = WebSearchTool::new(Some(format!("http://{addr}")), 10, 5);
        let output = tool
            .execute(json!({"query": "an extremely obscure query"}), &ctx())
            .await
            .unwrap();

        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert_eq!(text, "no results found");
    }

    #[tokio::test]
    async fn execute_handles_a_malformed_response_shape() {
        let body = r#"{"unexpected": "shape"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let addr = spawn_mock_http_server(Box::leak(response.into_boxed_str())).await;

        let tool = WebSearchTool::new(Some(format!("http://{addr}")), 10, 5);
        let output = tool
            .execute(json!({"query": "test"}), &ctx())
            .await
            .unwrap();

        // `results` is `#[serde(default)]`, so a response missing that key
        // entirely still parses successfully as zero results, not an
        // error — matches SearXNG's own actual JSON shape tolerance.
        let ToolOutput::Ok(text) = output else {
            panic!("expected Ok output")
        };
        assert_eq!(text, "no results found");
    }

    #[test]
    fn permission_request_is_read_tier_with_no_confirmation_needed() {
        let tool = WebSearchTool::new(None, 10, 5);
        let request = tool
            .permission_request(&json!({"query": "test"}), Path::new("."))
            .unwrap();
        assert_eq!(request.action, ActionKind::Read);
    }
}
