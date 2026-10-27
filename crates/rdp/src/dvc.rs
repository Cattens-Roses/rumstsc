//! Static drdynvc fragmentation and DVC v1/v2 transport, implemented from the wire.
use crate::{
    Result, invalid,
    wire::{Reader, u32le},
};
const LIMIT: usize = 64 * 1024 * 1024;
#[derive(Default)]
pub(crate) struct Dvc {
    static_data: Vec<u8>,
    static_size: Option<usize>,
    id: Option<u32>,
    data: Vec<u8>,
    size: Option<usize>,
}
pub(crate) enum Action {
    Reply(Vec<u8>),
    Graphics(Vec<u8>),
    Opened,
    Closed,
}
fn integer(r: &mut Reader<'_>, size: u8) -> Result<u32> {
    match size {
        0 => Ok(r.u8()? as u32),
        1 => Ok(r.u16()? as u32),
        2 => r.u32(),
        _ => Err(invalid("DVC integer width")),
    }
}
fn header(command: u8, id: u32) -> Vec<u8> {
    let mut p = vec![command << 4 | 2];
    u32le(&mut p, id);
    p
}
impl Dvc {
    pub fn send(&self, data: &[u8]) -> Result<Vec<Vec<u8>>> {
        let id = self
            .id
            .ok_or_else(|| invalid("graphics channel is closed"))?;
        let mut packets = vec![];
        for (i, chunk) in data.chunks(1500).enumerate() {
            let mut p = header(if i == 0 && data.len() > 1500 { 2 } else { 3 }, id);
            if i == 0 && data.len() > 1500 {
                p[0] |= 2 << 2;
                u32le(&mut p, data.len() as u32);
            }
            p.extend(chunk);
            packets.push(p);
        }
        Ok(packets)
    }
    pub fn receive(&mut self, payload: &[u8]) -> Result<Vec<Action>> {
        let mut r = Reader::new(payload);
        let size = r.u32()? as usize;
        let flags = r.u32()?;
        if size > LIMIT || flags & !0x13 != 0 {
            return Err(invalid("static graphics channel flags/size"));
        }
        if flags & 1 != 0 {
            if self.static_size.is_some() {
                return Err(invalid("nested static fragment"));
            }
            self.static_size = Some(size);
            self.static_data.clear();
        }
        if self.static_size != Some(size) {
            return Err(invalid("orphan static fragment"));
        }
        if r.remaining() > size.saturating_sub(self.static_data.len()) {
            return Err(invalid("static fragment overflow"));
        }
        self.static_data.extend(r.rest());
        if flags & 2 == 0 {
            return Ok(vec![]);
        }
        self.static_size = None;
        if self.static_data.len() != size {
            return Err(invalid("static fragment length"));
        }
        let data = std::mem::take(&mut self.static_data);
        let mut r = Reader::new(&data);
        let control = r.u8()?;
        let mut actions = vec![];
        if control >> 4 == 5 {
            r.u8()?;
            let version = r.u16()?;
            if !(1..=3).contains(&version) {
                return Err(invalid("DVC version"));
            }
            if version >= 2 {
                r.take(8)?;
            }
            if r.remaining() != 0 {
                return Err(invalid("DVC capabilities length"));
            }
            // Version 2 adds priority classes; one graphics channel needs no
            // competing-class scheduler. Never advertise v3 compressed DVC data.
            // Some servers leave graphics unopened if the response selects v1.
            actions.push(Action::Reply(vec![0x50, 0, version.min(2) as u8, 0]));
            return Ok(actions);
        }
        let id = integer(&mut r, control & 3)?;
        match control >> 4 {
            1 => {
                let name = r.rest();
                let accepted = name == b"Microsoft::Windows::RDS::Graphics\0" && self.id.is_none();
                let mut reply = header(1, id);
                u32le(&mut reply, if accepted { 0 } else { 0xc000000d });
                actions.push(Action::Reply(reply));
                if accepted {
                    self.id = Some(id);
                    actions.push(Action::Opened);
                }
            }
            2 | 3 => {
                if self.id != Some(id) {
                    return Err(invalid("unknown graphics DVC"));
                }
                if control >> 4 == 2 {
                    let size = integer(&mut r, (control >> 2) & 3)? as usize;
                    if size > LIMIT || self.size.is_some() {
                        return Err(invalid("DVC data-first length"));
                    }
                    self.size = Some(size);
                    self.data.clear();
                }
                if let Some(size) = self.size {
                    if r.remaining() > size.saturating_sub(self.data.len()) {
                        return Err(invalid("DVC data overflow"));
                    }
                    self.data.extend(r.rest());
                    if self.data.len() == size {
                        self.size = None;
                        actions.push(Action::Graphics(std::mem::take(&mut self.data)));
                    }
                } else {
                    actions.push(Action::Graphics(r.rest().to_vec()));
                }
            }
            4 => {
                if self.id == Some(id) {
                    self.id = None;
                    self.size = None;
                    self.data.clear();
                    actions.push(Action::Reply(header(4, id)));
                    actions.push(Action::Closed);
                }
            }
            _ => return Err(invalid("unsupported DVC command")),
        }
        Ok(actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn channel(p: &[u8]) -> Vec<u8> {
        let mut v = vec![];
        u32le(&mut v, p.len() as u32);
        u32le(&mut v, 3);
        v.extend(p);
        v
    }
    #[test]
    fn capabilities_creation_fragmentation_and_close() {
        let mut d = Dvc::default();
        let caps = d.receive(&channel(&[0x50, 0, 1, 0])).unwrap();
        assert!(matches!(&caps[0],Action::Reply(p) if p==&[0x50,0,1,0]));
        let mut create = vec![0x10, 7];
        create.extend(b"Microsoft::Windows::RDS::Graphics\0");
        let actions = d.receive(&channel(&create)).unwrap();
        assert!(matches!(actions[1], Action::Opened));
        let mut first = vec![0x28, 7];
        u32le(&mut first, 6);
        first.extend(b"abc");
        assert!(d.receive(&channel(&first)).unwrap().is_empty());
        let data = d.receive(&channel(&[0x30, 7, b'd', b'e', b'f'])).unwrap();
        assert!(matches!(&data[0],Action::Graphics(p) if p==b"abcdef"));
        let packets = d.send(&vec![42; 3001]).unwrap();
        assert_eq!(packets.len(), 3);
        assert_eq!(packets[0][0] >> 4, 2);
        assert_eq!(packets[1][0] >> 4, 3);
        assert!(matches!(
            d.receive(&channel(&[0x40, 7])).unwrap()[1],
            Action::Closed
        ));
        assert!(d.send(b"a").is_err());
    }
    #[test]
    fn negotiates_v2_priorities_without_claiming_v3_compression() {
        for version in [2u8, 3] {
            let mut d = Dvc::default();
            let mut request = vec![0x50, 0, version, 0];
            request.extend([1, 0, 2, 0, 3, 0, 4, 0]);
            let actions = d.receive(&channel(&request)).unwrap();
            assert!(matches!(&actions[0], Action::Reply(p) if p == &[0x50, 0, 2, 0]));
            let mut create = vec![0x1c, 9]; // priority class 3, one-byte channel ID
            create.extend(b"Microsoft::Windows::RDS::Graphics\0");
            assert!(matches!(
                d.receive(&channel(&create)).unwrap()[1],
                Action::Opened
            ));
        }
        let mut d = Dvc::default();
        assert!(d.receive(&channel(&[0x50, 0, 2, 0])).is_err());
    }
    #[test]
    fn static_fragment_lengths_and_unrequested_channels() {
        let mut d = Dvc::default();
        let mut first = vec![];
        u32le(&mut first, 4);
        u32le(&mut first, 1);
        first.extend([0x50, 0]);
        assert!(d.receive(&first).unwrap().is_empty());
        let mut last = vec![];
        u32le(&mut last, 4);
        u32le(&mut last, 2);
        last.extend([1, 0]);
        assert_eq!(d.receive(&last).unwrap().len(), 1);
        assert!(d.receive(&last).is_err());
        assert!(d.receive(&channel(&[0x30, 77, 0])).is_err());
        let mut create = vec![0x10, 7];
        create.extend(b"unknown\0");
        assert_eq!(d.receive(&channel(&create)).unwrap().len(), 1);
        assert!(d.id.is_none());
    }
}
