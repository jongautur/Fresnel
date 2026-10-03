//! DNS lookups for the Tools page: one query to one server, with the whole
//! answer (response code, flags, every section, TTLs) and the time it took.
//!
//! `hickory-proto` builds and parses the messages; the transport is ours
//! (UDP, then TCP when the answer is truncated, or TCP only), so a query can
//! be bound to the Wi-Fi interface like every other test. "System" means
//! the DNS servers the OS is configured with, queried directly; which one
//! answered is part of the result.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::time::{Duration, Instant};

use hickory_proto::op::{Edns, Message, MessageType, OpCode, Query};
use hickory_proto::rr::{Name, Record, RecordType};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::binding::{self, ConnectError, WifiBinding};
use super::{Cancel, TestError, TestResult};
use crate::{Result, WifiError};

/// Results JSON layout version, stored with each run.
pub const DNS_RESULTS_VERSION: u32 = 1;
pub const DNS_PORT: u16 = 53;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);
/// Largest UDP answer we ask for (the DNS Flag Day 2020 value).
const EDNS_PAYLOAD: u16 = 1232;
/// Record types the Tools page offers.
pub const RECORD_TYPES: &[&str] = &[
    "A", "AAAA", "CNAME", "MX", "TXT", "NS", "SOA", "PTR", "SRV", "CAA",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsTransport {
    /// UDP, retried over TCP if the answer is truncated.
    Udp,
    Tcp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsQuery {
    /// A name, or for PTR also an IP address (turned into its
    /// `in-addr.arpa` / `ip6.arpa` name).
    pub name: String,
    pub record_type: String,
    pub server: SocketAddr,
    pub transport: DnsTransport,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsSection {
    Answer,
    Authority,
    Additional,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsRecord {
    pub section: DnsSection,
    pub name: String,
    pub record_type: String,
    pub ttl: u32,
    /// The record data in zone-file text form.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsResult {
    pub version: u32,
    /// The name actually asked for (for PTR from an IP, the arpa name).
    pub query_name: String,
    pub record_type: String,
    pub server: String,
    /// The transport that produced the answer.
    pub transport: DnsTransport,
    /// The UDP answer was truncated, so the query was repeated over TCP.
    pub retried_over_tcp: bool,
    /// `NoError`, `NXDomain`, `ServFail`, `Refused`, …
    pub response_code: String,
    pub authoritative: bool,
    pub recursion_available: bool,
    /// The server says it validated the answer with DNSSEC (AD flag). Only
    /// as trustworthy as the path to the server.
    pub authentic_data: bool,
    /// Truncated even over TCP (shouldn't happen) or UDP-only answer cut.
    pub truncated: bool,
    /// From sending the query to the full answer (TCP: including connect).
    pub query_ms: f64,
    pub response_bytes: usize,
    pub records: Vec<DnsRecord>,
}

/// Parse "A", "aaaa", … into a record type we offer.
pub fn record_type(text: &str) -> Result<RecordType> {
    let upper = text.trim().to_ascii_uppercase();
    if !RECORD_TYPES.contains(&upper.as_str()) {
        return Err(WifiError::InvalidInput(format!(
            "record type must be one of {}",
            RECORD_TYPES.join(", ")
        )));
    }
    RecordType::from_str(&upper)
        .map_err(|e| WifiError::InvalidInput(format!("record type {upper}: {e}")))
}

/// The DNS name to ask for: for PTR an IP address becomes its arpa name.
pub fn query_name(name: &str, rtype: RecordType) -> Result<Name> {
    let name = name.trim();
    if name.is_empty() {
        return Err(WifiError::InvalidInput("enter a name to look up".into()));
    }
    if rtype == RecordType::PTR {
        if let Ok(ip) = name.parse::<IpAddr>() {
            return Ok(Name::from(ip));
        }
    }
    let mut n = Name::from_utf8(name)
        .map_err(|e| WifiError::InvalidInput(format!("“{name}” is not a valid DNS name: {e}")))?;
    n.set_fqdn(true);
    Ok(n)
}

/// Parse a server the user typed: `1.1.1.1`, `1.1.1.1:5353`,
/// `2606:4700::1111`, `[2606:4700::1111]:53`.
pub fn parse_server(text: &str) -> Result<SocketAddr> {
    let text = text.trim();
    if let Ok(addr) = text.parse::<SocketAddr>() {
        if addr.port() == 0 {
            return Err(WifiError::InvalidInput(
                "the DNS server port must be 1–65535".into(),
            ));
        }
        return Ok(addr);
    }
    let bare = text
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .unwrap_or(text);
    bare.parse::<IpAddr>()
        .map(|ip| SocketAddr::new(ip, DNS_PORT))
        .map_err(|_| {
            WifiError::InvalidInput(format!(
                "“{text}” is not a DNS server address (an IP address, optionally with :port)"
            ))
        })
}

/// Send one query and wait for its answer.
pub async fn lookup(
    binding: Option<&WifiBinding>,
    query: &DnsQuery,
    cancel: &Cancel,
) -> TestResult<DnsResult> {
    let rtype = record_type(&query.record_type)?;
    let name = query_name(&query.name, rtype)?;
    if query.timeout.is_zero() || query.timeout > Duration::from_secs(30) {
        return Err(WifiError::InvalidInput("DNS timeout must be up to 30 s".into()).into());
    }
    let mut request = Message::new(rand_id(), MessageType::Query, OpCode::Query);
    request.metadata.recursion_desired = true;
    request.add_query(Query::query(name.clone(), rtype));
    let mut edns = Edns::new();
    edns.set_max_payload(EDNS_PAYLOAD);
    request.set_edns(edns);
    let bytes = request
        .to_vec()
        .map_err(|e| WifiError::Backend(format!("cannot encode the DNS query: {e}")))?;

    let run = async {
        let start = Instant::now();
        let (raw, transport, retried) = match query.transport {
            DnsTransport::Tcp => (
                tcp_exchange(binding, query.server, &bytes, query.timeout).await?,
                DnsTransport::Tcp,
                false,
            ),
            DnsTransport::Udp => {
                let raw = udp_exchange(
                    binding,
                    query.server,
                    &bytes,
                    request.metadata.id,
                    query.timeout,
                )
                .await?;
                if is_truncated(&raw) {
                    (
                        tcp_exchange(binding, query.server, &bytes, query.timeout).await?,
                        DnsTransport::Tcp,
                        true,
                    )
                } else {
                    (raw, DnsTransport::Udp, false)
                }
            }
        };
        let query_ms = start.elapsed().as_secs_f64() * 1000.0;
        let response = Message::from_vec(&raw).map_err(|e| {
            WifiError::Backend(format!(
                "the answer from {} is not valid DNS: {e}",
                query.server
            ))
        })?;
        if response.metadata.id != request.metadata.id
            || response.metadata.message_type != MessageType::Response
        {
            return Err(WifiError::Backend(format!(
                "{} answered with a message that doesn't match the query",
                query.server
            )));
        }
        Ok(to_result(
            &name,
            rtype,
            query.server,
            transport,
            retried,
            query_ms,
            raw.len(),
            &response,
        ))
    };
    tokio::select! {
        _ = cancel.cancelled() => Err(TestError::Cancelled),
        r = run => r.map_err(TestError::from),
    }
}

#[allow(clippy::too_many_arguments)]
fn to_result(
    name: &Name,
    rtype: RecordType,
    server: SocketAddr,
    transport: DnsTransport,
    retried_over_tcp: bool,
    query_ms: f64,
    response_bytes: usize,
    m: &Message,
) -> DnsResult {
    let section = |records: &[Record], section: DnsSection| -> Vec<DnsRecord> {
        records
            .iter()
            .map(|r| DnsRecord {
                section,
                name: r.name.to_string(),
                record_type: r.record_type().to_string(),
                ttl: r.ttl,
                data: r.data.to_string(),
            })
            .collect()
    };
    let mut records = section(&m.answers, DnsSection::Answer);
    records.extend(section(&m.authorities, DnsSection::Authority));
    records.extend(section(&m.additionals, DnsSection::Additional));
    DnsResult {
        version: DNS_RESULTS_VERSION,
        query_name: name.to_string(),
        record_type: rtype.to_string(),
        server: server.to_string(),
        transport,
        retried_over_tcp,
        response_code: format!("{:?}", m.metadata.response_code),
        authoritative: m.metadata.authoritative,
        recursion_available: m.metadata.recursion_available,
        authentic_data: m.metadata.authentic_data,
        truncated: m.metadata.truncation,
        query_ms,
        response_bytes,
        records,
    }
}

fn rand_id() -> u16 {
    // Query IDs only need to differ between our own queries; the clock's
    // nanoseconds are enough and avoid another dependency.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos ^ (nanos >> 16)) as u16
}

/// The TC bit of a raw DNS header.
fn is_truncated(raw: &[u8]) -> bool {
    raw.len() > 2 && raw[2] & 0x02 != 0
}

async fn udp_exchange(
    binding: Option<&WifiBinding>,
    server: SocketAddr,
    request: &[u8],
    id: u16,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let socket = binding::udp_socket(binding, server.ip())?;
    socket
        .connect(server)
        .await
        .map_err(|e| unreachable(server, &e))?;
    socket
        .send(request)
        .await
        .map_err(|e| unreachable(server, &e))?;
    let mut buf = vec![0u8; 65535];
    let wait = async {
        loop {
            let n = socket.recv(&mut buf).await.map_err(|e| match e.kind() {
                std::io::ErrorKind::ConnectionRefused => WifiError::Backend(format!(
                    "{server} refused the query: no DNS server listens there"
                )),
                _ => unreachable(server, &e),
            })?;
            // A stray or late datagram for another query: keep waiting.
            if n >= 12 && u16::from_be_bytes([buf[0], buf[1]]) == id {
                return Ok(buf[..n].to_vec());
            }
        }
    };
    tokio::time::timeout(timeout, wait)
        .await
        .unwrap_or_else(|_| Err(no_answer(server, timeout)))
}

async fn tcp_exchange(
    binding: Option<&WifiBinding>,
    server: SocketAddr,
    request: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let exchange = async {
        let mut stream = match binding::connect(binding, server, timeout).await {
            Ok(s) => s,
            Err(ConnectError::Bind(e)) => return Err(e),
            Err(ConnectError::Connect(e)) => {
                return Err(match e.kind() {
                    std::io::ErrorKind::ConnectionRefused => WifiError::Backend(format!(
                        "{server} refused the TCP connection: no DNS server listens there over TCP"
                    )),
                    std::io::ErrorKind::TimedOut => no_answer(server, timeout),
                    _ => unreachable(server, &e),
                })
            }
        };
        let len = u16::try_from(request.len())
            .map_err(|_| WifiError::Backend("the DNS query is too long".into()))?;
        let mut framed = len.to_be_bytes().to_vec();
        framed.extend_from_slice(request);
        stream
            .write_all(&framed)
            .await
            .map_err(|e| unreachable(server, &e))?;
        let mut len = [0u8; 2];
        stream
            .read_exact(&mut len)
            .await
            .map_err(|e| unreachable(server, &e))?;
        let mut body = vec![0u8; usize::from(u16::from_be_bytes(len))];
        stream
            .read_exact(&mut body)
            .await
            .map_err(|e| unreachable(server, &e))?;
        Ok(body)
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .unwrap_or_else(|_| Err(no_answer(server, timeout)))
}

fn no_answer(server: SocketAddr, timeout: Duration) -> WifiError {
    WifiError::Timeout(format!(
        "{server} didn't answer within {:.1} s",
        timeout.as_secs_f64()
    ))
    .with_hint("The server may be down, blocked by a firewall, or not a DNS server.")
}

fn unreachable(server: SocketAddr, e: &std::io::Error) -> WifiError {
    WifiError::Backend(format!("cannot reach {server}: {e}"))
}

/// A DNS server from the OS configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemDnsServer {
    pub address: IpAddr,
    /// Where it came from: `/etc/resolv.conf`, "systemd-resolved
    /// upstream", or a Windows interface name.
    pub source: String,
}

/// The DNS servers this computer is configured with, in order, without
/// duplicates. On Linux with systemd-resolved, `/etc/resolv.conf` names
/// the local stub (127.0.0.53, what applications use); its upstream
/// servers follow.
pub async fn system_servers() -> Result<Vec<SystemDnsServer>> {
    super::blocking(
        "reading the DNS configuration",
        super::OS_CALL_TIMEOUT,
        || {
            #[cfg(windows)]
            {
                super::windows::dns_servers()
            }
            #[cfg(not(windows))]
            {
                let mut out: Vec<SystemDnsServer> = Vec::new();
                for (path, source) in [
                    ("/etc/resolv.conf", "/etc/resolv.conf"),
                    (
                        "/run/systemd/resolve/resolv.conf",
                        "systemd-resolved upstream",
                    ),
                ] {
                    if let Ok(text) = std::fs::read_to_string(path) {
                        for address in parse_resolv_conf(&text) {
                            if !out.iter().any(|s| s.address == address) {
                                out.push(SystemDnsServer {
                                    address,
                                    source: source.into(),
                                });
                            }
                        }
                    }
                }
                Ok(out)
            }
        },
    )
    .await
}

/// `nameserver` lines of a resolv.conf (zone IDs on link-local IPv6
/// addresses are dropped, so those servers are skipped).
pub fn parse_resolv_conf(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next()? == "nameserver").then_some(())?;
            words.next()?.parse().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_are_checked() {
        assert_eq!(record_type("aaaa").unwrap(), RecordType::AAAA);
        assert!(record_type("AXFR").is_err());
        assert_eq!(
            query_name("192.0.2.5", RecordType::PTR)
                .unwrap()
                .to_string(),
            "5.2.0.192.in-addr.arpa."
        );
        assert_eq!(
            query_name("example.com", RecordType::A)
                .unwrap()
                .to_string(),
            "example.com."
        );
        assert!(query_name("  ", RecordType::A).is_err());
        assert_eq!(
            parse_server("1.1.1.1").unwrap(),
            "1.1.1.1:53".parse().unwrap()
        );
        assert_eq!(
            parse_server("[::1]:5353").unwrap(),
            "[::1]:5353".parse().unwrap()
        );
        assert_eq!(parse_server("::1").unwrap(), "[::1]:53".parse().unwrap());
        assert!(parse_server("dns.google").is_err());
    }

    #[test]
    fn resolv_conf_nameservers() {
        let text = "# generated\nnameserver 127.0.0.53\noptions edns0\nnameserver 2001:db8::53\n\
                    nameserver fe80::1%wlan0\nsearch lan\n";
        assert_eq!(
            parse_resolv_conf(text),
            vec![
                "127.0.0.53".parse::<IpAddr>().unwrap(),
                "2001:db8::53".parse().unwrap()
            ]
        );
    }

    /// A tiny DNS server on loopback: answers A queries over UDP (or says
    /// truncated) and over TCP.
    async fn fake_server(truncate_udp: bool) -> SocketAddr {
        use hickory_proto::rr::rdata::A;
        use hickory_proto::rr::RData;
        let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = udp.local_addr().unwrap();
        let tcp = tokio::net::TcpListener::bind(addr).await.unwrap();
        let answer = |req: &[u8], truncate: bool| -> Vec<u8> {
            let req = Message::from_vec(req).unwrap();
            let mut resp = Message::response(req.metadata.id, OpCode::Query);
            resp.metadata.recursion_available = true;
            resp.add_queries(req.queries.clone());
            if truncate {
                resp.metadata.truncation = true;
            } else {
                let name = req.queries[0].name().clone();
                resp.answers.push(Record::from_rdata(
                    name,
                    300,
                    RData::A(A::new(192, 0, 2, 7)),
                ));
            }
            resp.to_vec().unwrap()
        };
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            while let Ok((n, peer)) = udp.recv_from(&mut buf).await {
                let _ = udp.send_to(&answer(&buf[..n], truncate_udp), peer).await;
            }
        });
        tokio::spawn(async move {
            while let Ok((mut s, _)) = tcp.accept().await {
                let mut len = [0u8; 2];
                s.read_exact(&mut len).await.unwrap();
                let mut req = vec![0u8; usize::from(u16::from_be_bytes(len))];
                s.read_exact(&mut req).await.unwrap();
                let resp = answer(&req, false);
                let mut out = (resp.len() as u16).to_be_bytes().to_vec();
                out.extend(resp);
                s.write_all(&out).await.unwrap();
            }
        });
        addr
    }

    fn query(server: SocketAddr, transport: DnsTransport) -> DnsQuery {
        DnsQuery {
            name: "printer.lan".into(),
            record_type: "A".into(),
            server,
            transport,
            timeout: Duration::from_secs(2),
        }
    }

    #[tokio::test]
    async fn udp_tcp_and_truncation_retry() {
        let server = fake_server(false).await;
        let r = lookup(None, &query(server, DnsTransport::Udp), &Cancel::never())
            .await
            .unwrap();
        assert_eq!(
            (r.transport, r.retried_over_tcp),
            (DnsTransport::Udp, false)
        );
        assert_eq!(r.response_code, "NoError");
        assert!(r.recursion_available);
        assert_eq!(r.records.len(), 1);
        assert_eq!(
            (
                r.records[0].data.as_str(),
                r.records[0].ttl,
                r.records[0].section
            ),
            ("192.0.2.7", 300, DnsSection::Answer)
        );
        assert_eq!(r.query_name, "printer.lan.");

        let r = lookup(None, &query(server, DnsTransport::Tcp), &Cancel::never())
            .await
            .unwrap();
        assert_eq!(r.transport, DnsTransport::Tcp);

        let truncating = fake_server(true).await;
        let r = lookup(
            None,
            &query(truncating, DnsTransport::Udp),
            &Cancel::never(),
        )
        .await
        .unwrap();
        assert_eq!((r.transport, r.retried_over_tcp), (DnsTransport::Tcp, true));
        assert_eq!(r.records.len(), 1);
    }

    #[tokio::test]
    async fn silent_server_times_out() {
        // Bound but never answers.
        let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut q = query(silent.local_addr().unwrap(), DnsTransport::Udp);
        q.timeout = Duration::from_millis(200);
        let e = lookup(None, &q, &Cancel::never()).await.unwrap_err();
        assert!(
            matches!(e, TestError::Failed(ref e) if e.kind() == "timeout"),
            "{e}"
        );
    }
}
