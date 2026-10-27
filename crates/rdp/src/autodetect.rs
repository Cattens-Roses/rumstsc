//! MS-RDPBCGR 2.2.14 network detection over the MCS message channel.
use crate::{
    Error, Result, invalid,
    wire::{Reader, u16le, u32le},
};
use std::time::Instant;
struct Measurement {
    started: Instant,
    bytes: u64,
    continuous: bool,
}
#[derive(Default)]
pub(crate) struct AutoDetect {
    measurement: Option<Measurement>,
}
fn response(sequence: u16, kind: u16, body: &[u8]) -> Vec<u8> {
    let mut p = vec![(6 + body.len()) as u8, 1];
    u16le(&mut p, sequence);
    u16le(&mut p, kind);
    p.extend(body);
    p
}
impl AutoDetect {
    // Continuous measurements count all received MCS user data, excluding security headers.
    pub fn observe(&mut self, bytes: usize) {
        if let Some(m) = &mut self.measurement
            && m.continuous
        {
            m.bytes = m.bytes.saturating_add(bytes as u64);
        }
    }
    pub fn receive(&mut self, data: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut r = Reader::new(data);
        let length = r.u8()? as usize;
        if r.u8()? != 0 {
            return Err(invalid("auto-detect request header type"));
        }
        let sequence = r.u16()?;
        let kind = r.u16()?;
        let expected = match kind {
            0x0002 | 0x002b => 8,
            0x0840 | 0x0880 => 14,
            0x08c0 => 18,
            _ => 6,
        };
        if length != expected {
            return Err(invalid("auto-detect request header length"));
        }
        let reply = match kind {
            0x0001 | 0x1001 => Some(response(sequence, 0, &[])),
            0x0014 | 0x1014 => {
                self.measurement = Some(Measurement {
                    started: Instant::now(),
                    bytes: 0,
                    continuous: kind == 0x0014,
                });
                None
            }
            0x0002 => {
                self.payload(&mut r, data.len(), false)?;
                None
            }
            0x002b | 0x0429 => {
                if kind == 0x002b {
                    self.payload(&mut r, data.len(), true)?;
                }
                let m = self
                    .measurement
                    .take()
                    .ok_or_else(|| invalid("auto-detect stop before start"))?;
                if m.continuous != (kind == 0x0429) {
                    return Err(invalid("auto-detect measurement mode"));
                }
                let mut body = vec![];
                u32le(
                    &mut body,
                    m.started.elapsed().as_millis().min(u32::MAX as u128) as u32,
                );
                u32le(&mut body, m.bytes.min(u32::MAX as u64) as u32);
                Some(response(
                    sequence,
                    if kind == 0x002b { 3 } else { 11 },
                    &body,
                ))
            }
            0x0840 | 0x0880 | 0x08c0 => {
                r.take(length - 6)?;
                None
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "auto-detect request 0x{kind:04x}"
                )));
            }
        };
        if r.remaining() != 0 {
            return Err(invalid("auto-detect trailing data"));
        }
        Ok(reply)
    }
    fn payload(&mut self, r: &mut Reader<'_>, total: usize, stop: bool) -> Result<()> {
        let count = r.u16()? as usize;
        if stop && count == 0 {
            return Err(invalid("empty auto-detect stop payload"));
        }
        r.take(count)?;
        let m = self
            .measurement
            .as_mut()
            .ok_or_else(|| invalid("auto-detect payload before start"))?;
        if !m.continuous {
            m.bytes = m.bytes.saturating_add(total as u64);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connect_time_and_continuous_measurements() {
        let mut a = AutoDetect::default();
        assert_eq!(
            a.receive(&[6, 0, 9, 0, 1, 16]).unwrap().unwrap(),
            [6, 1, 9, 0, 0, 0]
        );
        a.receive(&[6, 0, 10, 0, 20, 16]).unwrap();
        a.receive(&[8, 0, 11, 0, 2, 0, 4, 0, 1, 2, 3, 4]).unwrap();
        let result = a
            .receive(&[8, 0, 12, 0, 43, 0, 4, 0, 5, 6, 7, 8])
            .unwrap()
            .unwrap();
        assert_eq!(&result[..6], &[14, 1, 12, 0, 3, 0]);
        assert_eq!(&result[10..], &24u32.to_le_bytes());
        a.receive(&[6, 0, 13, 0, 20, 0]).unwrap();
        a.observe(1234);
        a.receive(&[6, 0, 14, 0, 20, 0]).unwrap(); // a new START resets the measurement
        a.observe(56);
        let result = a.receive(&[6, 0, 15, 0, 41, 4]).unwrap().unwrap();
        assert_eq!(&result[..6], &[14, 1, 15, 0, 11, 0]);
        assert_eq!(&result[10..], &56u32.to_le_bytes());
        for kind in [0x0840u16, 0x0880, 0x08c0] {
            let len = if kind == 0x08c0 { 18 } else { 14 };
            let mut p = vec![len, 0, 16, 0];
            u16le(&mut p, kind);
            p.resize(len as usize, 0);
            assert!(a.receive(&p).unwrap().is_none());
        }
    }
    #[test]
    fn malformed_measurements_are_rejected() {
        let mut a = AutoDetect::default();
        assert!(a.receive(&[6, 0, 0, 0, 41, 4]).is_err());
        let p = [8, 0, 1, 0, 2, 0, 4, 0, 1, 2, 3, 4];
        for length in 0..p.len() {
            assert!(a.receive(&p[..length]).is_err());
        }
        assert!(a.receive(&p).is_err());
        a.receive(&[6, 0, 1, 0, 20, 16]).unwrap();
        assert!(a.receive(&[8, 0, 1, 0, 2, 0, 4, 0, 1, 2, 3]).is_err());
    }
}
