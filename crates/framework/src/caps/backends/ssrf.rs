use corelib::wire::Json;

use crate::caps::*;

/// Block fetches to private / loopback / link-local / metadata hosts (SSRF).
/// True if `ip` is a private / loopback / link-local / ULA / metadata / unspecified
/// address — anything a capability fetch must not reach. Covers IPv6 (incl. IPv4-mapped /
/// -compatible forms) so a bracketed `[::ffff:127.0.0.1]` can't slip through.
pub(crate) fn is_blocked_ip(ip: &std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local() // 169.254/16 — incl. the 169.254.169.254 cloud metadata IP
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1])) // 100.64/10 carrier-grade NAT
        }
        IpAddr::V6(v6) => {
            // Re-check any embedded IPv4 (mapped or compatible) as IPv4.
            if let Some(m) = v6.to_ipv4_mapped() {
                return is_blocked_ip(&IpAddr::V4(m));
            }
            if let Some(c) = v6.to_ipv4() {
                return is_blocked_ip(&IpAddr::V4(c));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg0 & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (seg0 & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// Resolve `host:port` (a hostname OR any numeric IP encoding — decimal/octal/hex/IPv6 —
/// which `getaddrinfo` normalizes the same way `curl` will) and reject if ANY resolved
/// address is blocked (defeats DNS rebinding: a public name resolving to a private IP is
/// caught). Returns a vetted IP to PIN, so the later fetch can't re-resolve to a different
/// address.
pub(crate) fn ssrf_resolve(host: &str, port: u16) -> Result<std::net::IpAddr, String> {
    use std::net::ToSocketAddrs;
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return Err("blocked host (SSRF): localhost".into());
    }
    let addrs = (host, port).to_socket_addrs().map_err(|e| format!("blocked host (SSRF): cannot resolve {host}: {e}"))?;
    let mut chosen: Option<std::net::IpAddr> = None;
    for sa in addrs {
        let ip = sa.ip();
        if is_blocked_ip(&ip) {
            return Err(format!("blocked host (SSRF): {host} → {ip}"));
        }
        if chosen.is_none() {
            chosen = Some(ip);
        }
    }
    chosen.ok_or_else(|| format!("blocked host (SSRF): {host} did not resolve"))
}

/// Parse a URL into `(host, port)` (default port by scheme; handles `[IPv6]:port`,
/// `user@host`, and a path/query suffix).
pub(crate) fn url_host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default = if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority); // strip userinfo
    if let Some(after_lb) = authority.strip_prefix('[') {
        let (h, tail) = after_lb.split_once(']')?;
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(default);
        Some((h.to_string(), port))
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        match p.parse::<u16>() {
            Ok(port) => Some((h.to_string(), port)),
            Err(_) => Some((authority.to_string(), default)),
        }
    } else {
        Some((authority.to_string(), default))
    }
}

/// Vet a fetch URL against SSRF and return the `host:port:ip` directive to PIN it (so the
/// fetch connects only to the vetted IP and never re-resolves / follows a redirect to an
/// internal host).
pub(crate) fn ssrf_pin(url: &str) -> Result<String, String> {
    let (host, port) = url_host_port(url).ok_or("net: cannot parse url host")?;
    let ip = ssrf_resolve(&host, port)?;
    Ok(format!("{host}:{port}:{ip}"))
}


// ----- net.get -------------------------------------------------------------

pub(crate) fn net(method: &str, args: &[(String, String)], ctx: &CapCtx) -> Result<Json, String> {
    match method {
        "net.get" => {
            let url = arg(args, 0, "url").ok_or("net.get: missing url")?;
            if !ctx.remote_enabled {
                return Err("network is disabled ([ai] network = false)".into());
            }
            if !url.starts_with("https://") {
                return Err("net.get: https only".into());
            }
            Ok(Json::Str(net::https_get(url, &ssrf_pin(url)?)?))
        }
        _ => Err(format!("unknown net method '{method}'")),
    }
}
