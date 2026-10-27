use crate::{
    Error, Result, invalid,
    wire::{self, Reader},
};
use std::{
    io::Write,
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}
impl Endpoint {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() || input.chars().any(char::is_whitespace) {
            return Err(invalid("enter a hostname or IP address"));
        }
        let (host, port) = if let Some(v) = input.strip_prefix('[') {
            let (host, tail) = v
                .split_once(']')
                .ok_or_else(|| invalid("unclosed IPv6 address"))?;
            if host.parse::<std::net::Ipv6Addr>().is_err() {
                return Err(invalid("IPv6 address"));
            }
            let port = if tail.is_empty() {
                3389
            } else {
                tail.strip_prefix(':')
                    .ok_or_else(|| invalid("IPv6 port"))?
                    .parse()
                    .map_err(|_| invalid("port must be 1–65535"))?
            };
            (host.to_string(), port)
        } else if input.parse::<std::net::Ipv6Addr>().is_ok() {
            (input.into(), 3389)
        } else if let Some((host, port)) = input.rsplit_once(':') {
            if host.contains(':') {
                return Err(invalid("use [IPv6]:port"));
            }
            (
                host.into(),
                port.parse().map_err(|_| invalid("port must be 1–65535"))?,
            )
        } else {
            (input.into(), 3389)
        };
        if host.is_empty() || port == 0 || host.contains(['/', '\\', '@', '[', ']']) {
            return Err(invalid("invalid host or port"));
        }
        Ok(Self { host, port })
    }
    pub fn connect(&self) -> Result<TcpStream> {
        self.connect_inner(None)
    }
    pub fn connect_traced(&self, trace: &crate::trace::Trace) -> Result<TcpStream> {
        self.connect_inner(Some(trace))
    }
    fn connect_inner(&self, trace: Option<&crate::trace::Trace>) -> Result<TcpStream> {
        let addresses = {
            let _span = trace.map(|t| t.span("dns_resolution", "network", 0));
            (self.host.as_str(), self.port).to_socket_addrs()?
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let mut last = None;
        for address in addresses.take(16) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let _span = trace.map(|t| t.span("tcp_connect_attempt", "network", 0));
            match TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(2))) {
                Ok(s) => {
                    s.set_nodelay(true)?;
                    s.set_read_timeout(Some(Duration::from_secs(8)))?;
                    s.set_write_timeout(Some(Duration::from_secs(8)))?;
                    return Ok(s);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last
            .unwrap_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no reachable address")
            })
            .into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Legacy,
    Tls,
    Nla,
    NlaExtended,
    Rdstls,
    Unknown(u32),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SecurityMode {
    #[default]
    Auto,
    Tls,
    Nla,
    Legacy,
}
impl SecurityMode {
    pub fn protocols(self) -> u32 {
        match self {
            Self::Auto => 3,
            Self::Tls => 1,
            Self::Nla => 3,
            Self::Legacy => 0,
        }
    }
    pub fn accepts(self, security: Security) -> bool {
        match self {
            Self::Auto => matches!(security, Security::Tls | Security::Nla),
            Self::Tls => security == Security::Tls,
            Self::Nla => security == Security::Nla,
            Self::Legacy => security == Security::Legacy,
        }
    }
}
impl std::fmt::Display for Security {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Legacy => "Standard RDP",
                Self::Tls => "TLS",
                Self::Nla => "NLA / CredSSP",
                Self::NlaExtended => "NLA extended",
                Self::Rdstls => "RDSTLS",
                Self::Unknown(_) => "unknown security",
            }
        )
    }
}
/// Request only security mechanisms implemented by this engine. There is no retry downgrade.
pub fn request() -> Vec<u8> {
    request_protocols(0)
}
pub fn request_protocols(protocols: u32) -> Vec<u8> {
    let mut p = vec![14, 0xe0, 0, 0, 0, 0, 0, 1, 0, 8, 0];
    p.extend(protocols.to_le_bytes());
    wire::tpkt(&p).unwrap()
}
pub fn parse_confirm(packet: &[u8]) -> Result<Security> {
    parse_confirm_flags(packet).map(|(security, _)| security)
}
fn parse_confirm_flags(packet: &[u8]) -> Result<(Security, u8)> {
    let mut r = Reader::new(packet);
    let len = r.u8()? as usize;
    if len + 1 != packet.len() || len < 6 || r.u8()? != 0xd0 {
        return Err(invalid("X.224 connection confirm"));
    }
    r.take(5)?;
    if r.remaining() == 0 {
        return Ok((Security::Legacy, 0));
    }
    let kind = r.u8()?;
    let flags = r.u8()?;
    if r.u16()? != 8 {
        return Err(invalid("negotiation response length"));
    }
    let value = r.u32()?;
    if r.remaining() != 0 {
        return Err(invalid("trailing negotiation data"));
    }
    if kind == 3 {
        let reason = match value {
            1 => "server requires TLS",
            2 => "server accepts only Standard RDP",
            3 => "server has no TLS certificate",
            4 => "server flags are inconsistent",
            5 => "server requires NLA / CredSSP",
            6 => "server requires TLS with user authentication",
            _ => "unknown server security failure",
        };
        return Err(Error::Unsupported(format!(
            "{reason} (negotiation code {value}); select a compatible security mode"
        )));
    }
    if kind != 2 {
        return Err(invalid("negotiation response type"));
    }
    let security = match value {
        0 => Security::Legacy,
        1 => Security::Tls,
        2 => Security::Nla,
        4 => Security::Rdstls,
        8 => Security::NlaExtended,
        n => Security::Unknown(n),
    };
    Ok((security, flags))
}
pub fn negotiate(stream: &mut TcpStream) -> Result<Security> {
    stream.write_all(&request())?;
    parse_confirm(&wire::read_tpkt(stream)?)
}
pub fn negotiate_mode(stream: &mut TcpStream, mode: SecurityMode) -> Result<Security> {
    negotiate_mode_flags(stream, mode).map(|(security, _)| security)
}
pub(crate) fn negotiate_mode_flags(
    stream: &mut TcpStream,
    mode: SecurityMode,
) -> Result<(Security, u8)> {
    stream.write_all(&request_protocols(mode.protocols()))?;
    let (security, flags) = parse_confirm_flags(&wire::read_tpkt(stream)?)?;
    if !mode.accepts(security) {
        return Err(Error::Security(format!(
            "server selected {security}, refused by {mode:?} security policy; no fallback was attempted"
        )));
    }
    Ok((security, flags))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints() {
        for (s, h, p) in [
            ("server", "server", 3389),
            ("127.0.0.1:3390", "127.0.0.1", 3390),
            ("[::1]:3391", "::1", 3391),
            ("::1", "::1", 3389),
        ] {
            assert_eq!(
                Endpoint::parse(s).unwrap(),
                Endpoint {
                    host: h.into(),
                    port: p
                }
            );
        }
        for s in [
            "",
            "foo:0",
            "foo:65536",
            "rdp://host",
            "[bad]:3",
            "x y",
            "a:b:c",
            "a@b",
        ] {
            assert!(Endpoint::parse(s).is_err(), "{s}");
        }
    }
    #[test]
    fn negotiation_packets() {
        assert_eq!(
            request(),
            [3, 0, 0, 19, 14, 224, 0, 0, 0, 0, 0, 1, 0, 8, 0, 0, 0, 0, 0]
        );
        let mut p = vec![14, 208, 0, 0, 0, 0, 0, 2, 0, 8, 0, 0, 0, 0, 0];
        assert_eq!(parse_confirm(&p).unwrap(), Security::Legacy);
        p[11] = 2;
        p[8] = 1;
        assert_eq!(parse_confirm_flags(&p).unwrap(), (Security::Nla, 1));
        assert_eq!(parse_confirm(&p).unwrap(), Security::Nla);
        p[7] = 3;
        p[11] = 5;
        assert!(parse_confirm(&p).unwrap_err().to_string().contains("NLA"));
        for n in 0..p.len() {
            assert!(parse_confirm(&p[..n]).is_err());
        }
    }
}
