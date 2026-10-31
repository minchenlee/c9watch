//! Host, Origin, CSP and LAN checks shared by both gateway hosts.
use super::policy::{self as remote_access, RemoteAccess};
use std::net::IpAddr;
/// A peer may connect when it is loopback, or when LAN access is on.
pub(crate) fn peer_allowed(lan_enabled: bool, peer: IpAddr) -> bool {
    lan_enabled || remote_access::is_loopback_peer(peer)
}

/// Loopback names a page or client may use for this server. Each one must
/// carry the listener's port.
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// Splits a `Host` value (or an allowed-host entry) into a lowercase host and
/// an optional port. IPv6 literals keep their brackets. Returns `None` for a
/// value that is not a bare `host` or `host:port`.
pub(crate) fn split_host(value: &str) -> Option<(String, Option<u16>)> {
    let value = value.trim().to_ascii_lowercase();
    let (host, port) = if value.starts_with('[') {
        let end = value.find(']')?;
        let (host, rest) = value.split_at(end + 1);
        match rest {
            "" => (host.to_string(), None),
            _ => (host.to_string(), Some(rest.strip_prefix(':')?)),
        }
    } else {
        match value.split_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (value.clone(), None),
        }
    };
    let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']');
    if host.is_empty() || !host.chars().all(ok_char) {
        return None;
    }
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok()?),
        None => None,
    };
    Some((host, port))
}

/// The IP address a `Host` names, when it is an IP literal.
pub(crate) fn host_ip(host: &str) -> Option<IpAddr> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse().ok()
}

/// True when this server answers to `host` (a `Host` header value). This
/// blocks DNS rebinding: a page on `evil.example` that resolves to
/// 127.0.0.1 still sends `Host: evil.example`.
///
/// - A loopback name with the listener's port.
/// - An `extraAllowedHosts` entry. An entry with a port must match exactly.
///   An entry without a port matches the host with no port (a proxy on its
///   default port, such as `tailscale serve` on 443) or with the
///   listener's port.
/// - In LAN mode, an IP address of a local interface with the listener's
///   port. `local_ips` is called only then, so the addresses are read fresh
///   per request and never cached.
pub(crate) fn host_allowed(
    host: &str,
    remote: &RemoteAccess,
    port: u16,
    local_ips: impl FnOnce() -> Vec<IpAddr>,
) -> bool {
    let Some((name, given_port)) = split_host(host) else {
        return false;
    };
    if LOOPBACK_HOSTS.contains(&name.as_str()) && given_port == Some(port) {
        return true;
    }
    let extra = remote
        .extra_allowed_hosts
        .iter()
        .any(|entry| match split_host(entry) {
            Some((entry_name, Some(entry_port))) => {
                entry_name == name && given_port == Some(entry_port)
            }
            Some((entry_name, None)) => {
                entry_name == name && (given_port.is_none() || given_port == Some(port))
            }
            None => false,
        });
    if extra {
        return true;
    }
    if remote.lan_enabled && given_port == Some(port) {
        if let Some(ip) = host_ip(&name) {
            return local_ips().contains(&ip);
        }
    }
    false
}

/// A canonical full origin. Path, credentials, wildcard and opaque origins fail closed.
pub fn canonical_origin(value: &str) -> Option<String> {
    let origin = value.trim().to_ascii_lowercase();
    let (scheme, host) = origin.split_once("://")?;
    if !matches!(scheme, "http" | "https") || host.contains(['/', '?', '#', '@', '*']) {
        return None;
    }
    let (name, port) = split_host(host)?;
    // Validate IPv6 literals, rather than accepting arbitrary bracket syntax.
    if name.starts_with('[') && host_ip(&name).is_none() {
        return None;
    }
    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    Some(format!("{scheme}://{name}:{port}"))
}

/// Origins allowed in debug builds only: the Vite dev server.
#[cfg(debug_assertions)]
const DEV_ORIGINS: &[&str] = &["http://localhost:1420"];
#[cfg(not(debug_assertions))]
const DEV_ORIGINS: &[&str] = &[];

/// True when a WS upgrade with this `Origin` may proceed.
///
/// No `Origin` is allowed on purpose. Browsers always send `Origin` on a
/// WebSocket upgrade, so a missing one means a client that is not a
/// browser (a script, the CLI, a native app). Such a client cannot be
/// driven by a hostile web page, which is what this check stops, and it
/// still has to present a valid token.
///
/// A present `Origin` must be `http://` or `https://` plus a host that
/// [`host_allowed`] accepts. `null` and every other scheme are rejected.
pub(crate) fn origin_allowed(
    origin: Option<&str>,
    remote: &RemoteAccess,
    port: u16,
    local_ips: impl FnOnce() -> Vec<IpAddr>,
) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let origin = origin.trim().to_ascii_lowercase();
    if remote.allowed_origins.iter().any(|allowed| {
        canonical_origin(allowed).is_some_and(|a| Some(a) == canonical_origin(&origin))
    }) {
        return true;
    }
    if DEV_ORIGINS.contains(&origin.as_str()) {
        return true;
    }
    let Some(host) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    !host.contains('/') && host_allowed(host, remote, port, local_ips)
}

/// One `'sha256-…'` source per inline `<script>` (a script without `src`)
/// in `html`. The hash covers the exact text between the tags, as the
/// browser computes it.
pub(crate) fn inline_script_hashes(html: &str) -> Vec<String> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    // ASCII lowercasing keeps byte offsets, so slices of `lower` index `html`.
    let lower = html.to_ascii_lowercase();
    let mut hashes = Vec::new();
    let mut at = 0;
    while let Some(found) = lower[at..].find("<script") {
        let start = at + found;
        let name_end = start + "<script".len();
        let Some(open_len) = lower[name_end..].find('>') else {
            break;
        };
        let open_end = name_end + open_len;
        let Some(close_len) = lower[open_end..].find("</script") else {
            break;
        };
        let close = open_end + close_len;
        at = close;
        let attrs = &lower[name_end..open_end];
        if !attrs.is_empty() && !attrs.starts_with(|c: char| c.is_ascii_whitespace()) {
            continue; // e.g. `<scripts>`, not a script tag
        }
        let has_src = attrs
            .split(|c: char| c.is_ascii_whitespace())
            .any(|attr| attr == "src" || attr.starts_with("src="));
        if has_src {
            continue;
        }
        let digest = Sha256::digest(&html.as_bytes()[open_end + 1..close]);
        let hash = base64::engine::general_purpose::STANDARD.encode(digest);
        let source = format!("'sha256-{hash}'");
        if !hashes.contains(&source) {
            hashes.push(source);
        }
    }
    hashes
}

/// The CSP for every response. `connect-src` lists each loopback alias,
/// because the page may connect its socket to a different alias than the
/// one it was loaded from (`localhost` is rewritten to `127.0.0.1`), plus
/// the checked `Host` for LAN and proxy pages.
pub(crate) fn content_security_policy(
    script_hashes: &str,
    port: u16,
    host: Option<&str>,
) -> String {
    let mut connect = vec!["'self'".to_string()];
    let mut add = |source: String| {
        if !connect.contains(&source) {
            connect.push(source);
        }
    };
    for alias in LOOPBACK_HOSTS {
        add(format!("ws://{alias}:{port}"));
    }
    if let Some(host) = host {
        add(format!("ws://{host}"));
        add(format!("wss://{host}"));
    }
    let script = match script_hashes {
        "" => "'self'".to_string(),
        hashes => format!("'self' {hashes}"),
    };
    format!(
        "default-src 'self'; script-src {script}; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data:; font-src 'self'; connect-src {}; object-src 'none'; \
         base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        connect.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const PORT: u16 = 9210;
    const LAN_IP: &str = "192.168.1.20";
    fn lan_ips() -> Vec<IpAddr> {
        vec![LAN_IP.parse().unwrap(), "fd00::20".parse().unwrap()]
    }

    fn policy(lan_enabled: bool, extra: &[&str]) -> RemoteAccess {
        RemoteAccess {
            lan_enabled,
            extra_allowed_hosts: extra.iter().map(|h| h.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn host_check_table() {
        let off = policy(false, &[]);
        let lan = policy(true, &[]);
        let extra = policy(false, &["mac.tail1234.ts.net", "box.lan:8000"]);
        let cases: &[(&str, &RemoteAccess, bool)] = &[
            ("127.0.0.1:9210", &off, true),
            ("localhost:9210", &off, true),
            ("LOCALHOST:9210", &off, true),
            ("[::1]:9210", &off, true),
            ("evil.example:9210", &off, false),
            ("evil.example", &off, false),
            ("localhost", &off, false),
            ("127.0.0.1:8000", &off, false),
            ("localhost:9211", &off, false),
            ("localhost:x", &off, false),
            ("localhost.:9210", &off, false),
            ("", &off, false),
            ("192.168.1.20:9210", &off, false),
            ("192.168.1.20:9210", &lan, true),
            ("[fd00::20]:9210", &lan, true),
            ("192.168.1.20:8000", &lan, false),
            ("192.168.1.99:9210", &lan, false),
            ("evil.example:9210", &lan, false),
            ("mac.tail1234.ts.net", &extra, true),
            ("mac.tail1234.ts.net:9210", &extra, true),
            ("mac.tail1234.ts.net:443", &extra, false),
            ("box.lan:8000", &extra, true),
            ("box.lan", &extra, false),
            ("box.lan:9210", &extra, false),
            ("other.ts.net", &extra, false),
        ];
        for (host, remote, want) in cases {
            assert_eq!(
                host_allowed(host, remote, PORT, lan_ips),
                *want,
                "Host {host:?} lan={}",
                remote.lan_enabled
            );
        }
    }

    #[test]
    fn inline_script_hashes_skip_external_scripts() {
        let html = "<head><script src=\"/a.js\"></script>\
                    <SCRIPT type=\"module\" src=/b.js></SCRIPT></head>\
                    <body><script>alert(1)</script><scripts></scripts>\
                    <script type=\"text/javascript\">\n  run();\n</script>\
                    <script>alert(1)</script></body>";
        assert_eq!(
            inline_script_hashes(html),
            [
                "'sha256-bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI='",
                "'sha256-2P5SgcCngN/gHHrSMQVmAm2SPYlTnahjTR7WEmfSs8Y='",
            ]
        );
        assert!(inline_script_hashes("<p>no scripts</p>").is_empty());
    }

    #[test]
    fn csp_adds_the_checked_host_and_the_script_hashes() {
        let csp = content_security_policy("'sha256-abc='", PORT, Some("192.168.1.20:9210"));
        assert!(csp.contains("script-src 'self' 'sha256-abc=';"), "{csp}");
        assert!(
            csp.contains(
                "connect-src 'self' ws://127.0.0.1:9210 ws://localhost:9210 \
                 ws://[::1]:9210 ws://192.168.1.20:9210 wss://192.168.1.20:9210;"
            ),
            "{csp}"
        );
        let loopback = content_security_policy("", PORT, Some("localhost:9210"));
        assert!(loopback.contains("script-src 'self';"), "{loopback}");
        assert_eq!(
            loopback.matches("ws://localhost:9210").count(),
            1,
            "{loopback}"
        );
    }

    #[test]
    fn interface_ips_are_read_only_for_lan_ip_hosts() {
        let never = || -> Vec<IpAddr> { panic!("interfaces read") };
        assert!(host_allowed(
            "localhost:9210",
            &policy(true, &[]),
            PORT,
            never
        ));
        assert!(!host_allowed(
            "192.168.1.20:9210",
            &policy(false, &[]),
            PORT,
            never
        ));
    }

    #[test]
    fn origin_check_table() {
        let off = policy(false, &[]);
        let lan = policy(true, &[]);
        let extra = policy(false, &["mac.tail1234.ts.net"]);
        let cases: &[(Option<&str>, &RemoteAccess, bool)] = &[
            (None, &off, true),
            (Some("http://127.0.0.1:9210"), &off, true),
            (Some("http://localhost:9210"), &off, true),
            (Some("http://[::1]:9210"), &off, true),
            (Some("https://mac.tail1234.ts.net"), &extra, true),
            (Some("http://192.168.1.20:9210"), &lan, true),
            (Some("http://192.168.1.20:9210"), &off, false),
            (Some("http://evil.example"), &off, false),
            (Some("http://evil.localhost:8000"), &off, false),
            (Some("http://localhost:8000"), &off, false),
            (Some("null"), &off, false),
            (Some(""), &off, false),
            (Some("tauri://localhost"), &off, false),
            (Some("ws://localhost:9210"), &off, false),
            (Some("http://localhost:9210/x"), &off, false),
        ];
        for (origin, remote, want) in cases {
            assert_eq!(
                origin_allowed(*origin, remote, PORT, lan_ips),
                *want,
                "Origin {origin:?}"
            );
        }
        assert_eq!(
            origin_allowed(Some("http://localhost:1420"), &off, PORT, lan_ips),
            cfg!(debug_assertions),
            "the Vite dev origin is a debug-build exception"
        );
    }

    #[test]
    fn non_loopback_peers_need_lan() {
        let lan_peer: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(!peer_allowed(false, lan_peer));
        assert!(peer_allowed(true, lan_peer));
        assert!(peer_allowed(false, "::ffff:127.0.0.1".parse().unwrap()));
    }
    #[test]
    fn extra_origins_are_exact_and_do_not_expand_hosts() {
        let mut p = RemoteAccess::default();
        p.allowed_origins = vec!["https://view.example:443".into()];
        assert!(origin_allowed(
            Some("https://view.example"),
            &p,
            PORT,
            lan_ips
        ));
        assert!(!origin_allowed(
            Some("http://view.example"),
            &p,
            PORT,
            lan_ips
        ));
        assert!(!origin_allowed(
            Some("https://view.example:444"),
            &p,
            PORT,
            lan_ips
        ));
        assert!(!host_allowed("view.example:9210", &p, PORT, lan_ips));
        for origin in [
            "null",
            "https://*.example",
            "https://user@view.example",
            "https://view.example/path",
            "https://view.example?x=1",
            "https://[bad]:443",
        ] {
            assert!(canonical_origin(origin).is_none(), "{origin}");
        }
    }
}
