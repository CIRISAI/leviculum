//! +ciris (leviculum#101): the address facts port mapping needs. Which
//! external addresses make a mapping useless, where the default gateway is,
//! and whether this host has a global IPv6 address.
//!
//! The parsers take text, so they are tested without the OS. Only the thin
//! readers around them touch `/proc`, `route` or the interface list.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

/// Why an external address cannot be reached from the internet, or `None`
/// when it is public. A router that reports one of these is itself behind
/// another NAT (carrier-grade, or a second home router), so a mapping on it
/// opens nothing the internet can reach.
pub fn non_public_reason(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            if o[0] == 100 && (o[1] & 0xC0) == 64 {
                Some("carrier-grade NAT shared range (100.64.0.0/10)")
            } else if v4.is_private() {
                Some("private range (RFC 1918): a second NAT sits upstream")
            } else if v4.is_link_local() {
                Some("link-local (169.254.0.0/16)")
            } else if v4.is_loopback() {
                Some("loopback")
            } else if v4.is_unspecified() {
                Some("unspecified (0.0.0.0): the router has no WAN address")
            } else {
                None
            }
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return non_public_reason(IpAddr::V4(v4));
            }
            if is_global_v6(v6) {
                None
            } else {
                Some("not a global IPv6 address")
            }
        }
    }
}

/// A global unicast IPv6 address (2000::/3, less the documentation prefix).
/// ULA (fc00::/7), link-local and the rest are not.
pub fn is_global_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    (s[0] & 0xE000) == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8)
}

/// The global IPv6 addresses on this host's interfaces. Such an address
/// makes the host addressable without any mapping; whether the router's
/// firewall lets a connection in is a separate question this cannot answer.
pub(crate) fn global_ipv6_addresses() -> Vec<Ipv6Addr> {
    let mut out: Vec<Ipv6Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| match iface.ip() {
            IpAddr::V6(v6) if is_global_v6(v6) => Some(v6),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The local address the kernel would send from toward `dest`. A connected
/// UDP socket sends nothing; it only resolves the route.
pub(crate) fn local_address_toward(dest: SocketAddr) -> Option<IpAddr> {
    let bind: SocketAddr = if dest.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(dest).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// The IPv4 default gateway, read from the OS routing table.
pub(crate) fn default_gateway_v4() -> Option<Ipv4Addr> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let text = std::fs::read_to_string("/proc/net/route").ok()?;
        parse_proc_net_route(&text)
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        let out = std::process::Command::new("route")
            .args(["-n", "get", "default"])
            .output()
            .ok()?;
        parse_route_get(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: a GUI host must not flash a console.
        let out = std::process::Command::new("route")
            .args(["print", "-4", "0.0.0.0"])
            .creation_flags(0x0800_0000)
            .output()
            .ok()?;
        parse_windows_route_print(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        windows
    )))]
    {
        None
    }
}

/// The IPv6 default gateway and the scope id of the interface it is on
/// (a router's address is usually link-local). Linux only; elsewhere `None`,
/// and an IPv6 pinhole is not attempted.
pub(crate) fn default_gateway_v6() -> Option<(Ipv6Addr, u32)> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let text = std::fs::read_to_string("/proc/net/ipv6_route").ok()?;
        let (gw, ifname) = parse_proc_ipv6_route(&text)?;
        let name = std::ffi::CString::new(ifname).ok()?;
        // SAFETY: `name` is a valid NUL-terminated string for the call.
        let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
        Some((gw, index))
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        None
    }
}

/// The gateway of the lowest-metric default route in `/proc/net/route`
/// (fields are little-endian hex).
pub(crate) fn parse_proc_net_route(text: &str) -> Option<Ipv4Addr> {
    const RTF_UP: u32 = 0x1;
    const RTF_GATEWAY: u32 = 0x2;
    let mut best: Option<(u32, Ipv4Addr)> = None;
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 8 {
            continue;
        }
        let (Ok(dest), Ok(gw), Ok(flags), Ok(metric), Ok(mask)) = (
            u32::from_str_radix(f[1], 16),
            u32::from_str_radix(f[2], 16),
            u32::from_str_radix(f[3], 16),
            f[6].parse::<u32>(),
            u32::from_str_radix(f[7], 16),
        ) else {
            continue;
        };
        if dest != 0 || mask != 0 || flags & (RTF_UP | RTF_GATEWAY) != (RTF_UP | RTF_GATEWAY) {
            continue;
        }
        let ip = Ipv4Addr::from(gw.to_le_bytes());
        if best.is_none_or(|(m, _)| metric < m) {
            best = Some((metric, ip));
        }
    }
    best.map(|(_, ip)| ip)
}

/// The next hop and interface of the default route in
/// `/proc/net/ipv6_route` (`::/0` with a non-zero next hop).
pub(crate) fn parse_proc_ipv6_route(text: &str) -> Option<(Ipv6Addr, String)> {
    let mut best: Option<(u32, Ipv6Addr, String)> = None;
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || f[0] != "00000000000000000000000000000000" || f[1] != "00" {
            continue;
        }
        let Ok(hop) = u128::from_str_radix(f[4], 16) else {
            continue;
        };
        if hop == 0 {
            continue;
        }
        let metric = u32::from_str_radix(f[5], 16).unwrap_or(u32::MAX);
        if best.as_ref().is_none_or(|(m, _, _)| metric < *m) {
            best = Some((metric, Ipv6Addr::from(hop), f[9].to_string()));
        }
    }
    best.map(|(_, ip, name)| (ip, name))
}

/// The `gateway:` line of BSD/macOS `route -n get default`.
#[allow(dead_code)] // used on the BSDs, tested everywhere
pub(crate) fn parse_route_get(text: &str) -> Option<Ipv4Addr> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("gateway:"))
        .find_map(|v| v.trim().parse().ok())
}

/// The gateway column of the `0.0.0.0 0.0.0.0` row of Windows
/// `route print -4 0.0.0.0`, lowest metric first. The header is localised;
/// the rows are not.
#[allow(dead_code)] // used on Windows, tested everywhere
pub(crate) fn parse_windows_route_print(text: &str) -> Option<Ipv4Addr> {
    let mut best: Option<(u32, Ipv4Addr)> = None;
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 5 || f[0] != "0.0.0.0" || f[1] != "0.0.0.0" {
            continue;
        }
        let (Ok(gw), Ok(metric)) = (f[2].parse::<Ipv4Addr>(), f[4].parse::<u32>()) else {
            continue;
        };
        if best.is_none_or(|(m, _)| metric < m) {
            best = Some((metric, gw));
        }
    }
    best.map(|(_, ip)| ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_and_private_ranges_are_not_public() {
        let reason = |s: &str| non_public_reason(s.parse().unwrap());
        assert!(reason("100.64.0.1").unwrap().contains("carrier-grade"));
        assert!(reason("100.127.255.254").unwrap().contains("carrier-grade"));
        assert_eq!(reason("100.128.0.1"), None);
        assert!(reason("10.1.2.3").unwrap().contains("RFC 1918"));
        assert!(reason("172.16.0.1").unwrap().contains("RFC 1918"));
        assert!(reason("192.168.1.1").unwrap().contains("RFC 1918"));
        assert!(reason("169.254.3.4").is_some());
        assert!(reason("0.0.0.0").is_some());
        assert_eq!(reason("203.0.113.7"), None);
        assert_eq!(reason("8.8.8.8"), None);
        assert!(reason("::ffff:100.64.1.1")
            .unwrap()
            .contains("carrier-grade"));
        assert_eq!(reason("2a01:4f8::1"), None);
        assert!(reason("fd00::1").is_some());
        assert!(reason("fe80::1").is_some());
        assert!(reason("2001:db8::1").is_some());
    }

    #[test]
    fn proc_net_route_gives_the_default_gateway() {
        let text =
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
            wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
            eth0\t00000000\t0100000A\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
            docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n";
        assert_eq!(parse_proc_net_route(text), Some(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(parse_proc_net_route("Iface\tDestination\n"), None);
    }

    #[test]
    fn proc_ipv6_route_gives_the_default_hop() {
        let text = "fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001  eth0\n\
            00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003  wlan0\n";
        assert_eq!(
            parse_proc_ipv6_route(text),
            Some(("fe80::1".parse().unwrap(), "wlan0".to_string()))
        );
    }

    #[test]
    fn bsd_and_windows_route_output_parse() {
        let bsd = "   route to: default\ndestination: default\n       mask: default\n    gateway: 192.168.0.1\n  interface: en0\n";
        assert_eq!(parse_route_get(bsd), Some(Ipv4Addr::new(192, 168, 0, 1)));
        let win = "===========\nIPv4 Route Table\n===========\nActive Routes:\nNetwork Destination        Netmask          Gateway       Interface  Metric\n          0.0.0.0          0.0.0.0      192.168.1.1     192.168.1.24     35\n          0.0.0.0          0.0.0.0      10.0.0.1        10.0.0.5         25\n";
        assert_eq!(
            parse_windows_route_print(win),
            Some(Ipv4Addr::new(10, 0, 0, 1))
        );
    }
}
