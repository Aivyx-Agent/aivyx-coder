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
    let [a, b, ..] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        // 0.0.0.0/8 ("this network"), not just 0.0.0.0.
        || a == 0
        // 100.64.0.0/10: carrier-grade NAT, where Tailscale and similar
        // overlay networks put the machines on them.
        || (a == 100 && (b & 0xc0) == 64)
        || v4.is_broadcast()
        || v4.is_multicast()
        // 198.18.0.0/15: benchmarking networks; 192.0.0.0/24: IETF protocol
        // assignments.
        || (a == 198 && (b & 0xfe) == 18)
        || v4.octets()[..3] == [192, 0, 0]
}

fn is_ipv6_private_or_local(v6: Ipv6Addr) -> bool {
    // ::ffff:0:0/96 - IPv4-mapped addresses. A well-known SSRF-filter-bypass
    // vector: ::ffff:x.y.z.w is equivalent to x.y.z.w on dual-stack systems,
    // so it must be checked against the same rules as a plain IPv4 address
    // rather than waved through because it's syntactically an IPv6 literal.
    if let Some(v4) = v6.to_ipv4_mapped() {
        return is_ipv4_private_or_local(v4);
    }
    if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() {
        return true;
    }
    let segments = v6.segments();
    // Forms that carry an IPv4 address and reach it: IPv4-compatible
    // (::a.b.c.d), NAT64 (64:ff9b::/96 and the local-use 64:ff9b:1::/48)
    // and 6to4 (2002::/16, the address in the next 32 bits). Checked by the
    // IPv4 they embed.
    let v4 = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    if segments[..6] == [0; 6] {
        return is_ipv4_private_or_local(v4(segments[6], segments[7]));
    }
    if (segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0]) || segments[..3] == [0x64, 0xff9b, 1] {
        return is_ipv4_private_or_local(v4(segments[6], segments[7]));
    }
    // 2001:0::/32: Teredo tunnelling.
    if segments[0] == 0x2001 && segments[1] == 0 {
        return true;
    }
    if segments[0] == 0x2002 {
        return is_ipv4_private_or_local(v4(segments[1], segments[2]));
    }
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
/// loopback/private/link-local; otherwise returns the addresses it checked.
/// The caller must connect to exactly those (`reqwest`'s
/// `resolve_to_addrs`), never resolve again: a DNS answer that changes
/// between a check and a second lookup (DNS rebinding) would otherwise let
/// a checked-safe name reach a private address. This is a best-effort
/// guard for a tool with no per-call confirmation, not the project's main
/// boundary (the sandbox and `ConfirmationGate`).
pub(crate) async fn resolve_and_check(
    url: &reqwest::Url,
) -> Result<Vec<std::net::SocketAddr>, ToolError> {
    let host = url
        .host_str()
        .ok_or_else(|| ToolError::InvalidArguments("URL has no host".to_string()))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|err| ToolError::ExecutionFailed(format!("failed to resolve host {host}: {err}")))?;

    let addrs: Vec<std::net::SocketAddr> = addrs.collect();
    for addr in &addrs {
        if is_private_or_local(addr.ip()) {
            return Err(ToolError::ExecutionFailed(format!(
                "refusing to fetch {host}: resolved to {} (a private/local address) — set \
                 [web] allow_private_targets = true to override",
                addr.ip()
            )));
        }
    }
    if addrs.is_empty() {
        return Err(ToolError::ExecutionFailed(format!("{host} has no addresses")));
    }
    Ok(addrs)
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
    fn ipv4_shared_and_special_ranges_are_blocked() {
        // 100.64.0.0/10: carrier-grade NAT — Tailscale gives tailnet
        // machines addresses here.
        assert!(is_private_or_local("100.64.0.1".parse().unwrap()));
        assert!(is_private_or_local("100.127.255.254".parse().unwrap()));
        assert!(!is_private_or_local("100.128.0.1".parse().unwrap()));
        // The rest of 0.0.0.0/8, broadcast and multicast.
        assert!(is_private_or_local("0.1.2.3".parse().unwrap()));
        assert!(is_private_or_local("255.255.255.255".parse().unwrap()));
        assert!(is_private_or_local("224.0.0.1".parse().unwrap()));
        // Benchmarking, IETF protocol assignments and Teredo.
        assert!(is_private_or_local("198.18.0.1".parse().unwrap()));
        assert!(is_private_or_local("198.19.255.254".parse().unwrap()));
        assert!(!is_private_or_local("198.20.0.1".parse().unwrap()));
        assert!(is_private_or_local("192.0.0.8".parse().unwrap()));
        assert!(!is_private_or_local("192.0.2.1".parse().unwrap()));
        assert!(is_private_or_local("2001:0:4136:e378::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_forms_embedding_a_private_ipv4_are_blocked() {
        // NAT64 (well-known and local-use prefixes), 6to4, IPv4-compatible.
        assert!(is_private_or_local("64:ff9b::7f00:1".parse().unwrap()));
        assert!(is_private_or_local("64:ff9b::a9fe:a9fe".parse().unwrap()));
        assert!(is_private_or_local("64:ff9b:1::c0a8:101".parse().unwrap()));
        assert!(is_private_or_local("2002:7f00:1::1".parse().unwrap()));
        assert!(is_private_or_local("::10.0.0.1".parse().unwrap()));
        // The same forms around a public IPv4 stay allowed.
        assert!(!is_private_or_local("64:ff9b::808:808".parse().unwrap()));
        assert!(!is_private_or_local("2002:808:808::1".parse().unwrap()));
        // IPv6 multicast.
        assert!(is_private_or_local("ff02::1".parse().unwrap()));
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
    async fn resolve_and_check_returns_the_addresses_it_checked() {
        let url = reqwest::Url::parse("http://8.8.8.8:8080/").unwrap();
        let addrs = resolve_and_check(&url).await.unwrap();
        assert_eq!(addrs, vec!["8.8.8.8:8080".parse().unwrap()]);
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
