//! aivyx-coder only talks to local models: its model-server clients must
//! never go through `HTTP_PROXY`/`ALL_PROXY`, which reqwest otherwise
//! applies even to loopback addresses (aivyx-vision audit 2026-11-11, X1).
//!
//! Its own test binary: it sets process-wide proxy variables.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn model_server_requests_ignore_proxy_environment_variables() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    // SAFETY: this binary runs this one test; nothing else reads the
    // environment concurrently.
    unsafe {
        for var in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            std::env::set_var(var, &proxy_url);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }
    let proxied = tokio::spawn(async move {
        let (mut socket, _) = proxy.accept().await.unwrap();
        let mut buf = [0u8; 512];
        let n = socket.read(&mut buf).await.unwrap_or(0);
        let _ = socket
            .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n")
            .await;
        String::from_utf8_lossy(&buf[..n]).into_owned()
    });

    // Nothing listens on port 9, so direct requests fail to connect; a
    // proxied one would reach the listener above instead.
    let listed =
        aivyx_llm::list_models::list_openai_compatible_models("http://127.0.0.1:9/v1").await;
    assert!(listed.is_err(), "nothing listens on the target port");
    let verified = aivyx_llm::verify_model_responds("http://127.0.0.1:9/v1", "m").await;
    assert!(verified.is_err(), "nothing listens on the target port");

    let seen = tokio::time::timeout(Duration::from_millis(300), proxied).await;
    assert!(
        seen.is_err(),
        "a model-server request went through the proxy: {:?}",
        seen.ok().and_then(Result::ok)
    );
}
