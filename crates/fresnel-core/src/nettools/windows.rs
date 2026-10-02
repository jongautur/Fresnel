//! Windows: the Wi-Fi interface's addresses and indices (IP Helper
//! `GetAdaptersAddresses`), the best interface for a target
//! (`GetBestInterfaceEx`), and ICMP echo without admin rights
//! (`IcmpSendEcho2Ex` from the Wi-Fi source address, `Icmp6SendEcho2`).
//!
//! All calls are blocking and run on the blocking pool (see
//! `nettools::blocking` and `ping::ping`); each echo has its own timeout.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Instant;

use windows::Win32::Foundation::{
    GetLastError, ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, HANDLE, NO_ERROR,
};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GetBestInterfaceEx, Icmp6CreateFile, Icmp6SendEcho2, IcmpCloseHandle,
    IcmpCreateFile, IcmpSendEcho2Ex, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
    GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, ICMPV6_ECHO_REPLY_LH, ICMP_ECHO_REPLY,
    IP_ADAPTER_ADDRESSES_LH, IP_BAD_DESTINATION, IP_DEST_HOST_UNREACHABLE, IP_DEST_NET_UNREACHABLE,
    IP_DEST_PORT_UNREACHABLE, IP_DEST_PROT_UNREACHABLE, IP_DEST_SCOPE_MISMATCH,
    IP_DEST_UNREACHABLE, IP_REQ_TIMED_OUT, IP_SUCCESS, IP_TTL_EXPIRED_TRANSIT,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ADDRESS,
};

use super::binding::{adapter_guid_matches, WifiBinding};
use super::ping::{IcmpError, PingConfig, PingResult, ProbeMethod, ProbeOutcome};
use super::route::InterfaceNames;
use super::Cancel;
use crate::{Result, WifiError};

/// Windows reports RTTs in whole milliseconds.
const RESOLUTION_MS: f64 = 1.0;

struct Interface {
    guid: String,
    name: String,
    index_v4: u32,
    index_v6: u32,
    up: bool,
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
    gateway_v4: Option<Ipv4Addr>,
}

fn interfaces() -> Result<Vec<Interface>> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    // The list can grow between the size query and the call: retry a few times.
    for _ in 0..4 {
        // u64 elements: the structures need 8-byte alignment.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        // SAFETY: `buf` holds `size` bytes, suitably aligned.
        let code = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC.0),
                flags,
                None,
                Some(buf.as_mut_ptr().cast()),
                &mut size,
            )
        };
        if code == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        if code == ERROR_NO_DATA.0 {
            return Ok(Vec::new());
        }
        if code != NO_ERROR.0 {
            return Err(WifiError::Backend(format!(
                "GetAdaptersAddresses failed (error {code})"
            )));
        }
        let mut out = Vec::new();
        let mut p = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: a linked list inside `buf`, which outlives the loop.
        while let Some(a) = unsafe { p.as_ref() } {
            out.push(unsafe { interface(a) });
            p = a.Next;
        }
        return Ok(out);
    }
    Err(WifiError::Backend(
        "GetAdaptersAddresses kept asking for a larger buffer".into(),
    ))
}

/// SAFETY: `a` comes from GetAdaptersAddresses and its buffer is alive.
unsafe fn interface(a: &IP_ADAPTER_ADDRESSES_LH) -> Interface {
    let mut ipv4 = Vec::new();
    let mut ipv6 = Vec::new();
    let mut u = a.FirstUnicastAddress;
    while let Some(entry) = unsafe { u.as_ref() } {
        match unsafe { socket_ip(&entry.Address) } {
            Some(IpAddr::V4(v4)) => ipv4.push(v4),
            Some(IpAddr::V6(v6)) => ipv6.push(v6),
            None => {}
        }
        u = entry.Next;
    }
    let mut gateway_v4 = None;
    let mut g = a.FirstGatewayAddress;
    while let Some(entry) = unsafe { g.as_ref() } {
        if let Some(IpAddr::V4(v4)) = unsafe { socket_ip(&entry.Address) } {
            gateway_v4.get_or_insert(v4);
        }
        g = entry.Next;
    }
    Interface {
        guid: unsafe { a.AdapterName.to_string() }.unwrap_or_default(),
        name: unsafe { a.FriendlyName.to_string() }.unwrap_or_default(),
        index_v4: unsafe { a.Anonymous1.Anonymous.IfIndex },
        index_v6: a.Ipv6IfIndex,
        up: a.OperStatus == IfOperStatusUp,
        ipv4,
        ipv6,
        gateway_v4,
    }
}

/// SAFETY: `s` points at a valid socket address or is null.
unsafe fn socket_ip(s: &SOCKET_ADDRESS) -> Option<IpAddr> {
    let sa = unsafe { s.lpSockaddr.as_ref()? };
    if sa.sa_family == AF_INET {
        let sin = unsafe { (s.lpSockaddr as *const SOCKADDR_IN).read_unaligned() };
        let octets = unsafe { sin.sin_addr.S_un.S_addr }.to_ne_bytes();
        Some(IpAddr::V4(Ipv4Addr::from(octets)))
    } else if sa.sa_family == AF_INET6 {
        let sin6 = unsafe { (s.lpSockaddr as *const SOCKADDR_IN6).read_unaligned() };
        Some(IpAddr::V6(Ipv6Addr::from(unsafe { sin6.sin6_addr.u.Byte })))
    } else {
        None
    }
}

/// The interface behind a `windows:{guid}` adapter ID. Native Wifi
/// identifies interfaces by GUID; IP Helper's `AdapterName` is the same
/// GUID in braces.
pub(super) fn binding_for_adapter(adapter_id: &str) -> Result<WifiBinding> {
    let all = interfaces()?;
    let i = all
        .iter()
        .find(|i| adapter_guid_matches(&i.guid, adapter_id))
        .ok_or_else(|| WifiError::AdapterUnavailable {
            id: adapter_id.into(),
            reason: "Windows doesn't list an IP interface for this Wi-Fi adapter".into(),
        })?;
    if !i.up {
        return Err(WifiError::AdapterUnavailable {
            id: adapter_id.into(),
            reason: format!("“{}” is not connected", i.name),
        });
    }
    Ok(WifiBinding {
        iface: if i.name.is_empty() {
            i.guid.clone()
        } else {
            i.name.clone()
        },
        // Link-local (APIPA) addresses mean DHCP failed; don't test from them.
        ipv4: i.ipv4.iter().copied().find(|a| !a.is_link_local()),
        ipv6: i
            .ipv6
            .iter()
            .copied()
            .find(|a| (a.segments()[0] & 0xffc0) != 0xfe80),
        if_index_v4: (i.index_v4 != 0).then_some(i.index_v4),
        if_index_v6: (i.index_v6 != 0).then_some(i.index_v6),
        gateway_v4: i.gateway_v4,
    })
}

fn sockaddr_in(ip: Ipv4Addr) -> SOCKADDR_IN {
    let mut sa = SOCKADDR_IN {
        sin_family: AF_INET,
        ..Default::default()
    };
    sa.sin_addr.S_un.S_addr = u32::from_ne_bytes(ip.octets());
    sa
}

fn sockaddr_in6(ip: Ipv6Addr) -> SOCKADDR_IN6 {
    let mut sa = SOCKADDR_IN6 {
        sin6_family: AF_INET6,
        ..Default::default()
    };
    sa.sin6_addr.u.Byte = ip.octets();
    sa
}

/// The interface Windows would use for `target`, and every interface's
/// index and name (for the error message).
pub(super) fn best_interface(target: IpAddr) -> Result<(Option<u32>, InterfaceNames)> {
    let mut index = 0u32;
    // SAFETY: valid socket addresses on the stack, cast to the generic header.
    let code = unsafe {
        match target {
            IpAddr::V4(v4) => {
                let sa = sockaddr_in(v4);
                GetBestInterfaceEx((&sa as *const SOCKADDR_IN).cast::<SOCKADDR>(), &mut index)
            }
            IpAddr::V6(v6) => {
                let sa = sockaddr_in6(v6);
                GetBestInterfaceEx((&sa as *const SOCKADDR_IN6).cast::<SOCKADDR>(), &mut index)
            }
        }
    };
    let names = interfaces()?
        .into_iter()
        .flat_map(|i| [(i.index_v4, i.name.clone()), (i.index_v6, i.name)])
        .filter(|(index, _)| *index != 0)
        .collect();
    Ok(((code == NO_ERROR.0).then_some(index), names))
}

struct IcmpHandle(HANDLE);

impl Drop for IcmpHandle {
    fn drop(&mut self) {
        // SAFETY: a handle from Icmp(6)CreateFile, closed once.
        let _ = unsafe { IcmpCloseHandle(self.0) };
    }
}

/// Echo from the Wi-Fi source address. RTTs have 1 ms resolution
/// (sub-millisecond replies read 0 ms), recorded in the result.
pub(super) fn icmp(
    binding: &WifiBinding,
    target: IpAddr,
    config: PingConfig,
    cancel: &Cancel,
) -> std::result::Result<PingResult, IcmpError> {
    let source = binding.source_for(target)?;
    // SAFETY: plain constructors; failure is reported via the Result.
    let handle = unsafe {
        match target {
            IpAddr::V4(_) => IcmpCreateFile(),
            IpAddr::V6(_) => Icmp6CreateFile(),
        }
    }
    .map_err(|e| IcmpError::Unavailable(format!("cannot open an ICMP handle: {e}")))?;
    let handle = IcmpHandle(handle);
    let request = *b"Fresnel Wi-Fi survey echo 0123456";
    // Room for the reply, the echoed data, an ICMP error and an
    // IO_STATUS_BLOCK; u64 for alignment.
    let mut reply = vec![0u64; 256];
    let reply_len = (reply.len() * 8) as u32;
    let timeout_ms = config.timeout.as_millis().clamp(1, u128::from(u32::MAX)) as u32;
    let mut probes = Vec::with_capacity(config.count as usize);
    for seq in 0..config.count {
        cancel.check()?;
        let start = Instant::now();
        reply.fill(0);
        // SAFETY: buffers outlive the synchronous calls (no event, no APC).
        let (replies, status, rtt) = unsafe {
            match (source, target) {
                (IpAddr::V4(src), IpAddr::V4(dst)) => {
                    let n = IcmpSendEcho2Ex(
                        handle.0,
                        None,
                        None,
                        None,
                        u32::from_ne_bytes(src.octets()),
                        u32::from_ne_bytes(dst.octets()),
                        request.as_ptr().cast(),
                        request.len() as u16,
                        None,
                        reply.as_mut_ptr().cast(),
                        reply_len,
                        timeout_ms,
                    );
                    if n == 0 {
                        (0, GetLastError().0, 0)
                    } else {
                        let r = reply.as_ptr().cast::<ICMP_ECHO_REPLY>().read_unaligned();
                        (n, r.Status, r.RoundTripTime)
                    }
                }
                (IpAddr::V6(src), IpAddr::V6(dst)) => {
                    let (s, d) = (sockaddr_in6(src), sockaddr_in6(dst));
                    let n = Icmp6SendEcho2(
                        handle.0,
                        None,
                        None,
                        None,
                        &s,
                        &d,
                        request.as_ptr().cast(),
                        request.len() as u16,
                        None,
                        reply.as_mut_ptr().cast(),
                        reply_len,
                        timeout_ms,
                    );
                    if n == 0 {
                        (0, GetLastError().0, 0)
                    } else {
                        let r = reply
                            .as_ptr()
                            .cast::<ICMPV6_ECHO_REPLY_LH>()
                            .read_unaligned();
                        (n, r.Status, r.RoundTripTime)
                    }
                }
                _ => unreachable!("source_for returns the target's family"),
            }
        };
        probes.push(outcome(replies, status, rtt));
        if seq + 1 < config.count {
            cancel.sleep_blocking(config.interval.saturating_sub(start.elapsed()))?;
        }
    }
    drop(handle);
    Ok(PingResult::from_probes(
        ProbeMethod::Icmp,
        None,
        Some(RESOLUTION_MS),
        probes,
    ))
}

fn outcome(replies: u32, status: u32, rtt_ms: u32) -> ProbeOutcome {
    match status {
        IP_SUCCESS if replies > 0 => ProbeOutcome::Reply {
            rtt_ms: f64::from(rtt_ms),
        },
        IP_REQ_TIMED_OUT => ProbeOutcome::Timeout,
        IP_DEST_NET_UNREACHABLE => unreachable_probe("network unreachable"),
        IP_DEST_HOST_UNREACHABLE => unreachable_probe("host unreachable"),
        IP_DEST_PROT_UNREACHABLE => unreachable_probe("protocol unreachable or prohibited"),
        IP_DEST_PORT_UNREACHABLE => unreachable_probe("port unreachable"),
        IP_DEST_UNREACHABLE => unreachable_probe("destination unreachable"),
        IP_DEST_SCOPE_MISMATCH => unreachable_probe("address scope mismatch"),
        IP_TTL_EXPIRED_TRANSIT => unreachable_probe("TTL expired in transit"),
        IP_BAD_DESTINATION => unreachable_probe("bad destination"),
        other => ProbeOutcome::Error {
            detail: format!("ICMP status {other}"),
        },
    }
}

fn unreachable_probe(detail: &str) -> ProbeOutcome {
    ProbeOutcome::Unreachable {
        detail: detail.into(),
    }
}
