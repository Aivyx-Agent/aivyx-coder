//! aivyx-coder's model-server clients never follow a redirect: a 307/308
//! from a local server would otherwise re-send the prompt and code to any
//! host it names, past the local-only check.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aivyx_llm::{ChatRequest, LlmBackend, LlmError, OpenAiCompatBackend};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A server that counts the connections it gets and answers each with a
/// 404, standing in for wherever a redirect points.
async fn redirect_target() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });
    (url, hits)
}

/// A model server that answers every request with `status` to `location`.
async fn redirecting_server(status: &'static str, location: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = [0u8; 8192];
            let _ = socket.read(&mut buf).await;
            let reply = format!(
                "HTTP/1.1 {status}\r\nlocation: {location}\r\ncontent-length: 0\r\n\r\n"
            );
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    url
}

#[tokio::test]
async fn chat_does_not_follow_a_redirect() {
    for status in ["307 Temporary Redirect", "308 Permanent Redirect"] {
        let (target, hits) = redirect_target().await;
        let base =
            redirecting_server(status, format!("{target}/v1/chat/completions")).await;
        let backend = OpenAiCompatBackend::new(base, "m", None).unwrap();
        let err = match backend.stream_chat(ChatRequest::new(Vec::new())).await {
            Err(err) => err,
            Ok(_) => panic!("{status}: a redirect is not a reply"),
        };
        assert!(
            matches!(err, LlmError::BackendError { status: 307 | 308, .. }),
            "{status}: {err:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "{status}: the redirect was followed");
    }
}

#[tokio::test]
async fn the_probes_and_model_listing_do_not_follow_a_redirect() {
    let (target, hits) = redirect_target().await;
    let base = redirecting_server("307 Temporary Redirect", format!("{target}/v1/x")).await;

    let verified = aivyx_llm::probe::verify_model_responds(&base, "m").await;
    assert!(verified.unwrap_err().contains("307"));
    let tools = aivyx_llm::probe::probe_tool_calling(&base, "m").await;
    assert!(
        matches!(tools, aivyx_llm::probe::ToolProbe::Unknown(_)),
        "{tools:?}"
    );
    let _ = aivyx_llm::probe::probe_served_context(&base, "m").await;
    assert!(aivyx_llm::list_models::list_ollama_models(&base).await.is_err());
    assert!(
        aivyx_llm::list_models::list_openai_compatible_models(&base)
            .await
            .is_err()
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "a redirect was followed");
}
