//! Shared infrastructure for this crate's network-reaching tools
//! (`web_fetch`/`web_search`): the SSRF pre-flight check both tools that
//! touch arbitrary URLs need, since neither has a per-call human
//! confirmation to catch a bad target the way a mutating tool would.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::ToolError;

/// True if `ip` is loopback, private, link-local, or unspecified — the
/// ranges a same-host or same-LAN service could be reachable on, which a
/// network-reaching tool with no per-call confirmation must not be able
/// to reach unless the user explicitly opts in
/// (`[web] allow_private_targets = true`). Unspecified (`0.0.0.0` /
/// `::`) is included because on Linux `connect()` to it actually reaches
/// localhost — a loopback-equivalent bypass of the loopback check if left
/// unblocked.
///
/// IPv4 uses `std`'s long-stable `Ipv4Addr` predicates directly. IPv6 uses
/// hand-rolled bitmask checks for the unique-local (`fc00::/7`) and
/// link-local (`fe80::/10`) ranges rather than relying on
/// `Ipv6Addr::is_unique_local`/`is_unicast_link_local`, whose standard
/// library stability has moved around across Rust versions — the bitmask
/// checks are correct regardless of what's stable in the toolchain this
/// crate happens to build with. `Ipv6Addr::is_loopback` (`::1`) and
/// `is_unspecified` (`::`) are long-stable and used directly.
pub(crate) fn is_private_or_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_ipv4_private_or_local(v4),
        IpAddr::V6(v6) => is_ipv6_private_or_local(v6),
    }
}

fn is_ipv4_private_or_local(v4: Ipv4Addr) -> bool {
    v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
}

fn is_ipv6_private_or_local(v6: Ipv6Addr) -> bool {
    // ::ffff:0:0/96 - IPv4-mapped addresses. A well-known SSRF-filter-bypass
    // vector: ::ffff:x.y.z.w is equivalent to x.y.z.w on dual-stack systems,
    // so it must be checked against the same rules as a plain IPv4 address
    // rather than waved through because it's syntactically an IPv6 literal.
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_ipv4_private_or_local(v4);
    }
    if v6.is_loopback() || v6.is_unspecified() {
        return true;
    }
    let segments = v6.segments();
    // fc00::/7 - unique local addresses.
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // fe80::/10 - link-local addresses.
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    false
}

/// Resolves `url`'s host and refuses if any resolved address is
/// loopback/private/link-local. Known limitation, accepted as a
/// best-effort mitigation rather than the project's primary security
/// boundary (which remains the sandbox/`ConfirmationGate` — see the
/// AGENTS.md trust-boundary note in README.md for the same distinction
/// applied to a different feature): this resolves DNS itself to check it,
/// then `reqwest` resolves DNS again to actually connect: a DNS answer
/// that changes between the two lookups (e.g. DNS rebinding) could in
/// principle let a checked-safe hostname connect to a different, unchecked
/// address. Closing that gap fully would need a custom `reqwest` resolver
/// pinning the exact checked address for the subsequent connection — out
/// of scope for this pass.
pub(crate) async fn resolve_and_check(url: &reqwest::Url) -> Result<(), ToolError> {
    let host = url
        .host_str()
        .ok_or_else(|| ToolError::InvalidArguments("URL has no host".to_string()))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|err| ToolError::ExecutionFailed(format!("failed to resolve host {host}: {err}")))?;

    for addr in addrs {
        if is_private_or_local(addr.ip()) {
            return Err(ToolError::ExecutionFailed(format!(
                "refusing to fetch {host}: resolved to {} (a private/local address) — set \
                 [web] allow_private_targets = true to override",
                addr.ip()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spawns a minimal, single-shot HTTP mock server bound to an
    /// OS-assigned ephemeral port on `127.0.0.1`: reads (and discards)
    /// one raw HTTP request, writes back `response` verbatim (a caller
    /// must include the full status line and headers, e.g.
    /// `"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: \
    /// 5\r\n\r\nhello"`), then closes. No HTTP-mocking crate dependency —
    /// this is the only test double this crate's network tools need.
    pub(crate) async fn spawn_mock_http_server(response: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_loopback_is_blocked() {
        assert!(is_private_or_local("127.0.0.1".parse().unwrap()));
        assert!(is_private_or_local("127.255.255.255".parse().unwrap()));
    }

    #[test]
    fn ipv4_private_ranges_are_blocked() {
        assert!(is_private_or_local("10.0.0.1".parse().unwrap()));
        assert!(is_private_or_local("172.16.0.1".parse().unwrap()));
        assert!(is_private_or_local("172.31.255.255".parse().unwrap()));
        assert!(is_private_or_local("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn ipv4_link_local_is_blocked() {
        assert!(is_private_or_local("169.254.1.1".parse().unwrap()));
    }

    #[test]
    fn ipv4_public_address_is_allowed() {
        assert!(!is_private_or_local("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn ipv4_unspecified_is_blocked() {
        // On Linux, connect() to 0.0.0.0 actually reaches localhost — a
        // loopback-equivalent bypass of the loopback check if left
        // unblocked.
        assert!(is_private_or_local("0.0.0.0".parse().unwrap()));
    }

    #[test]
    fn ipv6_loopback_is_blocked() {
        assert!(is_private_or_local("::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_unique_local_is_blocked() {
        assert!(is_private_or_local("fc00::1".parse().unwrap()));
        assert!(is_private_or_local("fdff:ffff::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_link_local_is_blocked() {
        assert!(is_private_or_local("fe80::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_public_address_is_allowed() {
        // 2001:4860:4860::8888 is one of Google's public DNS IPv6 addresses.
        assert!(!is_private_or_local("2001:4860:4860::8888".parse().unwrap()));
    }

    #[test]
    fn ipv6_unspecified_is_blocked() {
        // Same reasoning as the IPv4 0.0.0.0 case: connect() to :: can
        // reach localhost, so it must be blocked like the loopback address.
        assert!(is_private_or_local("::".parse().unwrap()));
    }

    #[tokio::test]
    async fn resolve_and_check_refuses_a_loopback_target() {
        let url = reqwest::Url::parse("http://127.0.0.1:9/").unwrap();
        let err = resolve_and_check(&url).await.unwrap_err();
        let ToolError::ExecutionFailed(msg) = err else {
            panic!("expected ExecutionFailed")
        };
        assert!(msg.contains("127.0.0.1"));
        assert!(msg.contains("allow_private_targets"));
    }

    #[tokio::test]
    async fn resolve_and_check_allows_a_real_mock_servers_loopback_address_when_asked_directly() {
        // resolve_and_check itself has no allow_private_targets bypass —
        // that flag is checked by the *caller* (web_fetch, Task 3) before
        // even calling resolve_and_check. This test just confirms the
        // check correctly identifies our own mock server's address as
        // blocked, proving the function works against a real bound socket
        // rather than only a synthetic IP literal.
        let addr = test_support::spawn_mock_http_server("HTTP/1.1 200 OK\r\n\r\n").await;
        let url = reqwest::Url::parse(&format!("http://{addr}/")).unwrap();
        let err = resolve_and_check(&url).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }

    #[test]
    fn ipv4_mapped_ipv6_addresses_are_checked_as_their_embedded_ipv4_address() {
        // A well-known SSRF-filter-bypass vector: ::ffff:x.y.z.w is
        // equivalent to x.y.z.w on dual-stack systems, so it must be
        // checked against the same rules as a plain IPv4 address, not
        // waved through because it's syntactically an IPv6 literal.
        assert!(is_private_or_local("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_private_or_local("::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_private_or_local("::ffff:192.168.1.1".parse().unwrap()));
        assert!(is_private_or_local("::ffff:169.254.1.1".parse().unwrap()));
        // A public IPv4-mapped address must still be allowed.
        assert!(!is_private_or_local("::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn ipv4_private_range_boundaries_are_respected() {
        // 10.0.0.0/8
        assert!(!is_private_or_local("9.255.255.255".parse().unwrap()));
        assert!(is_private_or_local("10.0.0.0".parse().unwrap()));
        assert!(is_private_or_local("10.255.255.255".parse().unwrap()));
        assert!(!is_private_or_local("11.0.0.0".parse().unwrap()));
        // 192.168.0.0/16
        assert!(!is_private_or_local("192.167.255.255".parse().unwrap()));
        assert!(is_private_or_local("192.168.0.0".parse().unwrap()));
        assert!(is_private_or_local("192.168.255.255".parse().unwrap()));
        assert!(!is_private_or_local("192.169.0.0".parse().unwrap()));
        // 169.254.0.0/16
        assert!(!is_private_or_local("169.253.255.255".parse().unwrap()));
        assert!(is_private_or_local("169.254.0.0".parse().unwrap()));
        assert!(is_private_or_local("169.254.255.255".parse().unwrap()));
        assert!(!is_private_or_local("169.255.0.0".parse().unwrap()));
    }

    #[test]
    fn ipv6_range_boundaries_are_respected() {
        // fc00::/7
        assert!(!is_private_or_local("fbff:ffff::1".parse().unwrap()));
        assert!(is_private_or_local("fc00::".parse().unwrap()));
        assert!(is_private_or_local("fdff:ffff::1".parse().unwrap()));
        assert!(!is_private_or_local("fe00::1".parse().unwrap()));
        // fe80::/10
        assert!(!is_private_or_local("fe7f:ffff::1".parse().unwrap()));
        assert!(is_private_or_local("fe80::1".parse().unwrap()));
        assert!(!is_private_or_local("fec0::1".parse().unwrap()));
    }

    #[tokio::test]
    async fn resolve_and_check_refuses_localhost_via_real_dns_resolution() {
        // Unlike the existing tests (which pass IP literals — parsed
        // directly, never touching the OS resolver), "localhost" forces
        // tokio::net::lookup_host to actually resolve a hostname, proving
        // the resolution code path itself works, not just IP-literal
        // handling.
        let url = reqwest::Url::parse("http://localhost:9/").unwrap();
        let err = resolve_and_check(&url).await.unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed(_)));
    }
}
