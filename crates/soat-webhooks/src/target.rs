//! Which URLs a webhook may point at.
//!
//! A merchant chooses the URL and the server calls it, so an unchecked URL
//! lets anyone make SOAT request internal services (cloud metadata,
//! databases on the private network). By default only `https://` to public
//! addresses is allowed, checked again on every send after DNS resolution.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// What targets are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPolicy {
    /// `https://` to public addresses only (production).
    PublicHttps,
    /// Anything, including `http://localhost` (development and tests).
    Any,
}

/// Checks the URL's shape: scheme and literal host. Returns why not.
pub fn check_target(url: &str, policy: TargetPolicy) -> Result<http::Uri, String> {
    let uri: http::Uri = url.parse().map_err(|_| "not a valid URL".to_owned())?;
    let scheme = uri.scheme_str().unwrap_or_default();
    let host = uri.host().ok_or("the URL has no host")?.to_owned();
    if !matches!(scheme, "https" | "http") {
        return Err("use an https:// URL".into());
    }
    if policy == TargetPolicy::Any {
        return Ok(uri);
    }
    if scheme != "https" {
        return Err("use an https:// URL".into());
    }
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.eq_ignore_ascii_case("localhost") || bare.ends_with(".localhost") {
        return Err("localhost is not allowed".into());
    }
    if let Ok(ip) = bare.parse::<IpAddr>() {
        if !is_public(ip) {
            return Err("private and reserved addresses are not allowed".into());
        }
    }
    Ok(uri)
}

/// Whether an address is on the public internet.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        // Carrier-grade NAT 100.64.0.0/10.
        || (a == 100 && (64..128).contains(&b))
        // Benchmarking 198.18.0.0/15, reserved 240.0.0.0/4.
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // Unique local fc00::/7, link-local fe80::/10.
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_allows_only_public_https() {
        let ok = ["https://example.com/hook", "https://93.184.216.34/x"];
        for url in ok {
            assert!(
                check_target(url, TargetPolicy::PublicHttps).is_ok(),
                "{url}"
            );
        }
        let bad = [
            "http://example.com/hook",
            "ftp://example.com",
            "https://localhost/x",
            "https://api.localhost/x",
            "https://127.0.0.1/x",
            "https://10.0.0.5/x",
            "https://169.254.169.254/latest/meta-data",
            "https://192.168.1.1/",
            "https://100.64.0.1/",
            "https://[::1]/",
            "https://[fd00::1]/",
            "https://[::ffff:10.0.0.1]/",
            "not a url",
        ];
        for url in bad {
            assert!(
                check_target(url, TargetPolicy::PublicHttps).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn development_allows_local_http() {
        assert!(check_target("http://127.0.0.1:9000/x", TargetPolicy::Any).is_ok());
        assert!(check_target("ftp://x", TargetPolicy::Any).is_err());
    }
}
