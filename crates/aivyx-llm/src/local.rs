//! The local-only guard for async code: aivyx-coder talks only to model
//! servers on this machine or the operator's own network
//! (`aivyx_route::require_local`). The check may resolve a DNS name, which
//! blocks, so it runs on tokio's blocking pool.

use std::sync::Arc;

pub use aivyx_route::{NotLocal, NotLocalReason, Resolve, SystemResolver};

/// [`aivyx_route::require_local`], off the async worker threads.
pub async fn require_local(url: &str) -> Result<(), NotLocal> {
    require_local_with(url, Arc::new(SystemResolver)).await
}

/// [`require_local`] with an explicit resolver (a seam for tests).
pub async fn require_local_with(url: &str, resolver: Arc<dyn Resolve>) -> Result<(), NotLocal> {
    // Local by its spelling (a loopback or private address, `localhost`, a
    // `.local` name, ...): no lookup, so no thread hop.
    if aivyx_route::url_locality(url) == aivyx_route::Locality::Local {
        return Ok(());
    }
    let owned = url.to_owned();
    match tokio::task::spawn_blocking(move || aivyx_route::require_local_with(&owned, &*resolver))
        .await
    {
        Ok(result) => result,
        // The lookup panicked or was cancelled: fail closed, as a name
        // that doesn't resolve.
        Err(_) => aivyx_route::require_local_with(url, &NeverResolves),
    }
}

/// A resolver that never answers.
struct NeverResolves;

impl Resolve for NeverResolves {
    fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<std::net::IpAddr>> {
        Err(std::io::Error::other("lookup did not complete"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    struct Fixed(Vec<IpAddr>);

    impl Resolve for Fixed {
        fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn a_name_is_judged_by_what_it_resolves_to() {
        let lan = Arc::new(Fixed(vec!["192.168.1.20".parse().unwrap()]));
        assert!(require_local_with("http://gpu.example.com:8080/v1", lan).await.is_ok());

        let public = Arc::new(Fixed(vec!["8.8.8.8".parse().unwrap()]));
        let err = require_local_with("http://gpu.example.com:8080/v1", public)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "http://gpu.example.com:8080/v1 is not local (resolves to a public address 8.8.8.8) \
             — aivyx is local-only; point it at a model server on this machine or your own \
             network"
        );
    }

    #[tokio::test]
    async fn a_local_spelling_needs_no_lookup() {
        let never = Arc::new(NeverResolves);
        for url in ["http://127.0.0.1:8090/v1", "http://localhost:11434", "http://10.0.0.5:8080"] {
            assert!(require_local_with(url, never.clone()).await.is_ok(), "{url}");
        }
    }

    #[tokio::test]
    async fn a_cloud_host_is_refused_without_a_lookup() {
        let err = require_local_with("https://api.openai.com/v1", Arc::new(NeverResolves))
            .await
            .unwrap_err();
        assert_eq!(err.reason, NotLocalReason::CloudHost);
    }
}
