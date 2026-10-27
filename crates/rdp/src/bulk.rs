//! RDP 4/5 MPPC bulk decompression (MS-RDPBCGR 3.1.8 and RFC 2118).
//! Separate from bitmap RLE: one bounded history persists across Share Data PDUs.
use crate::{Result, invalid};

#[derive(Default)]
pub(crate) struct Bulk {
    history: History,
}
#[derive(Default)]
struct History {
    bytes: Vec<u8>,
    position: usize,
}
impl Bulk {
    pub fn decode<'a>(&'a mut self, flags: u8, payload: &'a [u8]) -> Result<&'a [u8]> {
        if flags & 0x10 != 0 || flags & 15 > 1 {
            return Err(invalid("unsupported bulk compression flags"));
        }
        if flags & 0x80 != 0 {
            self.history.bytes.fill(0);
            self.history.position = 0;
        }
        if flags & 0x20 == 0 {
            if flags & 0x40 != 0 {
                return Err(invalid("bulk AT_FRONT without COMPRESSED"));
            }
            // Raw packets do not enter the compression history.
            return Ok(payload);
        }
        let level = (flags & 15) as usize;
        let size = if level == 0 { 8192 } else { 65536 };
        let h = &mut self.history;
        if h.bytes.is_empty() {
            h.bytes.resize(65536, 0);
        }
        if flags & 0x40 != 0 {
            h.position = 0;
        }
        if h.position > size {
            return Err(invalid("bulk history position"));
        }
        let start = h.position;
        let mut bits = Bits {
            bytes: payload,
            bit: 0,
        };
        while bits.remaining() >= 8 {
            if bits.read(1)? == 0 {
                let byte = bits.read(7)? as u8;
                h.literal(byte, size)?;
            } else if bits.read(1)? == 0 {
                let byte = 0x80 | bits.read(7)? as u8;
                h.literal(byte, size)?;
            } else {
                let offset = if bits.read(1)? == 0 {
                    if level == 0 {
                        320 + bits.read(13)?
                    } else {
                        2368 + bits.read(16)?
                    }
                } else if bits.read(1)? == 0 {
                    if level == 0 {
                        64 + bits.read(8)?
                    } else {
                        320 + bits.read(11)?
                    }
                } else if level == 0 {
                    bits.read(6)?
                } else if bits.read(1)? == 0 {
                    64 + bits.read(8)?
                } else {
                    bits.read(6)?
                };
                let mut ones = 0;
                while bits.read(1)? != 0 {
                    ones += 1;
                    if ones > if level == 0 { 11 } else { 14 } {
                        return Err(invalid("bulk match length prefix"));
                    }
                }
                let length = if ones == 0 {
                    3
                } else {
                    let n = ones + 1;
                    (1 << n) + bits.read(n)?
                };
                if offset >= size || length > size - h.position {
                    return Err(invalid("bulk history bounds"));
                }
                // Ring addressing is required for sources before position zero;
                // bytewise forward copying preserves overlapping repetitions.
                for _ in 0..length {
                    let source = h.position.wrapping_sub(offset) & (size - 1);
                    let byte = h.bytes[source];
                    h.literal(byte, size)?;
                }
            }
        }
        Ok(&h.bytes[start..h.position])
    }
}
impl History {
    fn literal(&mut self, byte: u8, limit: usize) -> Result<()> {
        if self.position >= limit {
            return Err(invalid("bulk history overflow"));
        }
        let destination = self
            .bytes
            .get_mut(self.position)
            .ok_or_else(|| invalid("bulk history overflow"))?;
        *destination = byte;
        self.position += 1;
        Ok(())
    }
}
struct Bits<'a> {
    bytes: &'a [u8],
    bit: usize,
}
impl Bits<'_> {
    fn remaining(&self) -> usize {
        self.bytes.len() * 8 - self.bit
    }
    fn read(&mut self, mut n: usize) -> Result<usize> {
        if n > self.remaining() {
            return Err(invalid("truncated bulk token"));
        }
        let mut value = 0;
        while n > 0 {
            let available = 8 - self.bit % 8;
            let take = n.min(available);
            value = (value << take)
                | ((self.bytes[self.bit / 8] as usize >> (available - take)) & ((1 << take) - 1));
            self.bit += take;
            n -= take;
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn packed(bits: &str) -> Vec<u8> {
        let bits: Vec<_> = bits.bytes().filter(|b| *b == b'0' || *b == b'1').collect();
        let mut bytes = vec![0; bits.len().div_ceil(8)];
        for (i, b) in bits.iter().enumerate() {
            bytes[i / 8] |= (b - b'0') << (7 - i % 8);
        }
        bytes
    }
    #[test]
    fn literals_overlap_history_and_raw_packets() {
        for (level, reference) in [(0, "1111 000011 0"), (1, "11111 000011 0")] {
            let mut b = Bulk::default();
            assert_eq!(b.decode(0xe0 | level, b"ABC").unwrap(), b"ABC");
            assert_eq!(b.decode(0x20 | level, &packed(reference)).unwrap(), b"ABC");
            b.decode(0, b"uncompressed").unwrap();
            let repeat = if level == 0 {
                "1111 000001 110 000"
            } else {
                "11111 000001 110 000"
            };
            assert_eq!(
                b.decode(0x20 | level, &packed(repeat)).unwrap(),
                b"CCCCCCCC"
            );
            assert_eq!(b.decode(0xa0 | level, &packed(reference)).unwrap(), &[0; 3]);
        }
        assert_eq!(
            Bulk::default()
                .decode(0xe0, &packed("10 1111111 10 0000000"))
                .unwrap(),
            &[255, 128]
        );
    }
    #[test]
    fn offset_ranges_ring_sources_and_front() {
        for (level, cases) in [
            (
                0,
                vec![
                    ("1111 000000", 0),
                    ("1111 111111", 63),
                    ("1110 00000000", 64),
                    ("1110 11111111", 319),
                    ("110 0000000000000", 320),
                    ("110 1111010111111", 8191),
                ],
            ),
            (
                1,
                vec![
                    ("11111 000000", 0),
                    ("11111 111111", 63),
                    ("11110 00000000", 64),
                    ("11110 11111111", 319),
                    ("1110 00000000000", 320),
                    ("1110 11111111111", 2367),
                    ("110 0000000000000000", 2368),
                    ("110 1111011010111111", 65535),
                ],
            ),
        ] {
            for (code, offset) in cases {
                let mut b = Bulk::default();
                b.decode(0xe0 | level, b"ABC").unwrap();
                let h = &mut b.history;
                let size = if level == 0 { 8192 } else { 65536 };
                let source = 0usize.wrapping_sub(offset) & (size - 1);
                h.bytes[source] = 99;
                let encoded = packed(&format!("{code} 0"));
                let result = b.decode(0x60 | level, &encoded).unwrap();
                assert_eq!(result[0], 99, "offset {offset}, level {level}");
            }
        }
    }
    #[test]
    fn all_length_classes_and_output_limit() {
        for level in 0..=1 {
            for n in 2..=if level == 0 { 12 } else { 15 } {
                for low in [0, (1 << n) - 1] {
                    let mut b = Bulk::default();
                    let offset = if level == 0 {
                        "1111 000001"
                    } else {
                        "11111 000001"
                    };
                    let code = format!("01000001 {offset} {}0 {low:0n$b}", "1".repeat(n - 1));
                    let length = (1 << n) + low;
                    let encoded = packed(&code);
                    let decoded = b.decode(0xe0 | level, &encoded).unwrap();
                    assert_eq!(decoded.len(), 1 + length);
                    assert!(decoded.iter().all(|v| *v == b'A'));
                    assert!(b.decode(0x20 | level, &vec![1; 65536]).is_err());
                }
            }
        }
    }
    #[test]
    fn invalid_tokens_and_flags_are_rejected() {
        for data in [&[0x80][..], &[0xff][..], &[0xff, 0xff, 0xff, 0xff][..]] {
            assert!(Bulk::default().decode(0xe0, data).is_err());
        }
        for flags in [0x10, 0x22, 0x40, 0x70] {
            assert!(Bulk::default().decode(flags, b"abc").is_err());
        }
    }
}
