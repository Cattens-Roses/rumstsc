//! T.125 MCS and T.124 GCC codecs used by the basic RDP connection sequence.
use crate::{
    Error, Result, invalid,
    wire::{self, Reader, u16le, u32le},
};

fn block(kind: u16, data: &[u8]) -> Vec<u8> {
    let mut b = vec![];
    u16le(&mut b, kind);
    u16le(&mut b, (data.len() + 4) as u16);
    b.extend(data);
    b
}
fn domain(values: [u32; 8]) -> Vec<u8> {
    let mut out = vec![];
    for value in values {
        let bytes = value.to_be_bytes();
        let i = bytes.iter().position(|&x| x != 0).unwrap_or(3);
        let mut v = bytes[i..].to_vec();
        if v[0] & 128 != 0 {
            v.insert(0, 0);
        }
        out.extend(wire::ber(&[2], &v));
    }
    wire::ber(&[0x30], &out)
}
pub fn connect_initial(width: u16, height: u16) -> Result<Vec<u8>> {
    connect_initial_with(width, height, 0, false)
}
pub fn connect_initial_with(
    width: u16,
    height: u16,
    selected: u32,
    legacy: bool,
) -> Result<Vec<u8>> {
    connect_initial_graphics(width, height, selected, legacy, false)
}
pub fn connect_initial_graphics(
    width: u16,
    height: u16,
    selected: u32,
    legacy: bool,
    graphics: bool,
) -> Result<Vec<u8>> {
    let mut core = vec![];
    u32le(&mut core, 0x00080004);
    u16le(&mut core, width);
    u16le(&mut core, height);
    u16le(&mut core, 0xca01);
    u16le(&mut core, 0xaa03);
    u32le(&mut core, 0x409);
    u32le(&mut core, 2600);
    let mut name = [0u8; 32];
    for (i, c) in "rumstsc".encode_utf16().enumerate() {
        name[i * 2..i * 2 + 2].copy_from_slice(&c.to_le_bytes());
    }
    core.extend(name);
    u32le(&mut core, 4);
    u32le(&mut core, 0);
    u32le(&mut core, 12);
    core.extend([0; 64]);
    u16le(&mut core, 0xca01);
    u16le(&mut core, 1);
    u32le(&mut core, 0);
    u16le(&mut core, 24);
    u16le(&mut core, 7);
    u16le(&mut core, 0);
    core.extend([0; 64]);
    core.extend([6, 0]);
    u32le(&mut core, selected);
    debug_assert_eq!(core.len(), 212);
    if graphics {
        core[138..140].copy_from_slice(&15u16.to_le_bytes());
        core[140..142].copy_from_slice(&0x182u16.to_le_bytes());
    }
    let mut blocks = block(0xc001, &core);
    let mut security = vec![];
    u32le(&mut security, if legacy { 0x0b } else { 0 });
    u32le(&mut security, 0);
    blocks.extend(block(0xc002, &security));
    let mut network = vec![];
    u32le(&mut network, if graphics { 1 } else { 0 });
    if graphics {
        network.extend(b"drdynvc\0");
        u32le(&mut network, 0x80000000);
    }
    blocks.extend(block(0xc003, &network));
    if graphics {
        blocks.extend(block(0xc006, &0u32.to_le_bytes()));
    }
    let mut gcc = vec![0, 5, 0, 0x14, 0x7c, 0, 1];
    let mut conference = vec![0, 8, 0, 0x10, 0, 1, 0xc0, 0, b'D', b'u', b'c', b'a'];
    wire::per_length(&mut conference, blocks.len())?;
    conference.extend(blocks);
    wire::per_length(&mut gcc, conference.len())?;
    gcc.extend(conference);
    let mut body = vec![4, 1, 1, 4, 1, 1, 1, 1, 0xff];
    body.extend(domain([34, 2, 0, 1, 0, 1, 65535, 2]));
    body.extend(domain([1, 1, 1, 1, 0, 1, 1056, 2]));
    body.extend(domain([65535, 64535, 65535, 1, 0, 1, 65535, 2]));
    body.extend(wire::ber(&[4], &gcc));
    Ok(wire::ber(&[0x7f, 0x65], &body))
}

#[derive(Debug)]
pub struct ServerSettings {
    pub channel: u16,
    pub static_channels: Vec<u16>,
    pub message_channel: Option<u16>,
    pub encryption_method: u32,
    pub encryption_level: u32,
    pub server_random: Vec<u8>,
    pub certificate: Vec<u8>,
    pub requested_protocols: Option<u32>,
}
pub fn connect_response(data: &[u8]) -> Result<ServerSettings> {
    let mut outer = Reader::new(data);
    let mut r = Reader::new(outer.ber(&[0x7f, 0x66])?);
    if outer.remaining() != 0 {
        return Err(invalid("trailing MCS response"));
    }
    if r.ber(&[0x0a])? != [0] {
        return Err(invalid("MCS connection rejected"));
    }
    r.ber(&[2])?;
    r.ber(&[0x30])?;
    let gcc = r.ber(&[4])?;
    // GCC response carries one H.221 nonstandard key, McDn. This key is not
    // searched outside the declared BER userData boundary.
    if !gcc.starts_with(&[0, 5, 0, 0x14, 0x7c, 0, 1]) {
        return Err(invalid("GCC object identifier"));
    }
    let key = gcc
        .windows(4)
        .position(|v| v == b"McDn")
        .ok_or_else(|| invalid("GCC server key"))?;
    let mut g = Reader::new(&gcc[key + 4..]);
    let n = g.per_length()?;
    let mut blocks = Reader::new(g.take(n)?);
    if g.remaining() != 0 {
        return Err(invalid("GCC trailing user data"));
    }
    let mut channel = None;
    let mut static_channels = vec![];
    let mut message_channel = None;
    let mut has_message_channel = false;
    let mut security = None;
    let mut has_core = false;
    let mut server_random = vec![];
    let mut certificate = vec![];
    let mut requested_protocols = None;
    while blocks.remaining() > 0 {
        let kind = blocks.u16()?;
        let len = blocks.u16()? as usize;
        if len < 4 {
            return Err(invalid("GCC block length"));
        }
        let mut b = Reader::new(blocks.take(len - 4)?);
        match kind {
            0x0c01 => {
                b.u32()?;
                has_core = true;
                if b.remaining() >= 4 {
                    requested_protocols = Some(b.u32()?);
                }
            }
            0x0c02 => {
                if security.is_some() {
                    return Err(invalid("duplicate security block"));
                }
                let method = b.u32()?;
                let level = b.u32()?;
                if method != 0 || level != 0 {
                    let random_len = b.u32()? as usize;
                    let cert_len = b.u32()? as usize;
                    if random_len != 32 || cert_len > 32768 {
                        return Err(invalid("legacy encryption security data lengths"));
                    }
                    server_random = b.take(random_len)?.to_vec();
                    certificate = b.take(cert_len)?.to_vec();
                }
                if b.remaining() != 0 {
                    return Err(invalid("server security block trailing bytes"));
                }
                security = Some((method, level));
            }
            0x0c03 => {
                if channel.is_some() {
                    return Err(invalid("duplicate network block"));
                }
                channel = Some(b.u16()?);
                let count = b.u16()? as usize;
                if count > 31 {
                    return Err(invalid("server channel count"));
                }
                for _ in 0..count {
                    let id = b.u16()?;
                    if id == 0 || channel == Some(id) || static_channels.contains(&id) {
                        return Err(invalid("server channel identity"));
                    }
                    static_channels.push(id);
                }
                if !count.is_multiple_of(2) {
                    b.u16()?;
                }
                if b.remaining() != 0 {
                    return Err(invalid("server network block length"));
                }
            }
            0x0c04 => {
                if has_message_channel {
                    return Err(invalid("duplicate message channel"));
                }
                has_message_channel = true;
                let id = b.u16()?;
                if b.remaining() != 0 {
                    return Err(invalid("message channel length"));
                }
                message_channel = (id != 0).then_some(id);
            }
            _ => {}
        }
    }
    if message_channel.is_some_and(|id| channel == Some(id) || static_channels.contains(&id)) {
        return Err(invalid("duplicate MCS channel identity"));
    }
    if !has_core {
        return Err(invalid("missing server core data"));
    }
    let (encryption_method, encryption_level) =
        security.ok_or_else(|| invalid("missing server security data"))?;
    Ok(ServerSettings {
        channel: channel.ok_or_else(|| invalid("missing server channel"))?,
        static_channels,
        message_channel,
        encryption_method,
        encryption_level,
        server_random,
        certificate,
        requested_protocols,
    })
}
pub fn attach_confirm(data: &[u8]) -> Result<u16> {
    let mut r = Reader::new(data);
    if r.u8()? != 0x2e || r.u8()? != 0 {
        return Err(invalid("MCS attach rejected"));
    }
    let user = r
        .be16()?
        .checked_add(1001)
        .ok_or_else(|| invalid("MCS user ID overflow"))?;
    if r.remaining() != 0 {
        return Err(invalid("MCS attach trailing data"));
    }
    Ok(user)
}
pub fn join(user: u16, channel: u16) -> Vec<u8> {
    let mut p = vec![0x38];
    p.extend((user - 1001).to_be_bytes());
    p.extend(channel.to_be_bytes());
    p
}
pub fn join_confirm(data: &[u8], user: u16, channel: u16) -> Result<()> {
    let mut r = Reader::new(data);
    let kind = r.u8()?;
    if kind & 0xfc != 0x3c || r.u8()? != 0 || r.be16()? != user - 1001 || r.be16()? != channel {
        return Err(invalid("MCS channel join rejected"));
    }
    if kind & 2 != 0 && r.be16()? != channel {
        return Err(invalid("MCS joined channel mismatch"));
    }
    if r.remaining() != 0 {
        return Err(invalid("MCS join trailing data"));
    }
    Ok(())
}
pub fn send_data(user: u16, channel: u16, payload: &[u8]) -> Result<Vec<u8>> {
    let mut out = vec![0x64];
    out.extend((user - 1001).to_be_bytes());
    out.extend(channel.to_be_bytes());
    out.push(0x70);
    wire::per_length(&mut out, payload.len())?;
    out.extend(payload);
    Ok(out)
}
pub fn receive_data(data: &[u8], channel: u16) -> Result<&[u8]> {
    let (received, payload) = receive_channel_data(data)?;
    if received != channel {
        return Err(invalid("unexpected MCS channel"));
    }
    Ok(payload)
}
pub fn receive_channel_data(data: &[u8]) -> Result<(u16, &[u8])> {
    let mut r = Reader::new(data);
    let kind = r.u8()?;
    if kind & 0xfc == 0x20 {
        return Err(Error::Protocol(
            "server disconnected the MCS session".into(),
        ));
    }
    if kind != 0x68 {
        return Err(invalid("expected MCS SendDataIndication"));
    }
    r.be16()?;
    let channel = r.be16()?;
    if r.u8()? != 0x70 {
        return Err(Error::Unsupported("segmented MCS data".into()));
    }
    let n = r.per_length()?;
    let payload = r.take(n)?;
    if r.remaining() != 0 {
        return Err(invalid("MCS payload length"));
    }
    Ok((channel, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_lengths_are_consistent() {
        let initial = connect_initial(1280, 720).unwrap();
        let mut r = Reader::new(&initial);
        let body = r.ber(&[0x7f, 0x65]).unwrap();
        assert_eq!(r.remaining(), 0);
        assert!(body.windows(4).any(|v| v == b"Duca"));
    }
    #[test]
    fn channel_codec() {
        let mut p = send_data(1005, 1003, &[1, 2, 3]).unwrap();
        p[0] = 0x68;
        assert_eq!(receive_data(&p, 1003).unwrap(), [1, 2, 3]);
        assert!(receive_data(&p, 1004).is_err());
        for n in 0..p.len() {
            assert!(receive_data(&p[..n], 1003).is_err());
        }
        assert_eq!(attach_confirm(&[0x2e, 0, 0, 4]).unwrap(), 1005);
        assert!(attach_confirm(&[0x2e, 0, 0xff, 0xff]).is_err());
    }
}
