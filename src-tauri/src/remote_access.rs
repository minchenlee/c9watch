//! Remote-access policy for the WebSocket server: which addresses it binds
//! and the settings stored in `remote-access.json`.
//!
//! The default binds loopback only, so nothing on the network can reach
//! port 9210. LAN access is an explicit opt-in.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;

/// File name inside the app config directory.
pub const FILE_NAME: &str = "remote-access.json";

/// The settings in `remote-access.json`. A missing field takes its default,
/// so an older file still loads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteAccess {
    /// Bind every interface instead of loopback only.
    pub lan_enabled: bool,
    /// Serve full conversations over WS (enforced from C0h-15).
    pub allow_conversation: bool,
    /// Allow `openSession` over WS (enforced from C0h-15).
    pub allow_open: bool,
    /// Extra `Host` values to accept (enforced from C0h-10).
    pub extra_allowed_hosts: Vec<String>,
}

/// Most extra hosts a user can add.
pub const MAX_EXTRA_HOSTS: usize = 16;

/// Cleans the extra allowed hosts: trims and lowercases each entry, drops
/// empty lines and duplicates. Rejects anything that is not a bare
/// `host` or `host:port` (no scheme, path, user info or spaces).
pub fn normalize_hosts(hosts: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in hosts {
        let host = raw.trim().to_ascii_lowercase();
        if host.is_empty() || out.contains(&host) {
            continue;
        }
        let bad = host.len() > 260
            || host
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']')));
        if bad {
            return Err(format!(
                "Not a host name: {:?}. Use a bare host or host:port, one per line.",
                raw.trim()
            ));
        }
        out.push(host);
    }
    if out.len() > MAX_EXTRA_HOSTS {
        return Err(format!("At most {MAX_EXTRA_HOSTS} extra hosts"));
    }
    Ok(out)
}

/// How a listener carries the WebSocket protocol.
///
/// Only plain `ws` exists today. LAN mode sends traffic in clear text, which
/// the settings UI says. A TLS (`wss`) transport would be a second variant
/// here; the bind and serve code takes a plan, not a fixed address, so it can
/// grow one without a rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    PlainWs,
}

/// The addresses to bind and how to serve them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindPlan {
    /// The first address must bind; the others are best effort.
    pub addrs: Vec<SocketAddr>,
    pub transport: Transport,
}

/// Loopback mode binds `127.0.0.1` and, best effort, `[::1]`.
///
/// LAN mode binds only `[::]`. On macOS `[::]` is dual-stack, so it also
/// takes IPv4; binding `127.0.0.1` on the same port as well would fail with
/// `EADDRINUSE`. The `[::]` listener replaces the loopback ones.
pub fn bind_plan(remote: &RemoteAccess, port: u16) -> BindPlan {
    let addrs = if remote.lan_enabled {
        vec![SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port)]
    } else {
        vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        ]
    };
    BindPlan {
        addrs,
        transport: Transport::PlainWs,
    }
}

/// True when `ip` is a loopback address, including `::ffff:127.0.0.1`,
/// which a dual-stack `[::]` listener reports for IPv4 loopback peers.
pub fn is_loopback_peer(ip: IpAddr) -> bool {
    ip.to_canonical().is_loopback()
}

/// Reads the settings. A missing file gives the defaults. A file that does
/// not parse also gives the defaults, and the error is logged.
pub fn load(path: &Path) -> RemoteAccess {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            crate::debug_log::log_error(&format!(
                "Invalid {FILE_NAME}, using defaults (remote access off): {e}"
            ));
            RemoteAccess::default()
        }),
        Err(_) => RemoteAccess::default(),
    }
}

/// Writes the settings with mode 0600. The data goes to a temp file that is
/// created with 0600 (never wider, not even for a moment), then renamed over
/// the real file.
pub fn save(path: &Path, remote: &RemoteAccess) -> Result<(), String> {
    let dir = path.parent().ok_or("Missing remote access directory")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(remote).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    // `mode` applies only when the file is created, so a stale temp file
    // with wider bits must go first.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// The IP addresses of the interfaces that are up, without loopback and
/// IPv6 link-local addresses. Read fresh on each call (DHCP or a VPN can
/// change them at any time).
#[cfg(unix)]
pub fn local_interface_ips() -> Vec<IpAddr> {
    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list we free below with
    // freeifaddrs; we only read the nodes while the list is alive.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return out;
    }
    let mut node = head;
    while !node.is_null() {
        // SAFETY: `node` is a live element of the list from getifaddrs.
        let ifa = unsafe { &*node };
        node = ifa.ifa_next;
        if ifa.ifa_flags & libc::IFF_UP as libc::c_uint == 0 || ifa.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: `ifa_addr` is non-null and points to a sockaddr whose
        // family tells which concrete struct it is.
        let ip = unsafe {
            match (*ifa.ifa_addr).sa_family as libc::c_int {
                libc::AF_INET => {
                    let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let sin6 = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr))
                }
                _ => continue,
            }
        };
        let link_local_v6 = matches!(ip, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80);
        if ip.is_loopback() || link_local_v6 || out.contains(&ip) {
            continue;
        }
        out.push(ip);
    }
    // SAFETY: `head` came from a successful getifaddrs call.
    unsafe { libc::freeifaddrs(head) };
    out
}

/// Without getifaddrs, fall back to the address of the default route.
#[cfg(not(unix))]
pub fn local_interface_ips() -> Vec<IpAddr> {
    crate::auth::get_local_ip()
        .parse::<IpAddr>()
        .ok()
        .filter(|ip| !ip.is_loopback())
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn default_policy_binds_only_loopback() {
        let plan = bind_plan(&RemoteAccess::default(), 9210);
        assert!(!plan.addrs.is_empty());
        assert!(plan.addrs.iter().all(|a| a.ip().is_loopback()), "{plan:?}");
        assert_eq!(plan.transport, Transport::PlainWs);
    }

    #[test]
    fn lan_policy_binds_only_the_dual_stack_wildcard() {
        let lan = RemoteAccess {
            lan_enabled: true,
            ..Default::default()
        };
        let plan = bind_plan(&lan, 9210);
        assert_eq!(
            plan.addrs,
            vec!["[::]:9210".parse::<SocketAddr>().unwrap()],
            "LAN replaces the loopback listeners instead of adding one"
        );
    }

    #[tokio::test]
    async fn default_policy_listeners_are_loopback() {
        let plan = bind_plan(&RemoteAccess::default(), 0);
        let listener = tokio::net::TcpListener::bind(plan.addrs[0]).await.unwrap();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
    }

    #[test]
    fn mapped_ipv4_loopback_counts_as_loopback() {
        assert!(is_loopback_peer("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_loopback_peer("::1".parse().unwrap()));
        assert!(!is_loopback_peer("::ffff:192.168.1.5".parse().unwrap()));
        assert!(!is_loopback_peer("192.168.1.5".parse().unwrap()));
    }

    #[test]
    fn settings_round_trip_with_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);
        let remote = RemoteAccess {
            lan_enabled: true,
            allow_conversation: true,
            allow_open: false,
            extra_allowed_hosts: vec!["mac.tail1234.ts.net".into()],
        };
        save(&path, &remote).unwrap();
        assert_eq!(load(&path), remote);
        assert!(!path.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn save_replaces_a_stale_world_readable_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, b"old").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        save(&path, &RemoteAccess::default()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn corrupt_or_missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(load(&path), RemoteAccess::default());
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(load(&path), RemoteAccess::default());
        std::fs::write(&path, br#"{"lanEnabled":true}"#).unwrap();
        assert!(load(&path).lan_enabled, "missing fields take defaults");
    }

    #[test]
    fn hosts_are_trimmed_lowercased_and_deduplicated() {
        let hosts = [
            " Mac.Example.NET ",
            "",
            "mac.example.net",
            "box.lan:8000",
            "[::1]:9210",
        ]
        .map(String::from);
        assert_eq!(
            normalize_hosts(&hosts).unwrap(),
            ["mac.example.net", "box.lan:8000", "[::1]:9210"]
        );
    }

    #[test]
    fn hosts_with_a_scheme_path_or_space_are_rejected() {
        for bad in [
            "http://mac.lan",
            "mac.lan/x",
            "user@mac.lan",
            "mac lan",
            "*.lan",
        ] {
            assert!(normalize_hosts(&[bad.to_string()]).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..=MAX_EXTRA_HOSTS).map(|i| format!("h{i}.lan")).collect();
        assert!(normalize_hosts(&many).is_err());
    }

    #[test]
    fn interface_ips_exclude_loopback_and_link_local() {
        for ip in local_interface_ips() {
            assert!(!ip.is_loopback(), "{ip}");
            if let IpAddr::V6(v6) = ip {
                assert_ne!(v6.segments()[0] & 0xffc0, 0xfe80, "{ip}");
            }
        }
    }
}
