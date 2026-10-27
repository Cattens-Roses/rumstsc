//! Independent RDP8 segmented bulk decoder. All reads and expansion are bounded.
use crate::{Result, invalid, wire::Reader};
const HISTORY: usize = 2_500_000;
const LIMIT: usize = 64 * 1024 * 1024;
#[derive(Default)]
pub(crate) struct Zgfx {
    history: Vec<u8>,
    position: usize,
}
struct Bits<'a> {
    data: &'a [u8],
    position: usize,
    end: usize,
}
impl Bits<'_> {
    fn read(&mut self, n: usize) -> Result<usize> {
        if n > self.end - self.position {
            return Err(invalid("truncated RDP8 token"));
        }
        let mut result = 0;
        for _ in 0..n {
            result = (result << 1)
                | ((self.data[self.position / 8] >> (7 - self.position % 8)) & 1) as usize;
            self.position += 1;
        }
        Ok(result)
    }
}
impl Zgfx {
    pub fn decode(&mut self, input: &[u8]) -> Result<Vec<u8>> {
        let mut r = Reader::new(input);
        let tag = r.u8()?;
        let mut output = vec![];
        match tag {
            0xe0 => self.segment(r.rest(), &mut output)?,
            0xe1 => {
                let count = r.u16()?;
                let size = r.u32()? as usize;
                if size > LIMIT || count == 0 {
                    return Err(invalid("RDP8 multipart size"));
                }
                for _ in 0..count {
                    let n = r.u32()? as usize;
                    self.segment(r.take(n)?, &mut output)?;
                    if output.len() > size {
                        return Err(invalid("RDP8 multipart expansion"));
                    }
                }
                if r.remaining() != 0 || output.len() != size {
                    return Err(invalid("RDP8 multipart length"));
                }
            }
            _ => return Err(invalid("RDP8 segment descriptor")),
        }
        Ok(output)
    }
    fn append(&mut self, byte: u8, out: &mut Vec<u8>, start: usize) -> Result<()> {
        if out.len() - start >= 65535 || out.len() >= LIMIT {
            return Err(invalid("RDP8 expansion limit"));
        }
        out.push(byte);
        self.history[self.position] = byte;
        self.position = (self.position + 1) % HISTORY;
        Ok(())
    }
    fn segment(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<()> {
        if input.is_empty() || input.len() > 66536 {
            return Err(invalid("RDP8 segment size"));
        }
        if self.history.is_empty() {
            self.history.resize(HISTORY, 0);
        }
        let flags = input[0];
        if flags & 15 != 4 {
            return Err(invalid("RDP8 compression type"));
        }
        let start = out.len();
        if flags & 0x20 == 0 {
            for &b in &input[1..] {
                self.append(b, out, start)?;
            }
            return Ok(());
        }
        if input.len() < 2 || input[input.len() - 1] > 7 {
            return Err(invalid("RDP8 padding"));
        }
        let data = &input[1..input.len() - 1];
        let padding = input[input.len() - 1] as usize;
        let end = data
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_sub(padding))
            .ok_or_else(|| invalid("RDP8 bit length"))?;
        let mut bits = Bits {
            data,
            position: 0,
            end,
        };
        // Prefix codes and distance ranges are the RDP8 wire codebook, not a
        // general-purpose compression library. Generic literals use 0 + eight bits.
        const DIST: &[(usize, usize, usize, usize)] = &[
            (5, 17, 5, 0),
            (5, 18, 7, 32),
            (5, 19, 9, 160),
            (5, 20, 10, 672),
            (5, 21, 12, 1696),
            (6, 44, 14, 5792),
            (6, 45, 15, 22176),
            (7, 92, 18, 54944),
            (7, 93, 20, 317088),
            (8, 188, 20, 1365664),
            (8, 189, 21, 2414240),
            (9, 380, 22, 4511392),
            (9, 381, 23, 8705696),
            (9, 382, 24, 17094304),
        ];
        const LITERALS: &[(usize, usize, u8)] = &[
            (5, 24, 0),
            (5, 25, 1),
            (6, 52, 2),
            (6, 53, 3),
            (6, 54, 255),
            (7, 110, 4),
            (7, 111, 5),
            (7, 112, 6),
            (7, 113, 7),
            (7, 114, 8),
            (7, 115, 9),
            (7, 116, 10),
            (7, 117, 11),
            (7, 118, 58),
            (7, 119, 59),
            (7, 120, 60),
            (7, 121, 61),
            (7, 122, 62),
            (7, 123, 63),
            (7, 124, 64),
            (7, 125, 128),
            (8, 252, 12),
            (8, 253, 56),
            (8, 254, 57),
            (8, 255, 102),
        ];
        while bits.position < bits.end {
            let mut code = bits.read(1)?;
            if code == 0 {
                let b = bits.read(8)? as u8;
                self.append(b, out, start)?;
                continue;
            }
            let mut found = false;
            for length in 2..=9 {
                code = (code << 1) | bits.read(1)?;
                if let Some(&(_, _, literal)) =
                    LITERALS.iter().find(|&&(n, c, _)| n == length && c == code)
                {
                    self.append(literal, out, start)?;
                    found = true;
                    break;
                }
                if let Some(&(_, _, extra, base)) =
                    DIST.iter().find(|&&(n, c, _, _)| n == length && c == code)
                {
                    let distance = base + bits.read(extra)?;
                    if distance == 0 {
                        let count = bits.read(15)?;
                        let align = (8 - bits.position % 8) % 8;
                        bits.read(align)?;
                        for _ in 0..count {
                            let b = bits.read(8)? as u8;
                            self.append(b, out, start)?;
                        }
                    } else {
                        if distance > HISTORY {
                            return Err(invalid("RDP8 history distance"));
                        }
                        let mut ones = 0;
                        while bits.read(1)? != 0 {
                            ones += 1;
                            if ones > 14 {
                                return Err(invalid("RDP8 match length"));
                            }
                        }
                        let count = if ones == 0 {
                            3
                        } else {
                            (1 << (ones + 1)) + bits.read(ones + 1)?
                        };
                        if count > 65535 - (out.len() - start) {
                            return Err(invalid("RDP8 match expansion"));
                        }
                        for _ in 0..count {
                            let b = self.history[(self.position + HISTORY - distance) % HISTORY];
                            self.append(b, out, start)?;
                        }
                    }
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(invalid("RDP8 prefix"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn compressed(code: &str) -> Vec<u8> {
        let code: Vec<_> = code.bytes().filter(|b| *b == b'0' || *b == b'1').collect();
        let mut p = vec![0xe0, 0x24];
        let mut bytes = vec![0; code.len().div_ceil(8)];
        for (i, b) in code.iter().enumerate() {
            bytes[i / 8] |= (b - b'0') << (7 - i % 8);
        }
        p.extend(bytes);
        p.push(((8 - code.len() % 8) % 8) as u8);
        p
    }
    #[test]
    fn literals_matches_and_cross_message_history() {
        let mut z = Zgfx::default();
        assert_eq!(z.decode(&[0xe0, 4, b'A', b'B', b'C']).unwrap(), b"ABC");
        assert_eq!(z.decode(&compressed("10001 00011 0")).unwrap(), b"ABC");
        assert_eq!(
            z.decode(&compressed("11000 11001 110100 110110")).unwrap(),
            [0, 1, 2, 255]
        );
        assert_eq!(
            z.decode(&compressed("0 01000001 10001 00001 10 00"))
                .unwrap(),
            b"AAAAA"
        );
    }
    #[test]
    fn unencoded_block_multipart_and_limits() {
        let mut z = Zgfx::default();
        let mut p = compressed("10001 00000 000000000000011 0000000");
        // The token ends at bit 25 and aligns to bit 32 before raw bytes.
        p.pop();
        p.extend(b"xyz");
        p.push(0);
        assert_eq!(z.decode(&p).unwrap(), b"xyz");
        let mut p = vec![0xe1, 2, 0, 6, 0, 0, 0, 4, 0, 0, 0, 4];
        p.extend(b"abc");
        p.extend([4, 0, 0, 0, 4]);
        p.extend(b"def");
        assert_eq!(z.decode(&p).unwrap(), b"abcdef");
        for p in [
            vec![],
            vec![0xe0],
            vec![0xe0, 0x24, 0xff],
            vec![0xe1, 1, 0, 0xff, 0xff, 0xff, 0xff],
        ] {
            assert!(z.decode(&p).is_err());
        }
    }
}
