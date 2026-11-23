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
    if aivyx_route::url_is_local(url) {
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

/// The blocking detector: proves an async path runs its local-only check
/// off the runtime thread. On a `current_thread` runtime a ticker task
/// can't advance while that thread is blocked, so a path that ticks it
/// during a [`SlowLan`] lookup did the lookup on the blocking pool.
#[cfg(test)]
pub(crate) mod detector {
    use std::future::Future;
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::Resolve;

    /// A name for [`SlowLan`]: not local by its spelling, so it is looked up.
    pub(crate) const URL: &str = "http://gpu.example.com:8090/v1";

    /// Answers `192.168.1.5` after a 300 ms (blocking) sleep.
    pub(crate) struct SlowLan;

    impl Resolve for SlowLan {
        fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            std::thread::sleep(Duration::from_millis(300));
            Ok(vec!["192.168.1.5".parse().unwrap()])
        }
    }

    /// Awaits `call` beside a ticker that counts every 20 ms; returns the
    /// call's output and how often the ticker advanced meanwhile.
    pub(crate) async fn ticks_during<F: Future>(call: F) -> (F::Output, u32) {
        let ticks = Arc::new(AtomicU32::new(0));
        let ticker = tokio::spawn({
            let ticks = Arc::clone(&ticks);
            async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    ticks.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        // Let the ticker start before the call.
        tokio::task::yield_now().await;
        let start = ticks.load(Ordering::SeqCst);
        let output = call.await;
        let advanced = ticks.load(Ordering::SeqCst) - start;
        ticker.abort();
        (output, advanced)
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

    #[tokio::test(flavor = "current_thread")]
    async fn the_lookup_runs_off_the_runtime_thread() {
        use super::detector::{SlowLan, URL, ticks_during};
        let (result, ticks) = ticks_during(require_local_with(URL, Arc::new(SlowLan))).await;
        assert!(result.is_ok());
        assert!(ticks >= 5, "the ticker advanced {ticks} times during the lookup");
    }

    #[tokio::test]
    async fn a_cloud_host_is_refused_without_a_lookup() {
        let err = require_local_with("https://api.openai.com/v1", Arc::new(NeverResolves))
            .await
            .unwrap_err();
        assert_eq!(err.reason, NotLocalReason::CloudHost);
    }
}
