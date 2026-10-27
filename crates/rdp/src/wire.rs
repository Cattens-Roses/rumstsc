//! Bounds-checked little-endian, BER and TPKT primitives.
use crate::{Result, invalid};
use std::io::{Read, Write};

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(invalid("truncated field"));
        }
        let start = self.pos;
        self.pos += n;
        Ok(&self.data[start..self.pos])
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn be16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }
    pub fn per_length(&mut self) -> Result<usize> {
        let first = self.u8()?;
        Ok(if first & 0x80 != 0 {
            (usize::from(first & 0x7f) << 8) | usize::from(self.u8()?)
        } else {
            first.into()
        })
    }
    pub fn ber(&mut self, tag: &[u8]) -> Result<&'a [u8]> {
        if self.take(tag.len())? != tag {
            return Err(invalid("unexpected BER tag"));
        }
        let first = self.u8()?;
        let n = if first & 0x80 == 0 {
            first as usize
        } else {
            let count = (first & 0x7f) as usize;
            if count == 0 || count > 4 {
                return Err(invalid("BER length"));
            }
            let mut n = 0usize;
            for &b in self.take(count)? {
                n = (n << 8) | b as usize;
            }
            n
        };
        self.take(n)
    }
}
pub fn u16le(out: &mut Vec<u8>, v: u16) {
    out.extend(v.to_le_bytes());
}
pub fn u32le(out: &mut Vec<u8>, v: u32) {
    out.extend(v.to_le_bytes());
}
pub fn per_length(out: &mut Vec<u8>, n: usize) -> Result<()> {
    if n > 0x7fff {
        return Err(invalid("PER payload too large"));
    }
    if n < 128 {
        out.push(n as u8);
    } else {
        out.extend(((n as u16) | 0x8000).to_be_bytes());
    }
    Ok(())
}
pub fn ber(tag: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    if data.len() < 128 {
        out.push(data.len() as u8);
    } else if data.len() <= 255 {
        out.extend([0x81, data.len() as u8]);
    } else {
        out.push(0x82);
        out.extend((data.len() as u16).to_be_bytes());
    }
    out.extend(data);
    out
}
pub fn tpkt(payload: &[u8]) -> Result<Vec<u8>> {
    let n = u16::try_from(payload.len() + 4).map_err(|_| invalid("TPKT too large"))?;
    let mut out = vec![3, 0];
    out.extend(n.to_be_bytes());
    out.extend(payload);
    Ok(out)
}
pub fn write_data(stream: &mut impl Write, payload: &[u8]) -> Result<()> {
    let mut data = vec![2, 0xf0, 0x80];
    data.extend(payload);
    stream.write_all(&tpkt(&data)?)?;
    Ok(())
}
pub fn read_tpkt(stream: &mut impl Read) -> Result<Vec<u8>> {
    let mut h = [0; 4];
    stream.read_exact(&mut h)?;
    if h[..2] != [3, 0] {
        return Err(invalid("expected TPKT v3 (fast-path was not advertised)"));
    }
    let n = u16::from_be_bytes([h[2], h[3]]) as usize;
    if n < 7 {
        return Err(invalid("short TPKT"));
    }
    let mut packet = vec![0; n - 4];
    stream.read_exact(&mut packet)?;
    Ok(packet)
}
pub fn read_data(stream: &mut impl Read) -> Result<Vec<u8>> {
    let p = read_tpkt(stream)?;
    if p.get(..3) != Some(&[2, 0xf0, 0x80]) {
        return Err(invalid("expected X.224 data TPDU"));
    }
    Ok(p[3..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncated_fields_are_errors() {
        for n in 0..4 {
            assert!(Reader::new(&[0; 4][..n]).u32().is_err());
        }
        assert!(Reader::new(&[4, 3]).ber(&[4]).is_err());
        assert!(read_tpkt(&mut &b"\x03\x00\x00\x03"[..]).is_err());
    }
    #[test]
    fn ber_roundtrip() {
        for n in [0, 127, 128, 255, 256, 4096] {
            let data = vec![7; n];
            let encoded = ber(&[4], &data);
            assert_eq!(Reader::new(&encoded).ber(&[4]).unwrap(), data);
        }
    }
    #[test]
    fn per_boundaries() {
        for n in [0, 127, 128, 32767] {
            let mut p = vec![];
            per_length(&mut p, n).unwrap();
            assert_eq!(Reader::new(&p).per_length().unwrap(), n);
        }
        assert!(per_length(&mut vec![], 32768).is_err());
    }
}
