//! Proxy URL parsing + SOCKS5 UDP_ASSOCIATE probe. Mirrors the launcher's
//! pre-launch UDP check that gates QUIC + WebRTC policy.

use std::net::SocketAddr;

use anyhow::{anyhow, Context, Result};
use url::Url;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyScheme {
    Socks5,
    Http,
    Https,
}

impl ProxyScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            ProxyScheme::Socks5 => "socks5",
            ProxyScheme::Http => "http",
            ProxyScheme::Https => "https",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParsedProxy {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

pub fn parse_proxy(url: &str) -> Result<ParsedProxy> {
    let u = Url::parse(url).with_context(|| format!("bad proxy URL: {url}"))?;
    let scheme = match u.scheme() {
        "socks5" | "socks5h" => ProxyScheme::Socks5,
        "http" => ProxyScheme::Http,
        "https" => ProxyScheme::Https,
        other => return Err(anyhow!("Unsupported proxy scheme: {other}")),
    };
    let host = u.host_str().ok_or_else(|| anyhow!("proxy URL missing host"))?;
    let port = u.port().ok_or_else(|| anyhow!("proxy URL missing port"))?;
    let decode = |s: &str| {
        percent_decode(s)
    };
    Ok(ParsedProxy {
        scheme,
        host: host.to_string(),
        port,
        username: decode(u.username()),
        password: decode(u.password().unwrap_or("")),
    })
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={s}").as_bytes())
        .next()
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| s.to_string())
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Format as the ShardX engine's `--proxy-server` argument (URL-encoded
/// `user:pass@` when present). Mirrors the launcher's
/// `ProxyEntry::to_proxy_server_arg`.
pub fn proxy_to_arg(p: &ParsedProxy) -> String {
    let host_port = format!("{}:{}", p.host, p.port);
    if p.username.is_empty() && p.password.is_empty() {
        format!("{}://{host_port}", p.scheme.as_str())
    } else {
        format!(
            "{}://{}:{}@{host_port}",
            p.scheme.as_str(),
            encode(&p.username),
            encode(&p.password)
        )
    }
}

/// Resolve a public STUN server to IPv4 (probe target for the UDP relay).
async fn resolve_stun_ipv4() -> Result<(std::net::Ipv4Addr, u16)> {
    const HOSTS: &[&str] = &[
        "stun.l.google.com:19302",
        "stun1.l.google.com:19302",
        "stun.cloudflare.com:3478",
    ];
    for h in HOSTS {
        if let Ok(addrs) = tokio::net::lookup_host(*h).await {
            for a in addrs {
                if let std::net::IpAddr::V4(v4) = a.ip() {
                    return Ok((v4, a.port()));
                }
            }
        }
    }
    Err(anyhow!("no STUN server resolved to IPv4"))
}

/// UDP RTT (ms) through the SOCKS5 relay. Err when unavailable (caller maps
/// to `None`). Only valid for SOCKS5.
pub async fn probe_udp(entry: &ParsedProxy, timeout_ms: u64) -> Result<u128> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpStream, UdpSocket};
    use tokio::time::{timeout, Duration, Instant};

    if entry.scheme != ProxyScheme::Socks5 {
        return Err(anyhow!("UDP probe only supported for SOCKS5"));
    }
    let dur = Duration::from_millis(timeout_ms);
    let started = Instant::now();
    let mut tcp = timeout(
        dur,
        TcpStream::connect(format!("{}:{}", entry.host, entry.port)),
    )
    .await
    .context("connect timeout")??;

    let auth_method: u8 = if entry.username.is_empty() { 0x00 } else { 0x02 };
    tcp.write_all(&[0x05, 0x01, auth_method]).await?;
    let mut greet = [0u8; 2];
    tcp.read_exact(&mut greet).await?;
    if greet[1] == 0xFF {
        return Err(anyhow!("no acceptable auth method"));
    }
    if auth_method == 0x02 {
        let mut buf = vec![0x01u8];
        buf.push(entry.username.len() as u8);
        buf.extend_from_slice(entry.username.as_bytes());
        buf.push(entry.password.len() as u8);
        buf.extend_from_slice(entry.password.as_bytes());
        tcp.write_all(&buf).await?;
        let mut ar = [0u8; 2];
        tcp.read_exact(&mut ar).await?;
        if ar[1] != 0x00 {
            return Err(anyhow!("auth failed"));
        }
    }
    // UDP_ASSOCIATE: cmd=0x03, ATYP=IPv4, addr=0.0.0.0, port=0
    tcp.write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;
    let mut hdr = [0u8; 4];
    tcp.read_exact(&mut hdr).await?;
    if hdr[1] != 0x00 {
        return Err(anyhow!("UDP_ASSOCIATE refused (rep={:#x})", hdr[1]));
    }
    let bind_addr: SocketAddr = match hdr[3] {
        0x01 => {
            let mut ip = [0u8; 4];
            tcp.read_exact(&mut ip).await?;
            let mut p = [0u8; 2];
            tcp.read_exact(&mut p).await?;
            let port = u16::from_be_bytes(p);
            let v4 = std::net::Ipv4Addr::from(ip);
            if v4.is_unspecified() {
                let peer = tcp.peer_addr()?;
                SocketAddr::new(peer.ip(), port)
            } else {
                SocketAddr::new(std::net::IpAddr::V4(v4), port)
            }
        }
        0x04 => {
            let mut ip = [0u8; 16];
            tcp.read_exact(&mut ip).await?;
            let mut p = [0u8; 2];
            tcp.read_exact(&mut p).await?;
            SocketAddr::new(
                std::net::IpAddr::V6(std::net::Ipv6Addr::from(ip)),
                u16::from_be_bytes(p),
            )
        }
        _ => return Err(anyhow!("unsupported ATYP in UDP reply")),
    };

    let (stun_ip, stun_port) = resolve_stun_ipv4()
        .await
        .context("could not resolve a STUN server")?;

    let udp = UdpSocket::bind("0.0.0.0:0").await?;
    udp.connect(bind_addr).await?;
    let mut pkt: Vec<u8> = Vec::with_capacity(32);
    // SOCKS5 UDP header: RSV(2)=0, FRAG=0, ATYP=IPv4, DST=<stun>, PORT.
    pkt.extend_from_slice(&[0, 0, 0, 0x01]);
    pkt.extend_from_slice(&stun_ip.octets());
    pkt.extend_from_slice(&stun_port.to_be_bytes());
    // STUN Binding Request (RFC 5389): type=0x0001, magic 0x2112A442, 12B txid.
    let mut stun = vec![0x00u8, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42];
    let txid: [u8; 12] = rand::random();
    stun.extend_from_slice(&txid);
    pkt.extend_from_slice(&stun);
    udp.send(&pkt).await?;

    let mut buf = vec![0u8; 1500];
    let n = timeout(dur, udp.recv(&mut buf))
        .await
        .context("UDP reply timeout — proxy doesn't relay UDP")??;
    if n < 20 {
        return Err(anyhow!("UDP reply too short"));
    }
    drop(tcp);
    Ok(started.elapsed().as_millis())
}
