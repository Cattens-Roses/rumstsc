//! MS-RDPECLIP text clipboard. No file, image or stream capabilities are advertised.
use crate::{
    Result, invalid,
    wire::{Reader, u16le, u32le},
};
use std::collections::VecDeque;

pub const LIMIT: usize = 512 * 1024;
pub(crate) struct Local {
    pub text: Option<String>,
    pub request: Option<u64>,
}
pub(crate) enum Action {
    Ready,
    Text(String),
    Settled(bool),
}
pub(crate) fn packet(kind: u16, flags: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![];
    u16le(&mut bytes, kind);
    u16le(&mut bytes, flags);
    u32le(&mut bytes, body.len() as u32);
    bytes.extend(body);
    bytes
}
#[derive(Default)]
pub(crate) struct Clipboard {
    pub outgoing: VecDeque<Vec<u8>>,
    pub ready: bool,
    local: Option<String>,
    dirty: bool,
    waiting_ack: bool,
    revision: u64,
    requested: Option<u64>,
    remote_text: bool,
    fragments: Vec<u8>,
    total: usize,
}
impl Clipboard {
    pub fn set_local(&mut self, text: Option<String>) -> Result<()> {
        let text = text.map(|text| {
            text.replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace('\n', "\r\n")
        });
        if text
            .as_ref()
            .is_some_and(|s| s.encode_utf16().count() * 2 + 10 > LIMIT || s.contains('\0'))
        {
            return Err(invalid("clipboard text exceeds 512 KiB or contains NUL"));
        }
        self.local = text;
        self.revision = self.revision.wrapping_add(1); // invalidate an older remote read
        self.remote_text = false;
        self.dirty = true;
        self.announce();
        Ok(())
    }
    fn announce(&mut self) {
        if !self.ready || !self.dirty || self.waiting_ack {
            return;
        }
        self.dirty = false;
        self.waiting_ack = true;
        let mut formats = vec![];
        if self.local.is_some() {
            u32le(&mut formats, 13); // CF_UNICODETEXT, fixed short-format empty name
            formats.extend([0; 32]);
        }
        self.outgoing.push_back(packet(2, 0, &formats));
    }
    pub fn chunk(&mut self, total: usize, flags: u32, bytes: &[u8]) -> Result<Vec<Action>> {
        if !(8..=LIMIT).contains(&total) {
            return Err(invalid("clipboard channel length"));
        }
        if flags & 1 != 0 {
            if !self.fragments.is_empty() {
                return Err(invalid("overlapping clipboard fragments"));
            }
            self.total = total;
        } else if self.fragments.is_empty() {
            return Err(invalid("clipboard continuation without start"));
        }
        if self.total != total || self.fragments.len() + bytes.len() > total {
            return Err(invalid("clipboard fragment length"));
        }
        self.fragments.extend(bytes);
        if flags & 2 == 0 {
            return Ok(vec![]);
        }
        if self.fragments.len() != total {
            return Err(invalid("incomplete clipboard message"));
        }
        let message = std::mem::take(&mut self.fragments);
        self.receive(&message)
    }
    fn request_text(&mut self) {
        if self.remote_text && self.requested.is_none() {
            self.requested = Some(self.revision);
            self.outgoing.push_back(packet(4, 0, &13u32.to_le_bytes()));
        }
    }
    fn receive(&mut self, bytes: &[u8]) -> Result<Vec<Action>> {
        let mut r = Reader::new(bytes);
        let kind = r.u16()?;
        let flags = r.u16()?;
        let n = r.u32()? as usize;
        // MS-RDPECLIP 6, Appendix A <1>: Windows may append four bytes
        // outside dataLen. Parse only the declared body; never interpret the
        // optional trailer as capability, format, or UTF-16 data.
        if n > r.remaining() || ![0, 4].contains(&(r.remaining() - n)) {
            return Err(invalid(&format!(
                "clipboard PDU length (type {kind:#06x}, declared {n}, received {})",
                r.remaining()
            )));
        }
        let body = r.take(n)?;
        let mut actions = vec![];
        match kind {
            7 => {
                // Capability sets: we deliberately negotiate no optional flags.
                let mut r = Reader::new(body);
                let count = r.u16()?;
                r.u16()?;
                if count > 64 {
                    return Err(invalid("clipboard capability count"));
                }
                for _ in 0..count {
                    let _kind = r.u16()?;
                    let size = r.u16()? as usize;
                    if size < 4 {
                        return Err(invalid("clipboard capability length"));
                    }
                    r.take(size - 4)?;
                }
                if r.remaining() != 0 {
                    return Err(invalid("clipboard capability trailer"));
                }
            }
            1 => {
                if n != 0 || self.ready {
                    return Err(invalid("clipboard monitor ready"));
                }
                self.ready = true;
                self.outgoing.push_back(packet(
                    7,
                    0,
                    &[1, 0, 0, 0, 1, 0, 12, 0, 2, 0, 0, 0, 0, 0, 0, 0],
                ));
                self.dirty = true;
                self.announce();
                actions.push(Action::Ready);
            }
            2 => {
                if !body.len().is_multiple_of(36) || body.len() / 36 > 4096 {
                    return Err(invalid("clipboard short formats"));
                }
                self.revision = self.revision.wrapping_add(1);
                self.remote_text = body
                    .as_chunks::<36>()
                    .0
                    .iter()
                    .any(|format| format[..4] == 13u32.to_le_bytes());
                self.outgoing.push_back(packet(3, 1, &[]));
                self.request_text();
            }
            3 => {
                if n != 0 || ![1, 2].contains(&flags) {
                    return Err(invalid("clipboard format acknowledgement"));
                }
                if self.waiting_ack {
                    self.waiting_ack = false;
                    if self.dirty {
                        self.announce();
                    } else {
                        actions.push(Action::Settled(flags == 1));
                    }
                }
            }
            4 => {
                if let Some(text) = self.local.as_ref().filter(|_| body == 13u32.to_le_bytes()) {
                    let data: Vec<u8> = text
                        .encode_utf16()
                        .chain([0])
                        .flat_map(u16::to_le_bytes)
                        .collect();
                    self.outgoing.push_back(packet(5, 1, &data));
                } else {
                    self.outgoing.push_back(packet(5, 2, &[]));
                }
            }
            5 => {
                let requested = self
                    .requested
                    .take()
                    .ok_or_else(|| invalid("unsolicited clipboard data"))?;
                if ![1, 2].contains(&flags) {
                    return Err(invalid("clipboard data response flags"));
                }
                if requested == self.revision && flags == 1 {
                    if body.len() < 2
                        || !body.len().is_multiple_of(2)
                        || body[body.len() - 2..] != [0, 0]
                    {
                        return Err(invalid("clipboard UTF-16 terminator"));
                    }
                    let units: Vec<u16> = body
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .take_while(|u| *u != 0)
                        .collect();
                    let text =
                        String::from_utf16(&units).map_err(|_| invalid("clipboard UTF-16"))?;
                    actions.push(Action::Text(text));
                    self.remote_text = false;
                } else if requested != self.revision {
                    self.request_text();
                }
            }
            6 => {} // server temporary directory, no file transfer support
            _ => {} // no optional capabilities are advertised
        }
        if self.outgoing.len() > 16 {
            return Err(invalid("clipboard reply queue limit"));
        }
        Ok(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receive(c: &mut Clipboard, p: Vec<u8>) -> Vec<Action> {
        c.chunk(p.len(), 3, &p).unwrap()
    }
    #[test]
    fn windows_four_byte_trailer_is_outside_declared_pdu_body() {
        // MS-RDPECLIP Appendix A <1>: Windows appends four bytes that are not
        // included in dataLen. Their values are not part of the clipboard PDU.
        fn padded(kind: u16, flags: u16, body: &[u8]) -> Vec<u8> {
            let mut p = packet(kind, flags, body);
            p.extend([0xa5, 0x5a, 0x12, 0x34]);
            p
        }
        let mut c = Clipboard::default();
        receive(
            &mut c,
            padded(7, 0, &[1, 0, 0, 0, 1, 0, 12, 0, 2, 0, 0, 0, 2, 0, 0, 0]),
        );
        assert!(matches!(
            receive(&mut c, padded(1, 0, &[]))[0],
            Action::Ready
        ));
        c.outgoing.clear();
        assert!(matches!(
            receive(&mut c, padded(3, 1, &[]))[0],
            Action::Settled(true)
        ));
        c.set_local(Some("local @".into())).unwrap();
        c.outgoing.clear();
        receive(&mut c, padded(4, 0, &13u32.to_le_bytes()));
        let local: Vec<u8> = "local @"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(c.outgoing.pop_front().unwrap(), packet(5, 1, &local));
        let mut formats = 13u32.to_le_bytes().to_vec();
        formats.extend([0; 32]);
        receive(&mut c, padded(2, 0, &formats));
        let remote: Vec<u8> = "ż@"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(matches!(&receive(&mut c,padded(5,1,&remote))[0],Action::Text(text) if text=="ż@"));
        for extra in [1, 2, 3, 5, 8] {
            let mut p = packet(1, 0, &[]);
            p.resize(p.len() + extra, 0);
            assert!(Clipboard::default().chunk(p.len(), 3, &p).is_err());
        }
        let mut truncated = packet(7, 0, &[0; 16]);
        truncated.pop();
        assert!(
            Clipboard::default()
                .chunk(truncated.len(), 3, &truncated)
                .is_err()
        );
    }
    #[test]
    fn unicode_bidirectional_and_serialized_advertisements() {
        let mut c = Clipboard::default();
        c.set_local(Some("Zażółć 🦀\n@".into())).unwrap();
        receive(&mut c, packet(1, 0, &[]));
        assert_eq!(c.outgoing.pop_front().unwrap()[..2], [7, 0]);
        assert_eq!(c.outgoing.pop_front().unwrap().len(), 44);
        c.set_local(Some("new 🦀".into())).unwrap();
        assert!(c.outgoing.is_empty());
        assert!(receive(&mut c, packet(3, 1, &[])).is_empty());
        c.outgoing.clear();
        assert!(matches!(
            receive(&mut c, packet(3, 1, &[]))[0],
            Action::Settled(true)
        ));
        receive(&mut c, packet(4, 0, &13u32.to_le_bytes()));
        let response = c.outgoing.pop_front().unwrap();
        let expected: Vec<u8> = "new 🦀"
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(response, packet(5, 1, &expected));
        let mut formats = 13u32.to_le_bytes().to_vec();
        formats.extend([0; 32]);
        receive(&mut c, packet(2, 0, &formats));
        c.outgoing.clear();
        assert!(matches!(&receive(&mut c,packet(5,1,&expected))[0],Action::Text(s) if s=="new 🦀"));
    }
    #[test]
    fn fragmented_messages_stale_responses_and_bounds() {
        let mut c = Clipboard::default();
        let p = packet(1, 0, &[]);
        assert!(c.chunk(p.len(), 1, &p[..3]).unwrap().is_empty());
        assert!(matches!(
            c.chunk(p.len(), 2, &p[3..]).unwrap()[0],
            Action::Ready
        ));
        assert!(c.chunk(LIMIT + 1, 3, &[]).is_err());
        assert!(c.set_local(Some("a".repeat(LIMIT))).is_err());
        let mut formats = 13u32.to_le_bytes().to_vec();
        formats.extend([0; 32]);
        receive(&mut c, packet(2, 0, &formats));
        c.set_local(Some("local wins".into())).unwrap();
        assert!(receive(&mut c, packet(5, 1, &[65, 0, 0, 0])).is_empty());
        assert!(c.chunk(9, 3, &p).is_err());
    }
}
